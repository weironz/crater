//! 蓝图编辑器的后端 —— 文件列举 / 读 / 写,以及编辑器视图。
//!
//! 这一块直接对着"AWX 要你事先在别处准备好 YAML、UI 里改不了"那个痛点。
//! 能在 UI 里编辑的前提是能读能写文件,而那立刻带来一个安全问题:
//! **HTTP 上的任意路径读写**。所以下面每一条路径都必须落在工作目录内,
//! 且经 canonicalize 之后再比对 —— 只挡 `..` 是不够的,符号链接照样能逃出去。

use std::path::{Path, PathBuf};

use axum::{
    extract::Query,
    http::StatusCode,
    response::{Html, IntoResponse, Json},
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
pub struct PathQuery {
    pub path: String,
}

/// 允许读写的根 —— 进程的工作目录。
///
/// 不做成可配置的:UI 能 apply/delete 已经足够危险,再加一个"想读哪儿读哪儿"
/// 的开关,一次配置失误就是整台机器的文件。要换目录就换个工作目录起进程。
fn root() -> std::io::Result<PathBuf> {
    std::env::current_dir()?.canonicalize()
}

/// 把请求里的相对路径解析成绝对路径,并确认它**确实**落在根内。
///
/// 对尚不存在的文件(新建),canonicalize 会失败 —— 这时改为核对它的父目录,
/// 否则"另存为新文件"就永远做不了。
pub(crate) fn confine(rel: &str) -> Result<PathBuf, (StatusCode, String)> {
    resolve(rel)
}

fn resolve(rel: &str) -> Result<PathBuf, (StatusCode, String)> {
    let root = root().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let joined = root.join(rel);
    let checked = match joined.canonicalize() {
        Ok(p) => p,
        Err(_) => {
            let parent = joined
                .parent()
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "路径没有父目录".to_string()))?;
            let parent = parent
                .canonicalize()
                .map_err(|_| (StatusCode::BAD_REQUEST, format!("父目录不存在:{rel}")))?;
            parent.join(joined.file_name().unwrap_or_default())
        }
    };
    if !checked.starts_with(&root) {
        // 措辞刻意具体:模糊的 403 会让人以为是权限配置问题,反复重试。
        return Err((
            StatusCode::FORBIDDEN,
            format!("路径越出工作目录({}):{rel}", root.display()),
        ));
    }
    Ok(checked)
}

/// `GET /api/files` —— 列出工作目录下的 YAML,并标出它是什么。
///
/// 标注 kind 不是装饰:蓝图、栈、inventory 在编辑器里的校验方式完全不同,
/// 列表里就分清楚,能免掉"打开才发现打错文件"。
pub async fn files() -> impl IntoResponse {
    let root = match root() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let mut out = Vec::new();
    walk(&root, &root, 0, &mut out);
    out.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Json(json!({ "root": root.display().to_string(), "files": out })).into_response()
}

/// 深度有限的遍历:仓库里 target/ 之类目录动辄十万文件,无节制递归会让
/// 一次列举卡住整个 UI。
fn walk(root: &Path, dir: &Path, depth: usize, out: &mut Vec<serde_json::Value>) {
    if depth > 4 || out.len() > 500 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules") {
            continue;
        }
        if p.is_dir() {
            walk(root, &p, depth + 1, out);
        } else if matches!(
            p.extension().and_then(|s| s.to_str()),
            Some("yaml") | Some("yml")
        ) {
            let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
            out.push(json!({ "path": rel, "kind": classify(&p) }));
        }
    }
}

/// 靠**内容形状**分辨,不靠文件名 —— 与 CLI 的分派规则同源。
fn classify(p: &Path) -> &'static str {
    if crate::stack_cmd::is_stack_file(p) {
        return "stack";
    }
    if crate::blueprint::is_blueprint_file(p) {
        return "blueprint";
    }
    let Ok(text) = std::fs::read_to_string(p) else {
        return "other";
    };
    match serde_yaml::from_str::<serde_yaml::Value>(&text) {
        Ok(v) if crate::ui_app::is_app_value(&v) => "app",
        Ok(v) if v.get("inventory").is_some() => "inventory",
        _ => "other",
    }
}

/// `GET /api/file?path=…`
pub async fn file_get(Query(q): Query<PathQuery>) -> impl IntoResponse {
    let path = match resolve(&q.path) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Json(json!({ "path": q.path, "text": text, "kind": classify(&path) })).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// `POST /api/file?path=…` —— 正文即全文。
///
/// 写之前先备份成 `.bak`:UI 里一次误操作就能覆盖掉几百行带注释的蓝图,
/// 而编辑器没有版本历史。一份最近备份的成本近乎为零。
pub async fn file_put(Query(q): Query<PathQuery>, body: String) -> impl IntoResponse {
    let path = match resolve(&q.path) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    if path.exists() {
        let _ = std::fs::copy(&path, path.with_extension("yaml.bak"));
    }
    match std::fs::write(&path, body.as_bytes()) {
        Ok(()) => Json(json!({ "ok": true, "path": q.path, "bytes": body.len() })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

/// `POST /api/file/trash?path=…` —— 移入回收站,不做永久删除。
///
/// UI 里一次误点就能删掉几百行带注释的蓝图;`.crater-trash/` 让"删错了"
/// 是一次拖回,不是一次事故。删除前做**引用检查**:被 app 文件引用的
/// 蓝图/inventory 拒删并列出引用者(先解绑再删,顺序不能省)。
pub async fn file_trash(Query(q): Query<PathQuery>) -> impl IntoResponse {
    let path = match resolve(&q.path) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    if !path.exists() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "文件不存在" }))).into_response();
    }
    let refs: Vec<String> = crate::ui_app::list_apps()
        .into_iter()
        .filter(|a| a.blueprint == q.path || a.inventory == q.path)
        .map(|a| a.path)
        .collect();
    if !refs.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": format!("被 {} 引用,先解绑再删", refs.join(", ")), "refs": refs })),
        )
            .into_response();
    }
    let root = match root() {
        Ok(r) => r,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    };
    let trash = root.join(".crater-trash");
    let _ = std::fs::create_dir_all(&trash);
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dest = trash.join(format!(
        "{}.{stamp}",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    match std::fs::rename(&path, &dest) {
        Ok(()) => Json(json!({ "ok": true, "trashed_to": dest.display().to_string() })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

/// `POST /api/file/rename?path=…`(正文=新相对路径)。同样过引用检查:
/// 改名会让 app 里的引用悬空,先改引用再改名。
pub async fn file_rename(Query(q): Query<PathQuery>, body: String) -> impl IntoResponse {
    let from = match resolve(&q.path) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    let to = match resolve(body.trim()) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    let refs: Vec<String> = crate::ui_app::list_apps()
        .into_iter()
        .filter(|a| a.blueprint == q.path || a.inventory == q.path)
        .map(|a| a.path)
        .collect();
    if !refs.is_empty() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": format!("被 {} 引用,先改引用再改名", refs.join(", ")) })),
        )
            .into_response();
    }
    if to.exists() {
        return (StatusCode::CONFLICT, Json(json!({ "error": "目标已存在" }))).into_response();
    }
    match std::fs::rename(&from, &to) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response(),
    }
}

pub async fn view_edit() -> Html<&'static str> {
    Html(EDIT_HTML)
}

/// 编辑器视图。
///
/// **零外部依赖**:没有 CodeMirror/Monaco,也没有 JS 构建步骤 —— 那与项目
/// "无额外依赖、可离线"的底线冲突,而且一个百兆的编辑器包为了语法高亮并不值。
///
/// 用的是经典做法:一个透明的 `<textarea>` 盖在 `<pre>` 上,pre 里渲染着色
/// 后的同一份文本,两者滚动同步。约百行原生 JS 换来真实的 YAML 高亮、行号槽、
/// 错误行标记与点击跳转。
const EDIT_HTML: &str = r##"<section class="panel">
  <h2><span class="mk">✎</span> 蓝图编辑器</h2>
  <div class="ed-bar">
    <select id="ed-file"><option value="">— 选择文件 —</option></select>
    <span id="ed-kind" class="ed-kind"></span>
    <button class="btn" onclick="edSave()">保存</button>
    <button class="btn" onclick="edSkeleton()">生成 inventory 骨架</button>
    <button class="btn" onclick="edNew('blueprint')">新建蓝图</button>
    <button class="btn" onclick="edNew('app')">新建 App</button>
    <span id="ed-status" class="ed-status"></span>
  </div>
  <div id="ed-wiz" class="ed-wiz" style="display:none"></div>
  <div class="ed-wrap">
    <div class="ed-gutter" id="ed-gutter"></div>
    <div class="ed-code">
      <pre id="ed-hl" aria-hidden="true"></pre>
      <textarea id="ed-ta" spellcheck="false" wrap="off"></textarea>
    </div>
  </div>
  <div class="ed-below">
    <div id="ed-diag" class="ed-diag"></div>
    <div id="ed-card" class="ed-card"></div>
  </div>
</section>

<style>
  .ed-bar{display:flex;gap:8px;align-items:center;margin-bottom:10px;flex-wrap:wrap}
  .ed-bar select{background:var(--surface-2);color:var(--text);border:1px solid var(--border);
    border-radius:8px;padding:6px 10px;font:inherit;min-width:280px}
  .ed-kind{font-size:12px;color:var(--muted);padding:2px 8px;border-radius:6px;background:var(--surface-2)}
  .ed-status{font-size:12px;color:var(--muted);margin-left:auto}
  .ed-wrap{display:flex;border:1px solid var(--border);border-radius:10px;overflow:hidden;
    background:var(--surface);height:56vh;min-height:320px}
  .ed-gutter{flex:0 0 56px;background:var(--surface-2);border-right:1px solid var(--border);
    padding:10px 6px;text-align:right;overflow:hidden;
    font:12px/1.55 ui-monospace,SFMono-Regular,Menlo,monospace;color:var(--faint);user-select:none}
  .ed-gutter .gl.err{color:#fff;background:var(--drift);border-radius:4px}
  .ed-gutter .gl.warn{color:#fff;background:var(--accent);border-radius:4px}
  .ed-code{position:relative;flex:1;overflow:hidden}
  .ed-code pre,.ed-code textarea{
    margin:0;padding:10px 12px;border:0;outline:none;white-space:pre;overflow:auto;
    font:12px/1.55 ui-monospace,SFMono-Regular,Menlo,monospace;
    position:absolute;inset:0;tab-size:2}
  .ed-code pre{pointer-events:none;color:var(--text)}
  .ed-code textarea{background:transparent;color:transparent;caret-color:var(--accent);resize:none}
  .ed-code .k{color:var(--accent-2)} .ed-code .s{color:var(--ok)}
  .ed-code .c{color:var(--faint);font-style:italic} .ed-code .n{color:#8b5cf6}
  .ed-code .v{color:var(--text)}
  [data-theme="dark"] .ed-code .n{color:#c4b5fd}
  .ed-diag{margin-top:10px;max-height:22vh;overflow:auto;font-size:13px}
  .ed-diag .d{display:flex;gap:8px;padding:6px 8px;border-radius:8px;cursor:pointer;align-items:baseline}
  .ed-diag .d:hover{background:var(--surface-2)}
  .ed-diag .d .ln{flex:0 0 52px;color:var(--muted);font:12px ui-monospace,monospace}
  .ed-diag .d.err .ln{color:var(--drift)} .ed-diag .d.warn .ln{color:var(--accent)}
  .ed-diag .d .at{color:var(--faint);font-size:12px}
  .ed-ok{color:var(--ok);padding:6px 8px}
  .ed-wiz{border:1px solid var(--border);border-radius:10px;padding:12px;margin-bottom:10px;
    background:var(--surface-2);display:flex;gap:8px;flex-wrap:wrap;align-items:center;font-size:13px}
  .ed-wiz input,.ed-wiz select{background:var(--surface);color:var(--text);border:1px solid var(--border);
    border-radius:8px;padding:5px 9px;font:inherit}
  .ed-wiz .types{display:flex;gap:4px;flex-wrap:wrap;max-width:100%}
  .ed-wiz .types label{border:1px solid var(--border);border-radius:99px;padding:2px 9px;cursor:pointer;font-size:12px}
  .ed-wiz .types label:has(input:checked){background:var(--tint);border-color:var(--accent);color:var(--accent)}
  .ed-wiz .types input{display:none}
  .ed-below{display:flex;gap:12px;align-items:flex-start}
  .ed-below .ed-diag{flex:1}
  .ed-card{flex:0 0 300px;border:1px solid var(--border);border-radius:10px;padding:10px 12px;
    background:var(--surface-2);font-size:12.5px;margin-top:10px;display:none}
  .ed-card.on{display:block}
  .ed-card h4{margin:0 0 6px;font-size:13px}
  .ed-card .req{color:var(--drift);font-size:11px;margin-left:6px}
  .ed-card .doc{color:var(--muted);margin:4px 0 8px}
  .ed-card .ty{font:11px ui-monospace,monospace;color:var(--faint)}
  .ed-card .vals{display:flex;gap:5px;flex-wrap:wrap;margin-top:6px}
  .ed-card .vals button{border:1px solid var(--border);background:var(--surface);color:var(--text);
    border-radius:99px;padding:2px 10px;cursor:pointer;font-size:12px}
  .ed-card .vals button:hover{border-color:var(--accent);color:var(--accent)}
  .ed-card .setrow{display:flex;gap:6px;margin-top:8px}
  .ed-card .setrow input{flex:1;background:var(--surface);color:var(--text);
    border:1px solid var(--border);border-radius:8px;padding:4px 8px;font:inherit;font-size:12px}
  .ed-card .flist{margin:4px 0 0;padding-left:16px;color:var(--muted)}
  .ed-card .flist b{color:var(--text);font-weight:600}
  .ed-card .err{color:var(--drift);margin-top:6px}
</style>

<script>
(function(){
  const ta = document.getElementById('ed-ta');
  const hl = document.getElementById('ed-hl');
  const gutter = document.getElementById('ed-gutter');
  const diag = document.getElementById('ed-diag');
  const status = document.getElementById('ed-status');
  const sel = document.getElementById('ed-file');
  const kindEl = document.getElementById('ed-kind');
  let marks = {}, timer = null, cur = '';

  // YAML 着色:注释 / 键 / 引号串 / 数字 —— 刻意只认这四类。
  // 再多就要写一个真解析器,而那正是我们已经有的那个(在 Rust 那边),
  // 在前端复制一份必然与它分家。
  function paint(src){
    const esc = s => s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
    return src.split('\n').map(line=>{
      const m = line.match(/^(\s*#.*)$/);
      if (m) return '<span class="c">'+esc(line)+'</span>';
      let out = esc(line);
      out = out.replace(/(\s#.*)$/, '<span class="c">$1</span>');
      out = out.replace(/^(\s*-?\s*)([A-Za-z_][\w.-]*)(\s*:)/, '$1<span class="k">$2</span>$3');
      out = out.replace(/(&quot;[^&]*?&quot;|&#39;[^&]*?&#39;|'[^']*')/g, '<span class="s">$1</span>');
      out = out.replace(/\b(\d+)\b/g, '<span class="n">$1</span>');
      return out;
    }).join('\n');
  }

  function renderGutter(){
    const n = ta.value.split('\n').length;
    let h = '';
    for (let i=1;i<=n;i++){
      const c = marks[i] ? ' '+marks[i] : '';
      h += '<div class="gl'+c+'">'+i+'</div>';
    }
    gutter.innerHTML = h;
    gutter.scrollTop = ta.scrollTop;
  }

  function sync(){
    hl.innerHTML = paint(ta.value);
    hl.scrollTop = ta.scrollTop; hl.scrollLeft = ta.scrollLeft;
    renderGutter();
  }

  // 校验走后端 /api/lint —— 与 `crater lint` 同一套措辞、行号与拼写建议。
  // 在前端复制一份校验逻辑,两边迟早分家,而用户只会相信眼前这个。
  async function lint(){
    const text = ta.value;
    if (!text.trim()){ diag.innerHTML=''; marks={}; renderGutter(); return; }
    const kind = sel.selectedOptions[0]?.dataset.kind;
    if (kind === 'inventory' || kind === 'other'){
      diag.innerHTML = '<div class="ed-ok">(这份文件不是蓝图,不做蓝图校验)</div>';
      marks={}; renderGutter(); return;
    }
    try{
      const r = await fetch('/api/lint', {method:'POST', body:text});
      const d = await r.json();
      marks = {};
      for (const x of d.diagnostics) if (x.line) marks[x.line] = x.severity === 'error' ? 'err' : 'warn';
      renderGutter();
      if (!d.diagnostics.length){
        const s = d.summary;
        diag.innerHTML = '<div class="ed-ok">✓ '+s.name+' —— '+s.resources+' 资源 · '
          + s.materials+' 物料 · '+s.procedures+' procedure · '+s.health+' 健康探针</div>';
      } else {
        diag.innerHTML = d.diagnostics.map(x =>
          '<div class="d '+(x.severity==='error'?'err':'warn')+'" onclick="edGoto('+(x.line||1)+')">'
          + '<span class="ln">'+(x.line?('L'+x.line):'—')+'</span>'
          + '<span>'+x.message.replace(/</g,'&lt;')+' <span class="at">('+x.at+')</span></span></div>').join('');
      }
      status.textContent = d.ok ? '校验通过' : (d.summary ? d.summary.errors+' 个错误' : '解析失败');
    }catch(e){ status.textContent = '校验请求失败:'+e; }
  }

  // 点诊断跳到那一行:把光标放过去并滚动到可见处。
  window.edGoto = function(line){
    const lines = ta.value.split('\n');
    let pos = 0;
    for (let i=0;i<line-1 && i<lines.length;i++) pos += lines[i].length + 1;
    ta.focus();
    ta.setSelectionRange(pos, pos + (lines[line-1]||'').length);
    const lh = 12*1.55;
    ta.scrollTop = Math.max(0, (line-4)*lh);
    sync();
  };

  window.edSave = async function(){
    if (!cur){ status.textContent = '先选一个文件'; return; }
    const r = await fetch('/api/file?path='+encodeURIComponent(cur), {method:'POST', body: ta.value});
    const d = await r.json();
    status.textContent = d.ok ? ('已保存 '+d.bytes+' 字节(旧版存为 .bak)') : ('保存失败:'+(d.error||''));
  };

  // 直击"要你事先备好 inventory"那个痛点:蓝图已经声明了需要哪些组、
  // 每组最少几台,骨架本就该由机器生成。
  window.edSkeleton = async function(){
    const r = await fetch('/api/inventory/skeleton', {method:'POST', body: ta.value});
    const d = await r.json();
    if (d.error){ status.textContent = '生成失败:'+d.error; return; }
    // 带注释的 YAML 文本(不是 JSON 投影):注释解释"为什么这组留空",
    // 那是骨架一半的价值。
    diag.innerHTML = '<div style="margin:6px 0"><button class="btn" onclick="edAdopt()">✎ 载入为新 inventory 文件</button></div>'
      + '<pre id="ed-skel" style="white-space:pre-wrap;font-size:12px">'
      + d.yaml.replace(/</g,'&lt;') + '</pre>';
    status.textContent = '骨架已生成(按蓝图的 fleet.groups)';
  };

  // 骨架 → 编辑缓冲区:起个配套文件名,用户改完口令按"保存"落盘。
  window.edAdopt = function(){
    const y = document.getElementById('ed-skel');
    if (!y) return;
    const base = (cur||'inventory').replace(/\.blueprint\.yaml$/,'').replace(/\.yaml$/,'');
    cur = base + '.inventory.yaml';
    ta.value = y.textContent; kindEl.textContent = 'inventory(未保存)';
    diag.innerHTML=''; sync();
    status.textContent = '未保存 → ' + cur + '(改好地址口令后点"保存")';
  };

  // ── 新建向导:蓝图(选类型拼骨架)/ App(绑定蓝图×机群×参数)──
  // 表单字段全部来自后端(/api/types、/api/files):UI 不硬编码任何类型名。
  window.edWizHide = function(){ document.getElementById('ed-wiz').style.display='none'; };
  window.edNew = async function(kind){
    const wiz = document.getElementById('ed-wiz');
    wiz.style.display='flex';
    if (kind==='blueprint'){
      const d = await (await fetch("/api/types")).json();
      const list = Array.isArray(d) ? d : d.types;
      wiz.innerHTML = '<b>新建蓝图</b> 名字 <input id="wz-name" placeholder="my-svc" size="12">'
        + '<span class="types">' + list.map(t=>
            '<label title="'+(t.doc||'').replace(/"/g,'&quot;')+'"><input type="checkbox" value="'+t.name+'">'+t.name+'</label>'
          ).join('') + '</span>'
        + '<button class="btn" onclick="edNewBp()">生成骨架</button>'
        + '<button class="btn" onclick="edWizHide()">×</button>';
    } else {
      const f = await (await fetch('/api/files')).json();
      const opt = k => f.files.filter(x=>x.kind===k && !x.path.includes('fixtures') && !x.path.includes('/tests/')).map(x=>'<option>'+x.path+'</option>').join('');
      wiz.innerHTML = '<b>新建 App</b> 名字 <input id="wz-name" placeholder="prod-nginx" size="12">'
        + ' 蓝图 <select id="wz-bp">'+opt('blueprint')+'</select>'
        + ' inventory <select id="wz-inv"><option value=""></option>'+opt('inventory')+'</select>'
        + ' 参数 <input id="wz-params" placeholder="k=v,k2=v2" size="14">'
        + ' 巡检 <input id="wz-iv" placeholder="30m(留空=只手动)" size="14">'
        + ' <button class="btn" onclick="edNewApp()">创建</button>'
        + '<button class="btn" onclick="edWizHide()">×</button>';
    }
  };
  window.edNewBp = async function(){
    const name = document.getElementById('wz-name').value.trim() || 'unnamed';
    const types = [...document.querySelectorAll('#ed-wiz input:checked')].map(x=>x.value);
    const d = await (await fetch('/api/blueprint/skeleton',{method:'POST',
      headers:{'Content-Type':'application/json'}, body:JSON.stringify({name,types})})).json();
    if (d.error){ status.textContent = d.error; return; }
    cur = name + '.blueprint.yaml';
    ta.value = d.yaml; kindEl.textContent = 'blueprint(未保存)';
    document.getElementById('ed-wiz').style.display='none';
    sync(); lint();
    status.textContent = '未保存 → ' + cur + '(补完 TODO 后点"保存")';
  };
  window.edNewApp = async function(){
    const name = document.getElementById('wz-name').value.trim();
    const params = document.getElementById('wz-params').value.split(',')
      .map(s=>s.trim()).filter(Boolean).map(s=>{const i=s.indexOf('=');return [s.slice(0,i),s.slice(i+1)];});
    const d = await (await fetch('/api/app/create',{method:'POST',
      headers:{'Content-Type':'application/json'}, body:JSON.stringify({
        name, blueprint:document.getElementById('wz-bp').value,
        inventory:document.getElementById('wz-inv').value, params,
        verify_interval:document.getElementById('wz-iv').value.trim()})})).json();
    if (d.error){ status.textContent = d.error; return; }
    document.getElementById('ed-wiz').style.display='none';
    // app 文件立即落盘(它是绑定关系,半成品没有意义),直接载入并跑跨文件校验
    const o = document.createElement('option');
    o.value = d.path; o.textContent = d.path + '  · app'; o.dataset.kind='app';
    sel.appendChild(o); sel.value = d.path;
    load(d.path);
    const lr = await (await fetch('/api/lint-project',{method:'POST',body:d.path})).json();
    status.textContent = '已创建 ' + d.path + (lr.ok?'(校验通过)':'(有 '+lr.diagnostics.length+' 个问题,见下方)');
    if (!lr.ok) diag.innerHTML = lr.diagnostics.map(x =>
      '<div class="d err"><span class="ln">—</span><span>'+x.message.replace(/</g,'&lt;')+'</span></div>').join('');
  };

  async function load(path){
    cur = path;
    if (!path){ ta.value=''; sync(); diag.innerHTML=''; kindEl.textContent=''; return; }
    const r = await fetch('/api/file?path='+encodeURIComponent(path));
    const d = await r.json();
    if (d.error){ status.textContent = d.error; return; }
    ta.value = d.text; kindEl.textContent = d.kind; sync(); lint();
    status.textContent = '已载入';
  }

  // ── 表单投影:光标字段卡(登记表 → 编辑现场)+ span 级定点补丁 ──
  // 字段说明与可选值来自 /api/context(登记表同源);写回经 /api/patch
  // (单行 scalar,保注释;锚点/flow 由服务端拒绝并降级只读)。
  const card = document.getElementById('ed-card');
  let ctxTimer = null, curLine = 0;
  function cursorLine(){ return ta.value.slice(0, ta.selectionStart).split('\n').length; }
  async function showCtx(){
    if (!ta.value.trim() || (sel.selectedOptions[0]?.dataset.kind||'') === 'inventory'){ card.className='ed-card'; return; }
    curLine = cursorLine();
    const d = await (await fetch('/api/context',{method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({text:ta.value, line:curLine})})).json();
    const esc = x => String(x).replace(/</g,'&lt;');
    if (d.context === 'field'){
      const c = d.card;
      let h = '<h4>'+esc(d.type)+' · '+esc(d.field)
        + (c.required?'<span class="req">必填</span>':'')+'</h4>'
        + '<div class="doc">'+esc(c.doc||'')+'</div>'
        + '<div class="ty">'+esc(c.type||'')+(c.one_of?' · 互斥组 '+esc(c.one_of):'')+'</div>';
      if ((c.values||[]).length){
        h += '<div class="vals">'+c.values.map(v=>
          '<button onclick="edSet('+JSON.stringify(String(v)).replace(/"/g,'&quot;')+')">'+esc(v)+'</button>').join('')+'</div>';
      } else {
        h += '<div class="setrow"><input id="ed-setv" placeholder="新值"><button class="btn" onclick="edSet(document.getElementById(\'ed-setv\').value)">应用</button></div>';
      }
      h += '<div id="ed-cardmsg" class="err"></div>';
      card.innerHTML = h; card.className='ed-card on';
    } else if (d.context === 'type'){
      const c = d.card;
      card.innerHTML = '<h4>'+esc(d.type)+'</h4><div class="doc">'+esc(c.doc||'')+'</div>'
        + '<ul class="flist">'+(c.fields||[]).map(f=>
            '<li><b>'+esc(f.name)+'</b>'+(f.required?'<span class="req">必填</span>':'')
            +' — '+esc(f.doc||'')+'</li>').join('')+'</ul>';
      card.className='ed-card on';
    } else if (d.context === 'unknown_field'){
      card.innerHTML = '<h4>'+esc(d.type)+' · '+esc(d.field)+'</h4><div class="err">登记表没有这个字段'
        + (d.suggestion?',是不是 <b>'+esc(d.suggestion)+'</b>?':'')+'</div>';
      card.className='ed-card on';
    } else if (d.context === 'custom'){
      card.innerHTML = '<h4>'+esc(d.type)+'</h4><div class="doc">'+esc(d.note)+'</div>';
      card.className='ed-card on';
    } else { card.className='ed-card'; }
  }
  window.edSet = async function(v){
    const r = await fetch('/api/patch',{method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({text:ta.value, line:curLine, value:String(v)})});
    const d = await r.json();
    if (d.error){ const m=document.getElementById('ed-cardmsg'); if(m) m.textContent=d.error; return; }
    const pos = ta.selectionStart;
    ta.value = d.text; ta.setSelectionRange(pos, pos); sync(); lint();
    status.textContent = 'L'+curLine+' 已改(未保存)';
  };
  const ctxKick = () => { clearTimeout(ctxTimer); ctxTimer = setTimeout(showCtx, 300); };
  ta.addEventListener('click', ctxKick);
  ta.addEventListener('keyup', e => { if (!e.ctrlKey && !e.metaKey) ctxKick(); });

  sel.addEventListener('change', () => load(sel.value));
  ta.addEventListener('input', () => { sync(); clearTimeout(timer); timer = setTimeout(lint, 400); });
  ta.addEventListener('scroll', () => { hl.scrollTop=ta.scrollTop; hl.scrollLeft=ta.scrollLeft; gutter.scrollTop=ta.scrollTop; });
  // Tab 缩进两格:YAML 里 Tab 是非法字符,浏览器默认的"跳到下一个控件"
  // 在编辑器里也毫无用处。
  ta.addEventListener('keydown', e => {
    if (e.key === 'Tab'){ e.preventDefault();
      const s=ta.selectionStart, t=ta.selectionEnd;
      ta.value = ta.value.slice(0,s)+'  '+ta.value.slice(t);
      ta.selectionStart = ta.selectionEnd = s+2; sync(); }
  });

  fetch('/api/files').then(r=>r.json()).then(d=>{
    for (const f of d.files){
      const o = document.createElement('option');
      o.value = f.path; o.textContent = f.path + '  · ' + f.kind; o.dataset.kind = f.kind;
      sel.appendChild(o);
    }
    status.textContent = d.files.length + ' 个 YAML(根:' + d.root + ')';
  });
})();
</script>
"##;
