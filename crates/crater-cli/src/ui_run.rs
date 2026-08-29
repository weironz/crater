//! 作业系统(阶段①:执行打通)。
//!
//! 取代 ui.rs 里那个内存 BTreeMap:**job 的元数据与日志全部落盘**
//! (`~/.crater/jobs/<id>/meta.json` + `log.txt`),UI 进程重启后历史不丢,
//! 正在跑的 job 能被识别为 interrupted 而不是凭空消失。
//!
//! 刻意不用数据库表:job 目录本身就是索引,列几百个目录的成本可以忽略,
//! 而免掉的是 schema/migration/一致性三份心智负担。设计文档"最小盘"原则
//! 的更彻底版本 —— DB 只该存"关于执行的事实",而文件系统已经存得下。

use std::path::{Path, PathBuf};

use axum::{
    extract::{Path as AxPath, Query},
    http::StatusCode,
    response::{Html, IntoResponse, Json, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct JobMeta {
    pub id: String,
    /// 人读标题,如 "apply k8s-ha @ inventory.prod"。
    pub title: String,
    pub verb: String,
    pub blueprint: String,
    pub inventory: String,
    pub args: Vec<String>,
    pub started: u64,
    pub finished: Option<u64>,
    /// running / ok / failed / interrupted / canceled
    pub status: String,
    pub pid: Option<u32>,
}

fn jobs_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    Path::new(&home).join(".crater").join("jobs")
}

/// plan 闸门:plan 成功时把(蓝图 sha, inventory sha, 参数快照)钉进闸门文件;
/// apply 必须持有匹配的闸门,否则 409。
///
/// **参数也在快照里** —— 否则同一份文件、两次不同的 --set,后者可以骑着
/// 前者的 plan 直接过闸(评审抓出的洞)。
fn gate_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".crater").join("ui").join("plans")
}

fn gate_key(blueprint: &str, inventory: &str) -> String {
    format!("{blueprint}|{inventory}")
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect()
}

fn file_sha(rel: &str) -> Option<String> {
    std::fs::read(rel).ok().map(|b| crater_core::bundle::sha256_hex(&b))
}

fn params_snapshot(sets: &[String]) -> String {
    let mut v: Vec<&String> = sets.iter().collect();
    v.sort();
    v.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("&")
}

fn write_gate(blueprint: &str, inventory: &str, sets: &[String]) {
    let _ = std::fs::create_dir_all(gate_dir());
    let doc = serde_json::json!({
        "blueprint_sha": file_sha(blueprint),
        "inventory_sha": if inventory.is_empty() { None } else { file_sha(inventory) },
        "params": params_snapshot(sets),
        "ts": now(),
        // 指纹在提交时钉(plan 展示的就是此刻的文件),但闸门要等 plan **成功**
        // 才生效 —— 失败的 plan 什么都没展示,不配放行 apply。
        "pending": true,
    });
    let _ = std::fs::write(
        gate_dir().join(format!("{}.json", gate_key(blueprint, inventory))),
        serde_json::to_string(&doc).unwrap_or_default(),
    );
}

/// 校验闸门。Ok(()) = 放行;Err(原因) = 409。
fn check_gate(blueprint: &str, inventory: &str, sets: &[String]) -> Result<(), String> {
    let path = gate_dir().join(format!("{}.json", gate_key(blueprint, inventory)));
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Err("还没有对应的 Plan —— 先 Plan 看清将发生什么,再 Apply".into());
    };
    let Ok(g) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Err("Plan 记录损坏,请重新 Plan".into());
    };
    if g["pending"].as_bool().unwrap_or(false) {
        return Err("上一次 Plan 未成功完成 —— 等它跑完(或重新 Plan)".into());
    }
    if g["blueprint_sha"].as_str() != file_sha(blueprint).as_deref() {
        return Err("蓝图在 Plan 之后被改过 —— 期望态已变,请重新 Plan".into());
    }
    if !inventory.is_empty() && g["inventory_sha"].as_str() != file_sha(inventory).as_deref() {
        return Err("inventory 在 Plan 之后被改过 —— 请重新 Plan".into());
    }
    if g["params"].as_str().unwrap_or("") != params_snapshot(sets) {
        return Err("参数与 Plan 时不同 —— 换了 --set 就要重新 Plan".into());
    }
    Ok(())
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn meta_path(id: &str) -> PathBuf {
    jobs_root().join(id).join("meta.json")
}
fn log_path(id: &str) -> PathBuf {
    jobs_root().join(id).join("log.txt")
}

fn read_meta(id: &str) -> Option<JobMeta> {
    let text = std::fs::read_to_string(meta_path(id)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_meta(m: &JobMeta) {
    if let Ok(t) = serde_json::to_string_pretty(m) {
        let p = meta_path(&m.id);
        let _ = std::fs::create_dir_all(p.parent().unwrap());
        // 先写临时再改名:meta 是状态判定的唯一依据,半截 JSON 会让这条 job
        // 从此在列表里渲染失败。
        let tmp = p.with_extension("json.tmp");
        if std::fs::write(&tmp, t).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}

/// 进程启动时清账:标着 running 但 pid 已死的,判为 interrupted。
///
/// 这是"UI 从看板升格为常驻服务"必须回答的第一道题 —— 不清账的话,
/// 一次重启就会留下永远转圈的僵尸行。
pub fn sweep_on_start() {
    for m in list() {
        if m.status != "running" {
            continue;
        }
        let alive = m
            .pid
            .map(|p| Path::new(&format!("/proc/{p}")).exists())
            .unwrap_or(false);
        if alive {
            // 子进程直写日志、自记退出码,所以它**还活着是合法状态** ——
            // UI 重启不该杀它(那是一次正在进行的部署)。挂个看护,
            // 等它退了按 exit_code 补记结论。
            let id = m.id.clone();
            let pid = m.pid.unwrap();
            tokio::spawn(async move {
                while Path::new(&format!("/proc/{pid}")).exists() {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                }
                finalize(&id);
            });
        } else {
            finalize(&m.id);
        }
    }
}

pub fn list() -> Vec<JobMeta> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(jobs_root()) {
        for e in rd.flatten() {
            if let Some(m) = read_meta(&e.file_name().to_string_lossy()) {
                out.push(m);
            }
        }
    }
    // id 以时间开头,倒序即最新在前。
    out.sort_by(|a, b| b.id.cmp(&a.id));
    out
}

/// 起一个 job:spawn 本体 CLI,行式追加日志,退出时改写 meta。
pub fn spawn(title: String, verb: String, blueprint: String, inventory: String, args: Vec<String>) -> String {
    // 时间戳 + 计数后缀:同一秒多次提交也不撞。
    let base = format!("{}", now());
    let mut id = base.clone();
    let mut n = 1;
    while meta_path(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let mut meta = JobMeta {
        id: id.clone(),
        title,
        verb,
        blueprint,
        inventory,
        args: args.clone(),
        started: now(),
        status: "running".into(),
        ..Default::default()
    };
    write_meta(&meta);

    let jid = id.clone();
    tokio::spawn(async move {
        let fail = |meta: &mut JobMeta, msg: String| {
            meta.status = "failed".into();
            meta.finished = Some(now());
            let _ = std::fs::write(log_path(&meta.id), msg);
            write_meta(meta);
        };
        let exe = match std::env::current_exe() {
            Ok(e) => e,
            Err(e) => return fail(&mut meta, format!("spawn failed: {e}")),
        };
        // 开发期热换二进制后,/proc/self/exe 会带 " (deleted)" 后缀 ——
        // 磁盘上的新文件就在原路径,剥掉后缀即可;生产环境这行恒等。
        let exe = PathBuf::from(
            exe.display().to_string().trim_end_matches(" (deleted)").to_string(),
        );
        // **子进程直写日志文件,不经 UI 管道中转。**
        //
        // 管道中转的隐患对部署工具是不可接受的:UI 进程一死,管道没人读,
        // 内核缓冲写满后子进程会**卡死在半路的 apply 上** —— 仪表盘的死活
        // 绝不能影响部署本身。直写文件后,UI 死掉子进程照跑照写,
        // 重启后的清账(sweep)靠 exit_code 文件补记结论。
        //
        // 外面包一层 sh:把退出码写进 exit_code 文件 —— 等它的人可能已经
        // 不在了(UI 重启),退出码必须自己落地。
        let dir = jobs_root().join(&jid);
        // __JOBDIR__ 占位:spawn 前才知道 job 目录。
        let args: Vec<String> = args
            .iter()
            .map(|a| a.replace("__JOBDIR__", &dir.display().to_string()))
            .collect();
        let shline = format!(
            "exec >>{log} 2>&1; {exe} {args}; c=$?; echo $c > {dir}/exit_code; exit $c",
            log = shq(&log_path(&jid).display().to_string()),
            exe = shq(&exe.display().to_string()),
            args = args.iter().map(|a| shq(a)).collect::<Vec<_>>().join(" "),
            dir = shq(&dir.display().to_string()),
        );
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&shline).env("CRATER_LOG", "info");
        // 新会话(进程组长):取消时对整组发信号,sh 与真正的 CLI 一起收到。
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return fail(&mut meta, format!("spawn failed: {e}")),
        };
        meta.pid = child.id();
        write_meta(&meta);
        let _ = child.wait().await;
        finalize(&jid);
    });
    id
}

/// 按 exit_code 文件补记结论。等它的人可能已经换了一茬(UI 重启),
/// 所以结论必须从磁盘读,而不是从内存里的 wait() 返回值。
/// plan 成功 → 闸门生效(pending → false)。
fn promote_gate(m: &JobMeta) {
    let path = gate_dir().join(format!("{}.json", gate_key(&m.blueprint, &m.inventory)));
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(mut g) = serde_json::from_str::<serde_json::Value>(&text) {
            g["pending"] = serde_json::json!(false);
            let _ = std::fs::write(&path, serde_json::to_string(&g).unwrap_or_default());
        }
    }
}

fn finalize(id: &str) {
    // verify 报告 → 对账快照。放在结论判定之前:exit=1(有漂移)时报告同样有效 ——
    // "发现漂移"正是快照最有价值的时刻。
    let vr = jobs_root().join(id).join("verify.json");
    if vr.exists() {
        crate::ui_overview::stash_verify_report(&vr);
    }
    let Some(mut m) = read_meta(id) else { return };
    if m.status != "running" {
        return; // cancel() 已下结论,不覆盖
    }
    let code = std::fs::read_to_string(jobs_root().join(id).join("exit_code"))
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok());
    m.status = match code {
        Some(0) => "ok".into(),
        Some(_) => "failed".into(),
        // 没有 exit_code 文件 = 进程没走到收尾(被 kill / 断电)。
        None => "interrupted".into(),
    };
    m.finished = Some(now());
    if m.verb == "plan" && m.status == "ok" {
        promote_gate(&m);
    }
    write_meta(&m);
}

/// shell 单引号转义(与引擎侧 sh() 同法)。
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

// ---------------------------------------------------------------- HTTP 面

/// `POST /api/run` —— 参数化执行:动词 × 蓝图 × inventory × 参数。
///
/// 这一步消灭了旧 UI 写死的 `inventory.yaml` 约定:AWX 式"平台配置好唯一
/// inventory"正是从零编排做不了的一环。路径全部过工作目录禁闭。
#[derive(Deserialize)]
pub struct RunReq {
    pub verb: String,
    pub blueprint: String,
    #[serde(default)]
    pub inventory: String,
    #[serde(default)]
    pub closure: String,
    #[serde(default)]
    pub sets: Vec<String>,
}

pub async fn run(Json(req): Json<RunReq>) -> Response {
    // 动词白名单:HTTP 面能起什么进程,必须是穷举的。
    const VERBS: &[&str] = &["plan", "apply", "verify", "destroy", "lint", "build", "procedure"];
    if !VERBS.contains(&req.verb.as_str()) {
        return err(StatusCode::BAD_REQUEST, format!("不支持的动词 `{}`(可用:{})", req.verb, VERBS.join("/")));
    }
    // 路径禁闭复用编辑器那套:canonicalize 后必须落在工作目录内。
    let bp = match crate::ui_edit::confine(&req.blueprint) {
        Ok(p) => p,
        Err((c, m)) => return err(c, m),
    };
    if !bp.is_file() {
        return err(StatusCode::BAD_REQUEST, format!("蓝图不存在:{}", req.blueprint));
    }
    let mut args: Vec<String> = vec![req.verb.clone(), bp.display().to_string()];
    if !req.inventory.is_empty() {
        let inv = match crate::ui_edit::confine(&req.inventory) {
            Ok(p) => p,
            Err((c, m)) => return err(c, m),
        };
        args.push("-i".into());
        args.push(inv.display().to_string());
    }
    if !req.closure.is_empty() {
        let cl = match crate::ui_edit::confine(&req.closure) {
            Ok(p) => p,
            Err((c, m)) => return err(c, m),
        };
        args.push("--closure".into());
        args.push(cl.display().to_string());
    }
    for kv in &req.sets {
        if !kv.contains('=') {
            return err(StatusCode::BAD_REQUEST, format!("--set 要写成 k=v:{kv}"));
        }
        args.push("--set".into());
        args.push(kv.clone());
    }
    // plan-gated apply:把"先看 diff 再动手"做成结构而非美德。
    if req.verb == "apply" {
        if let Err(why) = check_gate(&req.blueprint, &req.inventory, &req.sets) {
            return err(StatusCode::CONFLICT, why);
        }
    }
    if req.verb == "plan" {
        // 闸门在**提交时**钉指纹(不是 job 结束时):plan 展示的就是此刻文件的
        // 内容,期间文件再改,apply 时对不上 —— 这正是想要的。
        write_gate(&req.blueprint, &req.inventory, &req.sets);
    }
    // verify 自动带 --json:对账看板的供血管道 —— 报告落在 job 目录,
    // finalize 时拆成每条部署记录一份快照。
    if req.verb == "verify" {
        args.push("--json".into());
        args.push("__JOBDIR__/verify.json".into());
    }
    // destroy 的 --yes 由服务端追加 —— 客户端只表达意图,阶梯在这里成为结构:
    // 没有经过这个端点的确认语义,就没有 --yes。
    if req.verb == "destroy" {
        args.push("--yes".into());
    }
    // build 的蓝图参数形状不同(-f)。
    if req.verb == "build" {
        args = vec!["build".into(), "-f".into(), bp.display().to_string()];
        args.push("-o".into());
        args.push(format!("{}.closure.tar", bp.file_stem().unwrap_or_default().to_string_lossy()));
    }
    let title = format!(
        "{} {}{}",
        req.verb,
        Path::new(&req.blueprint).file_name().unwrap_or_default().to_string_lossy(),
        if req.inventory.is_empty() { String::new() } else { format!(" @ {}", req.inventory) }
    );
    let id = spawn(title, req.verb, req.blueprint, req.inventory, args);
    Json(json!({ "ok": true, "job": id })).into_response()
}

fn err(code: StatusCode, msg: String) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

/// `POST /api/job2/{id}/cancel`
pub async fn cancel(AxPath(id): AxPath<String>) -> Response {
    let Some(mut m) = read_meta(&id) else {
        return err(StatusCode::NOT_FOUND, "没有这个 job".into());
    };
    if m.status != "running" {
        return err(StatusCode::CONFLICT, format!("job 已是 {}", m.status));
    }
    if let Some(pid) = m.pid {
        // 负 pid = 对整个进程组:外层 sh 与真正的 CLI 一起收到。
        // SIGTERM 而非 KILL —— 部署工具被 KILL 留下的半成品比多等几秒贵得多。
        unsafe { libc::kill(-(pid as i32), libc::SIGTERM) };
    }
    m.status = "canceled".into();
    m.finished = Some(now());
    write_meta(&m);
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
pub struct TailQuery {
    #[serde(default)]
    pub after: u64,
}

/// `GET /api/job2/{id}?after=<字节游标>` —— 增量取日志。
///
/// job 结束且客户端已读到末尾时回 **286**(htmx 的停轮惯例):
/// 轮询自然终止,不需要客户端算状态。
pub async fn tail(AxPath(id): AxPath<String>, Query(q): Query<TailQuery>) -> Response {
    let Some(m) = read_meta(&id) else {
        return err(StatusCode::NOT_FOUND, "没有这个 job".into());
    };
    let data = std::fs::read(log_path(&id)).unwrap_or_default();
    let chunk = data.get(q.after as usize..).unwrap_or_default();
    let text = String::from_utf8_lossy(chunk);
    let next = data.len() as u64;
    let done = m.status != "running";
    let code = if done && chunk.is_empty() { 286 } else { 200 };
    (
        StatusCode::from_u16(code).unwrap(),
        [("X-Log-Cursor", next.to_string()), ("X-Job-Status", m.status.clone())],
        text.into_owned(),
    )
        .into_response()
}

/// `GET /api/jobs` —— 历史列表片段(htmx)。
pub async fn jobs_fragment() -> Html<String> {
    let rows: String = list()
        .into_iter()
        .take(100)
        .map(|m| {
            let dur = m
                .finished
                .map(|f| format!("{}s", f.saturating_sub(m.started)))
                .unwrap_or_else(|| "…".into());
            let chip = match m.status.as_str() {
                "ok" => "chip ok",
                "running" => "chip run",
                "canceled" | "interrupted" => "chip warn",
                _ => "chip fail",
            };
            format!(
                "<tr onclick=\"htmx.ajax('GET','/view/job/{id}','#view')\"><td><span class='{chip}'>{st}</span></td>\
                 <td>{title}</td><td>{when}</td><td>{dur}</td></tr>",
                id = m.id,
                st = m.status,
                title = esc(&m.title),
                when = fmt_ts(m.started),
            )
        })
        .collect();
    Html(format!(
        "<table class='jobs'><thead><tr><th>状态</th><th>作业</th><th>开始</th><th>耗时</th></tr></thead>\
         <tbody>{rows}</tbody></table>"
    ))
}

fn fmt_ts(secs: u64) -> String {
    // 不引时区库:UI 与目标机同在运维语境,本机 date 语义足够。
    let out = std::process::Command::new("date")
        .args(["-d", &format!("@{secs}"), "+%m-%d %H:%M:%S"])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => secs.to_string(),
    }
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// `GET /view/jobs`
pub async fn view_jobs() -> Html<&'static str> {
    Html(
        r##"<section class="panel"><h2><span class="mk">≡</span> 作业历史</h2>
  <div id="jobs" hx-get="/api/jobs" hx-trigger="load, every 5s" hx-swap="innerHTML"></div>
</section>
<style>
  .jobs{width:100%;border-collapse:collapse;font-size:13px}
  .jobs th{text-align:left;color:var(--muted);font-weight:600;padding:6px 8px;border-bottom:1px solid var(--border)}
  .jobs td{padding:7px 8px;border-bottom:1px solid var(--border)}
  .jobs tbody tr{cursor:pointer}
  .jobs tbody tr:hover{background:var(--surface-2)}
  .chip{padding:2px 8px;border-radius:99px;font-size:12px}
  .chip.ok{background:var(--ok-bg);color:var(--ok)} .chip.fail{background:var(--drift-bg);color:var(--drift)}
  .chip.run{background:var(--tint);color:var(--accent)} .chip.warn{background:var(--unknown-bg);color:var(--unknown)}
</style>"##,
    )
}

/// `GET /view/job/{id}` —— 单个 job:标题 + 日志流(游标轮询)。
pub async fn view_job(AxPath(id): AxPath<String>) -> Response {
    let Some(m) = read_meta(&id) else {
        return (StatusCode::NOT_FOUND, Html("<p>没有这个 job</p>")).into_response();
    };
    Html(format!(
        r##"<section class="panel"><h2><span class="mk">▸</span> {title}</h2>
  <div class="job-bar">
    <span id="job-status" class="chip run">{status}</span>
    <button class="btn" onclick="cancelJob()">取消</button>
    <a class="btn" onclick="htmx.ajax('GET','/view/jobs','#view')">← 全部作业</a>
  </div>
  <pre id="job-log" class="job-log"></pre>
</section>
<style>
  .job-bar{{display:flex;gap:8px;align-items:center;margin-bottom:10px}}
  .job-log{{background:var(--surface-2);border:1px solid var(--border);border-radius:10px;
    padding:12px;font:12px/1.5 ui-monospace,monospace;max-height:64vh;overflow:auto;white-space:pre-wrap}}
  .chip{{padding:2px 8px;border-radius:99px;font-size:12px}}
  .chip.ok{{background:var(--ok-bg);color:var(--ok)}} .chip.fail{{background:var(--drift-bg);color:var(--drift)}}
  .chip.run{{background:var(--tint);color:var(--accent)}} .chip.warn{{background:var(--unknown-bg);color:var(--unknown)}}
</style>
<script>
(function(){{
  const log = document.getElementById('job-log');
  const st = document.getElementById('job-status');
  let cursor = 0, stop = false;
  window.cancelJob = async () => {{ await fetch('/api/job2/{id}/cancel', {{method:'POST'}}); }};
  async function poll(){{
    if (stop) return;
    const r = await fetch('/api/job2/{id}?after='+cursor);
    cursor = parseInt(r.headers.get('X-Log-Cursor')||cursor);
    const s = r.headers.get('X-Job-Status')||'?';
    st.textContent = s;
    st.className = 'chip ' + (s==='ok'?'ok':s==='running'?'run':(s==='canceled'||s==='interrupted')?'warn':'fail');
    const t = await r.text();
    if (t) {{ log.textContent += t; log.scrollTop = log.scrollHeight; }}
    if (r.status === 286) {{ stop = true; return; }}
    setTimeout(poll, 1000);
  }}
  poll();
}})();
</script>"##,
        title = esc(&m.title),
        status = m.status,
        id = id,
    ))
    .into_response()
}

/// `GET /view/run` —— 参数化启动表单:动词 × 蓝图 × inventory × 闭包 × --set。
pub async fn view_run() -> Html<&'static str> {
    Html(
        r##"<section class="panel"><h2><span class="mk">▶</span> 运行</h2>
  <div class="run-form">
    <label>动词
      <select id="r-verb">
        <option value="plan">plan(零写入预演)</option>
        <option value="verify">verify(漂移核对,只读)</option>
        <option value="apply">apply(收敛)</option>
        <option value="destroy">destroy(退役 —— 服务端追加 --yes)</option>
        <option value="build">build(烤闭包)</option>
        <option value="lint">lint</option>
      </select></label>
    <label>蓝图 / 栈 <select id="r-bp"></select></label>
    <label>inventory <select id="r-inv"><option value="">(不指定 —— 本机)</option></select></label>
    <label>闭包(可选)<input id="r-closure" placeholder="path/to/x.closure.tar"></label>
    <label>--set(每行一个 k=v)<textarea id="r-sets" rows="3"></textarea></label>
    <button class="btn primary" onclick="launch()">启动</button>
    <span id="r-msg" class="ed-status"></span>
  </div>
</section>
<style>
  .run-form{display:flex;flex-direction:column;gap:10px;max-width:560px}
  .run-form label{display:flex;flex-direction:column;gap:4px;font-size:13px;color:var(--muted)}
  .run-form select,.run-form input,.run-form textarea{background:var(--surface-2);color:var(--text);
    border:1px solid var(--border);border-radius:8px;padding:7px 10px;font:inherit}
  .btn.primary{background:var(--accent);color:#fff;border:0}
</style>
<script>
(function(){
  fetch('/api/files').then(r=>r.json()).then(d=>{
    const bp = document.getElementById('r-bp'), inv = document.getElementById('r-inv');
    for (const f of d.files){
      if (f.kind === 'blueprint' || f.kind === 'stack'){
        const o = document.createElement('option'); o.value=f.path; o.textContent=f.path+' · '+f.kind; bp.appendChild(o);
      } else if (f.kind === 'inventory'){
        const o = document.createElement('option'); o.value=f.path; o.textContent=f.path; inv.appendChild(o);
      }
    }
  });
  window.launch = async () => {
    const verb = document.getElementById('r-verb').value;
    if ((verb === 'apply' || verb === 'destroy') &&
        !confirm(verb === 'destroy' ? '确认退役?将拆除该蓝图声明的全部资源。' : '确认收敛?将对目标机做出变更。')) return;
    const sets = document.getElementById('r-sets').value.split('\n').map(s=>s.trim()).filter(Boolean);
    const r = await fetch('/api/run', {method:'POST', headers:{'Content-Type':'application/json'},
      body: JSON.stringify({verb, blueprint: document.getElementById('r-bp').value,
        inventory: document.getElementById('r-inv').value,
        closure: document.getElementById('r-closure').value.trim(), sets})});
    const d = await r.json();
    if (d.ok) htmx.ajax('GET', '/view/job/'+d.job, '#view');
    else document.getElementById('r-msg').textContent = d.error || '启动失败';
  };
})();
</script>"##,
    )
}
