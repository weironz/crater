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
    <span id="ed-status" class="ed-status"></span>
  </div>
  <div class="ed-wrap">
    <div class="ed-gutter" id="ed-gutter"></div>
    <div class="ed-code">
      <pre id="ed-hl" aria-hidden="true"></pre>
      <textarea id="ed-ta" spellcheck="false" wrap="off"></textarea>
    </div>
  </div>
  <div id="ed-diag" class="ed-diag"></div>
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
    diag.innerHTML = '<pre style="white-space:pre-wrap;font-size:12px">'
      + JSON.stringify(d.inventory, null, 2).replace(/</g,'&lt;')
      + '\n\n# ' + d.note + '</pre>';
    status.textContent = '骨架已生成(按蓝图的 fleet.groups)';
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
