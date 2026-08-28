//! `crater lint` —— **零连接**静态检查(D-107)。
//!
//! 这是新 DSL 的第一个用户可见兑现:整个仓库扫一遍,毫秒级,不碰任何目标机。
//! Ansible 里"连上机器、跑到那一行才炸"的一整类错误(模块名/参数名/参数引用拼错、
//! 作用域外变量、未声明物料、fact 无人导出…)在这里就地报出,并给拼写建议。

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};
use crater_ir::{lint, parse, Blueprint, Diagnostic, Severity};

/// 一个文件的检查结果。
struct FileReport {
    path: PathBuf,
    /// 解析失败时只有这一条;解析成功则是 lint 诊断(可能为空)。
    outcome: Outcome,
}

enum Outcome {
    // Box:成功分支比其它分支大一个数量级,而多数文件走的是跳过分支。
    Parsed(Box<Parsed>),
    ParseFailed(String),
    /// 旧版 task 格式 —— 不是错,是还没迁移。
    LegacyTask,
    /// 压根不是 blueprint(inventory / CI 配置 / k8s manifest…)。
    /// 只在**目录扫描**时静默跳过;命令行点名的文件一律照常解析并报错。
    NotABlueprint,
}

struct Parsed {
    bp: Blueprint,
    diags: Vec<Diagnostic>,
}

/// 各顶层节的内容行数(非空非注释)。
///
/// 刻意是**信息而非判决**:行数不度量复杂度 —— 48 行的物料声明是纯数据,
/// 读起来零成本;48 行的嵌套条件不是。按行数一刀切,等于对"数据多"和"逻辑绕"
/// 给同样的判断,而在健康文件上叫唤的规则会让整个 lint 输出失去可信度。
/// 所以这里只报数,并在可外置时**提一句**有这个选项,不计入 error/warn。
fn section_sizes(text: &str) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // 顶层键(零缩进且以冒号结尾或后跟值)开启新的一节
        if !line.starts_with(char::is_whitespace) && !line.starts_with('-') {
            if let Some((key, _)) = trimmed.split_once(':') {
                current = Some(key.to_string());
                out.push((key.to_string(), 0));
                continue;
            }
        }
        if let Some(cur) = &current {
            if let Some(entry) = out.iter_mut().rev().find(|(k, _)| k == cur) {
                entry.1 += 1;
            }
        }
    }
    out.retain(|(_, n)| *n > 0);
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

fn print_stats(path: &Path, text: &str) {
    let sizes = section_sizes(text);
    let total: usize = text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.trim().starts_with('#'))
        .count();
    println!("{}: {total} 内容行", relative(path).display());
    for (name, n) in &sizes {
        // 只对**可外置**的节提一句 —— 这是选项,不是批评。
        let hint = if crater_ir::parse::SPLITTABLE.contains(&name.as_str()) && *n >= 60 {
            format!("   (可 crater fmt --split {name} 外置)")
        } else {
            String::new()
        };
        println!("  {name:<14} {n:>4}{hint}");
    }
}

pub fn run(paths: &[PathBuf], strict: bool, json: bool, stats: bool) -> Result<()> {
    let files = collect(paths)?;
    if files.is_empty() {
        bail!("没有找到 .yaml/.yml 文件:{}", render_paths(paths));
    }

    let reports: Vec<FileReport> = files
        .into_iter()
        .map(|(path, explicit)| check_one(path, explicit))
        .collect();

    if json {
        print_json(&reports);
    } else {
        print_human(&reports);
        if stats {
            println!();
            for r in &reports {
                if matches!(r.outcome, Outcome::Parsed(_)) {
                    if let Ok(text) = std::fs::read_to_string(&r.path) {
                        print_stats(&r.path, &text);
                    }
                }
            }
        }
    }

    let (errors, warns, legacy, skipped) = tally(&reports);
    if !json {
        summarize(reports.len() - skipped, errors, warns, legacy, strict);
    }
    if errors > 0 || (strict && warns > 0) {
        std::process::exit(1);
    }
    Ok(())
}

/// `explicit` = 用户在命令行点名的文件(而非目录扫描发现的)。点名的一定要给答复,
/// 扫描发现的则宽容:一个仓库里绝大多数 YAML 本来就不是 blueprint。
fn check_one(path: PathBuf, explicit: bool) -> FileReport {
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            return FileReport { path, outcome: Outcome::ParseFailed(format!("读文件失败:{e}")) }
        }
    };
    // 旧格式先认出来,免得把"还没迁移"报成"写错了"。
    if is_legacy_task(&text) {
        return FileReport { path, outcome: Outcome::LegacyTask };
    }
    if !explicit && !looks_like_blueprint(&text) {
        return FileReport { path, outcome: Outcome::NotABlueprint };
    }
    match parse::blueprint_from_path(&path) {
        Ok(bp) => {
            let diags = lint::lint(&bp);
            FileReport { path, outcome: Outcome::Parsed(Box::new(Parsed { bp, diags })) }
        }
        Err(e) => FileReport { path, outcome: Outcome::ParseFailed(e.to_string()) },
    }
}

/// blueprint 的特征段:有其中任何一个就当它想成为 blueprint(于是错了要报)。
fn looks_like_blueprint(text: &str) -> bool {
    let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
        return false;
    };
    let Some(m) = v.as_mapping() else { return false };
    ["resources", "procedures", "types", "materials", "preflight", "health"]
        .iter()
        .any(|k| m.contains_key(serde_yaml::Value::from(*k)))
}

/// 旧版 task:顶层 `actions:`(新 DSL 用 `resources:`)。
fn is_legacy_task(text: &str) -> bool {
    let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
        return false;
    };
    let Some(m) = v.as_mapping() else { return false };
    m.contains_key(serde_yaml::Value::from("actions"))
        || m.contains_key(serde_yaml::Value::from("plays"))
}

// ---------------------------------------------------------------- 输出

fn print_human(reports: &[FileReport]) {
    for r in reports {
        // 相对于 cwd 显示 —— 绝对路径会淹没诊断本身。
        let file = relative(&r.path);
        let file = file.display();
        match &r.outcome {
            Outcome::LegacyTask => {
                println!("{file}: 旧版 task 格式,跳过(新 DSL 见 docs/research/ir-draft.md)");
            }
            Outcome::NotABlueprint => {}
            Outcome::ParseFailed(msg) => {
                println!("{file}: 解析失败");
                for line in msg.lines() {
                    println!("  {line}");
                }
            }
            Outcome::Parsed(p) if p.diags.is_empty() => {
                println!("{file}: ✓ {} ({})", p.bp.name, describe(&p.bp));
            }
            Outcome::Parsed(p) => {
                println!("{file}: {} ({})", p.bp.name, describe(&p.bp));
                for d in &p.diags {
                    // 一行一条,`file:line` 开头 —— 终端可点击直达。
                    let loc = match d.line {
                        Some(l) => format!("{file}:{l}"),
                        None => file.to_string(),
                    };
                    println!("  {loc}  {}  {}  ({})", tag(d), d.msg, d.at);
                }
            }
        }
    }
}

fn tag(d: &Diagnostic) -> &'static str {
    match d.severity {
        Severity::Error => "error",
        Severity::Warn => "warn ",
    }
}

fn describe(bp: &Blueprint) -> String {
    let mut parts = vec![format!("{} 资源", bp.resources.len())];
    if !bp.materials.is_empty() {
        parts.push(format!("{} 物料", bp.materials.len()));
    }
    if !bp.procedures.is_empty() {
        parts.push(format!("{} procedure", bp.procedures.len()));
    }
    if !bp.types.is_empty() {
        parts.push(format!("{} 自定义类型", bp.types.len()));
    }
    parts.join(", ")
}

fn print_json(reports: &[FileReport]) {
    // 手写 JSON:CI 与编辑器集成够用,不为此拉一个 serde_json 依赖。
    println!("{{\"files\":[");
    for (i, r) in reports.iter().enumerate() {
        let comma = if i + 1 < reports.len() { "," } else { "" };
        let file = esc(&relative(&r.path).display().to_string());
        match &r.outcome {
            Outcome::LegacyTask => {
                println!("  {{\"file\":\"{file}\",\"status\":\"legacy\"}}{comma}")
            }
            Outcome::NotABlueprint => {
                println!("  {{\"file\":\"{file}\",\"status\":\"skipped\"}}{comma}")
            }
            Outcome::ParseFailed(msg) => println!(
                "  {{\"file\":\"{file}\",\"status\":\"parse_error\",\"message\":\"{}\"}}{comma}",
                esc(msg)
            ),
            Outcome::Parsed(p) => {
                let items: Vec<String> = p
                    .diags
                    .iter()
                    .map(|d| {
                        format!(
                            "{{\"severity\":\"{}\",\"at\":\"{}\",\"line\":{},\"message\":\"{}\"}}",
                            if d.is_error() { "error" } else { "warning" },
                            esc(&d.at),
                            d.line.map(|l| l.to_string()).unwrap_or("null".into()),
                            esc(&d.msg)
                        )
                    })
                    .collect();
                println!(
                    "  {{\"file\":\"{file}\",\"status\":\"ok\",\"blueprint\":\"{}\",\"diagnostics\":[{}]}}{comma}",
                    esc(&p.bp.name),
                    items.join(",")
                );
            }
        }
    }
    println!("]}}");
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// (error 数, warn 数, 旧版 task 数, 非 blueprint 跳过数)
fn tally(reports: &[FileReport]) -> (usize, usize, usize, usize) {
    let mut errors = 0;
    let mut warns = 0;
    let mut legacy = 0;
    let mut skipped = 0;
    for r in reports {
        match &r.outcome {
            Outcome::NotABlueprint => skipped += 1,
            Outcome::LegacyTask => legacy += 1,
            Outcome::ParseFailed(_) => errors += 1,
            Outcome::Parsed(p) => {
                errors += p.diags.iter().filter(|d| d.is_error()).count();
                warns += p.diags.iter().filter(|d| !d.is_error()).count();
            }
        }
    }
    (errors, warns, legacy, skipped)
}

fn summarize(files: usize, errors: usize, warns: usize, legacy: usize, strict: bool) {
    let mut line = format!("检查 {files} 个文件:{errors} error, {warns} warn");
    if legacy > 0 {
        line.push_str(&format!(", {legacy} 个旧版 task 跳过"));
    }
    println!("{line}");
    if errors == 0 && warns > 0 && !strict {
        println!("(warn 不阻断部署;CI 里想让它阻断加 --strict)");
    }
}

// ---------------------------------------------------------------- 文件收集

/// → (路径, 是否命令行点名)
fn collect(paths: &[PathBuf]) -> Result<Vec<(PathBuf, bool)>> {
    let mut out = Vec::new();
    for p in paths {
        if p.is_dir() {
            let mut found = Vec::new();
            walk(p, &mut found)?;
            out.extend(found.into_iter().map(|f| (f, false)));
        } else if p.exists() {
            out.push((p.clone(), true));
        } else {
            bail!("路径不存在:{}", p.display());
        }
    }
    out.sort();
    // 同一文件既被点名又被扫到时,保留"点名"的那条(排序后 false 在前,dedup 保前者)。
    out.dedup_by(|a, b| a.0 == b.0);
    Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        // 隐藏目录与 target/ 不扫 —— 否则一个 cargo 项目要等半天。
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            walk(&path, out)?;
        } else if matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yaml") | Some("yml")
        ) {
            out.push(path);
        }
    }
    Ok(())
}

/// 能相对化就相对化(仅用于显示)。
fn relative(p: &Path) -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| p.strip_prefix(cwd).ok().map(Path::to_path_buf))
        .unwrap_or_else(|| p.to_path_buf())
}

fn render_paths(paths: &[PathBuf]) -> String {
    paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
}
