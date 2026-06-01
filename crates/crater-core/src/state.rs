//! Deployment state (D-051, Phase 1a). Two stores, by design:
//!
//! - **Per-target marker** (`/var/lib/crater/state/<task>.json`) — the AUTHORITATIVE
//!   record, written on the machine itself by `apply`, removed by `delete`. Any
//!   control machine can read it; survives control-side loss. Agentless: it's just
//!   a file the executor writes.
//! - **Control-side DB** ([`TursoStore`], `~/.crater/state.db`) — an aggregate/cache
//!   of what THIS control machine has applied + a job-run history, feeding
//!   `crater task list/history` and the web UI without touching every host.
//!
//! The DB is behind the [`StateStore`] trait so it can be swapped (redb, etc.)
//! without touching call sites.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::executor::Executor;

/// Where each target records what crater put on it.
pub const MARKER_DIR: &str = "/var/lib/crater/state";

/// On-target deployment marker (one JSON file per task).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Marker {
    pub name: String,
    pub version: String,
    /// What was applied: a task path, named task, or artifact ref.
    pub source: String,
    /// Unix epoch seconds when applied.
    pub applied_at: i64,
}

/// A deployment row in the control-side DB (= a marker + which host).
#[derive(Debug, Clone)]
pub struct Deployment {
    pub host: String,
    pub name: String,
    pub version: String,
    pub source: String,
    pub applied_at: i64,
}

/// One job-run history entry (apply or delete on a host).
#[derive(Debug, Clone)]
pub struct JobRun {
    pub ts: i64,
    pub action: String, // "apply" | "delete"
    pub task: String,
    pub host: String,
    pub result: String, // "ok" | "failed"
}

// ---- per-target markers (authoritative, written via the executor) -----------

/// Write the deployment marker on the target (apply success). Idempotent.
pub async fn write_marker(exec: &dyn Executor, m: &Marker) -> crate::Result<()> {
    exec.run(&format!("mkdir -p {MARKER_DIR}")).await?;
    let json = serde_json::to_vec_pretty(m)?;
    exec.write_file(&format!("{MARKER_DIR}/{}.json", m.name), &json).await
}

/// Remove the marker on the target (delete success). Idempotent (`rm -f`).
pub async fn remove_marker(exec: &dyn Executor, task: &str) -> crate::Result<()> {
    exec.run(&format!("rm -f {MARKER_DIR}/{task}.json")).await?;
    Ok(())
}

/// Read all markers on a target (authoritative `crater task list --host`).
pub async fn read_markers(exec: &dyn Executor) -> crate::Result<Vec<Marker>> {
    // One shot: cat every marker, each on its own line is invalid JSON, so we
    // emit a JSON array by joining with commas via the shell.
    let out = exec
        .run(&format!(
            "for f in {MARKER_DIR}/*.json; do [ -e \"$f\" ] && cat \"$f\"; done"
        ))
        .await?;
    let mut markers = Vec::new();
    // Markers are pretty-printed objects concatenated; parse with a streaming
    // deserializer that reads successive JSON values.
    let de = serde_json::Deserializer::from_str(&out.stdout).into_iter::<Marker>();
    for m in de {
        match m {
            Ok(m) => markers.push(m),
            Err(e) if e.is_eof() => break,
            Err(_) => break,
        }
    }
    Ok(markers)
}

// ---- control-side aggregate DB ----------------------------------------------

/// Aggregate/cache + history store (swappable). Async to match the DB backend.
#[async_trait]
pub trait StateStore {
    async fn record_apply(&self, host: &str, m: &Marker) -> crate::Result<()>;
    async fn record_delete(&self, host: &str, task: &str, ts: i64) -> crate::Result<()>;
    async fn list_deployments(&self) -> crate::Result<Vec<Deployment>>;
    async fn history(&self, limit: usize) -> crate::Result<Vec<JobRun>>;
}

/// Turso (pure-Rust SQLite) implementation, `~/.crater/state.db`.
pub struct TursoStore {
    db: turso::Database,
}

impl TursoStore {
    /// Open (creating if needed) the control-side state DB and ensure schema.
    pub async fn open() -> crate::Result<Self> {
        let dir = crate::store::ImageStore::home();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("state.db");
        let db = turso::Builder::new_local(path.to_str().unwrap())
            .build()
            .await
            .map_err(|e| anyhow::anyhow!("open state db: {e}"))?;
        let store = Self { db };
        store.init().await?;
        Ok(store)
    }

    async fn init(&self) -> crate::Result<()> {
        let conn = self.db.connect().map_err(|e| anyhow::anyhow!("db connect: {e}"))?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS deployments(
                 host TEXT NOT NULL,
                 task TEXT NOT NULL,
                 version TEXT NOT NULL,
                 source TEXT NOT NULL,
                 applied_at INTEGER NOT NULL,
                 PRIMARY KEY(host, task)
             );
             CREATE TABLE IF NOT EXISTS job_runs(
                 ts INTEGER NOT NULL,
                 action TEXT NOT NULL,
                 task TEXT NOT NULL,
                 host TEXT NOT NULL,
                 result TEXT NOT NULL
             );",
        )
        .await
        .map_err(|e| anyhow::anyhow!("init schema: {e}"))?;
        Ok(())
    }
}

#[async_trait]
impl StateStore for TursoStore {
    async fn record_apply(&self, host: &str, m: &Marker) -> crate::Result<()> {
        let conn = self.db.connect().map_err(|e| anyhow::anyhow!("db connect: {e}"))?;
        conn.execute(
            "INSERT INTO deployments(host, task, version, source, applied_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(host, task) DO UPDATE SET
                 version=excluded.version, source=excluded.source, applied_at=excluded.applied_at",
            (host, m.name.clone(), m.version.clone(), m.source.clone(), m.applied_at),
        )
        .await
        .map_err(|e| anyhow::anyhow!("record apply: {e}"))?;
        conn.execute(
            "INSERT INTO job_runs(ts, action, task, host, result) VALUES (?1, 'apply', ?2, ?3, 'ok')",
            (m.applied_at, m.name.clone(), host),
        )
        .await
        .map_err(|e| anyhow::anyhow!("record job: {e}"))?;
        Ok(())
    }

    async fn record_delete(&self, host: &str, task: &str, ts: i64) -> crate::Result<()> {
        let conn = self.db.connect().map_err(|e| anyhow::anyhow!("db connect: {e}"))?;
        conn.execute(
            "DELETE FROM deployments WHERE host=?1 AND task=?2",
            (host, task),
        )
        .await
        .map_err(|e| anyhow::anyhow!("record delete: {e}"))?;
        conn.execute(
            "INSERT INTO job_runs(ts, action, task, host, result) VALUES (?1, 'delete', ?2, ?3, 'ok')",
            (ts, task, host),
        )
        .await
        .map_err(|e| anyhow::anyhow!("record job: {e}"))?;
        Ok(())
    }

    async fn list_deployments(&self) -> crate::Result<Vec<Deployment>> {
        let conn = self.db.connect().map_err(|e| anyhow::anyhow!("db connect: {e}"))?;
        let mut rows = conn
            .query(
                "SELECT host, task, version, source, applied_at FROM deployments
                 ORDER BY host, task",
                (),
            )
            .await
            .map_err(|e| anyhow::anyhow!("list deployments: {e}"))?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.map_err(|e| anyhow::anyhow!("row: {e}"))? {
            out.push(Deployment {
                host: r.get::<String>(0).map_err(|e| anyhow::anyhow!("col: {e}"))?,
                name: r.get::<String>(1).map_err(|e| anyhow::anyhow!("col: {e}"))?,
                version: r.get::<String>(2).map_err(|e| anyhow::anyhow!("col: {e}"))?,
                source: r.get::<String>(3).map_err(|e| anyhow::anyhow!("col: {e}"))?,
                applied_at: r.get::<i64>(4).map_err(|e| anyhow::anyhow!("col: {e}"))?,
            });
        }
        Ok(out)
    }

    async fn history(&self, limit: usize) -> crate::Result<Vec<JobRun>> {
        let conn = self.db.connect().map_err(|e| anyhow::anyhow!("db connect: {e}"))?;
        let mut rows = conn
            .query(
                "SELECT ts, action, task, host, result FROM job_runs
                 ORDER BY ts DESC LIMIT ?1",
                (limit as i64,),
            )
            .await
            .map_err(|e| anyhow::anyhow!("history: {e}"))?;
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.map_err(|e| anyhow::anyhow!("row: {e}"))? {
            out.push(JobRun {
                ts: r.get::<i64>(0).map_err(|e| anyhow::anyhow!("col: {e}"))?,
                action: r.get::<String>(1).map_err(|e| anyhow::anyhow!("col: {e}"))?,
                task: r.get::<String>(2).map_err(|e| anyhow::anyhow!("col: {e}"))?,
                host: r.get::<String>(3).map_err(|e| anyhow::anyhow!("col: {e}"))?,
                result: r.get::<String>(4).map_err(|e| anyhow::anyhow!("col: {e}"))?,
            });
        }
        Ok(out)
    }
}

/// Current Unix epoch seconds.
pub fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Format epoch seconds as `YYYY-MM-DD HH:MM:SS` (UTC), no chrono dependency
/// (Howard Hinnant's civil-from-days).
pub fn fmt_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
}
