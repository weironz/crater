//! App 绑定文件(阶段③)—— "任务"的正身。
//!
//! App = 蓝图 × inventory × 参数固化值 × 巡检间隔,对应 ArgoCD 的 Application、
//! AWX 的 Job Template,但**它是一个带注释的 YAML 文件,不是数据库行**:
//! 可 git、可 diff、注释有处安放、随闭包进 air-gap。AWX 的 survey 与 playbook
//! 脱节的病根正是"定义在内容里、值在 DB 里" —— 这里两者同在一棵文件树上。
//!
//! 按评审裁定,app **不进** 26 类型登记表(五动词契约对"文档"无意义):
//! 它是约定形状的 YAML + 专用校验函数。校验走 `/api/lint-project`
//! (跨文件,读文件前过路径禁闭),与单文档 `/api/lint` 分层 ——
//! "校验只有一份"的承诺下沉到 lint 核心库函数层,不破。

use std::path::{Path, PathBuf};

use axum::{
    http::StatusCode,
    response::{IntoResponse, Json, Response},
};
use serde_json::json;

#[derive(Debug, Clone, Default)]
pub struct AppDef {
    pub path: String,
    pub name: String,
    pub blueprint: String,
    pub inventory: String,
    pub params: Vec<(String, String)>,
    /// 巡检间隔(秒);None = 只手动。
    pub verify_interval: Option<u64>,
    /// 只对机群里的这些主机 / 组执行(`--limit`);空 = 全量。
    pub limit: Vec<String>,
}

/// 这份 YAML 是不是 app 文件(形状判定,与其它 classify 同思路)。
pub fn is_app_value(v: &serde_yaml::Value) -> bool {
    v.get("app").map(|a| a.get("blueprint").is_some()).unwrap_or(false)
}

pub fn parse_app(path: &Path, text: &str) -> Result<AppDef, String> {
    let v: serde_yaml::Value = serde_yaml::from_str(text).map_err(|e| format!("YAML:{e}"))?;
    let a = v.get("app").ok_or("缺少顶层 `app:`")?;
    let s = |k: &str| a.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let mut params = Vec::new();
    if let Some(m) = a.get("params").and_then(|x| x.as_mapping()) {
        for (k, val) in m {
            let key = k.as_str().unwrap_or_default().to_string();
            let sv = match val {
                serde_yaml::Value::String(x) => x.clone(),
                other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
            };
            params.push((key, sv));
        }
    }
    let interval = a
        .get("verify")
        .and_then(|x| x.get("interval"))
        .and_then(|x| x.as_str())
        .map(parse_interval)
        .transpose()?;
    Ok(AppDef {
        path: path.display().to_string(),
        name: s("name").unwrap_or_else(|| {
            path.file_stem().unwrap_or_default().to_string_lossy().trim_end_matches(".app").to_string()
        }),
        blueprint: s("blueprint").ok_or("app 缺少 `blueprint:`")?,
        inventory: s("inventory").unwrap_or_default(),
        params,
        verify_interval: interval,
        limit: a
            .get("limit")
            .and_then(|x| x.as_sequence())
            .map(|xs| xs.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
    })
}

/// "30m" / "2h" / "90s" → 秒。写错单位要报错,不要静默当秒。
fn parse_interval(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mul) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        Some('d') => (&s[..s.len() - 1], 86400),
        _ => return Err(format!("verify.interval `{s}`:要带单位(s/m/h/d),如 30m")),
    };
    num.parse::<u64>()
        .map(|n| n * mul)
        .map_err(|_| format!("verify.interval `{s}` 不是数字+单位"))
}

/// 工作目录里全部 app 文件。
pub fn list_apps() -> Vec<AppDef> {
    let mut out = Vec::new();
    let Ok(root) = crate::ui_edit::root() else { return out };
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || matches!(name.as_str(), "target" | "node_modules") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            if !name.ends_with(".app.yaml") && !name.ends_with(".app.yml") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&p) {
                let rel = p.strip_prefix(&root).unwrap_or(&p).to_path_buf();
                if let Ok(app) = parse_app(&rel, &text) {
                    out.push(app);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// 跨文件校验:app 的引用完整性 + params 与蓝图声明对账 + fleet×inventory 对照。
///
/// 拼错的 app 参数会**静默失效**(蓝图按默认值跑),那是配置类 bug 里最难查的
/// 一种 —— 所以这里是 error 不是 warn。
pub fn lint_app(app: &AppDef) -> Vec<serde_json::Value> {
    let mut diags = Vec::new();
    let mut push = |sev: &str, msg: String| diags.push(json!({ "severity": sev, "message": msg }));

    // app 里的路径是**工作区相对路径**,不是进程 CWD 相对 —— UI 可以起在
    // 任意目录(--workspace),按 CWD 解析会把"文件明明在树里"报成不存在。
    let ws = crate::ui_edit::root().unwrap_or_else(|_| PathBuf::from("."));
    let bp_path = ws.join(&app.blueprint);
    let bp = if !bp_path.is_file() {
        push("error", format!("blueprint 不存在:{}", app.blueprint));
        None
    } else {
        match crater_ir::parse::blueprint_from_path(&bp_path) {
            Ok(b) => Some(b),
            Err(e) => {
                push("error", format!("blueprint 解析失败:{e}"));
                None
            }
        }
    };
    if let Some(bp) = &bp {
        for (k, _) in &app.params {
            if !bp.params.contains_key(k) {
                let names: Vec<&str> = bp.params.keys().map(String::as_str).collect();
                let hint = closest(k, &names)
                    .map(|c| format!(",是不是 `{c}`?"))
                    .unwrap_or_default();
                push("error", format!("params.{k}:蓝图 `{}` 没有这个参数{hint}", bp.name));
            }
        }
        for (name, spec) in &bp.params {
            if spec.default.is_none() && !app.params.iter().any(|(k, _)| k == name) {
                push("error", format!("蓝图必填参数 `{name}` 未提供(app.params 里补上,或设为启动时问)"));
            }
        }
    }
    if !app.inventory.is_empty() {
        let inv_path = ws.join(&app.inventory);
        if !inv_path.is_file() {
            push("error", format!("inventory 不存在:{}", app.inventory));
        } else if let Some(bp) = &bp {
            // fleet 契约在**保存 app 时**就核对,不等到连机器。
            match check_fleet(bp, &inv_path) {
                Ok(errs) => {
                    for e in errs {
                        push("error", format!("机群契约:{e}"));
                    }
                }
                Err(e) => push("warning", format!("inventory 读取:{e}")),
            }
        }
    }
    diags
}

/// 蓝图 fleet.groups × inventory 实际组员的对账(轻量解析,不连机器)。
fn check_fleet(bp: &crater_ir::ir::Blueprint, inv: &Path) -> Result<Vec<String>, String> {
    if bp.fleet.groups.is_empty() {
        return Ok(vec![]);
    }
    let text = std::fs::read_to_string(inv).map_err(|e| e.to_string())?;
    let v: serde_yaml::Value = serde_yaml::from_str(&text).map_err(|e| e.to_string())?;
    let groups = v
        .get("inventory")
        .and_then(|i| i.get("groups"))
        .and_then(|g| g.as_mapping());
    let mut errs = Vec::new();
    for (name, c) in &bp.fleet.groups {
        let have = groups
            .and_then(|g| g.get(serde_yaml::Value::from(name.clone())))
            .and_then(|e| e.get("hosts"))
            .and_then(|h| h.as_sequence())
            .map(|s| s.len());
        match have {
            None if c.min > 0 => errs.push(format!("inventory 缺少组 `{name}`(需 ≥{})", c.min)),
            Some(n) if n < c.min => errs.push(format!("组 `{name}` 只有 {n} 台,需 ≥{}", c.min)),
            _ => {}
        }
    }
    Ok(errs)
}

fn closest<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (*c, dist(word, c)))
        .filter(|(c, d)| *d <= 2 && *d * 2 <= c.len().max(word.len()))
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c)
}

fn dist(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

// ---------------------------------------------------------------- HTTP 面

/// `GET /api/apps` —— app 列表 + 各自的跨文件校验结论。
pub async fn apps() -> impl IntoResponse {
    let list: Vec<serde_json::Value> = list_apps()
        .iter()
        .map(|a| {
            let diags = lint_app(a);
            json!({
                "path": a.path,
                "name": a.name,
                "blueprint": a.blueprint,
                "inventory": a.inventory,
                "params": a.params.iter().map(|(k, v)| json!({"k": k, "v": v})).collect::<Vec<_>>(),
                "verify_interval": a.verify_interval,
                "limit": a.limit,
                "ok": diags.iter().all(|d| d["severity"] != "error"),
                "diagnostics": diags,
            })
        })
        .collect();
    Json(json!({ "apps": list }))
}

/// `POST /api/lint-project` —— 对一个路径(app 文件)做跨文件校验。
///
/// 与 `/api/lint`(纯函数、单文档)分层:这里要读文件,路径先过禁闭。
pub async fn lint_project(body: String) -> Response {
    let rel = body.trim();
    let path = match crate::ui_edit::confine(rel) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": format!("读不到 {rel}") }))).into_response();
    };
    // 相对工作目录解析 app 内的引用 —— app 文件里的路径是工作区相对路径。
    let rel_path = PathBuf::from(rel);
    match parse_app(&rel_path, &text) {
        Ok(app) => {
            let diags = lint_app(&app);
            Json(json!({
                "ok": diags.iter().all(|d| d["severity"] != "error"),
                "app": { "name": app.name, "blueprint": app.blueprint, "inventory": app.inventory },
                "diagnostics": diags,
            }))
            .into_response()
        }
        Err(e) => Json(json!({
            "ok": false,
            "diagnostics": [{ "severity": "error", "message": e }],
        }))
        .into_response(),
    }
}

/// `POST /api/app/create` —— 生成一份**带注释**的 app 文件(模板拼文本,
/// 不走序列化:序列化产不出注释,而注释是这类文件一半的价值)。
#[derive(serde::Deserialize)]
pub struct CreateApp {
    pub name: String,
    pub blueprint: String,
    #[serde(default)]
    pub inventory: String,
    #[serde(default)]
    pub params: Vec<(String, String)>,
    #[serde(default)]
    pub verify_interval: String,
    /// 主机名 / 组名;空 = 整份 inventory。
    #[serde(default)]
    pub limit: Vec<String>,
}

pub async fn create_app(Json(req): Json<CreateApp>) -> Response {
    if req.name.is_empty() || !req.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "app 名只允许字母数字-_" }))).into_response();
    }
    let file = format!("{}.app.yaml", req.name);
    let abs = match crate::ui_edit::confine(&file) {
        Ok(p) => p,
        Err((c, m)) => return (c, Json(json!({ "error": m }))).into_response(),
    };
    if abs.exists() {
        return (StatusCode::CONFLICT, Json(json!({ "error": format!("{file} 已存在") }))).into_response();
    }
    let mut body = format!(
        "# {name} —— 把这份蓝图钉在这群机器上。\n\
         # 这个文件就是\"任务\"本身:可 git、可 diff、可进闭包;改它 = 改任务。\n\
         app:\n  name: {name}\n  blueprint: {bp}\n",
        name = req.name,
        bp = req.blueprint,
    );
    if !req.inventory.is_empty() {
        body.push_str(&format!("  inventory: {}\n", req.inventory));
    }
    if !req.limit.is_empty() {
        body.push_str(&format!(
            "  limit: [{}]   # 只对这些主机/组执行;删掉 = 整份 inventory\n",
            req.limit.join(", ")
        ));
    }
    if !req.params.is_empty() {
        body.push_str("  params:            # 固化值;没列的参数 = 启动时问\n");
        for (k, v) in &req.params {
            body.push_str(&format!("    {k}: {v}\n"));
        }
    }
    if !req.verify_interval.is_empty() {
        body.push_str(&format!(
            "  verify:\n    interval: {}   # 定时漂移巡检;删掉这段 = 只手动\n",
            req.verify_interval
        ));
    }
    if let Err(e) = std::fs::write(&abs, &body) {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": e.to_string() }))).into_response();
    }
    Json(json!({ "ok": true, "path": file })).into_response()
}

// ---------------------------------------------------------------- 调度器

/// 定时 verify:读 app 文件的 verify.interval,到点起 verify job。
///
/// 调度器只是**点火器**,逻辑全在 CLI —— 不违反"UI 是视图"。
/// 全局同一时刻最多 1 个自动 verify(air-gap 环境对 SSH 压力敏感),
/// 错过的窗口不补跑(巡检要的是"最近看过没",不是打卡记录)。
pub fn start_scheduler() {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let auto_running = crate::ui_run::list()
                .iter()
                .any(|j| j.status == "running" && j.title.starts_with("auto-verify"));
            if auto_running {
                continue;
            }
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            for app in list_apps() {
                let Some(interval) = app.verify_interval else { continue };
                let interval = interval.max(300); // 下限 5 分钟
                let last = last_verify_ts(&app);
                if now.saturating_sub(last) < interval {
                    continue;
                }
                let mut args = vec![
                    "verify".into(),
                    app.blueprint.clone(),
                    "--json".into(),
                    "__JOBDIR__/verify.json".into(),
                ];
                if !app.inventory.is_empty() {
                    args.push("-i".into());
                    args.push(app.inventory.clone());
                }
                // 巡检要跟任务盯的是同一批机器 —— 少带 limit,巡检会去核对
                // 这个任务根本没管的主机,然后报一堆与它无关的"漂移"。
                if !app.limit.is_empty() {
                    args.push("--limit".into());
                    args.push(app.limit.join(","));
                }
                for (k, v) in &app.params {
                    args.push("--set".into());
                    args.push(format!("{k}={v}"));
                }
                crate::ui_run::spawn(
                    format!("auto-verify {}", app.name),
                    "verify".into(),
                    app.blueprint.clone(),
                    app.inventory.clone(),
                    args,
                );
                break; // 一轮只点一个,错峰
            }
        }
    });
}

/// 该 app 上一次 verify 的时刻:取该蓝图对应快照里最新的 ts。
fn last_verify_ts(app: &AppDef) -> u64 {
    // 快照按 record_id(蓝图名@目标)存;用蓝图名前缀匹配。
    let bp_name = crater_ir::parse::blueprint_from_path(Path::new(&app.blueprint))
        .map(|b| b.name)
        .unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = PathBuf::from(home).join(".crater").join("ui").join("verify");
    let mut latest = 0u64;
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let fname = e.file_name().to_string_lossy().into_owned();
            if !fname.starts_with(&format!("{bp_name}_")) {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(e.path()) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    latest = latest.max(v["ts"].as_u64().unwrap_or(0));
                }
            }
        }
    }
    // 也考虑正在跑/刚跑过的 auto job,防止 60s 轮询窗口内重复点火。
    for j in crate::ui_run::list() {
        if j.title == format!("auto-verify {}", app.name) {
            latest = latest.max(j.started);
        }
    }
    latest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interval_needs_a_unit() {
        // "30" 当 30 秒还是 30 分?猜错一个数量级,巡检要么打爆 SSH 要么形同虚设。
        assert!(parse_interval("30").is_err());
        assert_eq!(parse_interval("30m").unwrap(), 1800);
        assert_eq!(parse_interval("2h").unwrap(), 7200);
    }

    #[test]
    fn an_app_file_parses_with_params_and_interval() {
        let text = "app:\n  name: web\n  blueprint: b.yaml\n  inventory: i.yaml\n  params:\n    ha: true\n  verify:\n    interval: 30m\n";
        let a = parse_app(Path::new("web.app.yaml"), text).unwrap();
        assert_eq!(a.name, "web");
        assert_eq!(a.params, vec![("ha".into(), "true".into())]);
        assert_eq!(a.verify_interval, Some(1800));
    }

    #[test]
    fn a_misspelled_app_param_is_an_error_not_a_warning() {
        // 拼错的参数会静默失效(蓝图按默认值跑)—— 配置类 bug 里最难查的一种。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("b.yaml"),
            "name: b\nparams:\n  version: { default: \"1\" }\nresources:\n  - file: { path: /x, state: directory }\n",
        )
        .unwrap();
        let app = AppDef {
            blueprint: dir.path().join("b.yaml").display().to_string(),
            params: vec![("verion".into(), "2".into())],
            ..Default::default()
        };
        let diags = lint_app(&app);
        assert!(diags.iter().any(|d| d["severity"] == "error"
            && d["message"].as_str().unwrap().contains("version")), "{diags:?}");
    }
}

/// `GET /view/tasks` —— 任务:app 文件的增删改跑。
///
/// "任务"在新管线里不是数据库里的一行,而是一份 app 文件:蓝图 × 机群 ×
/// 参数的绑定。所以这一页做的是文件的增删,不是记录的增删 —— 它可 git、
/// 可 diff、可进闭包,而 UI 只是它的一个面。
pub async fn view_tasks() -> axum::response::Html<&'static str> {
    axum::response::Html(TASKS_HTML)
}

const TASKS_HTML: &str = r##"<section class="panel">
  <h2><span class="mk">✦</span> 任务</h2>
  <div class="tk-form">
    <label>名字<input id="t-name" placeholder="prod-nginx"></label>
    <label>蓝图<select id="t-bp"></select></label>
    <label>机群<select id="t-inv"><option value="">(不指定 —— 本机)</option></select></label>
    <label>限定范围<span id="t-limit" class="limitbox"><span class="hint">先选机群</span></span></label>
    <label>参数<input id="t-params" placeholder="k=v,k2=v2"></label>
    <label>巡检<input id="t-iv" placeholder="30m(留空=只手动)"></label>
    <button class="btn primary" onclick="taskAdd()">新建任务</button>
    <span id="t-msg" class="tk-msg"></span>
  </div>
  <div id="t-body"></div>
</section>
<style>
  .tk-form{display:flex;gap:8px;flex-wrap:wrap;align-items:flex-end;border:1px solid var(--border);
    border-radius:10px;padding:12px;background:var(--surface-2);margin-bottom:14px}
  .tk-form label{display:flex;flex-direction:column;gap:4px;font-size:12px;color:var(--muted)}
  .tk-form input,.tk-form select{background:var(--surface);color:var(--text);
    border:1px solid var(--border);border-radius:8px;padding:6px 9px;font:inherit;font-size:13px}
  .tk-form input{width:150px}
  .tk-form select{max-width:280px}
  .tk-msg{font-size:12px;color:var(--muted);margin-left:auto}
  .tk-msg.bad{color:var(--drift)}
  .btn.primary{background:var(--accent);color:#fff;border:0}
  .tk{border:1px solid var(--border);border-radius:10px;padding:10px 14px;background:var(--surface);
    margin-bottom:8px;display:flex;gap:10px;align-items:center;flex-wrap:wrap;font-size:13px}
  .tk .nm{font-weight:650}
  .tk .meta{color:var(--faint);font-size:12px}
  .tk .bad{color:var(--drift);font-size:12px}
  .tk .sp{margin-left:auto;display:flex;gap:6px}
  .tk .btn{font-size:12px;padding:3px 10px}
  .tk-empty{border:1px dashed var(--border);border-radius:12px;padding:20px;
    text-align:center;color:var(--muted)}
  .limitbox{display:flex;gap:4px;flex-wrap:wrap;padding:6px;border:1px solid var(--border);
    border-radius:8px;background:var(--surface);min-height:30px;max-width:340px}
  .limitbox label{border:1px solid var(--border);border-radius:99px;padding:1px 9px;
    cursor:pointer;font-size:12px;color:var(--text);flex-direction:row}
  .limitbox label:has(input:checked){background:var(--tint);border-color:var(--accent);color:var(--accent)}
  .limitbox label.grp{border-style:dashed}
  .limitbox input{display:none}
  .limitbox .hint{font-size:12px;color:var(--faint)}
</style>
<script>
(function(){
  const msg = document.getElementById('t-msg');
  const body = document.getElementById('t-body');
  function say(t, bad){ msg.textContent=t; msg.className='tk-msg'+(bad?' bad':''); }
  function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;'); }
  async function fillPickers(){
    const d = await (await fetch('/api/files')).json();
    const pick = k => d.files.filter(f => f.kind === k
      && !f.path.includes('fixtures') && !f.path.includes('/tests/'));
    document.getElementById('t-bp').innerHTML =
      pick('blueprint').concat(pick('stack')).map(f=>'<option>'+f.path+'</option>').join('');
    document.getElementById('t-inv').innerHTML = '<option value="">(不指定 —— 本机)</option>'
      + pick('inventory').map(f=>'<option>'+f.path+'</option>').join('');
    document.getElementById('t-inv').addEventListener('change', fillLimit);
  }
  // 限定候选来自选中的机群 —— 主机与组同列(CLI --limit 两者都收)。
  async function fillLimit(){
    const box = document.getElementById('t-limit');
    const inv = document.getElementById('t-inv').value;
    if (!inv){ box.innerHTML = '<span class="hint">不指定机群 = 本机,无从限定</span>'; return; }
    try{
      const d = await (await fetch('/api/inventory/read?path='+encodeURIComponent(inv))).json();
      if (d.error){ box.innerHTML = '<span class="hint">'+esc(d.error)+'</span>'; return; }
      box.innerHTML = d.groups.map(g=>'<label class="grp" title="组"><input type="checkbox" value="'
          + esc(g.name)+'">@'+esc(g.name)+'</label>').join('')
        + d.hosts.map(h=>'<label title="主机"><input type="checkbox" value="'
          + esc(h.name)+'">'+esc(h.name)+'</label>').join('')
        || '<span class="hint">这份机群是空的</span>';
    }catch(e){ box.innerHTML = '<span class="hint">读取失败</span>'; }
  }
  async function load(){
    const d = await (await fetch('/api/apps')).json();
    if (!d.apps.length){
      body.innerHTML = '<div class="tk-empty">还没有任务。<br><br>'
        + '任务 = 一份 <b>app 文件</b>:把某张蓝图钉在某群机器上,附带固化参数。<br>'
        + '用上面的表单建第一个 —— 它会落成工作区里的 <code>&lt;名字&gt;.app.yaml</code>,可 git 可 diff。</div>';
      return;
    }
    body.innerHTML = d.apps.map(a=>{
      const sets = (a.params||[]).map(p=>p.k+'='+p.v);
      const arg = `'${a.blueprint}','${a.inventory||''}',${JSON.stringify(sets).replace(/"/g,'&quot;')}`
        + ',' + JSON.stringify(a.limit||[]).replace(/"/g,'&quot;');
      const q = JSON.stringify(a.path).replace(/"/g,'&quot;');
      const bad = a.ok ? '' : '<span class="bad">✗ '
        + esc(a.diagnostics.map(x=>x.message).join(';')) + '</span>';
      const iv = a.verify_interval ? '巡检 '+Math.round(a.verify_interval/60)+'m' : '只手动';
      const lim = (a.limit||[]).length ? ' · 限定 '+esc(a.limit.join(', ')) : '';
      return `<div class="tk"><span class="nm">${esc(a.name)}</span>
        <span class="meta">${esc(a.blueprint)} × ${esc(a.inventory||'本机')} · ${iv}${lim}
          ${sets.length?' · '+esc(sets.join(' ')):''}</span>${bad}
        <span class="sp">
          <button class="btn" onclick="tkRun('verify',${arg})">Verify</button>
          <button class="btn" onclick="tkRun('plan',${arg})">Plan</button>
          <button class="btn" onclick="tkRun('apply',${arg})">Apply</button>
          <button class="btn" onclick="tkEdit(${q})">编辑</button>
          <button class="btn" onclick="tkDel(${q})">删除</button>
        </span></div>`;
    }).join('');
  }
  window.tkRun = async function(verb, bp, inv, sets, limit){
    const scope = (limit||[]).length ? ('\n限定范围:'+limit.join(', ')) : '\n范围:整份机群';
    if (verb === 'apply' && !confirm('确认收敛?将对目标机做出变更。'+scope)) return;
    const d = await (await fetch('/api/run',{method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({verb, blueprint:bp, inventory:inv, sets, limit:limit||[]})})).json();
    if (d.ok) htmx.ajax('GET','/view/job/'+d.job,'#view');
    else say(d.error||'启动失败', true);   // 409 = plan 闸门:先 Plan 再 Apply
  };
  window.tkAdd = null;
  window.taskAdd = async function(){
    const g = id => document.getElementById(id).value.trim();
    const params = g('t-params').split(',').map(s=>s.trim()).filter(Boolean)
      .map(s=>{const i=s.indexOf('='); return [s.slice(0,i), s.slice(i+1)];});
    const d = await (await fetch('/api/app/create',{method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({name:g('t-name'), blueprint:g('t-bp'), inventory:g('t-inv'),
        params, verify_interval:g('t-iv'),
        limit:[...document.querySelectorAll('#t-limit input:checked')].map(x=>x.value)})})).json();
    if (d.error){ say(d.error, true); return; }
    // 建完立刻跨文件校验:参数拼错、机群台数不够,现在就说,别等 plan。
    const lr = await (await fetch('/api/lint-project',{method:'POST',body:d.path})).json();
    say(lr.ok ? ('已创建 '+d.path+'(校验通过)')
              : ('已创建 '+d.path+',但有问题:'+lr.diagnostics.map(x=>x.message).join(';')), !lr.ok);
    for (const i of ['t-name','t-params','t-iv']) document.getElementById(i).value='';
    load();
  };
  window.tkEdit = function(path){
    localStorage.setItem('crater.edit', path);
    htmx.ajax('GET','/view/edit','#view');
  };
  window.tkDel = async function(path){
    if (!confirm('删掉任务 '+path+'?(进 .crater-trash,可拖回来;蓝图与机群不动)')) return;
    const d = await (await fetch('/api/file/trash?path='+encodeURIComponent(path),{method:'POST'})).json();
    if (d.error){ say(d.error, true); return; }
    say('已移入 .crater-trash'); load();
  };
  fillPickers().then(load);
})();
</script>
"##;
