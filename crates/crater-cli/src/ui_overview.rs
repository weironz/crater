//! 对账看板(阶段②:对账供血)—— 新首页。
//!
//! 回答的问题不是"我能触发什么",而是**"机群的现实偏离期望态了没有"**。
//! 每张卡两枚徽章,刻意不合并成一个灯(ArgoCD 最好的设计决策):
//! - 同步轴:期望 vs 现实(Synced / Drifted / OutOfDate / Progressing / …)
//! - 健康轴:上次核对能不能到、判了什么
//!
//! 数据来源全部是**已有事实**:FileStore 部署记录(数字孪生)、verify 快照
//! (~/.crater/ui/verify/,由 verify job 落下)、工作区蓝图文件的当前指纹、
//! 运行中的 job。看板不制造新状态,只把四处事实拼成一句话。

use std::collections::BTreeMap;
use std::path::PathBuf;

use axum::response::{Html, IntoResponse, Json};
use crater_ir::state::{FileStore, Store};
use serde_json::json;

fn snap_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".crater").join("ui").join("verify")
}

fn sanitize(id: &str) -> String {
    id.chars().map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect()
}

/// verify job 结束后由 ui_run::finalize 调:把 --json 报告拆成每记录一份快照。
pub fn stash_verify_report(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let Ok(doc) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let ts = doc["ts"].as_u64().unwrap_or(0);
    let _ = std::fs::create_dir_all(snap_dir());
    for h in doc["hosts"].as_array().cloned().unwrap_or_default() {
        let Some(rid) = h["record_id"].as_str() else { continue };
        let snap = json!({
            "ts": ts,
            "verdict": h["verdict"],
            "drifted_n": h["drifted"].as_array().map(|a| a.len()).unwrap_or(0),
            "drifted": h["drifted"],
            "unknown": h["unknown"],
        });
        let _ = std::fs::write(
            snap_dir().join(format!("{}.json", sanitize(rid))),
            serde_json::to_string_pretty(&snap).unwrap_or_default(),
        );
    }
}

fn load_snap(record_id: &str) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(snap_dir().join(format!("{}.json", sanitize(record_id)))).ok()?;
    serde_json::from_str(&text).ok()
}

/// 工作区里"蓝图名 → 文件路径"。记录存的是蓝图**名**(稳定身份),
/// 文件是它的当前载体 —— 同名多文件时如实报 ambiguous,不猜。
fn name_to_path() -> BTreeMap<String, Vec<String>> {
    let mut map: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let Ok(root) = crate::ui_edit::root() else { return map };
    let mut stack = vec![root.clone()];
    let mut seen = 0;
    while let Some(dir) = stack.pop() {
        if seen > 400 { break }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules") { continue }
            if p.is_dir() { stack.push(p); continue }
            if !matches!(p.extension().and_then(|s| s.to_str()), Some("yaml") | Some("yml")) { continue }
            seen += 1;
            if !crate::blueprint::is_blueprint_file(&p) { continue }
            if let Ok(bp) = crater_ir::parse::blueprint_from_path(&p) {
                let rel = p.strip_prefix(&root).unwrap_or(&p).display().to_string();
                map.entry(bp.name).or_default().push(rel);
            }
        }
    }
    map
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// `GET /api/overview` —— 卡片数据(JSON;页面片段由前端渲染)。
pub async fn overview() -> impl IntoResponse {
    let records = FileStore::default_location().list().unwrap_or_default();
    let paths = name_to_path();
    let running: Vec<crate::ui_run::JobMeta> = crate::ui_run::list()
        .into_iter()
        .filter(|m| m.status == "running")
        .collect();

    let cards: Vec<serde_json::Value> = records
        .iter()
        .map(|r| {
            let snap = load_snap(&r.id);
            let bp_paths = paths.get(&r.blueprint).cloned().unwrap_or_default();
            // 同步轴:优先级 Progressing > OutOfDate > 快照结论 > Unknown。
            // Progressing 压过一切:有 job 在动这份蓝图时,别的结论都是旧闻。
            let progressing = running.iter().any(|j| {
                j.blueprint.ends_with(&format!("{}.blueprint.yaml", r.blueprint))
                    || bp_paths.contains(&j.blueprint)
            });
            let out_of_date = match (&r.blueprint_sha256, bp_paths.first()) {
                (Some(rec_sha), Some(p)) if bp_paths.len() == 1 => std::fs::read(p)
                    .ok()
                    .map(|b| crater_core::bundle::sha256_hex(&b) != *rec_sha)
                    .unwrap_or(false),
                _ => false, // 无指纹(旧记录)/ 找不到文件 / 同名多文件 → 不误报
            };
            let (sync, drifted_n) = if progressing {
                ("progressing".to_string(), 0)
            } else if out_of_date {
                ("out_of_date".to_string(), 0)
            } else if let Some(s) = &snap {
                let n = s["drifted_n"].as_u64().unwrap_or(0);
                match s["verdict"].as_str().unwrap_or("") {
                    "in_sync" => ("synced".to_string(), 0),
                    "drifted" => ("drifted".to_string(), n),
                    "indeterminate" => ("indeterminate".to_string(), n),
                    "never" => ("never_applied".to_string(), 0),
                    _ => ("unknown".to_string(), 0),
                }
            } else {
                ("unknown".to_string(), 0)
            };
            let verified_age = r.verified_at.map(|t| now().saturating_sub(t));
            json!({
                "id": r.id,
                "blueprint": r.blueprint,
                "target": r.target,
                "version": r.version,
                "paths": bp_paths,
                "ambiguous": paths.get(&r.blueprint).map(|v| v.len() > 1).unwrap_or(false),
                "sync": sync,
                "drifted_n": drifted_n,
                "drifted": snap.as_ref().map(|s| s["drifted"].clone()).unwrap_or(json!([])),
                "applied_at": r.applied_at,
                "verified_at": r.verified_at,
                "verified_age": verified_age,
                "resources": r.resources.len(),
            })
        })
        .collect();

    // 孤儿 = 工作区里已没有对应蓝图文件的记录(删了蓝图/换了工作目录)。
    // 混在主墙里会把真部署淹没;单独归组,配"清理"动作。
    let (cards, orphans): (Vec<_>, Vec<_>) = cards
        .into_iter()
        .partition(|c| !c["paths"].as_array().map(|a| a.is_empty()).unwrap_or(true));
    let count = |k: &str| cards.iter().filter(|c| c["sync"] == k).count();
    Json(json!({
        "cards": cards,
        "orphans": orphans,
        "stats": {
            "total": cards.len(),
            "synced": count("synced"),
            "drifted": count("drifted"),
            "out_of_date": count("out_of_date"),
            "progressing": count("progressing"),
            "unknown": count("unknown") + count("indeterminate"),
        },
    }))
}

/// `DELETE /api/record/{id}` —— 清理一条部署记录(孤儿专用)。
///
/// 只删**记录**,不碰任何机器:孤儿的蓝图已不在,连"该拆什么"都无从谈起 ——
/// 这是承认现实,不是退役。
pub async fn delete_record(
    axum::extract::Path(id): axum::extract::Path<String>,
) -> impl IntoResponse {
    match FileStore::default_location().remove(&id) {
        Ok(true) => Json(json!({ "ok": true })).into_response(),
        Ok(false) => (axum::http::StatusCode::NOT_FOUND, Json(json!({ "error": "没有这条记录" }))).into_response(),
        Err(e) => (axum::http::StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// `GET /view/overview` —— 卡片墙。空态给三步引导(对着 AWX 六步对象链打)。
pub async fn view_overview() -> Html<&'static str> {
    Html(OVERVIEW_HTML)
}

const OVERVIEW_HTML: &str = r##"<section class="panel">
  <h2><span class="mk">◎</span> 机群对账</h2>
  <div id="ov-stats" class="ov-stats"></div>
  <div id="ov-apps" class="ov-apps"></div>
  <div id="ov-cards" class="ov-cards"></div>
</section>
<style>
  .ov-stats{display:flex;gap:14px;margin-bottom:14px;font-size:13px;color:var(--muted);flex-wrap:wrap}
  .ov-stats b{color:var(--text)}
  .ov-cards{display:grid;grid-template-columns:repeat(auto-fill,minmax(300px,1fr));gap:12px}
  .ov-card{border:1px solid var(--border);border-radius:12px;padding:12px 14px;background:var(--surface);cursor:pointer}
  .ov-card:hover{box-shadow:var(--shadow)}
  .ov-head{display:flex;justify-content:space-between;align-items:baseline;margin-bottom:6px}
  .ov-name{font-weight:650}
  .ov-badges{display:flex;gap:6px;margin:6px 0}
  .badge{padding:2px 9px;border-radius:99px;font-size:12px}
  .b-synced{background:var(--ok-bg);color:var(--ok)}
  .b-drifted{background:var(--drift-bg);color:var(--drift)}
  .b-out_of_date{background:var(--tint);color:var(--accent)}
  .b-progressing{background:var(--tint);color:var(--accent)}
  .b-unknown,.b-indeterminate,.b-never_applied{background:var(--unknown-bg);color:var(--unknown)}
  .ov-meta{font-size:12px;color:var(--faint)}
  .ov-drift{margin-top:6px;font:12px ui-monospace,monospace;color:var(--drift);white-space:pre-wrap}
  .ov-actions{margin-top:8px;display:flex;gap:6px}
  .ov-actions .btn{font-size:12px;padding:3px 10px}
  .ov-empty{border:1px dashed var(--border);border-radius:12px;padding:22px;text-align:center;color:var(--muted)}
  .ov-apps{display:flex;flex-direction:column;gap:8px;margin-bottom:14px}
  .ov-app{display:flex;gap:10px;align-items:center;border:1px solid var(--border);border-radius:10px;
    padding:8px 12px;background:var(--surface);font-size:13px;flex-wrap:wrap}
  .ov-app .nm{font-weight:650}
  .ov-app .meta{color:var(--faint);font-size:12px}
  .ov-app .sp{margin-left:auto;display:flex;gap:6px}
  .ov-app .btn{font-size:12px;padding:3px 10px}
</style>
<script>
(function(){
  const SYNC_LABEL = {synced:'Synced', drifted:'Drifted', out_of_date:'OutOfDate',
    progressing:'Progressing', unknown:'Unknown', indeterminate:'Indeterminate', never_applied:'NeverApplied'};
  function age(s){ if (s==null) return '从未核对';
    return s<90?'刚刚核对':s<3600?Math.floor(s/60)+' 分钟前核对':s<86400?Math.floor(s/3600)+' 小时前核对':Math.floor(s/86400)+' 天前核对'; }
  async function runVerb(verb, path, inv, sets, limit){
    const r = await fetch('/api/run',{method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({verb, blueprint:path, inventory:inv||'', sets:sets||[], limit:limit||[]})});
    const d = await r.json();
    if (d.ok) htmx.ajax('GET','/view/job/'+d.job,'#view');
    else alert(d.error||'启动失败');   // 409 = plan 闸门:提示先 Plan,不是故障
  }
  window.ovRun = runVerb;
  // apps 带:期望态绑定(文件)与部署记录(实际)之间的那条线。
  // lint 不过的 app 直接在这里亮红 —— 别等 plan 才发现绑错了参数。
  async function renderApps(){
    const d = await (await fetch('/api/apps')).json();
    const el = document.getElementById('ov-apps');
    if (!d.apps.length){ el.innerHTML=''; return; }
    el.innerHTML = d.apps.map(a=>{
      const sets = (a.params||[]).map(p=>p.k+'='+p.v);
      const bad = a.ok ? '' : ' <span style="color:var(--drift)">✗ '
        + a.diagnostics.map(x=>x.message).join(';').replace(/</g,'&lt;') + '</span>';
      const iv = a.verify_interval ? '巡检 '+Math.round(a.verify_interval/60)+'m' : '只手动';
      const arg = `'${a.blueprint}','${a.inventory||''}',${JSON.stringify(sets).replace(/"/g,'&quot;')}`
        + ',' + JSON.stringify(a.limit||[]).replace(/"/g,'&quot;');
      return `<div class="ov-app"><span class="nm">▶ ${a.name}</span>
        <span class="meta">${a.blueprint} × ${a.inventory||'(无 inventory)'} · ${iv}</span>${bad}
        <span class="sp">
          <button class="btn" onclick="ovRun('verify',${arg})">Verify</button>
          <button class="btn" onclick="ovRun('plan',${arg})">Plan</button>
          <button class="btn" onclick="ovRun('apply',${arg})">Apply</button>
        </span></div>`;
    }).join('');
  }
  async function refresh(){
    renderApps();
    const d = await (await fetch('/api/overview')).json();
    const s = d.stats;
    document.getElementById('ov-stats').innerHTML =
      `<span>部署 <b>${s.total}</b></span><span>Synced <b>${s.synced}</b></span>`+
      `<span>Drifted <b style="color:var(--drift)">${s.drifted}</b></span>`+
      `<span>OutOfDate <b style="color:var(--accent)">${s.out_of_date}</b></span>`+
      `<span>Progressing <b>${s.progressing}</b></span><span>Unknown <b>${s.unknown}</b></span>`;
    const wall = document.getElementById('ov-cards');
    if (!d.cards.length){
      wall.innerHTML = `<div class="ov-empty">还没有任何部署记录。<br><br>
        三步从零开始:<b>蓝图</b>页新建蓝图 → 生成 inventory 骨架 → <b>运行</b>页 plan/apply。</div>`;
      return;
    }
    wall.innerHTML = d.cards.map(c => {
      const p = c.paths[0] || '';
      const drift = c.drifted_n ? `<div class="ov-drift">${(c.drifted||[]).slice(0,3)
        .map(x=>x.id+' '+x.detail).join('\n').replace(/</g,'&lt;')}</div>` : '';
      const amb = c.ambiguous ? `<div class="ov-meta">⚠ 工作区有多个同名蓝图,OutOfDate 检测已停用</div>` : '';
      return `<div class="ov-card">
        <div class="ov-head"><span class="ov-name">${c.blueprint}</span>
          <span class="ov-meta">@ ${c.target}</span></div>
        <div class="ov-badges">
          <span class="badge b-${c.sync}">${SYNC_LABEL[c.sync]||c.sync}${c.drifted_n?`(${c.drifted_n})`:''}</span>
          <span class="badge b-unknown">${c.resources} 资源</span>
        </div>
        <div class="ov-meta">${age(c.verified_age)}${c.version?' · v'+c.version:''}</div>
        ${drift}${amb}
        <div class="ov-actions">
          ${p?`<button class="btn" onclick="ovRun('verify','${p}','')">Verify</button>
               <button class="btn" onclick="ovRun('plan','${p}','')">Plan</button>`:
            `<span class="ov-meta">(工作区找不到蓝图文件)</span>`}
        </div>
      </div>`;
    }).join('');
  }
  refresh();
  const t = setInterval(()=>{ if (!document.getElementById('ov-cards')) { clearInterval(t); return; } refresh(); }, 15000);
})();
</script>"##;
