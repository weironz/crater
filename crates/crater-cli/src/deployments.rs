//! Deployment state (D-051/D-055): `crater task list / show / history`,
//! gathering instances from the control DB or the targets' authoritative
//! markers, optional drift verify, and the control-side record helpers.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use tracing::info;

use crater_core::arch;
use crater_core::executor::Executor;
use crater_core::os;
use crater_core::state::{self, Marker, StateStore};

use crate::target::{connect_executor, TargetOpts};

/// A gathered deployment instance + optional drift status (Some(true)=ok,
/// Some(false)=DRIFT, None=not checked / no verify phase).
pub(crate) struct DepRow {
    dep: crater_core::state::Deployment,
    status: Option<bool>,
}

/// Gather deployment instances (one per host×task) from the right source:
/// the control DB, or — when a target is given — the authoritative markers read
/// off the hosts (D-051). With `verify`, re-run each deployment's verify phase
/// on the target to detect drift (D-055; requires `--host`/`-i`).
pub(crate) async fn gather_deployments(
    target: &TargetOpts,
    verify: bool,
) -> Result<Vec<DepRow>> {
    use crater_core::state::Deployment;
    if target.inventory.is_some() || target.host.is_some() {
        let hosts = target.hosts()?;
        // Persist verify results so the read-only UI can show drift (D-055).
        let store = if verify { state::TursoStore::open().await.ok() } else { None };
        let now = state::now_epoch();
        let mut out = Vec::new();
        for h in &hosts {
            let exec = connect_executor(h, true).await?;
            for m in state::read_markers(exec.as_ref()).await.unwrap_or_default() {
                let status = if verify {
                    let s = verify_on_host(exec.as_ref(), &m.source).await;
                    if let Some(st) = &store {
                        let _ = st.record_verify(&h.name, &m.name, s, now).await;
                    }
                    s
                } else {
                    None
                };
                let (status_str, checked) = match status {
                    Some(true) => ("ok".to_string(), now),
                    Some(false) => ("drift".to_string(), now),
                    None => ("unknown".to_string(), 0),
                };
                out.push(DepRow {
                    dep: Deployment {
                        host: h.name.clone(),
                        name: m.name.clone(),
                        deployment: if m.deployment.is_empty() { m.name } else { m.deployment },
                        version: m.version,
                        source: m.source,
                        applied_at: m.applied_at,
                        status: status_str,
                        checked_at: checked,
                    },
                    status,
                });
            }
        }
        Ok(out)
    } else {
        Ok(state::TursoStore::open()
            .await?
            .list_deployments()
            .await?
            .into_iter()
            .map(|dep| DepRow { dep, status: None })
            .collect())
    }
}

/// Drift check (D-055): resolve the task from its `source`, re-run only its
/// **verify-phase** actions on the host (read-only), report ok/DRIFT. Returns
/// `None` when the source can't be resolved locally or the task has no verify
/// phase (no health probe to judge by).
/// Resolve a deployment's recorded `source` (named task or task-file path) to a
/// local task file. Returns None for artifact-ref sources (not resolvable here).
pub(crate) fn resolve_task_path(source: &str) -> Option<PathBuf> {
    use crater_core::task::is_task_file;
    let p = PathBuf::from(source);
    if p.is_file() && is_task_file(&p) {
        return Some(p);
    }
    let named = PathBuf::from("tasks").join(format!("{source}.yaml"));
    if named.is_file() && is_task_file(&named) {
        return Some(named);
    }
    None
}

pub(crate) async fn verify_on_host(exec: &dyn Executor, source: &str) -> Option<bool> {
    use crater_core::engine::{self, Op, PlanContext, Phase};
    use crater_core::task::TaskFile;
    let path = resolve_task_path(source)?;
    let task = TaskFile::from_yaml_file(&path).ok()?;
    let spec_dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    let ver = task.effective_vars().get("version").cloned().unwrap_or_else(|| "latest".into());
    let mut ctx = PlanContext::new(os::detect_via(exec).await, ver, spec_dir);
    ctx.target_arch = arch::detect_via(exec).await;
    for (k, v) in &task.effective_vars() {
        ctx.vars.insert(k.clone(), v.clone());
    }
    for m in &task.materials {
        ctx.add_material(m.clone());
    }
    // Verify-phase actions only, `needs` cleared (we run them standalone).
    let verify_actions: Vec<_> = task
        .actions
        .iter()
        .filter(|a| a.phase == Phase::Verify)
        .map(|a| {
            let mut a = a.clone();
            a.needs.clear();
            a
        })
        .collect();
    if verify_actions.is_empty() {
        return None; // no health probe to judge drift by
    }
    let steps = engine::plan_from_task(&verify_actions, &ctx).ok()?;
    for s in &steps {
        if let Op::Shell { cmd, .. } = &s.op {
            match exec.run(cmd).await {
                Ok(o) if o.ok() => {}
                _ => return Some(false), // a verify check failed → drift
            }
        }
    }
    Some(true)
}

pub(crate) fn status_label(s: Option<bool>) -> &'static str {
    match s {
        Some(true) => "ok",
        Some(false) => "DRIFT",
        None => "?",
    }
}

/// `crater task list` (D-051/052/053): **deployment-centric** — one row per
/// deployment, hosts aggregated as a count. `--verify` adds a drift STATUS.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn task_list(target: TargetOpts, verify: bool) -> Result<()> {
    let from_targets = target.inventory.is_some() || target.host.is_some();
    if verify && !from_targets {
        anyhow::bail!("--verify needs --host or -i (it re-runs the verify phase on the targets)");
    }
    let rows = gather_deployments(&target, verify).await?;
    if rows.is_empty() {
        if from_targets {
            info!("no deployments found on the target(s)");
        } else {
            info!("no deployments recorded in the control DB (~/.crater/state.db); use --host/-i to read targets directly");
        }
        return Ok(());
    }
    // Aggregate by deployment label: tasks, versions, host count, latest, drift.
    struct Agg {
        tasks: BTreeSet<String>,
        versions: BTreeSet<String>,
        hosts: usize,
        last: i64,
        ok: usize,
        drift: usize,
    }
    let mut by_dep: BTreeMap<String, Agg> = BTreeMap::new();
    for r in rows {
        let e = by_dep.entry(r.dep.deployment).or_insert_with(|| Agg {
            tasks: BTreeSet::new(),
            versions: BTreeSet::new(),
            hosts: 0,
            last: 0,
            ok: 0,
            drift: 0,
        });
        e.tasks.insert(r.dep.name);
        e.versions.insert(r.dep.version);
        e.hosts += 1;
        e.last = e.last.max(r.dep.applied_at);
        match r.status {
            Some(true) => e.ok += 1,
            Some(false) => e.drift += 1,
            None => {}
        }
    }
    let join_or_mixed = |s: BTreeSet<String>| {
        if s.len() == 1 {
            s.into_iter().next().unwrap()
        } else {
            format!("{} (mixed)", s.into_iter().collect::<Vec<_>>().join(","))
        }
    };
    if verify {
        println!("{:<16} {:<12} {:<14} {:>6}  {:<14} LAST APPLIED (UTC)", "DEPLOYMENT", "TASK", "VERSION", "HOSTS", "STATUS");
    } else {
        println!("{:<16} {:<12} {:<14} {:>6}  LAST APPLIED (UTC)", "DEPLOYMENT", "TASK", "VERSION", "HOSTS");
    }
    for (dep, a) in by_dep {
        if verify {
            let status = if a.drift > 0 {
                format!("DRIFT {}/{}", a.drift, a.hosts)
            } else {
                format!("ok {}/{}", a.ok, a.hosts)
            };
            println!(
                "{:<16} {:<12} {:<14} {:>6}  {:<14} {}",
                dep, join_or_mixed(a.tasks), join_or_mixed(a.versions), a.hosts, status, state::fmt_epoch(a.last)
            );
        } else {
            println!(
                "{:<16} {:<12} {:<14} {:>6}  {}",
                dep, join_or_mixed(a.tasks), join_or_mixed(a.versions), a.hosts, state::fmt_epoch(a.last)
            );
        }
    }
    info!("(host names: crater task show <deployment>)");
    Ok(())
}

/// `crater task show <name>` (D-051): one deployment's per-host instances;
/// `--verify` adds per-host drift status.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn task_show(name: &str, target: TargetOpts, verify: bool) -> Result<()> {
    if verify && target.inventory.is_none() && target.host.is_none() {
        anyhow::bail!("--verify needs --host or -i (it re-runs the verify phase on the targets)");
    }
    let mut rows = gather_deployments(&target, verify).await?;
    rows.retain(|r| r.dep.deployment == name || r.dep.name == name);
    if rows.is_empty() {
        info!("deployment '{name}' has no recorded instances");
        return Ok(());
    }
    if verify {
        println!("{:<16} {:<12} {:<10} {:<8} {:<20} SOURCE", "HOST", "TASK", "VERSION", "STATUS", "APPLIED (UTC)");
        for r in rows {
            println!(
                "{:<16} {:<12} {:<10} {:<8} {:<20} {}",
                r.dep.host, r.dep.name, r.dep.version, status_label(r.status), state::fmt_epoch(r.dep.applied_at), r.dep.source
            );
        }
    } else {
        println!("{:<16} {:<12} {:<10} {:<20} SOURCE", "HOST", "TASK", "VERSION", "APPLIED (UTC)");
        for r in rows {
            println!(
                "{:<16} {:<12} {:<10} {:<20} {}",
                r.dep.host, r.dep.name, r.dep.version, state::fmt_epoch(r.dep.applied_at), r.dep.source
            );
        }
    }
    Ok(())
}

/// `crater task history` (D-051): recent apply/delete runs from the control DB.
pub(crate) async fn task_history(limit: usize) -> Result<()> {
    let store = state::TursoStore::open().await?;
    let runs = store.history(limit).await?;
    if runs.is_empty() {
        info!("no history recorded in the control DB (~/.crater/state.db)");
        return Ok(());
    }
    println!("{:<20} {:<8} {:<14} {:<12} {:<16} RESULT", "WHEN (UTC)", "ACTION", "DEPLOYMENT", "TASK", "HOST");
    for r in runs {
        println!(
            "{:<20} {:<8} {:<14} {:<12} {:<16} {}",
            state::fmt_epoch(r.ts), r.action, r.deployment, r.task, r.host, r.result
        );
    }
    Ok(())
}

/// Record apply/delete outcomes to the control-side aggregate DB (D-051).
pub(crate) async fn record_deployments(
    task: &crater_core::task::TaskFile,
    source: &str,
    deployment: &str,
    teardown: bool,
    hosts: &[String],
) -> Result<()> {
    if hosts.is_empty() {
        return Ok(());
    }
    let store = state::TursoStore::open().await?;
    let ver = task.effective_vars().get("version").cloned().unwrap_or_else(|| "latest".into());
    let ts = state::now_epoch();
    for h in hosts {
        if teardown {
            store.record_delete(h, &task.name, ts).await?;
        } else {
            let m = Marker {
                name: task.name.clone(),
                deployment: deployment.to_string(),
                version: ver.clone(),
                source: source.to_string(),
                applied_at: ts,
            };
            store.record_apply(h, &m).await?;
        }
    }
    Ok(())
}
