//! 主机 / 主机组:inventory 文件的结构化入口。
//!
//! **读走解析,写走行级文本编辑**。这条分工是刻意的:结构化表单最容易犯的
//! 错是"读进来、改一个字段、整份写回去"—— 那一趟往返会把注释、缩进风格、
//! 键序全部抹平,而 inventory 里"这台为什么留空""这个口令待换"恰恰写在
//! 注释里。所以写入只做三种行级动作:**插一行、删一行、换一行**,其余字节
//! 一个不碰(与阶段⑤的 span 补丁同源)。
//!
//! 碰到块式条目 / 嵌套组这类一改就牵连别处的形态,一律 409 降级只读 ——
//! 表单不该假装看得懂它没把握的结构。

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use std::path::Path;

// ───────────────────────────── 文本定位 ─────────────────────────────

/// `inventory:` 下某个直接子键(`hosts:` / `groups:`)的块范围。
struct Block {
    /// 键那一行的下标。
    key_line: usize,
    /// 键自身的缩进(= inventory 的子缩进)。
    key_indent: usize,
    /// 块内条目的缩进(空块时按 key_indent + 2 预设)。
    item_indent: usize,
    /// 块内最后一个**非空**行的下标;空块为 None。
    last_content: Option<usize>,
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

fn is_blank(l: &str) -> bool {
    l.trim().is_empty() || l.trim_start().starts_with('#')
}

/// 找 `inventory:` 下的直接子块。找不到返回 None —— 文件形态不认识时
/// 宁可说"不认识"也不猜(坏输入降级,不报错)。
fn block_of(lines: &[&str], key: &str) -> Option<Block> {
    let iv = lines
        .iter()
        .position(|l| l.trim_start().starts_with("inventory:") && indent_of(l) == 0)?;
    // 第一个非空子行的缩进 = 直接子键的缩进。
    let child_indent = lines[iv + 1..]
        .iter()
        .find(|l| !is_blank(l))
        .map(|l| indent_of(l))?;
    if child_indent == 0 {
        return None; // inventory: 下面没有子键
    }
    let key_line = lines[iv + 1..].iter().position(|l| {
        !is_blank(l)
            && indent_of(l) == child_indent
            && l.trim_start().starts_with(&format!("{key}:"))
    })? + iv
        + 1;

    // 块的内容:直到下一个缩进 <= key_indent 的非空行为止。
    let mut last_content = None;
    let mut item_indent = None;
    for (i, l) in lines.iter().enumerate().skip(key_line + 1) {
        if is_blank(l) {
            continue;
        }
        if indent_of(l) <= child_indent {
            break;
        }
        if item_indent.is_none() {
            item_indent = Some(indent_of(l));
        }
        last_content = Some(i);
    }
    Some(Block {
        key_line,
        key_indent: child_indent,
        item_indent: item_indent.unwrap_or(child_indent + 2),
        last_content,
    })
}

/// 单行条目守卫:这一行底下**没有**更深缩进的续行才算单行(可整行替换/删除)。
/// 块式条目返回 false —— 调用方据此 409。
fn is_single_line(lines: &[&str], i: usize) -> bool {
    let ind = indent_of(lines[i]);
    lines[i + 1..]
        .iter()
        .find(|l| !is_blank(l))
        .map(|l| indent_of(l) <= ind)
        .unwrap_or(true)
}

/// YAML 标量渲染:歧义裸词与含特殊字符的一律加引号。
/// **口令永远加引号** —— `password: 123456` 会被解析成整数,反序列化直接失败。
fn scalar(v: &str) -> String {
    let bare_ok = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        && !v.starts_with('-')
        && v.parse::<f64>().is_err()
        && !matches!(
            v.to_ascii_lowercase().as_str(),
            "yes" | "no" | "on" | "off" | "true" | "false" | "null" | "~"
        );
    if bare_ok {
        v.to_string()
    } else {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    }
}

fn read_text(rel: &str) -> Result<String, (StatusCode, String)> {
    let path = crate::ui_edit::confine(rel)?;
    std::fs::read_to_string(&path)
        .map_err(|e| (StatusCode::NOT_FOUND, format!("读不到 {rel}:{e}")))
}

/// 写回:与编辑器保存同款,旧版留 `.bak`(误操作要能拖回来)。
fn write_text(rel: &str, text: &str) -> Result<(), (StatusCode, String)> {
    let path = crate::ui_edit::confine(rel)?;
    if let Ok(old) = std::fs::read(&path) {
        let _ = std::fs::write(path.with_extension("bak"), old);
    }
    std::fs::write(&path, text)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("写入失败:{e}")))
}

fn err(c: StatusCode, m: impl Into<String>) -> Response {
    (c, Json(json!({ "error": m.into() }))).into_response()
}

fn join(lines: &[String], had_trailing_nl: bool) -> String {
    let mut t = lines.join("\n");
    if had_trailing_nl {
        t.push('\n');
    }
    t
}

// ───────────────────────────── 读 ─────────────────────────────

#[derive(Deserialize)]
pub struct PathQ {
    pub path: String,
}

/// `GET /api/inventory/read?path=…` —— 结构化读:主机表 + 组表。
///
/// 读用解析器(此刻文件是完整的,解析器给的信息最全);写才回到行级文本。
pub async fn inv_read(Query(q): Query<PathQ>) -> Response {
    let text = match read_text(&q.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let spec: crater_core::spec::CraterSpec = match serde_yaml::from_str(&text) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::CONFLICT, format!("inventory 解析失败:{e}")),
    };
    let inv = spec.inventory;
    let lines: Vec<&str> = text.lines().collect();
    // 可编辑性由**文本形态**决定,不是由数据决定:块式条目只读。
    let editable_host = |name: &str| -> bool {
        find_host_line(&lines, name)
            .map(|i| is_single_line(&lines, i))
            .unwrap_or(false)
    };
    let hosts: Vec<serde_json::Value> = inv
        .hosts
        .iter()
        .map(|h| {
            let groups: Vec<&String> = inv
                .groups
                .iter()
                .filter(|(_, g)| g.hosts.contains(&h.name))
                .map(|(n, _)| n)
                .collect();
            json!({
                "name": h.name, "address": h.address, "user": h.user, "port": h.port,
                "auth": if h.key.is_some() { "key" } else if h.password.is_some() { "password" } else { "none" },
                "key": h.key.as_ref().map(|p| p.display().to_string()),
                "vars": h.vars,
                "groups": groups,
                "editable": editable_host(&h.name),
                "local": h.is_local(),
            })
        })
        .collect();
    let groups: Vec<serde_json::Value> = inv
        .groups
        .iter()
        .map(|(name, g)| {
            let line = find_group_line(&lines, name);
            // 嵌套组 / 块式组:能看不能改 —— 整行替换会把嵌套关系吃掉。
            let editable = line
                .map(|i| is_single_line(&lines, i) && !lines[i].contains("groups:"))
                .unwrap_or(false);
            json!({
                "name": name, "hosts": g.hosts, "groups": g.groups,
                "editable": editable,
            })
        })
        .collect();
    Json(json!({ "hosts": hosts, "groups": groups, "vars": inv.vars, "path": q.path }))
        .into_response()
}

/// 主机条目所在行:`- { name: n1, … }` 或 `- name: n1`。
fn find_host_line(lines: &[&str], name: &str) -> Option<usize> {
    let b = block_of(lines, "hosts")?;
    (b.key_line + 1..lines.len())
        .take_while(|&i| is_blank(lines[i]) || indent_of(lines[i]) > b.key_indent)
        .find(|&i| {
            let t = lines[i].trim_start();
            t.starts_with("- ")
                && (t.contains(&format!("name: {name},"))
                    || t.contains(&format!("name: {name} "))
                    || t.trim_end().ends_with(&format!("name: {name}"))
                    || t.contains(&format!("name: \"{name}\"")))
        })
}

/// 组条目所在行:`  <name>: { … }` 或 `  <name>:`(块式)。
fn find_group_line(lines: &[&str], name: &str) -> Option<usize> {
    let b = block_of(lines, "groups")?;
    (b.key_line + 1..lines.len())
        .take_while(|&i| is_blank(lines[i]) || indent_of(lines[i]) > b.key_indent)
        .find(|&i| {
            let t = lines[i].trim_start();
            t.starts_with(&format!("{name}:")) || t.starts_with(&format!("\"{name}\":"))
        })
}

// ───────────────────────────── 写:主机 ─────────────────────────────

#[derive(Deserialize)]
pub struct HostAdd {
    pub path: String,
    pub name: String,
    pub address: String,
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub key: String,
    /// 主机变量,`k=v` 列表。
    #[serde(default)]
    pub vars: Vec<(String, String)>,
}

/// `POST /api/inventory/host` —— 新增一台主机(在 `hosts:` 块末尾**插一行**)。
pub async fn host_add(Json(req): Json<HostAdd>) -> Response {
    if req.name.is_empty()
        || !req.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return err(StatusCode::BAD_REQUEST, "主机名只允许字母数字-_");
    }
    if req.address.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "地址不能为空");
    }
    let text = match read_text(&req.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let lines: Vec<&str> = text.lines().collect();
    if find_host_line(&lines, &req.name).is_some() {
        return err(StatusCode::CONFLICT, format!("主机 `{}` 已存在", req.name));
    }
    let Some(b) = block_of(&lines, "hosts") else {
        return err(
            StatusCode::CONFLICT,
            "找不到 `inventory:` 下的 `hosts:` 块 —— 请直接改文本",
        );
    };

    let mut fields = vec![
        format!("name: {}", scalar(&req.name)),
        format!("address: {}", scalar(req.address.trim())),
    ];
    if let Some(p) = req.port.filter(|p| *p != 22) {
        fields.push(format!("port: {p}"));
    }
    let user = if req.user.trim().is_empty() { "root" } else { req.user.trim() };
    fields.push(format!("user: {}", scalar(user)));
    if !req.key.trim().is_empty() {
        fields.push(format!("key: {}", scalar(req.key.trim())));
    } else if !req.password.is_empty() {
        // 口令恒加引号 —— 纯数字口令裸写会被解析成整数,反序列化直接失败。
        fields.push(format!("password: \"{}\"", req.password.replace('"', "\\\"")));
    }
    if !req.vars.is_empty() {
        let vs: Vec<String> = req
            .vars
            .iter()
            .map(|(k, v)| format!("{k}: {}", scalar(v)))
            .collect();
        fields.push(format!("vars: {{ {} }}", vs.join(", ")));
    }
    let entry = format!("{}- {{ {} }}", " ".repeat(b.item_indent), fields.join(", "));

    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    match b.last_content {
        Some(i) => out.insert(i + 1, entry),
        None => {
            // 空块:`hosts: []` 要先摘掉那对空括号,否则插进去的行没人读。
            let k = &out[b.key_line];
            if k.trim_end().ends_with("[]") {
                out[b.key_line] = k.replacen("[]", "", 1).trim_end().to_string();
            }
            out.insert(b.key_line + 1, entry);
        }
    }
    match write_text(&req.path, &join(&out, text.ends_with('\n'))) {
        Ok(()) => Json(json!({ "ok": true, "host": req.name })).into_response(),
        Err((c, m)) => err(c, m),
    }
}

#[derive(Deserialize)]
pub struct HostDel {
    pub path: String,
    pub name: String,
}

/// `DELETE /api/inventory/host` —— 删一台主机(删一行),并从各组成员里摘掉它。
///
/// 摘组这一步不能省:留下指向不存在主机的组,蓝图求计划时才会炸,
/// 而那时人已经不在这个页面上了。
pub async fn host_remove(Json(req): Json<HostDel>) -> Response {
    let text = match read_text(&req.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some(i) = find_host_line(&lines, &req.name) else {
        return err(StatusCode::NOT_FOUND, format!("找不到主机 `{}`", req.name));
    };
    if !is_single_line(&lines, i) {
        return err(StatusCode::CONFLICT, "这是块式条目(多行)—— 请直接改文本");
    }
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    out.remove(i);

    // 从各组的 flow 成员列表里摘掉;块式组留给人工(会在返回里说明)。
    let mut manual: Vec<String> = Vec::new();
    let refreshed: Vec<&str> = out.iter().map(|s| s.as_str()).collect();
    let group_lines: Vec<(usize, String)> = match block_of(&refreshed, "groups") {
        Some(b) => (b.key_line + 1..refreshed.len())
            .take_while(|&j| is_blank(refreshed[j]) || indent_of(refreshed[j]) > b.key_indent)
            .filter(|&j| !is_blank(refreshed[j]))
            .map(|j| (j, refreshed[j].to_string()))
            .collect(),
        None => Vec::new(),
    };
    for (j, l) in group_lines {
        if !contains_member(&l, &req.name) {
            continue;
        }
        if !l.contains('[') {
            manual.push(l.trim().split(':').next().unwrap_or("?").to_string());
            continue;
        }
        out[j] = drop_member(&l, &req.name);
    }
    match write_text(&req.path, &join(&out, text.ends_with('\n'))) {
        Ok(()) => Json(json!({
            "ok": true,
            "manual_groups": manual,   // 块式组没自动摘,如实说
        }))
        .into_response(),
        Err((c, m)) => err(c, m),
    }
}

/// flow 成员列表里有没有这个名字(按 token 比,`n1` 不该命中 `n11`)。
fn contains_member(line: &str, name: &str) -> bool {
    line.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .any(|t| t == name)
}

/// 从 `[a, b, c]` 里摘掉一个成员,其余字节不动。
fn drop_member(line: &str, name: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let Some(close_rel) = rest[open..].find(']') else { break };
        let close = open + close_rel;
        out.push_str(&rest[..=open]);
        let inner: Vec<&str> = rest[open + 1..close]
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && *s != name && s.trim_matches('"') != name)
            .collect();
        out.push_str(&inner.join(", "));
        out.push(']');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
}

// ───────────────────────────── 写:主机组 ─────────────────────────────

#[derive(Deserialize)]
pub struct GroupSet {
    pub path: String,
    pub name: String,
    #[serde(default)]
    pub hosts: Vec<String>,
}

/// `POST /api/inventory/group` —— 建组或改组成员(换一行 / 插一行)。
pub async fn group_set(Json(req): Json<GroupSet>) -> Response {
    if req.name.is_empty()
        || !req.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return err(StatusCode::BAD_REQUEST, "组名只允许字母数字-_");
    }
    let text = match read_text(&req.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some(b) = block_of(&lines, "groups") else {
        return err(
            StatusCode::CONFLICT,
            "找不到 `inventory:` 下的 `groups:` 块 —— 请直接改文本",
        );
    };
    // 成员必须是本文件里真实存在的主机 —— 幽灵成员是最难查的一类问题。
    for h in &req.hosts {
        if find_host_line(&lines, h).is_none() {
            return err(
                StatusCode::CONFLICT,
                format!("这份 inventory 里没有主机 `{h}`"),
            );
        }
    }
    let members = req
        .hosts
        .iter()
        .map(|h| scalar(h))
        .collect::<Vec<_>>()
        .join(", ");
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();

    match find_group_line(&lines, &req.name) {
        Some(i) => {
            if !is_single_line(&lines, i) {
                return err(StatusCode::CONFLICT, "这是块式组(多行)—— 请直接改文本");
            }
            if lines[i].contains("groups:") {
                return err(
                    StatusCode::CONFLICT,
                    "这个组含嵌套组 —— 整行替换会丢掉嵌套关系,请直接改文本",
                );
            }
            // 行尾注释保真:`}` 之后的部分原样接回去。
            let tail = lines[i]
                .rfind('}')
                .map(|p| lines[i][p + 1..].to_string())
                .unwrap_or_default();
            out[i] = format!(
                "{}{}: {{ hosts: [{}] }}{}",
                " ".repeat(indent_of(lines[i])),
                req.name,
                members,
                tail
            );
        }
        None => {
            let entry = format!(
                "{}{}: {{ hosts: [{}] }}",
                " ".repeat(b.item_indent),
                req.name,
                members
            );
            match b.last_content {
                Some(i) => out.insert(i + 1, entry),
                None => {
                    let k = &out[b.key_line];
                    if k.trim_end().ends_with("{}") {
                        out[b.key_line] = k.replacen("{}", "", 1).trim_end().to_string();
                    }
                    out.insert(b.key_line + 1, entry);
                }
            }
        }
    }
    match write_text(&req.path, &join(&out, text.ends_with('\n'))) {
        Ok(()) => Json(json!({ "ok": true, "group": req.name })).into_response(),
        Err((c, m)) => err(c, m),
    }
}

#[derive(Deserialize)]
pub struct GroupDel {
    pub path: String,
    pub name: String,
}

/// `DELETE /api/inventory/group` —— 删一个组(删一行)。
///
/// 被蓝图 `fleet.groups` 依赖的组不拦:那是蓝图×inventory 的对账问题,
/// app 的 lint 会报出来 —— 在这里拦会让"先删组再改蓝图"这种正常顺序走不通。
pub async fn group_remove(Json(req): Json<GroupDel>) -> Response {
    let text = match read_text(&req.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some(i) = find_group_line(&lines, &req.name) else {
        return err(StatusCode::NOT_FOUND, format!("找不到组 `{}`", req.name));
    };
    if !is_single_line(&lines, i) {
        return err(StatusCode::CONFLICT, "这是块式组(多行)—— 请直接改文本");
    }
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    out.remove(i);
    match write_text(&req.path, &join(&out, text.ends_with('\n'))) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err((c, m)) => err(c, m),
    }
}

// ───────────────────────────── 新建 inventory ─────────────────────────────

#[derive(Deserialize)]
pub struct InvCreate {
    pub name: String,
}

/// `POST /api/inventory/create` —— 新建一份**带注释**的空 inventory。
pub async fn inv_create(Json(req): Json<InvCreate>) -> Response {
    let stem = req.name.trim().trim_end_matches(".yaml");
    if stem.is_empty()
        || !stem.chars().all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return err(StatusCode::BAD_REQUEST, "文件名只允许字母数字-_.");
    }
    let file = format!("{stem}.inventory.yaml");
    if Path::new(&file).exists() {
        return err(StatusCode::CONFLICT, format!("{file} 已存在"));
    }
    let body = concat!(
        "# 机群清单:哪些机器、怎么连、怎么分组。\n",
        "# 这个文件就是机群本身 —— 可 git、可 diff、可进闭包;改它 = 改机群。\n",
        "inventory:\n",
        "  hosts:\n",
        "    # 在\"主机\"页点【新增主机】往这里加;也可以直接写:\n",
        "    # - { name: n1, address: 192.0.2.1, user: root, password: \"改成真口令\" }\n",
        "  groups:\n",
        "    # 组名要与蓝图的 fleet.groups 对得上(对不上时 app 的 lint 会报)。\n",
        "    # all: { hosts: [n1] }\n",
    );
    if let Err(e) = std::fs::write(&file, body) {
        return err(StatusCode::INTERNAL_SERVER_ERROR, format!("写入失败:{e}"));
    }
    Json(json!({ "ok": true, "path": file })).into_response()
}

// ───────────────────────────── 连通性探测 ─────────────────────────────

#[derive(Deserialize)]
pub struct ProbeReq {
    pub path: String,
    #[serde(default)]
    pub name: String,
}

/// `POST /api/inventory/probe` —— 真连一次:新加的主机到底通不通。
///
/// 这是 UI 相对于手改 YAML 的实质增量 —— 口令打错、端口不对、密钥没授权,
/// 在这里花两秒知道,而不是在一次 apply 跑到一半时知道。
pub async fn probe(Json(req): Json<ProbeReq>) -> Response {
    let text = match read_text(&req.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let mut spec: crater_core::spec::CraterSpec = match serde_yaml::from_str(&text) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::CONFLICT, format!("inventory 解析失败:{e}")),
    };
    spec.inventory.resolve();
    let targets: Vec<crater_core::spec::Host> = spec
        .inventory
        .hosts
        .into_iter()
        .filter(|h| req.name.is_empty() || h.name == req.name)
        .collect();
    if targets.is_empty() {
        return err(StatusCode::NOT_FOUND, "没有匹配的主机");
    }
    let mut results = Vec::new();
    for h in targets {
        let label = format!("{}@{}:{}", h.user, h.address, h.port);
        // 探测是只读的一条命令,超时兜住"连得上但不回话"的机器。
        let r = tokio::time::timeout(std::time::Duration::from_secs(12), probe_one(h.clone())).await;
        let (ok, detail) = match r {
            Err(_) => (false, "超时(12s)".to_string()),
            Ok(Err(e)) => (false, format!("{e:#}")),
            Ok(Ok(s)) => (true, s),
        };
        results.push(json!({ "name": h.name, "target": label, "ok": ok, "detail": detail }));
    }
    Json(json!({ "results": results })).into_response()
}

async fn probe_one(h: crater_core::spec::Host) -> anyhow::Result<String> {
    let exec = crate::target::connect_executor(&h, true).await?;
    let out = exec
        .run(". /etc/os-release 2>/dev/null; echo \"${PRETTY_NAME:-$(uname -sr)}\"")
        .await?;
    let text = format!("{}{}", out.stdout, out.stderr);
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    const INV: &str = "\
# 顶注
inventory:
  hosts:
    - { name: n1, address: 10.0.0.1, user: root }   # 第一台
    - { name: n2, address: 10.0.0.2, user: root }
  groups:
    all: { hosts: [n1, n2] }
    db:  { hosts: [n1] }
";

    fn lines(s: &str) -> Vec<&str> {
        s.lines().collect()
    }

    #[test]
    fn locates_inventory_child_blocks() {
        let ls = lines(INV);
        let h = block_of(&ls, "hosts").unwrap();
        assert_eq!(h.key_indent, 2);
        assert_eq!(h.item_indent, 4);
        assert_eq!(h.last_content, Some(4), "hosts 块最后一行是 n2");
        let g = block_of(&ls, "groups").unwrap();
        assert_eq!(g.last_content, Some(7));
    }

    /// 组里的 `hosts:` 不该被误认成 inventory 的 hosts 块。
    #[test]
    fn nested_block_style_group_hosts_do_not_hijack_the_block() {
        let src = "inventory:\n  hosts:\n    - { name: n1, address: 10.0.0.1 }\n  groups:\n    all:\n      hosts:\n        - n1\n";
        let ls = lines(src);
        let h = block_of(&ls, "hosts").unwrap();
        assert_eq!(h.key_line, 1, "必须是 inventory 的直接子键那一个");
    }

    #[test]
    fn member_matching_is_token_wise() {
        assert!(contains_member("all: { hosts: [n1, n2] }", "n1"));
        assert!(
            !contains_member("all: { hosts: [n11] }", "n1"),
            "n1 不该命中 n11"
        );
    }

    #[test]
    fn dropping_a_member_leaves_everything_else_byte_identical() {
        assert_eq!(
            drop_member("    all: { hosts: [n1, n2, n3] }   # 注", "n2"),
            "    all: { hosts: [n1, n3] }   # 注"
        );
    }

    #[test]
    fn block_style_entries_are_not_single_line() {
        let src = "inventory:\n  hosts:\n    - name: n1\n      address: 10.0.0.1\n";
        let ls = lines(src);
        assert!(
            !is_single_line(&ls, 2),
            "块式主机条目必须判为多行(降级只读)"
        );
    }

    #[test]
    fn passwords_are_always_quoted() {
        // 纯数字口令裸写会被 YAML 读成整数,反序列化直接失败。
        assert_eq!(scalar("123456"), "\"123456\"");
        assert_eq!(scalar("n1"), "n1");
        assert_eq!(scalar("on"), "\"on\"", "YAML 1.1 的歧义词要引号");
    }

    #[test]
    fn finding_a_host_line_is_exact() {
        let ls = lines(INV);
        assert_eq!(find_host_line(&ls, "n1"), Some(3));
        assert_eq!(find_host_line(&ls, "n2"), Some(4));
        assert_eq!(find_host_line(&ls, "n3"), None);
        assert_eq!(find_group_line(&ls, "db"), Some(7));
    }
}

// ───────────────────────────── 视图 ─────────────────────────────

/// `GET /view/hosts` —— 主机表:选一份 inventory,增删改查 + 连通性探测。
pub async fn view_hosts() -> axum::response::Html<String> {
    axum::response::Html(format!(
        "{HOSTS_HTML}{INV_CSS}<script>(function(){{{INV_JS_HEAD}{HOSTS_JS}}})();</script>"
    ))
}

/// `GET /view/groups` —— 主机组:成员勾选式编辑。
pub async fn view_groups() -> axum::response::Html<String> {
    axum::response::Html(format!(
        "{GROUPS_HTML}{INV_CSS}<script>(function(){{{INV_JS_HEAD}{GROUPS_JS}}})();</script>"
    ))
}

/// 两个页面共用的样式与"选文件"骨架 —— inventory 是它们共同的模型。
const INV_CSS: &str = r##"<style>
  .inv-bar{display:flex;gap:8px;align-items:center;margin-bottom:12px;flex-wrap:wrap}
  .inv-bar select,.inv-bar input{background:var(--surface-2);color:var(--text);
    border:1px solid var(--border);border-radius:8px;padding:6px 10px;font:inherit}
  .inv-bar select{min-width:260px}
  .inv-msg{font-size:12px;color:var(--muted);margin-left:auto}
  .inv-msg.bad{color:var(--drift)}
  .inv-t{width:100%;border-collapse:collapse;font-size:13px}
  .inv-t th{text-align:left;color:var(--muted);font-weight:600;padding:6px 8px;
    border-bottom:1px solid var(--border);white-space:nowrap}
  .inv-t td{padding:7px 8px;border-bottom:1px solid var(--border);vertical-align:top}
  .inv-t td.mono{font-family:ui-monospace,monospace}
  .inv-t .btn{font-size:12px;padding:3px 9px}
  .tag{display:inline-block;padding:1px 8px;border-radius:99px;background:var(--surface-2);
    color:var(--muted);font-size:11.5px;margin:0 3px 3px 0}
  .tag.ro{background:var(--unknown-bg);color:var(--unknown)}
  .ok-d{color:var(--ok)} .bad-d{color:var(--drift)}
  .inv-form{display:flex;gap:8px;flex-wrap:wrap;align-items:flex-end;
    border:1px solid var(--border);border-radius:10px;padding:12px;background:var(--surface-2);margin:12px 0}
  .inv-form label{display:flex;flex-direction:column;gap:4px;font-size:12px;color:var(--muted)}
  .inv-form input{background:var(--surface);color:var(--text);border:1px solid var(--border);
    border-radius:8px;padding:6px 9px;font:inherit;font-size:13px;width:150px}
  .inv-form input.narrow{width:78px}
  .btn.primary{background:var(--accent);color:#fff;border:0}
  .inv-empty{border:1px dashed var(--border);border-radius:12px;padding:20px;
    text-align:center;color:var(--muted)}
  .mem{display:flex;gap:4px;flex-wrap:wrap}
  .mem label{border:1px solid var(--border);border-radius:99px;padding:2px 10px;
    cursor:pointer;font-size:12px}
  .mem label:has(input:checked){background:var(--tint);border-color:var(--accent);color:var(--accent)}
  .mem input{display:none}
</style>"##;

/// 选文件条 + 新建 inventory,两页共用的那段 JS。
const INV_JS_HEAD: &str = r##"
  const sel = document.getElementById('inv-file');
  const msg = document.getElementById('inv-msg');
  function say(t, bad){ msg.textContent = t; msg.className = 'inv-msg' + (bad?' bad':''); }
  function esc(s){ return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;'); }
  async function fileList(){
    const d = await (await fetch('/api/files')).json();
    const invs = d.files.filter(f => f.kind === 'inventory');
    sel.innerHTML = invs.map(f => '<option>'+f.path+'</option>').join('')
      || '<option value="">(工作区还没有 inventory)</option>';
    const want = localStorage.getItem('crater.inv');
    if (want && invs.some(f => f.path === want)) sel.value = want;
    return invs.length;
  }
  window.invCreate = async function(){
    const name = prompt('新 inventory 文件名(会存成 <名字>.inventory.yaml):', 'my-fleet');
    if (!name) return;
    const d = await (await fetch('/api/inventory/create',{method:'POST',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({name})})).json();
    if (d.error){ say(d.error, true); return; }
    await fileList(); sel.value = d.path; localStorage.setItem('crater.inv', d.path);
    say('已创建 ' + d.path); load();
  };
  sel.addEventListener('change', () => { localStorage.setItem('crater.inv', sel.value); load(); });
"##;

const HOSTS_HTML: &str = r##"<section class="panel">
  <h2><span class="mk">▤</span> 主机</h2>
  <div class="inv-bar">
    <select id="inv-file"></select>
    <button class="btn" onclick="invCreate()">新建 inventory</button>
    <button class="btn" onclick="probeAll()">探测全部</button>
    <span id="inv-msg" class="inv-msg"></span>
  </div>
  <div class="inv-form">
    <label>名字<input id="h-name" placeholder="n1"></label>
    <label>地址<input id="h-addr" placeholder="192.168.1.10"></label>
    <label>端口<input id="h-port" class="narrow" placeholder="22"></label>
    <label>用户<input id="h-user" placeholder="root"></label>
    <label>口令<input id="h-pass" type="password" placeholder="留空则用密钥"></label>
    <label>密钥路径<input id="h-key" placeholder="~/.ssh/id_ed25519"></label>
    <label>变量<input id="h-vars" placeholder="k=v,k2=v2"></label>
    <button class="btn primary" onclick="hostAdd()">新增主机</button>
  </div>
  <div id="inv-body"></div>
</section>
"## ;

const GROUPS_HTML: &str = r##"<section class="panel">
  <h2><span class="mk">⊞</span> 主机组</h2>
  <div class="inv-bar">
    <select id="inv-file"></select>
    <button class="btn" onclick="invCreate()">新建 inventory</button>
    <span id="inv-msg" class="inv-msg"></span>
  </div>
  <div class="inv-form">
    <label>组名<input id="g-name" placeholder="db"></label>
    <label style="flex:1">成员<span id="g-mem" class="mem"></span></label>
    <button class="btn primary" onclick="groupSet()">建组 / 覆盖</button>
  </div>
  <div id="inv-body"></div>
</section>
"##;

const HOSTS_JS: &str = r##"
  const body = document.getElementById('inv-body');
  let probes = {};
  window.load = async function(){
    if (!sel.value){ body.innerHTML = '<div class="inv-empty">工作区还没有 inventory 文件。<br><br>'
      + '点上面的【新建 inventory】开一份,再往里加主机。</div>'; return; }
    const d = await (await fetch('/api/inventory/read?path='+encodeURIComponent(sel.value))).json();
    if (d.error){ body.innerHTML = '<div class="inv-empty bad-d">'+esc(d.error)+'</div>'; return; }
    if (!d.hosts.length){
      body.innerHTML = '<div class="inv-empty">这份 inventory 还没有主机 —— 用上面的表单加第一台。</div>';
      return;
    }
    body.innerHTML = '<table class="inv-t"><thead><tr><th>名字</th><th>地址</th><th>端口</th>'
      + '<th>用户</th><th>认证</th><th>组</th><th>变量</th><th>探测</th><th></th></tr></thead><tbody>'
      + d.hosts.map(h => {
        const p = probes[h.name];
        const pd = p ? '<span class="'+(p.ok?'ok-d':'bad-d')+'" title="'+esc(p.detail)+'">'
          + (p.ok?'✓ ':'✗ ') + esc(p.detail).slice(0,28) + '</span>' : '';
        const vars = Object.entries(h.vars||{}).map(([k,v])=>'<span class="tag">'+esc(k)+'='+esc(v)+'</span>').join('');
        const del = h.editable
          ? '<button class="btn" onclick="hostDel('+JSON.stringify(h.name).replace(/"/g,'&quot;')+')">删除</button>'
          : '<span class="tag ro">块式条目,只读</span>';
        return '<tr><td class="mono">'+esc(h.name)+'</td><td class="mono">'+esc(h.address)+'</td>'
          + '<td>'+h.port+'</td><td>'+esc(h.user)+'</td><td>'+esc(h.auth)+'</td>'
          + '<td>'+(h.groups||[]).map(g=>'<span class="tag">'+esc(g)+'</span>').join('')+'</td>'
          + '<td>'+vars+'</td><td>'+pd+'</td>'
          + '<td><button class="btn" onclick="probeOne('+JSON.stringify(h.name).replace(/"/g,'&quot;')+')">探测</button> '
          + del + '</td></tr>';
      }).join('') + '</tbody></table>';
  };
  window.hostAdd = async function(){
    const g = id => document.getElementById(id).value.trim();
    const vars = g('h-vars').split(',').map(s=>s.trim()).filter(Boolean)
      .map(s=>{const i=s.indexOf('='); return [s.slice(0,i), s.slice(i+1)];});
    const r = await fetch('/api/inventory/host',{method:'POST',headers:{'Content-Type':'application/json'},
      body:JSON.stringify({path:sel.value, name:g('h-name'), address:g('h-addr'),
        port:g('h-port')?parseInt(g('h-port')):null, user:g('h-user'),
        password:document.getElementById('h-pass').value, key:g('h-key'), vars})});
    const d = await r.json();
    if (d.error){ say(d.error, true); return; }
    for (const i of ['h-name','h-addr','h-port','h-pass','h-key','h-vars']) document.getElementById(i).value='';
    say('已加入 '+d.host+'(未探测)'); load();
  };
  window.hostDel = async function(name){
    if (!confirm('从 inventory 里删掉主机 '+name+'?(机器本身不动;旧版存 .bak)')) return;
    const d = await (await fetch('/api/inventory/host',{method:'DELETE',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({path:sel.value,name})})).json();
    if (d.error){ say(d.error, true); return; }
    delete probes[name];
    say((d.manual_groups||[]).length
      ? '已删除;这些块式组里的成员要手工摘:'+d.manual_groups.join(', ')
      : '已删除,并从各组成员里摘掉');
    load();
  };
  window.probeOne = async function(name){
    say('探测 '+name+' …');
    const d = await (await fetch('/api/inventory/probe',{method:'POST',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({path:sel.value,name})})).json();
    if (d.error){ say(d.error, true); return; }
    for (const r of d.results) probes[r.name] = r;
    say('探测完成'); load();
  };
  window.probeAll = async function(){
    say('探测全部主机 …(逐台串行,慢一点)');
    const d = await (await fetch('/api/inventory/probe',{method:'POST',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({path:sel.value,name:''})})).json();
    if (d.error){ say(d.error, true); return; }
    for (const r of d.results) probes[r.name] = r;
    const bad = d.results.filter(r=>!r.ok).length;
    say(bad ? (d.results.length-bad)+' 通 / '+bad+' 不通' : '全部 '+d.results.length+' 台可达');
    load();
  };
  fileList().then(load);
"##;

const GROUPS_JS: &str = r##"
  const body = document.getElementById('inv-body');
  let hostNames = [];
  window.load = async function(){
    if (!sel.value){ body.innerHTML = '<div class="inv-empty">工作区还没有 inventory 文件。</div>'; return; }
    const d = await (await fetch('/api/inventory/read?path='+encodeURIComponent(sel.value))).json();
    if (d.error){ body.innerHTML = '<div class="inv-empty bad-d">'+esc(d.error)+'</div>'; return; }
    hostNames = d.hosts.map(h=>h.name);
    // 成员勾选来自这份 inventory 的真实主机 —— 打不出幽灵成员。
    document.getElementById('g-mem').innerHTML = hostNames.length
      ? hostNames.map(n=>'<label><input type="checkbox" value="'+esc(n)+'">'+esc(n)+'</label>').join('')
      : '<span class="tag ro">先去"主机"页加机器</span>';
    if (!d.groups.length){
      body.innerHTML = '<div class="inv-empty">还没有组。组名要与蓝图的 <code>fleet.groups</code> 对得上。</div>';
      return;
    }
    body.innerHTML = '<table class="inv-t"><thead><tr><th>组</th><th>成员</th><th>嵌套组</th><th></th></tr></thead><tbody>'
      + d.groups.map(g => {
        const q = JSON.stringify(g.name).replace(/"/g,'&quot;');
        const act = g.editable
          ? '<button class="btn" onclick="groupEdit('+q+','+JSON.stringify(g.hosts).replace(/"/g,'&quot;')+')">载入编辑</button> '
            + '<button class="btn" onclick="groupDel('+q+')">删除</button>'
          : '<span class="tag ro">块式/嵌套组,只读</span>';
        return '<tr><td class="mono">'+esc(g.name)+'</td>'
          + '<td>'+(g.hosts||[]).map(h=>'<span class="tag">'+esc(h)+'</span>').join('')+'</td>'
          + '<td>'+(g.groups||[]).map(h=>'<span class="tag">@'+esc(h)+'</span>').join('')+'</td>'
          + '<td>'+act+'</td></tr>';
      }).join('') + '</tbody></table>';
  };
  window.groupEdit = function(name, hosts){
    document.getElementById('g-name').value = name;
    for (const cb of document.querySelectorAll('#g-mem input')) cb.checked = hosts.includes(cb.value);
    say('已载入 '+name+' —— 改完点【建组 / 覆盖】');
  };
  window.groupSet = async function(){
    const name = document.getElementById('g-name').value.trim();
    const hosts = [...document.querySelectorAll('#g-mem input:checked')].map(x=>x.value);
    const d = await (await fetch('/api/inventory/group',{method:'POST',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({path:sel.value,name,hosts})})).json();
    if (d.error){ say(d.error, true); return; }
    document.getElementById('g-name').value='';
    for (const cb of document.querySelectorAll('#g-mem input')) cb.checked=false;
    say('已写入组 '+d.group); load();
  };
  window.groupDel = async function(name){
    if (!confirm('删掉组 '+name+'?(引用它的蓝图会在 app 的 lint 里报出来)')) return;
    const d = await (await fetch('/api/inventory/group',{method:'DELETE',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({path:sel.value,name})})).json();
    if (d.error){ say(d.error, true); return; }
    say('已删除'); load();
  };
  fileList().then(load);
"##;
