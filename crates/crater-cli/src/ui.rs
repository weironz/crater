//! `crater ui` (D-054/056/057/058/059/099): a web dashboard over the deployment
//! state DB. Axum + htmx (server-rendered fragments, polled). Pure Rust
//! (hyper/tower/tokio, no C); htmx.js + CSS embedded → works air-gapped.
//! Sidebar nav (Dashboard / Hosts / Host groups / Tasks). Light default theme +
//! dark toggle. The UI is a *view* — logic stays in the engine/CLI (D-036);
//! write actions (Verify / Heal / Delete) shell out to the CLI as background
//! JOBS whose output streams into a polled log panel (D-099); `--token` gates
//! everything when exposed beyond localhost.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::{
    extract::{Path as AxPath, Request, State},
    http::header,
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};

use crater_core::state::{self, StateStore, TursoStore};

/// htmx, vendored + embedded so the dashboard works with zero network (air-gap).
const HTMX_JS: &[u8] = include_bytes!("../assets/htmx.min.js");

/// Conventional inventory the write actions use (no launch flag, like AWX's
/// configured inventory). Read at click time; missing ⇒ a runtime note.
const INVENTORY: &str = "inventory.yaml";

// ---- background jobs (D-099) -------------------------------------------------
// Write actions used to BLOCK the HTTP request until the CLI finished — minutes
// for a real deployment. Now they spawn the CLI as a job; the UI polls the
// job's log fragment until it exits.

#[derive(Default)]
struct Job {
    title: String,
    lines: Vec<String>,
    done: bool,
    ok: bool,
}

#[derive(Clone, Default)]
struct AppState {
    jobs: Arc<Mutex<BTreeMap<u64, Job>>>,
    next_id: Arc<Mutex<u64>>,
    token: Option<String>,
}

impl AppState {
    /// Spawn `crater <args>` as a background job; returns the job id.
    fn spawn_job(&self, title: String, args: Vec<String>) -> u64 {
        let id = {
            let mut n = self.next_id.lock().unwrap();
            *n += 1;
            *n
        };
        self.jobs.lock().unwrap().insert(id, Job { title, ..Default::default() });
        let jobs = self.jobs.clone();
        tokio::spawn(async move {
            let push = |jobs: &Arc<Mutex<BTreeMap<u64, Job>>>, line: String| {
                if let Some(j) = jobs.lock().unwrap().get_mut(&id) {
                    j.lines.push(line);
                }
            };
            let exe = match std::env::current_exe() {
                Ok(e) => e,
                Err(e) => {
                    if let Some(j) = jobs.lock().unwrap().get_mut(&id) {
                        j.lines.push(format!("spawn failed: {e}"));
                        j.done = true;
                    }
                    return;
                }
            };
            use tokio::io::{AsyncBufReadExt, BufReader};
            let child = tokio::process::Command::new(exe)
                .args(&args)
                .env("CRATER_LOG", "info")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(e) => {
                    push(&jobs, format!("spawn failed: {e}"));
                    if let Some(j) = jobs.lock().unwrap().get_mut(&id) {
                        j.done = true;
                    }
                    return;
                }
            };
            // Interleave stdout+stderr as they arrive (tracing logs go to stderr).
            let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
            let mut err = BufReader::new(child.stderr.take().unwrap()).lines();
            let (jo, je) = (jobs.clone(), jobs.clone());
            let to = tokio::spawn(async move {
                while let Ok(Some(l)) = out.next_line().await {
                    if let Some(j) = jo.lock().unwrap().get_mut(&id) {
                        j.lines.push(l);
                    }
                }
            });
            let te = tokio::spawn(async move {
                while let Ok(Some(l)) = err.next_line().await {
                    if let Some(j) = je.lock().unwrap().get_mut(&id) {
                        j.lines.push(l);
                    }
                }
            });
            let status = child.wait().await;
            let _ = to.await;
            let _ = te.await;
            if let Some(j) = jobs.lock().unwrap().get_mut(&id) {
                j.ok = status.map(|s| s.success()).unwrap_or(false);
                j.done = true;
            }
        });
        id
    }
}

/// Token gate (D-099): when a token is configured, every request must carry it
/// — `?token=` (first visit; answered with a cookie redirect), the cookie, or
/// an `Authorization: Bearer` header. Constant requirement, no sessions.
async fn auth_mw(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let Some(want) = &st.token else { return next.run(req).await };
    // ?token=<t> → set cookie, redirect to clean URL (login-by-link).
    if let Some(q) = req.uri().query() {
        if let Some(t) = q.split('&').find_map(|kv| kv.strip_prefix("token=")) {
            if t == want {
                return (
                    [(header::SET_COOKIE, format!("crater_token={t}; Path=/; SameSite=Strict"))],
                    Redirect::to(req.uri().path()),
                )
                    .into_response();
            }
        }
    }
    let cookie_ok = req
        .headers()
        .get(header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .is_some_and(|c| c.split(';').any(|kv| kv.trim() == format!("crater_token={want}")));
    let bearer_ok = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h == format!("Bearer {want}"));
    if cookie_ok || bearer_ok {
        return next.run(req).await;
    }
    (
        axum::http::StatusCode::UNAUTHORIZED,
        Html("<h3>401</h3><p>带 token 访问:<code>http://&lt;host&gt;:&lt;port&gt;/?token=&lt;token&gt;</code>(之后走 cookie)</p>"),
    )
        .into_response()
}

pub async fn serve(bind: &str, port: u16, token: Option<String>) -> Result<()> {
    // Validate the DB is openable up front; handlers re-open per request so they
    // always see the latest writes from the CLI process (Turso cross-process
    // visibility — a fresh handle reads committed state, D-056).
    TursoStore::open().await?;
    // Exposure guard (D-099): the UI can apply/delete — refuse a non-localhost
    // bind without a token instead of silently serving the LAN.
    let local = bind == "127.0.0.1" || bind == "::1" || bind == "localhost";
    if !local && token.is_none() {
        anyhow::bail!("--bind {bind} 暴露到本机之外,必须配 --token <t>(UI 能 apply/delete)");
    }
    let st = AppState { token, ..Default::default() };
    let app = Router::new()
        .route("/", get(index))
        .route("/view/dashboard", get(view_dashboard))
        .route("/view/tasks", get(view_tasks))
        .route("/view/hosts", get(view_hosts))
        .route("/view/groups", get(view_groups))
        .route("/api/stats", get(stats_fragment))
        .route("/api/deployments", get(deployments_fragment))
        .route("/api/history", get(history_fragment))
        .route("/api/hosts", get(hosts_fragment))
        .route("/api/verify", post(verify_action))
        .route("/api/plan/{deployment}", post(plan_action))
        .route("/api/apply/{deployment}", post(apply_action))
        .route("/api/delete/{deployment}", post(delete_action))
        .route("/api/job/{id}", get(job_fragment))
        // ---- 契约 API(阶段 1)----
        // 把已有的机器可读契约接出来,供编辑器与表单消费。见 ui_contract.rs。
        .route("/api/types", get(crate::ui_contract::types))
        .route("/api/schema", post(crate::ui_contract::schema))
        .route("/api/lint", post(crate::ui_contract::lint))
        .route("/api/inventory/skeleton", post(crate::ui_contract::inventory_skeleton))
        .route("/htmx.min.js", get(htmx_js))
        .layer(middleware::from_fn_with_state(st.clone(), auth_mw))
        .with_state(st);
    let addr: SocketAddr = format!("{bind}:{port}")
        .parse()
        .map_err(|e| anyhow::anyhow!("bad bind address {bind}:{port}: {e}"))?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!("crater ui → http://{addr}  (Ctrl-C to stop)");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn htmx_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/javascript")], HTMX_JS)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

// ---- page shell (static; no format! so CSS braces stay literal) -------------

const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>crater · control</title>
<script src="/htmx.min.js"></script>
<style>
  :root{ /* light (default) */
    --bg:#f5f6f8; --surface:#ffffff; --surface-2:#f1f3f6; --border:#e4e7ec;
    --text:#1a1d29; --muted:#667085; --faint:#98a2b3;
    --accent:#ea580c; --accent-2:#c2410c;
    --ok:#059669; --ok-bg:rgba(5,150,105,.10);
    --drift:#dc2626; --drift-bg:rgba(220,38,38,.08);
    --unknown:#667085; --unknown-bg:rgba(102,112,133,.08);
    --topbar:rgba(255,255,255,.82); --tint:rgba(234,88,12,.08);
    --radius:14px; --shadow:0 1px 2px rgba(16,24,40,.06),0 6px 20px rgba(16,24,40,.07);
  }
  [data-theme="dark"]{
    --bg:#0c0e15; --surface:#151823; --surface-2:#1d2130; --border:#272c3b;
    --text:#e7e9f0; --muted:#8990a3; --faint:#5a6076;
    --accent:#ff6b35; --accent-2:#ffa94d;
    --ok:#34d399; --ok-bg:rgba(52,211,153,.13);
    --drift:#f87171; --drift-bg:rgba(248,113,113,.14);
    --unknown:#9aa0b3; --unknown-bg:rgba(154,160,179,.10);
    --topbar:rgba(13,15,22,.7); --tint:rgba(255,107,53,.10);
    --shadow:0 1px 2px rgba(0,0,0,.4),0 8px 24px rgba(0,0,0,.18);
  }
  *{box-sizing:border-box}
  body{margin:0;background:var(--bg);color:var(--text);
       font:14px/1.55 "Inter",system-ui,-apple-system,"Segoe UI",sans-serif;
       -webkit-font-smoothing:antialiased;transition:background .2s,color .2s}
  a{color:inherit;text-decoration:none}
  .app{display:flex;min-height:100vh}
  .sidebar{width:222px;flex-shrink:0;background:var(--surface);border-right:1px solid var(--border);
           padding:1.1rem .75rem;position:sticky;top:0;height:100vh;display:flex;flex-direction:column;gap:.25rem}
  .sidebar .brand{font-weight:750;font-size:1.12rem;display:flex;align-items:center;gap:.5rem;
                  padding:.3rem .65rem;margin-bottom:1rem}
  .sidebar .brand .mark{color:var(--accent);font-size:1.25rem;filter:drop-shadow(0 0 6px var(--tint))}
  .navlabel{color:var(--faint);font-size:.68rem;text-transform:uppercase;letter-spacing:.08em;padding:.6rem .65rem .25rem}
  .nav{display:flex;align-items:center;gap:.65rem;padding:.55rem .7rem;border-radius:9px;color:var(--muted);
       font-weight:550;font-size:.9rem;cursor:pointer;transition:.12s}
  .nav:hover{background:var(--surface-2);color:var(--text)}
  .nav.active{background:var(--tint);color:var(--accent)}
  .nav .ico{width:18px;text-align:center;font-size:1rem}
  .content{flex:1;min-width:0;display:flex;flex-direction:column}
  .topbar{display:flex;align-items:center;justify-content:space-between;padding:.85rem 1.5rem;
          border-bottom:1px solid var(--border);background:var(--topbar);backdrop-filter:blur(8px);
          position:sticky;top:0;z-index:10}
  .topbar h1{font-size:1.05rem;margin:0;font-weight:680}
  .meta{color:var(--muted);font-size:.78rem;display:flex;align-items:center;gap:.7rem}
  .live{display:inline-flex;align-items:center;gap:.4rem;color:var(--ok)}
  .live .dot{width:7px;height:7px;border-radius:50%;background:var(--ok);animation:pulse 2s infinite}
  @keyframes pulse{0%{box-shadow:0 0 0 0 var(--ok-bg)}70%{box-shadow:0 0 0 7px transparent}100%{box-shadow:0 0 0 0 transparent}}
  .theme-btn{background:var(--surface-2);border:1px solid var(--border);color:var(--text);border-radius:8px;
             width:30px;height:30px;cursor:pointer;font-size:.95rem;display:inline-flex;align-items:center;justify-content:center;transition:.12s}
  .theme-btn:hover{border-color:var(--accent);color:var(--accent)}
  #view{padding:1.5rem;display:flex;flex-direction:column;gap:1.4rem;max-width:1120px;width:100%;margin:0 auto}
  .stats{display:grid;grid-template-columns:repeat(4,1fr);gap:1rem}
  .card{background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);
        padding:1.05rem 1.25rem;box-shadow:var(--shadow);position:relative;overflow:hidden}
  .card::before{content:"";position:absolute;inset:0 auto 0 0;width:3px;opacity:0}
  .card.ok::before{background:var(--ok);opacity:.7} .card.drift::before{background:var(--drift);opacity:.85}
  .card .label{color:var(--muted);font-size:.72rem;text-transform:uppercase;letter-spacing:.07em}
  .card .value{font-size:2rem;font-weight:750;margin-top:.35rem;font-variant-numeric:tabular-nums;line-height:1}
  .card.drift .value{color:var(--drift)} .card.ok .value{color:var(--ok)}
  .panel{background:var(--surface);border:1px solid var(--border);border-radius:var(--radius);overflow:hidden;box-shadow:var(--shadow)}
  .panel>h2{margin:0;padding:.85rem 1.25rem;font-size:.78rem;text-transform:uppercase;letter-spacing:.06em;
            color:var(--muted);border-bottom:1px solid var(--border);display:flex;align-items:center;gap:.5rem}
  .panel>h2 .mk{color:var(--accent)}
  table{width:100%;border-collapse:collapse;font-size:.875rem}
  th{text-align:left;padding:.6rem 1.25rem;color:var(--faint);font-size:.7rem;text-transform:uppercase;letter-spacing:.05em;font-weight:600}
  td{padding:.72rem 1.25rem;border-top:1px solid var(--border);white-space:nowrap}
  tbody tr{transition:background .12s} tbody tr:hover{background:var(--surface-2)}
  .num{text-align:right;font-variant-numeric:tabular-nums}
  .muted{color:var(--muted)} .mono{font-family:ui-monospace,"SF Mono",Menlo,monospace}
  code{font-family:ui-monospace,"SF Mono",Menlo,monospace;background:var(--surface-2);padding:.12rem .45rem;
       border-radius:7px;font-size:.85em;color:var(--accent-2);border:1px solid var(--border)}
  .pill{display:inline-flex;align-items:center;gap:.35rem;padding:.18rem .6rem;border-radius:999px;font-size:.72rem;font-weight:650}
  .pill .d{width:6px;height:6px;border-radius:50%;background:currentColor}
  .pill.ok{color:var(--ok);background:var(--ok-bg)}
  .pill.drift{color:var(--drift);background:var(--drift-bg)}
  .pill.unknown{color:var(--unknown);background:var(--unknown-bg)}
  .pill.apply{color:var(--accent-2);background:var(--tint)}
  .pill.delete{color:var(--unknown);background:var(--unknown-bg)}
  .tag{display:inline-block;padding:.1rem .5rem;border-radius:6px;background:var(--surface-2);border:1px solid var(--border);font-size:.75rem;margin:.1rem .2rem .1rem 0}
  .empty{padding:2rem 1.25rem;color:var(--faint);text-align:center}
  .fail{color:var(--drift)}
  .toolbar{display:flex;justify-content:flex-end;gap:.5rem;padding:.55rem 1.25rem;border-bottom:1px solid var(--border)}
  .btn{font:inherit;font-size:.78rem;font-weight:600;color:var(--text);background:var(--surface-2);
       border:1px solid var(--border);border-radius:8px;padding:.35rem .75rem;cursor:pointer;display:inline-flex;align-items:center;gap:.35rem;transition:.12s}
  .btn:hover{border-color:var(--accent);color:var(--accent-2)}
  .btn-heal{font-size:.72rem;padding:.18rem .6rem;background:transparent}
  .btn-heal:hover{color:var(--accent-2);border-color:var(--accent)}
  .btn-danger:hover{color:var(--drift);border-color:var(--drift)}
  .joblog{margin:0;padding:.7rem 1.25rem;font-size:.74rem;line-height:1.5;color:var(--muted);
          background:var(--surface-2);border-top:1px solid var(--border);max-height:280px;overflow:auto;white-space:pre-wrap}
  .note{padding:.7rem 1.25rem;color:var(--faint);font-size:.8rem}
  .htmx-request.btn,.htmx-request .btn{opacity:.6;pointer-events:none}
  @media(max-width:820px){.stats{grid-template-columns:repeat(2,1fr)}.sidebar{width:64px}.sidebar .brand span,.nav span,.navlabel{display:none}}
</style>
</head>
<body>
<div class="app">
  <aside class="sidebar">
    <div class="brand"><span class="mark">▲</span> <span>crater</span></div>
    <div class="navlabel">Overview</div>
    <a class="nav active" onclick="nav(this)" hx-get="/view/dashboard" hx-target="#view" hx-swap="innerHTML"><span class="ico">◧</span><span>仪表盘</span></a>
    <div class="navlabel">Fleet</div>
    <a class="nav" onclick="nav(this)" hx-get="/view/hosts" hx-target="#view" hx-swap="innerHTML"><span class="ico">▤</span><span>主机</span></a>
    <a class="nav" onclick="nav(this)" hx-get="/view/groups" hx-target="#view" hx-swap="innerHTML"><span class="ico">⊞</span><span>主机组</span></a>
    <div class="navlabel">Deploy</div>
    <a class="nav" onclick="nav(this)" hx-get="/view/tasks" hx-target="#view" hx-swap="innerHTML"><span class="ico">✦</span><span>任务</span></a>
  </aside>
  <div class="content">
    <header class="topbar">
      <h1>crater control</h1>
      <div class="meta"><span class="live"><span class="dot"></span>live</span> · refresh 5s
        <button id="themeBtn" class="theme-btn" onclick="toggleTheme()" title="toggle theme">🌙</button>
      </div>
    </header>
    <main id="view" hx-get="/view/dashboard" hx-trigger="load" hx-swap="innerHTML"><div class="empty">loading…</div></main>
  </div>
</div>
<script>
  function applyTheme(t){var d=document.documentElement;if(t==='dark'){d.setAttribute('data-theme','dark')}else{d.removeAttribute('data-theme')}
    try{localStorage.setItem('crater-theme',t)}catch(e){} var b=document.getElementById('themeBtn');if(b)b.textContent=(t==='dark'?'☀':'🌙')}
  function toggleTheme(){applyTheme(document.documentElement.getAttribute('data-theme')==='dark'?'light':'dark')}
  function nav(el){document.querySelectorAll('.sidebar .nav').forEach(function(n){n.classList.remove('active')});el.classList.add('active')}
  (function(){var t='light';try{t=localStorage.getItem('crater-theme')||'light'}catch(e){}applyTheme(t)})();
</script>
</body>
</html>"##;

async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

// ---- views (compose polling fragments) --------------------------------------

async fn view_dashboard() -> Html<&'static str> {
    Html(
        r#"<section class="stats" hx-get="/api/stats" hx-trigger="load, every 5s" hx-swap="innerHTML"></section>
<section class="panel"><h2><span class="mk">◷</span> Activity</h2>
  <div hx-get="/api/history" hx-trigger="load, every 5s" hx-swap="innerHTML"><div class="empty">loading…</div></div></section>"#,
    )
}

async fn view_tasks() -> Html<&'static str> {
    // `#jobs` is a separate, non-polled area (D-099): a write action's log
    // panel lives there, safe from the deployments pane's own 5s refresh.
    Html(
        r#"<section class="panel"><h2><span class="mk">⚙</span> Operations</h2>
  <div id="jobs"><div class="note">no operation running — verify / heal / delete land here</div></div></section>
<section class="panel"><h2><span class="mk">✦</span> Deployments</h2>
  <div id="deps-pane" hx-get="/api/deployments" hx-trigger="load, every 5s" hx-swap="innerHTML"><div class="empty">loading…</div></div></section>"#,
    )
}

async fn view_hosts() -> Html<&'static str> {
    Html(
        r#"<section class="panel"><h2><span class="mk">▤</span> Hosts</h2>
  <div hx-get="/api/hosts" hx-trigger="load, every 5s" hx-swap="innerHTML"><div class="empty">loading…</div></div></section>"#,
    )
}

/// Host groups — read from the conventional inventory (data, not display-gating).
async fn view_groups() -> Html<String> {
    let text = match std::fs::read_to_string(INVENTORY) {
        Ok(t) => t,
        Err(_) => {
            return Html("<section class='panel'><h2><span class='mk'>⊞</span> Host groups</h2><div class='empty'>no <code>./inventory.yaml</code> — create one with <code>crater create inventory</code></div></section>".into());
        }
    };
    let spec: crater_core::spec::CraterSpec = match serde_yaml::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            return Html(format!("<section class='panel'><h2><span class='mk'>⊞</span> Host groups</h2><div class='empty fail'>inventory parse error: {}</div></section>", esc(&e.to_string())));
        }
    };
    let mut groups = String::new();
    if spec.inventory.groups.is_empty() {
        groups.push_str("<div class='empty'>no groups defined in inventory</div>");
    } else {
        groups.push_str("<table><thead><tr><th>Group</th><th>Members</th></tr></thead><tbody>");
        for (g, members) in &spec.inventory.groups {
            let tags: String = members
                .hosts
                .iter()
                .map(|m| format!("<span class='tag'>{}</span>", esc(m)))
                .chain(
                    members
                        .groups
                        .iter()
                        .map(|m| format!("<span class='tag'>@{}</span>", esc(m))),
                )
                .collect();
            groups.push_str(&format!("<tr><td><code>{}</code></td><td>{}</td></tr>", esc(g), tags));
        }
        groups.push_str("</tbody></table>");
    }
    // host → roles table
    let mut hosts = String::new();
    hosts.push_str("<table><thead><tr><th>Host</th><th>Address</th><th>Roles</th></tr></thead><tbody>");
    for h in &spec.inventory.hosts {
        let roles: String = if h.roles.is_empty() {
            "<span class='muted'>—</span>".into()
        } else {
            h.roles.iter().map(|r| format!("<span class='tag'>{}</span>", esc(r))).collect()
        };
        hosts.push_str(&format!(
            "<tr><td><code>{}</code></td><td class='mono muted'>{}</td><td>{}</td></tr>",
            esc(&h.name), esc(&h.address), roles
        ));
    }
    hosts.push_str("</tbody></table>");
    Html(format!(
        "<section class='panel'><h2><span class='mk'>⊞</span> Host groups</h2>{groups}</section>\
         <section class='panel'><h2><span class='mk'>▤</span> Inventory hosts</h2>{hosts}</section>"
    ))
}

// ---- data fragments (polled) ------------------------------------------------

fn card(label: &str, value: usize, kind: &str) -> String {
    format!("<div class='card {kind}'><div class='label'>{label}</div><div class='value'>{value}</div></div>")
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
    Html(render_deployments().await)
}

async fn render_deployments() -> String {
    let store = match TursoStore::open_read().await {
        Ok(s) => s,
        Err(e) => return format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string())),
    };
    let deps = match store.list_deployments().await {
        Ok(d) => d,
        Err(e) => return format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string())),
    };
    let toolbar = "<div class='toolbar'><button class='btn' hx-post='/api/verify' hx-target='#jobs' hx-swap='innerHTML'>↻ Verify now</button></div>";
    if deps.is_empty() {
        return format!("{toolbar}<div class='empty'>no deployments recorded</div>");
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
        // heal: plain confirm. delete: TYPE-THE-NAME prompt (D-099) — htmx
        // sends the answer as HX-Prompt; the server only proceeds on equality.
        // plan: read-only preview, no confirm (D-100). heal: plain confirm.
        // delete: TYPE-THE-NAME prompt (D-099) — htmx sends the answer as
        // HX-Prompt; the server only proceeds on equality.
        let heal = format!(
            "<td><button class='btn btn-heal' hx-post='/api/plan/{d}' hx-target='#jobs' hx-swap='innerHTML'>plan</button> \
             <button class='btn btn-heal' hx-post='/api/apply/{d}' hx-target='#jobs' hx-swap='innerHTML' hx-confirm='Re-apply (heal) {d} to its hosts?'>heal</button> \
             <button class='btn btn-heal btn-danger' hx-post='/api/delete/{d}' hx-target='#jobs' hx-swap='innerHTML' hx-prompt='危险:运行 {d} 的 teardown 卸载。输入部署名确认:'>delete</button></td>",
            d = esc(&dep)
        );
        rows.push_str(&format!(
            "<tr><td><code>{}</code></td><td>{}</td><td class='mono muted'>{}</td><td class='num'>{}</td><td>{}</td><td class='muted mono'>{}</td><td class='muted mono'>{}</td>{}</tr>",
            esc(&dep), esc(&join(a.tasks)), esc(&join(a.versions)), a.hosts, pill, checked, state::fmt_epoch(a.last), heal
        ));
    }
    format!(
        "{toolbar}<table><thead><tr><th>Deployment</th><th>Task</th><th>Version</th><th class='num'>Hosts</th><th>Status</th><th>Checked (UTC)</th><th>Last applied (UTC)</th><th></th></tr></thead><tbody>{rows}</tbody></table>"
    )
}

async fn hosts_fragment() -> Html<String> {
    let store = match TursoStore::open_read().await {
        Ok(s) => s,
        Err(e) => return Html(format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string()))),
    };
    let deps = match store.list_deployments().await {
        Ok(d) => d,
        Err(e) => return Html(format!("<div class='empty fail'>db error: {}</div>", esc(&e.to_string()))),
    };
    if deps.is_empty() {
        return Html("<div class='empty'>no hosts with deployments</div>".into());
    }
    // Pivot by host (D-059): deployments on each host + worst status + latest.
    #[derive(Default)]
    struct H {
        deps: BTreeSet<String>,
        ok: usize,
        drift: usize,
        last: i64,
    }
    let mut by: BTreeMap<String, H> = BTreeMap::new();
    for d in deps {
        let e = by.entry(d.host).or_default();
        e.deps.insert(d.deployment);
        e.last = e.last.max(d.applied_at);
        match d.status.as_str() {
            "ok" => e.ok += 1,
            "drift" => e.drift += 1,
            _ => {}
        }
    }
    let mut rows = String::new();
    for (host, h) in by {
        let pill = if h.drift > 0 {
            "<span class='pill drift'><span class='d'></span>drift</span>".to_string()
        } else if h.ok > 0 {
            "<span class='pill ok'><span class='d'></span>ok</span>".to_string()
        } else {
            "<span class='pill unknown'><span class='d'></span>unknown</span>".to_string()
        };
        let tags: String = h.deps.iter().map(|d| format!("<span class='tag'>{}</span>", esc(d))).collect();
        rows.push_str(&format!(
            "<tr><td class='mono'>{}</td><td class='num'>{}</td><td>{}</td><td>{}</td><td class='muted mono'>{}</td></tr>",
            esc(&host), h.deps.len(), tags, pill, state::fmt_epoch(h.last)
        ));
    }
    Html(format!(
        "<table><thead><tr><th>Host</th><th class='num'>Deployments</th><th>What</th><th>Status</th><th>Last applied (UTC)</th></tr></thead><tbody>{rows}</tbody></table>"
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

// ---- write actions: background jobs + polled log panel (D-058/D-099) --------

fn no_inventory_note() -> Html<String> {
    Html(format!(
        "<div class='note fail'>no <code>./{INVENTORY}</code> — create one (<code>crater create inventory</code>) to enable actions</div>"
    ))
}

/// The panel a write action returns: polls the job's log until it exits.
fn job_panel(id: u64) -> Html<String> {
    Html(format!(
        "<div hx-get='/api/job/{id}' hx-trigger='load, every 1s' hx-swap='innerHTML'><div class='note'>启动中…</div></div>"
    ))
}

/// Polled job fragment: last log lines + status. While the job runs, plain 200
/// keeps the panel's 1s poll alive; when done, htmx's **286** status code stops
/// the polling — the deployments table (own 5s poll) then shows the new state.
async fn job_fragment(State(st): State<AppState>, AxPath(id): AxPath<u64>) -> Response {
    let jobs = st.jobs.lock().unwrap();
    let Some(j) = jobs.get(&id) else {
        // 286 also stops the poll for an unknown id (e.g. after a UI restart).
        return (
            axum::http::StatusCode::from_u16(286).unwrap(),
            Html("<div class='note fail'>job not found(UI 重启过?)</div>".to_string()),
        )
            .into_response();
    };
    let tail: String = j
        .lines
        .iter()
        .rev()
        .take(14)
        .rev()
        .map(|l| format!("{}\n", esc(l)))
        .collect();
    let status = if !j.done {
        "<span class='pill apply'><span class='d'></span>running</span>"
    } else if j.ok {
        "<span class='pill ok'><span class='d'></span>完成</span>"
    } else {
        "<span class='pill drift'><span class='d'></span>失败</span>"
    };
    let body = format!(
        "<div class='note'><b>{}</b> {status}</div><pre class='joblog mono'>{}</pre>",
        esc(&j.title),
        if tail.is_empty() { "…".into() } else { tail }
    );
    if j.done {
        // 286: htmx's "stop polling" status.
        (axum::http::StatusCode::from_u16(286).unwrap(), Html(body)).into_response()
    } else {
        Html(body).into_response()
    }
}

async fn verify_action(State(st): State<AppState>) -> Html<String> {
    if !std::path::Path::new(INVENTORY).exists() {
        return no_inventory_note();
    }
    let id = st.spawn_job(
        "verify".into(),
        vec!["task".into(), "list".into(), "--verify".into(), "-i".into(), INVENTORY.into()],
    );
    job_panel(id)
}

/// Look up a deployment's source ref in the state DB.
async fn source_of(dep: &str) -> Option<String> {
    TursoStore::open_read()
        .await
        .ok()?
        .list_deployments()
        .await
        .ok()?
        .into_iter()
        .find(|d| d.deployment == dep)
        .map(|d| d.source)
}

/// Preview what a re-apply would change (D-100 wired into the UI): runs
/// `crater plan <source>` as a job. Read-only — no confirm needed.
async fn plan_action(State(st): State<AppState>, AxPath(dep): AxPath<String>) -> Html<String> {
    if !std::path::Path::new(INVENTORY).exists() {
        return no_inventory_note();
    }
    let Some(source) = source_of(&dep).await else {
        return Html(format!("<div class='note fail'>deployment '{}' not found</div>", esc(&dep)));
    };
    let id = st.spawn_job(
        format!("plan {dep}"),
        vec!["plan".into(), source, "-i".into(), INVENTORY.into()],
    );
    job_panel(id)
}

async fn apply_action(State(st): State<AppState>, AxPath(dep): AxPath<String>) -> Html<String> {
    if !std::path::Path::new(INVENTORY).exists() {
        return no_inventory_note();
    }
    let Some(source) = source_of(&dep).await else {
        return Html(format!("<div class='note fail'>deployment '{}' not found</div>", esc(&dep)));
    };
    let id = st.spawn_job(
        format!("heal {dep}"),
        vec!["apply".into(), dep, source, "-i".into(), INVENTORY.into()],
    );
    job_panel(id)
}

/// Delete a deployment (D-099). Stronger confirm than heal: the button's
/// `hx-prompt` answer must EQUAL the deployment name (HX-Prompt header) —
/// type-the-name, AWX/GitHub-style.
async fn delete_action(
    State(st): State<AppState>,
    headers: axum::http::HeaderMap,
    AxPath(dep): AxPath<String>,
) -> Html<String> {
    if !std::path::Path::new(INVENTORY).exists() {
        return no_inventory_note();
    }
    let typed = headers.get("HX-Prompt").and_then(|v| v.to_str().ok()).unwrap_or("");
    if typed != dep {
        return Html(format!(
            "<div class='note fail'>未删除:输入 '{}' 与部署名 '{}' 不一致</div>",
            esc(typed), esc(&dep)
        ));
    }
    let Some(source) = source_of(&dep).await else {
        return Html(format!("<div class='note fail'>deployment '{}' not found</div>", esc(&dep)));
    };
    let id = st.spawn_job(
        format!("delete {dep}"),
        vec!["delete".into(), source, "-i".into(), INVENTORY.into()],
    );
    job_panel(id)
}
