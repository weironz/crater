//! `crater ui` (D-054, Phase 2): a read-only web dashboard over the deployment
//! state DB. Axum backend + htmx frontend (server-rendered HTML fragments,
//! polled every few seconds). Pure Rust (hyper/tower/tokio, no C); htmx.js is
//! embedded in the binary so it works air-gapped. The UI is a *view* — all
//! logic stays in the engine/CLI (D-036 spirit); it never holds product logic.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;

use anyhow::Result;
use axum::{
    http::header,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};

use crater_core::state::{self, StateStore, TursoStore};

/// htmx, vendored + embedded so the dashboard works with zero network (air-gap).
const HTMX_JS: &[u8] = include_bytes!("../assets/htmx.min.js");

pub async fn serve(bind: &str, port: u16) -> Result<()> {
    // Validate the DB is openable up front; handlers re-open per request so
    // they always see the latest writes from the CLI process (Turso cross-
    // process visibility — a fresh handle reads committed state, D-055).
    TursoStore::open().await?;
    let app = Router::new()
        .route("/", get(index))
        .route("/api/deployments", get(deployments_fragment))
        .route("/api/history", get(history_fragment))
        .route("/htmx.min.js", get(htmx_js));
    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .map_err(|e| anyhow::anyhow!("bad bind address {bind}:{port}: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("crater ui → http://{addr}  (read-only; Ctrl-C to stop)");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn htmx_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/javascript")], HTMX_JS)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn index() -> Html<String> {
    Html(format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>crater · deployments</title>
<script src="/htmx.min.js"></script>
<style>
  body {{ font: 14px/1.5 -apple-system, system-ui, sans-serif; margin: 2rem auto; max-width: 1000px; color: #1a1a2e; }}
  h1 {{ font-size: 1.4rem; }} h2 {{ font-size: 1.1rem; margin-top: 2rem; color: #444; }}
  table {{ border-collapse: collapse; width: 100%; }}
  th, td {{ text-align: left; padding: .4rem .7rem; border-bottom: 1px solid #eee; }}
  th {{ color: #888; font-weight: 600; font-size: .8rem; text-transform: uppercase; letter-spacing: .03em; }}
  td.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  .ok {{ color: #137333; }} .fail {{ color: #c5221f; }}
  .muted {{ color: #999; }} code {{ background: #f3f3f7; padding: 0 .3rem; border-radius: 3px; }}
  .hdr {{ display: flex; align-items: baseline; gap: .8rem; }}
</style>
</head>
<body>
<div class="hdr"><h1>crater · deployments</h1><span class="muted">read-only · auto-refresh 5s</span></div>
<div hx-get="/api/deployments" hx-trigger="load, every 5s">loading…</div>
<h2>history</h2>
<div hx-get="/api/history" hx-trigger="load, every 5s">loading…</div>
</body>
</html>"#
    ))
}

async fn deployments_fragment() -> Html<String> {
    let store = match TursoStore::open().await {
        Ok(s) => s,
        Err(e) => return Html(format!("<p class='fail'>db error: {}</p>", esc(&e.to_string()))),
    };
    let deps = match store.list_deployments().await {
        Ok(d) => d,
        Err(e) => return Html(format!("<p class='fail'>db error: {}</p>", esc(&e.to_string()))),
    };
    if deps.is_empty() {
        return Html("<p class='muted'>no deployments recorded</p>".into());
    }
    // Aggregate by deployment label (D-052/053): tasks, versions, host count,
    // latest applied, drift status (D-055), latest check time.
    #[derive(Default)]
    struct Agg {
        tasks: BTreeSet<String>,
        versions: BTreeSet<String>,
        hosts: usize,
        last: i64,
        ok: usize,
        drift: usize,
        checked: i64,
    }
    let mut by: BTreeMap<String, Agg> = BTreeMap::new();
    for d in deps {
        let e = by.entry(d.deployment).or_default();
        e.tasks.insert(d.name);
        e.versions.insert(d.version);
        e.hosts += 1;
        e.last = e.last.max(d.applied_at);
        e.checked = e.checked.max(d.checked_at);
        match d.status.as_str() {
            "ok" => e.ok += 1,
            "drift" => e.drift += 1,
            _ => {}
        }
    }
    let join = |s: BTreeSet<String>| {
        if s.len() == 1 { s.into_iter().next().unwrap() } else { format!("{} (mixed)", s.into_iter().collect::<Vec<_>>().join(",")) }
    };
    let mut rows = String::new();
    for (dep, a) in by {
        let (cls, status) = if a.drift > 0 {
            ("fail", format!("DRIFT {}/{}", a.drift, a.hosts))
        } else if a.ok == a.hosts && a.hosts > 0 {
            ("ok", format!("ok {}/{}", a.ok, a.hosts))
        } else {
            ("muted", "unknown".to_string())
        };
        let checked = if a.checked > 0 { state::fmt_epoch(a.checked) } else { "—".to_string() };
        rows.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td>{}</td><td class='num'>{}</td><td class='{}'>{}</td><td class='muted'>{}</td><td class='muted'>{}</td></tr>",
            esc(&dep), esc(&join(a.tasks)), esc(&join(a.versions)), a.hosts, cls, status, checked, state::fmt_epoch(a.last)
        ));
    }
    Html(format!(
        "<table><thead><tr><th>Deployment</th><th>Task</th><th>Version</th><th>Hosts</th><th>Status</th><th>Checked (UTC)</th><th>Last applied (UTC)</th></tr></thead><tbody>{rows}</tbody></table>"
    ))
}

async fn history_fragment() -> Html<String> {
    let store = match TursoStore::open().await {
        Ok(s) => s,
        Err(e) => return Html(format!("<p class='fail'>db error: {}</p>", esc(&e.to_string()))),
    };
    let runs = match store.history(50).await {
        Ok(r) => r,
        Err(e) => return Html(format!("<p class='fail'>db error: {}</p>", esc(&e.to_string()))),
    };
    if runs.is_empty() {
        return Html("<p class='muted'>no history</p>".into());
    }
    let mut rows = String::new();
    for r in runs {
        let cls = if r.result == "ok" { "ok" } else { "fail" };
        rows.push_str(&format!(
            "<tr><td class='muted'>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td><td>{}</td><td class='{}'>{}</td></tr>",
            state::fmt_epoch(r.ts), esc(&r.action), esc(&r.deployment), esc(&r.task), esc(&r.host), cls, esc(&r.result)
        ));
    }
    Html(format!(
        "<table><thead><tr><th>When (UTC)</th><th>Action</th><th>Deployment</th><th>Task</th><th>Host</th><th>Result</th></tr></thead><tbody>{rows}</tbody></table>"
    ))
}
