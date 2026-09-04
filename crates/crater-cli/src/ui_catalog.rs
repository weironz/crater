//! 蓝图目录:把 `library/` 从"下拉框里的一串路径"变回"可安装的东西"。
//!
//! 这一层的全部原料**早就存在**,只是 UI 一直没用:每张蓝图都声明了
//! 自己的参数契约(名字/类型/默认值/说明/stage/是否敏感)和机群契约
//! (需要哪些组、每组至少几台),`crater inspect` 一直在读它们。
//!
//! 于是使用者的路径可以是"挑一个 → 填表单 → 选机群 → 建任务",**全程不看
//! YAML**;而写蓝图是另一个角色的事,他用编辑器 + lint + 字段卡。
//! 一个界面同时服务两个角色,是此前 UI 别扭的根源。

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

/// 参数类型 → 表单控件与校验的提示。
///
/// 只给**语义**,不给 HTML —— 前端据此决定用数字框还是开关。类型是登记在
/// 蓝图里的事实,渲染方式是前端的事,两边不该互相硬编码。
pub(crate) fn param_type_json(t: &crater_ir::schema::ParamType) -> serde_json::Value {
    use crater_ir::schema::ParamType as P;
    match t {
        P::String => json!({ "kind": "string" }),
        P::Int => json!({ "kind": "int" }),
        P::Bool => json!({ "kind": "bool" }),
        P::Ip => json!({ "kind": "ip", "hint": "如 192.168.1.10" }),
        P::Cidr => json!({ "kind": "cidr", "hint": "如 10.244.0.0/16" }),
        P::Version => json!({ "kind": "version", "hint": "如 1.36.1" }),
        P::Port => json!({ "kind": "port", "min": 1, "max": 65535 }),
        P::List(inner) => json!({ "kind": "list", "of": param_type_json(inner) }),
        P::Enum(vs) => json!({ "kind": "enum", "values": vs }),
    }
}

pub(crate) fn yaml_to_json(v: &serde_yaml::Value) -> serde_json::Value {
    serde_json::to_value(v).unwrap_or(serde_json::Value::Null)
}

/// 一张蓝图在目录里的样子。
///
/// 主体直接取包的契约([`crate::pkg::contract`])—— 目录卡片、`inspect`
/// 与远端 registry 上那份 config blob 因此是同一份数据。前端已有的字段名
/// (`groups` 与几个计数)在这里做一次映射,不去动线上契约的形状。
fn entry(path: &Path) -> Option<serde_json::Value> {
    let bp = crate::blueprint::load(path).ok()?;
    let c = crate::pkg::contract(&bp);
    Some(json!({
        "path": path.display().to_string(),
        "name": c["name"],
        "version": c["version"],
        "description": c["description"],
        "params": c["params"],
        "groups": c["fleet"],
        // 规模:一眼看出这是个小工具还是一整套子系统。
        "resources": c["counts"]["resources"],
        "materials": c["materials"].as_array().map(|a| a.len()).unwrap_or(0),
        "procedures": c["counts"]["procedures"],
        "health": c["counts"]["health"],
        "custom_types": c["counts"]["custom_types"],
    }))
}

/// `GET /api/catalog` —— 工作区里所有蓝图的契约。
pub async fn catalog() -> Response {
    let Ok(root) = crate::ui_edit::root() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "工作区不可读" })),
        )
            .into_response();
    };
    let mut items = Vec::new();
    let mut walk = vec![root.clone()];
    while let Some(dir) = walk.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules") {
                continue;
            }
            if p.is_dir() {
                walk.push(p);
                continue;
            }
            if !crate::blueprint::is_blueprint_file(&p) {
                continue;
            }
            // 解析不了的蓝图直接跳过 —— 目录是"能装什么"的清单,
            // 坏文件属于编辑器要处理的事,摆在目录里只会让人以为能点。
            if let Some(mut v) = entry(&p) {
                if let Ok(rel) = p.strip_prefix(&root) {
                    v["path"] = json!(rel.display().to_string());
                }
                items.push(v);
            }
        }
    }
    items.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Json(json!({ "items": items })).into_response()
}

#[derive(Deserialize)]
pub struct FitQ {
    /// 蓝图的工作区相对路径。
    pub blueprint: String,
    /// 要配的机群。
    pub inventory: String,
}

/// `GET /api/catalog/fit?blueprint=…&inventory=…` —— 这份机群配得上这张蓝图吗。
///
/// **不连机器**,纯粹按台数对账。目的是让"组名对不上""台数不够"在建任务时
/// 就红,而不是等到 plan 连了一半才报。
pub async fn fit(Query(q): Query<FitQ>) -> Response {
    let bpp = match crate::ui_edit::confine(&q.blueprint) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    let bp = match crate::blueprint::load(&bpp) {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": format!("蓝图解析失败:{e}") })),
            )
                .into_response()
        }
    };
    let invp = match crate::ui_edit::confine(&q.inventory) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    let text = match std::fs::read_to_string(&invp) {
        Ok(t) => t,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("读不到机群:{e}") })),
            )
                .into_response()
        }
    };
    let spec: crater_core::spec::CraterSpec = match serde_yaml::from_str(&text) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({ "error": format!("机群解析失败:{e}") })),
            )
                .into_response()
        }
    };
    let inv = spec.inventory;
    let rows: Vec<serde_json::Value> = bp
        .fleet
        .groups
        .iter()
        .map(|(name, c)| {
            let have = inv.groups.get(name).map(|g| g.hosts.len()).unwrap_or(0);
            let declared = inv.groups.contains_key(name);
            json!({
                "group": name,
                "need": c.min,
                "have": have,
                // 三种状态要分得开:组没声明 / 声明了但台数不够 / 满足。
                // "没这个组"和"这个组是空的"是两回事 —— 后者在单节点拓扑里
                // 完全合法(min: 0 的 worker)。
                "state": if !declared { "missing" }
                    else if have < c.min { "short" }
                    else { "ok" },
            })
        })
        .collect();
    let ok = rows.iter().all(|r| r["state"] == "ok");
    Json(json!({
        "ok": ok,
        "groups": rows,
        "hosts": inv.hosts.len(),
        "inventory_groups": inv.groups.keys().collect::<Vec<_>>(),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crater_ir::schema::ParamType as P;

    #[test]
    fn port_carries_its_range_so_the_form_can_validate() {
        let v = param_type_json(&P::Port);
        assert_eq!(v["kind"], "port");
        assert_eq!(v["min"], 1);
        assert_eq!(v["max"], 65535);
    }

    #[test]
    fn enum_carries_its_values_so_the_form_can_offer_them() {
        let v = param_type_json(&P::Enum(vec!["a".into(), "b".into()]));
        assert_eq!(v["kind"], "enum");
        assert_eq!(v["values"][1], "b");
    }

    /// 列表要能说出**元素**是什么类型,否则表单只能退化成一个自由文本框。
    #[test]
    fn list_reports_its_element_type() {
        let v = param_type_json(&P::List(Box::new(P::Port)));
        assert_eq!(v["kind"], "list");
        assert_eq!(v["of"]["kind"], "port");
    }
}

/// `GET /view/catalog` —— "我要装什么"。
///
/// 这一页刻意**不出现 YAML**:卡片来自蓝图的 description 与契约,表单来自
/// params 声明。UI 不硬编码任何参数名 —— 与登记表驱动字段卡是同一条原则,
/// 只是对象从"资源类型"换成了"蓝图参数"。
/// `GET /api/repos` —— 远端源:配了哪些仓库、里面有什么。
///
/// 数据全部来自**本地缓存的索引**,不连网。于是这一页在断网机房里照样能开,
/// 而"要不要联网"是使用者按"同步"时的一个明确决定。
pub async fn repos() -> Response {
    let ws = crate::ui_edit::root().ok();
    let items: Vec<serde_json::Value> = crate::repo::latest_entries()
        .into_iter()
        .map(|(repo, name, e)| {
            // 工作区里已经有同名目录 → 卡片上标出来,免得人反复拉同一个包。
            let here = ws.as_ref().map(|r| r.join(&name).is_dir()).unwrap_or(false);
            json!({
                "repo": repo, "name": name, "version": e.version,
                "reference": e.reference, "description": e.description,
                "groups": e.fleet.iter().map(|f| json!({"name": f.name, "min": f.min}))
                    .collect::<Vec<_>>(),
                "params": e.params, "platforms": e.platforms,
                "in_workspace": here,
            })
        })
        .collect();
    let repos: Vec<serde_json::Value> = crate::repo::repo_status()
        .into_iter()
        .map(|(name, url, n)| json!({ "name": name, "url": url, "packages": n }))
        .collect();
    Json(json!({ "repos": repos, "items": items })).into_response()
}

#[derive(Deserialize)]
pub struct RepoAddReq {
    pub name: String,
    pub url: String,
}

/// `POST /api/repos/add` —— 记下一个索引地址并同步一次。
pub async fn repo_add(Json(q): Json<RepoAddReq>) -> Response {
    match crate::repo::add(&q.name, &q.url).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `POST /api/repos/update` —— 拉一次全部索引。这是这一页**唯一**联网的动作。
pub async fn repo_update() -> Response {
    match crate::repo::update(None).await {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
pub struct RepoPullReq {
    pub reference: String,
    /// 连物料层一起拉(离线现场)。
    #[serde(default)]
    pub full: bool,
}

/// `POST /api/repos/pull` —— 把远端的包摊进工作区。
///
/// 摊进来之后它就是一张普通的本地蓝图,**后面的路一步不变**:参数表单 →
/// 机群对账 → 建任务 → plan 闸门。远端源这一页因此没有第二套表单逻辑 ——
/// 它只负责"把东西弄进来",不负责"怎么装"。
pub async fn repo_pull(Json(q): Json<RepoPullReq>) -> Response {
    let Ok(root) = crate::ui_edit::root() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "工作区不可读" })),
        )
            .into_response();
    };
    // 目录名取包名:与 CLI 的 `pull` 一致,人在两边看到的是同一棵树。
    let name = q
        .reference
        .rsplit('/')
        .next()
        .and_then(|s| s.split(':').next())
        .unwrap_or("pkg")
        .to_string();
    let dir = root.join(&name);
    match crate::pkg::pull(&q.reference, Some(&dir), q.full, None, false).await {
        Ok(()) => Json(json!({ "ok": true, "dir": name })).into_response(),
        Err(e) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

pub async fn view_catalog() -> axum::response::Html<&'static str> {
    axum::response::Html(CATALOG_HTML)
}

const CATALOG_HTML: &str = r##"<section class="panel">
  <h2><span class="mk">▦</span> 目录</h2>
  <div class="cat-tabs">
    <button id="tab-local" class="cat-tab on" onclick="catTab('local')">工作区</button>
    <button id="tab-remote" class="cat-tab" onclick="catTab('remote')">远端源</button>
  </div>
  <div id="cat-wall" class="cat-wall"></div>
  <div id="cat-remote" style="display:none">
    <div class="rp-bar">
      <span id="rp-list" class="rp-list"></span>
      <span style="flex:1"></span>
      <button class="btn" onclick="repoSync()">同步索引</button>
      <button class="btn" onclick="repoAddUI()">添加仓库</button>
    </div>
    <div id="rp-msg" class="cf-msg"></div>
    <div id="rp-wall" class="cat-wall"></div>
  </div>
  <div id="cat-form"></div>
</section>
<style>
  .cat-wall{display:grid;grid-template-columns:repeat(auto-fill,minmax(280px,1fr));gap:12px}
  .cat-card{border:1px solid var(--border);border-radius:12px;padding:14px;background:var(--surface);
    cursor:pointer;display:flex;flex-direction:column;gap:6px}
  .cat-card:hover{box-shadow:var(--shadow);border-color:var(--accent)}
  .cat-card.on{border-color:var(--accent);background:var(--tint)}
  .cat-name{font-weight:650;font-size:15px}
  .cat-desc{font-size:12.5px;color:var(--muted);line-height:1.5;min-height:2.6em}
  .cat-meta{font-size:11.5px;color:var(--faint);display:flex;gap:8px;flex-wrap:wrap}
  .cat-meta .g{color:var(--accent)}
  .cat-empty{border:1px dashed var(--border);border-radius:12px;padding:22px;text-align:center;color:var(--muted)}
  .cf{border:1px solid var(--border);border-radius:12px;padding:16px;background:var(--surface);margin-top:16px}
  .cf h3{margin:0 0 4px;font-size:15px}
  .cf .sub{color:var(--muted);font-size:12.5px;margin-bottom:14px}
  .cf-row{display:flex;gap:12px;align-items:flex-start;padding:9px 0;border-top:1px solid var(--border)}
  .cf-row:first-of-type{border-top:0}
  .cf-lab{flex:0 0 190px;font-size:13px}
  .cf-lab .nm{font-family:ui-monospace,monospace}
  .cf-lab .req{color:var(--drift);margin-left:5px;font-size:11px}
  .cf-lab .stage{color:var(--accent);margin-left:5px;font-size:11px}
  .cf-lab .d{color:var(--faint);font-size:11.5px;line-height:1.5;margin-top:3px}
  .cf-in{flex:1}
  .cf-in input,.cf-in select{width:100%;max-width:340px;background:var(--surface-2);color:var(--text);
    border:1px solid var(--border);border-radius:8px;padding:6px 10px;font:inherit;font-size:13px}
  .cf-in input[type=checkbox]{width:auto}
  .cf-in .hint{font-size:11.5px;color:var(--faint);margin-top:3px}
  .cf-in input:invalid{border-color:var(--drift)}
  .cf-fit{margin:12px 0;font-size:12.5px}
  .cf-fit .r{padding:3px 0}
  .cf-fit .ok{color:var(--ok)} .cf-fit .bad{color:var(--drift)}
  .cf-act{display:flex;gap:8px;align-items:center;margin-top:14px;flex-wrap:wrap}
  .cf-act .btn.primary{background:var(--accent);color:#fff;border:0}
  .cf-msg{font-size:12.5px;color:var(--muted)}
  .cf-msg.bad{color:var(--drift)}
  .cat-tabs{display:flex;gap:6px;margin-bottom:14px}
  .cat-tab{background:transparent;color:var(--muted);border:1px solid var(--border);
    border-radius:8px;padding:5px 14px;font:inherit;font-size:13px;cursor:pointer}
  .cat-tab.on{color:var(--text);border-color:var(--accent);background:var(--tint)}
  .rp-bar{display:flex;gap:8px;align-items:center;margin-bottom:10px;flex-wrap:wrap}
  .rp-list{font-size:12px;color:var(--faint)}
  .cat-card.have{opacity:.62}
  .cat-card .badge{font-size:11px;color:var(--accent)}
</style>
<script>
(function(){
  const wall = document.getElementById('cat-wall');
  const form = document.getElementById('cat-form');
  let items = [], invs = [], picked = null;
  const esc = s => String(s==null?'':s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/"/g,'&quot;');

  async function boot(){
    const [c, f] = await Promise.all([
      fetch('/api/catalog').then(r=>r.json()),
      fetch('/api/files').then(r=>r.json()),
    ]);
    items = c.items || [];
    invs = (f.files||[]).filter(x=>x.kind==='inventory').map(x=>x.path);
    if (!items.length){
      wall.innerHTML = '<div class="cat-empty">工作区里还没有蓝图。<br><br>'
        + '从发行的示例库复制一份进来(<code>cp -r library/rustfs ~/crater/</code>),'
        + '或在<b>蓝图</b>页新建。</div>';
      return;
    }
    wall.innerHTML = items.map((it,i)=>{
      const g = (it.groups||[]).map(x=>`${esc(x.name)}≥${x.min}`).join(' ') || '不限机群';
      return `<div class="cat-card" data-i="${i}" onclick="catPick(${i})">
        <span class="cat-name">${esc(it.name)}${it.version?' <span class="cat-meta">v'+esc(it.version)+'</span>':''}</span>
        <span class="cat-desc">${esc(it.description||'(蓝图没写 description)')}</span>
        <span class="cat-meta"><span class="g">${g}</span></span>
        <span class="cat-meta">${it.resources} 资源 · ${it.params.length} 参数${it.materials?' · '+it.materials+' 物料':''}${it.procedures?' · '+it.procedures+' procedure':''}</span>
      </div>`;
    }).join('');
  }

  // ── 远端源 ────────────────────────────────────────────────────────────
  //
  // 这一页只做一件事:把远端的包**弄进工作区**。弄进来之后它就是一张普通
  // 的本地蓝图,参数表单、机群对账、建任务、plan 闸门一步不变 —— 所以这里
  // 没有第二套表单逻辑,也就没有两套逻辑迟早跑偏的问题。
  let remote = [];

  window.catTab = function(which){
    const isL = which === 'local';
    document.getElementById('tab-local').classList.toggle('on', isL);
    document.getElementById('tab-remote').classList.toggle('on', !isL);
    wall.style.display = isL ? '' : 'none';
    document.getElementById('cat-remote').style.display = isL ? 'none' : '';
    form.innerHTML = '';
    if (!isL) loadRemote();
  };

  async function loadRemote(){
    const d = await fetch('/api/repos').then(r=>r.json());
    remote = d.items || [];
    const rl = document.getElementById('rp-list');
    rl.textContent = (d.repos||[]).length
      ? (d.repos||[]).map(r=>`${r.name}(${r.packages==null?'未同步':r.packages+' 个包'})`).join(' · ')
      : '还没有配仓库';
    const rw = document.getElementById('rp-wall');
    if (!remote.length){
      rw.innerHTML = '<div class="cat-empty">还没有可浏览的包。<br><br>'
        + '包的作者用 <code>crater index</code> 生成索引文件,托管在任意静态 HTTP 上;'
        + '你「添加仓库」填那个地址即可。<br>'
        + 'OCI 本身没有搜索接口,所以「有哪些包」由索引文件回答 —— 它也能随闭包进 U 盘。</div>';
      return;
    }
    rw.innerHTML = remote.map((it,i)=>{
      const g = (it.groups||[]).map(x=>`${esc(x.name)}≥${x.min}`).join(' ') || '不限机群';
      const plat = (it.platforms||[]).length ? ' · ' + it.platforms.map(esc).join(' ') : '';
      const have = it.in_workspace ? '<span class="badge">✓ 已在工作区</span>' : '';
      return `<div class="cat-card ${it.in_workspace?'have':''}" onclick="repoPull(${i})">
        <span class="cat-name">${esc(it.name)} <span class="cat-meta">${esc(it.version)}</span> ${have}</span>
        <span class="cat-desc">${esc(it.description||'(索引里没有描述)')}</span>
        <span class="cat-meta"><span class="g">${g}</span></span>
        <span class="cat-meta">${esc(it.repo)} · ${it.params} 参数${plat}</span>
      </div>`;
    }).join('');
  }

  function rpMsg(t, bad){
    const m = document.getElementById('rp-msg');
    m.textContent = t; m.className = 'cf-msg' + (bad?' bad':'');
  }

  window.repoSync = async function(){
    rpMsg('同步中……');
    const r = await fetch('/api/repos/update', {method:'POST'});
    const d = await r.json().catch(()=>({}));
    if (!r.ok){ rpMsg(d.error||'同步失败', true); return; }
    rpMsg('已同步'); loadRemote();
  };

  window.repoAddUI = async function(){
    const url = prompt('索引地址(http(s):// 或本地路径,由 crater index 生成)');
    if (!url) return;
    const name = prompt('给它起个名字', 'lab');
    if (!name) return;
    rpMsg('添加中……');
    const r = await fetch('/api/repos/add', {method:'POST',
      headers:{'content-type':'application/json'}, body:JSON.stringify({name,url})});
    const d = await r.json().catch(()=>({}));
    if (!r.ok){ rpMsg(d.error||'添加失败', true); return; }
    rpMsg('已添加 '+name); loadRemote();
  };

  window.repoPull = async function(i){
    const it = remote[i];
    if (it.in_workspace){
      // 已经在工作区了:直接切回去让人接着建任务,而不是再拉一遍。
      rpMsg(`${it.name} 已在工作区 —— 切到「工作区」标签建任务。`);
      return;
    }
    // 默认瘦拉:在线部署时目标机自己按 URL 取物料,几百兆的层不必经手。
    const full = confirm(`把 ${it.name} ${it.version} 拉进工作区。\n\n`
      + '确定 = 连物料字节一起拉(离线现场用,可能几百兆)\n'
      + '取消 = 只拉蓝图(在线部署够用,通常几十 KB)');
    rpMsg('拉取中……');
    const r = await fetch('/api/repos/pull', {method:'POST',
      headers:{'content-type':'application/json'},
      body:JSON.stringify({reference: it.reference, full})});
    const d = await r.json().catch(()=>({}));
    if (!r.ok){ rpMsg(d.error||'拉取失败', true); return; }
    rpMsg(`${it.name} 已进工作区 —— 切到「工作区」标签建任务。`);
    await boot();          // 本地墙要立刻看得见新来的这张
    loadRemote();
  };

  // 控件由**参数的声明类型**决定,UI 不硬编码任何参数名。
  function control(p){
    const t = p.type, id = 'cp-'+p.name;
    // 敏感值不预填:默认值往往只是占位符,预填出来会被当成真值提交。
    const dv = p.secret ? '' : (p.default==null ? '' : String(p.default));
    if (t.kind === 'bool')
      return `<input type="checkbox" id="${id}" ${String(p.default)==='true'?'checked':''}>`;
    if (t.kind === 'enum')
      return `<select id="${id}">${(t.values||[]).map(v=>
        `<option ${String(p.default)===v?'selected':''}>${esc(v)}</option>`).join('')}</select>`;
    if (t.kind === 'port')
      return `<input type="number" id="${id}" min="${t.min}" max="${t.max}" value="${esc(dv)}">`;
    if (t.kind === 'int')
      return `<input type="number" id="${id}" value="${esc(dv)}">`;
    const hint = t.hint ? `<div class="hint">${esc(t.hint)}</div>` : '';
    return `<input type="${p.secret?'password':'text'}" id="${id}" value="${esc(dv)}"
      ${p.secret?'placeholder="留空则用蓝图默认值"':''}>${hint}`;
  }

  window.catPick = function(i){
    picked = items[i];
    for (const el of document.querySelectorAll('.cat-card'))
      el.classList.toggle('on', el.dataset.i == i);
    const p = picked;
    form.innerHTML = `<div class="cf">
      <h3>装 ${esc(p.name)}</h3>
      <div class="sub">${esc(p.description||'')}</div>
      <div class="cf-row"><div class="cf-lab"><span class="nm">任务名</span>
        <div class="d">落成工作区里的 &lt;名字&gt;.app.yaml</div></div>
        <div class="cf-in"><input id="cf-name" value="${esc(p.name)}"></div></div>
      <div class="cf-row"><div class="cf-lab"><span class="nm">机群</span>
        <div class="d">这张蓝图需要:${(p.groups||[]).map(g=>esc(g.name)+'≥'+g.min).join('、')||'不限'}</div></div>
        <div class="cf-in"><select id="cf-inv" onchange="catFit()">
          <option value="">— 选一份机群 —</option>
          ${invs.map(x=>`<option>${esc(x)}</option>`).join('')}
        </select><div id="cf-fit" class="cf-fit"></div></div></div>
      ${p.params.map(pp=>`<div class="cf-row">
        <div class="cf-lab"><span class="nm">${esc(pp.name)}</span>
          ${pp.required?'<span class="req">必填</span>':''}
          ${pp.stage==='build'?'<span class="stage">打闭包时定死</span>':''}
          ${pp.desc?`<div class="d">${esc(pp.desc)}</div>`:''}</div>
        <div class="cf-in">${control(pp)}</div></div>`).join('')}
      <div class="cf-act">
        <button class="btn primary" onclick="catCreate()">建任务</button>
        <label style="font-size:12.5px;color:var(--muted)">巡检
          <input id="cf-iv" placeholder="30m,留空=只手动" style="width:130px;margin-left:6px"></label>
        <span id="cf-msg" class="cf-msg"></span>
      </div>
    </div>`;
    form.scrollIntoView({behavior:'smooth', block:'nearest'});
  };

  // 机群对账**不连机器**,纯按台数 —— 组名对不上、台数不够,建任务时就红,
  // 而不是等 plan 连了一半才报。
  window.catFit = async function(){
    const inv = document.getElementById('cf-inv').value;
    const box = document.getElementById('cf-fit');
    if (!inv){ box.innerHTML=''; return; }
    const d = await (await fetch('/api/catalog/fit?blueprint='+encodeURIComponent(picked.path)
      +'&inventory='+encodeURIComponent(inv))).json();
    if (d.error){ box.innerHTML = `<div class="r bad">${esc(d.error)}</div>`; return; }
    if (!d.groups.length){ box.innerHTML = `<div class="r ok">✓ 这张蓝图不限机群(${d.hosts} 台可用)</div>`; return; }
    box.innerHTML = d.groups.map(g=>{
      if (g.state==='ok') return `<div class="r ok">✓ ${esc(g.group)}:${g.have} 台(需要 ≥${g.need})</div>`;
      if (g.state==='missing') return `<div class="r bad">✗ ${esc(g.group)}:这份机群里没有这个组</div>`;
      return `<div class="r bad">✗ ${esc(g.group)}:只有 ${g.have} 台,需要 ≥${g.need}</div>`;
    }).join('');
  };

  window.catCreate = async function(){
    const msg = document.getElementById('cf-msg');
    const say = (t,bad)=>{ msg.textContent=t; msg.className='cf-msg'+(bad?' bad':''); };
    const name = document.getElementById('cf-name').value.trim();
    const inv = document.getElementById('cf-inv').value;
    // 只把**改过的**参数写进任务:与默认值相同的不写,任务文件才看得出意图。
    const params = [];
    for (const pp of picked.params){
      const el = document.getElementById('cp-'+pp.name);
      if (!el) continue;
      const v = pp.type.kind==='bool' ? String(el.checked) : el.value.trim();
      if (v === '') continue;
      if (String(pp.default) === v) continue;
      params.push([pp.name, v]);
    }
    const d = await (await fetch('/api/app/create',{method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({name, blueprint:picked.path, inventory:inv, params,
        verify_interval:document.getElementById('cf-iv').value.trim()})})).json();
    if (d.error){ say(d.error, true); return; }
    const lr = await (await fetch('/api/lint-project',{method:'POST',body:d.path})).json();
    say(lr.ok ? `已建任务 ${d.path} —— 去"任务"页 Plan / Apply`
              : `已建 ${d.path},但有问题:${lr.diagnostics.map(x=>x.message).join(';')}`, !lr.ok);
  };

  boot();
})();
</script>
"##;
