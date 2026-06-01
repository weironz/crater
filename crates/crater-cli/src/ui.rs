//! `crater ui` (D-054/056): a read-only web dashboard over the deployment state
//! DB. Axum backend + htmx frontend (server-rendered fragments, polled). Pure
//! Rust (hyper/tower/tokio, no C); htmx.js + all styling are embedded so it
//! works air-gapped (no CDN/toolchain). Modern dark theme. The UI is a *view* —
//! logic stays in the engine/CLI (D-036); handlers only read the state DB.

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
    // Validate the DB is openable up front; handlers re-open per request so they
    // always see the latest writes from the CLI process (Turso cross-process
    // visibility — a fresh handle reads committed state, D-056).
    TursoStore::open().await?;
    let app = Router::new()
        .route("/", get(index))
        .route("/api/stats", get(stats_fragment))
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

/// Full page shell — static, so no `format!` (CSS braces stay literal).
const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>crater · control</title>
<script src="/htmx.min.js"></script>
<style>
  :root{
    --bg:#0c0e15; --surface:#151823; --surface-2:#1d2130; --border:#272c3b;
    --text:#e7e9f0; --muted:#8990a3; --faint:#5a6076;
    --accent:#ff6b35; --accent-2:#ffa94d;
    --ok:#34d399; --ok-bg:rgba(52,211,153,.13);
    --drift:#f87171; --drift-bg:rgba(248,113,113,.14);
    --unknown:#9aa0b3; --unknown-bg:rgba(154,160,179,.10);
    --radius:14px; --shadow:0 1px 2px rgba(0,0,0,.4),0 8px 24px rgba(0,0,0,.18);
  }
  *{box-sizing:border-box}
  body{margin:0;background:radial-gradient(1200px 600px at 80% -10%,rgba(255,107,53,.08),transparent 60%),var(--bg);
       color:var(--text);font:14px/1.55 "Inter",system-ui,-apple-system,"Segoe UI",sans-serif;
       -webkit-font-smoothing:antialiased;min-height:100vh}
  .topbar{display:flex;align-items:center;justify-content:space-between;padding:.95rem 1.6rem;
          border-bottom:1px solid var(--border);background:rgba(13,15,22,.7);backdrop-filter:blur(8px);
          position:sticky;top:0;z-index:10}
  .brand{font-weight:750;font-size:1.08rem;letter-spacing:.01em;display:flex;align-items:center;gap:.55rem}
  .brand .mark{color:var(--accent);font-size:1.2rem;filter:drop-shadow(0 0 6px rgba(255,107,53,.5))}
  .brand .sub{color:var(--muted);font-weight:500;font-size:.85rem;border-left:1px solid var(--border);padding-left:.55rem;margin-left:.2rem}
  .meta{color:var(--muted);font-size:.78rem;display:flex;align-items:center;gap:.5rem}
  .live{display:inline-flex;align-items:center;gap:.4rem;color:var(--ok)}
  .live .dot{width:7px;height:7px;border-radius:50%;background:var(--ok);box-shadow:0 0 0 0 rgba(52,211,153,.5);animation:pulse 2s infinite}
  @keyframes pulse{0%{box-shadow:0 0 0 0 rgba(52,211,153,.5)}70%{box-shadow:0 0 0 6px rgba(52,211,153,0)}100%{box-shadow:0 0 0 0 rgba(52,211,153,0)}}
  main{max-width:1140px;margin:1.6rem auto;padding:0 1.6rem;display:flex;flex-direction:column;gap:1.4rem}
  .stats{display:grid;grid-template-columns:repeat(4,1fr);gap:1rem}
  .card{background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);
        padding:1.05rem 1.25rem;box-shadow:var(--shadow);position:relative;overflow:hidden}
  .card::before{content:"";position:absolute;inset:0 auto 0 0;width:3px;background:var(--accent);opacity:.0}
  .card.ok::before{background:var(--ok);opacity:.7}
  .card.drift::before{background:var(--drift);opacity:.85}
  .card .label{color:var(--muted);font-size:.72rem;text-transform:uppercase;letter-spacing:.07em}
  .card .value{font-size:2rem;font-weight:750;margin-top:.35rem;font-variant-numeric:tabular-nums;line-height:1}
  .card.drift .value{color:var(--drift)} .card.ok .value{color:var(--ok)}
  .panel{background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);
         overflow:hidden;box-shadow:var(--shadow)}
  .panel>h2{margin:0;padding:.85rem 1.25rem;font-size:.78rem;text-transform:uppercase;letter-spacing:.06em;
            color:var(--muted);border-bottom:1px solid var(--border);display:flex;align-items:center;gap:.5rem}
  .panel>h2 .mk{color:var(--accent)}
  table{width:100%;border-collapse:collapse;font-size:.875rem}
  th{text-align:left;padding:.6rem 1.25rem;color:var(--faint);font-size:.7rem;text-transform:uppercase;
     letter-spacing:.05em;font-weight:600}
  td{padding:.72rem 1.25rem;border-top:1px solid var(--border);white-space:nowrap}
  tbody tr{transition:background .12s} tbody tr:hover{background:var(--surface-2)}
  .num{text-align:right;font-variant-numeric:tabular-nums}
  .muted{color:var(--muted)} .mono{font-family:ui-monospace,"SF Mono",Menlo,monospace}
  code{font-family:ui-monospace,"SF Mono",Menlo,monospace;background:var(--surface-2);
       padding:.12rem .45rem;border-radius:7px;font-size:.85em;color:var(--accent-2);border:1px solid var(--border)}
  .pill{display:inline-flex;align-items:center;gap:.35rem;padding:.18rem .6rem;border-radius:999px;
        font-size:.72rem;font-weight:650;letter-spacing:.02em}
  .pill .d{width:6px;height:6px;border-radius:50%;background:currentColor}
  .pill.ok{color:var(--ok);background:var(--ok-bg)}
  .pill.drift{color:var(--drift);background:var(--drift-bg)}
  .pill.unknown{color:var(--unknown);background:var(--unknown-bg)}
  .pill.apply{color:var(--accent-2);background:rgba(255,169,77,.12)}
  .pill.delete{color:var(--unknown);background:var(--unknown-bg)}
  .empty{padding:2rem 1.25rem;color:var(--faint);text-align:center}
  .fail{color:var(--drift)}
  @media(max-width:760px){.stats{grid-template-columns:repeat(2,1fr)}}
</style>
</head>
<body>
<header class="topbar">
  <div class="brand"><span class="mark">▲</span> crater <span class="sub">control</span></div>
  <div class="meta"><span class="live"><span class="dot"></span>live</span> · read-only · refresh 5s</div>
</header>
<main>
  <section class="stats" hx-get="/api/stats" hx-trigger="load, every 5s" hx-swap="innerHTML"></section>
  <section class="panel">
    <h2><span class="mk">◆</span> Deployments</h2>
    <div hx-get="/api/deployments" hx-trigger="load, every 5s" hx-swap="innerHTML"><div class="empty">loading…</div></div>
  </section>
  <section class="panel">
    <h2><span class="mk">◷</span> Activity</h2>
    <div hx-get="/api/history" hx-trigger="load, every 5s" hx-swap="innerHTML"><div class="empty">loading…</div></div>
  </section>
</main>
</body>
</html>"#;

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

fn card(label: &str, value: usize, kind: &str) -> String {
    format!(
        "<div class='card {kind}'><div class='label'>{label}</div><div class='value'>{value}</div></div>"
    )
}

async fn stats_fragment() -> Html<String> {
    let store = match TursoStore::open_read().await {
        Ok(s) => s,
        Err(e) => return Html(format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string()))),
    };
    let deps = store.list_deployments().await.unwrap_or_default();
    let mut deployments: BTreeSet<String> = BTreeSet::new();
    let mut hosts: BTreeSet<String> = BTreeSet::new();
    let (mut ok, mut drift) = (0usize, 0usize);
    for d in &deps {
        deployments.insert(d.deployment.clone());
        hosts.insert(d.host.clone());
        match d.status.as_str() {
            "ok" => ok += 1,
            "drift" => drift += 1,
            _ => {}
        }
    }
    Html(format!(
        "{}{}{}{}",
        card("Deployments", deployments.len(), ""),
        card("Hosts", hosts.len(), ""),
        card("Healthy", ok, "ok"),
        card("Drift", drift, if drift > 0 { "drift" } else { "" }),
    ))
}

async fn deployments_fragment() -> Html<String> {
    let store = match TursoStore::open_read().await {
        Ok(s) => s,
        Err(e) => return Html(format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string()))),
    };
    let deps = match store.list_deployments().await {
        Ok(d) => d,
        Err(e) => return Html(format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string()))),
    };
    if deps.is_empty() {
        return Html("<div class='empty'>no deployments recorded</div>".into());
    }
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
        let pill = if a.drift > 0 {
            format!("<span class='pill drift'><span class='d'></span>DRIFT {}/{}</span>", a.drift, a.hosts)
        } else if a.ok == a.hosts && a.hosts > 0 {
            format!("<span class='pill ok'><span class='d'></span>ok {}/{}</span>", a.ok, a.hosts)
        } else {
            "<span class='pill unknown'><span class='d'></span>unknown</span>".to_string()
        };
        let checked = if a.checked > 0 { state::fmt_epoch(a.checked) } else { "—".to_string() };
        rows.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td class='mono muted'>{}</td><td class='num'>{}</td><td>{}</td><td class='muted mono'>{}</td><td class='muted mono'>{}</td></tr>",
            esc(&dep), esc(&join(a.tasks)), esc(&join(a.versions)), a.hosts, pill, checked, state::fmt_epoch(a.last)
        ));
    }
    Html(format!(
        "<table><thead><tr><th>Deployment</th><th>Task</th><th>Version</th><th class='num'>Hosts</th><th>Status</th><th>Checked (UTC)</th><th>Last applied (UTC)</th></tr></thead><tbody>{rows}</tbody></table>"
    ))
}

async fn history_fragment() -> Html<String> {
    let store = match TursoStore::open_read().await {
        Ok(s) => s,
        Err(e) => return Html(format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string()))),
    };
    let runs = match store.history(50).await {
        Ok(r) => r,
        Err(e) => return Html(format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string()))),
    };
    if runs.is_empty() {
        return Html("<div class='empty'>no activity yet</div>".into());
    }
    let mut rows = String::new();
    for r in runs {
        let action = format!("<span class='pill {a}'>{a}</span>", a = esc(&r.action));
        let res = if r.result == "ok" {
            "<span class='pill ok'><span class='d'></span>ok</span>"
        } else {
            "<span class='pill drift'><span class='d'></span>failed</span>"
        };
        rows.push_str(&format!(
            "<tr><td class='muted mono'>{}</td><td>{}</td><td><code>{}</code></td><td>{}</td><td class='mono'>{}</td><td>{}</td></tr>",
            state::fmt_epoch(r.ts), action, esc(&r.deployment), esc(&r.task), esc(&r.host), res
        ));
    }
    Html(format!(
        "<table><thead><tr><th>When (UTC)</th><th>Action</th><th>Deployment</th><th>Task</th><th>Host</th><th>Result</th></tr></thead><tbody>{rows}</tbody></table>"
    ))
}
