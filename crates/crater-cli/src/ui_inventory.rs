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

/// 一台主机的字段集。**不含 path** —— 新增与编辑共用它(flatten 进各自的
/// 请求体),内外都放一个 `path` 的话 flatten 会互相打架。
#[derive(Deserialize)]
pub struct HostFields {
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

#[derive(Deserialize)]
pub struct HostAdd {
    pub path: String,
    #[serde(flatten)]
    pub f: HostFields,
}

/// 把一台主机渲染成一行 flow 条目。**新增与编辑共用这一个渲染器** ——
/// 两份渲染代码迟早会在引号规则上分家,而分家的那天没人看得出来。
fn render_host(req: &HostFields, indent: usize) -> String {
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
    format!("{}- {{ {} }}", " ".repeat(indent), fields.join(", "))
}

/// 缺 `hosts:` / `groups:` 块时把它补出来 —— 只有 hosts 没有 groups 的
/// inventory 是常态,不该因此完全建不了组。返回补完之后的行。
fn ensure_block(lines: &[String], key: &str) -> Option<Vec<String>> {
    let refreshed: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
    if block_of(&refreshed, key).is_some() {
        return None;
    }
    let iv = refreshed
        .iter()
        .position(|l| l.trim_start().starts_with("inventory:") && indent_of(l) == 0)?;
    // 子键缩进跟已有的那个走(文件的风格是文件的,不是我们的)。
    let child = refreshed[iv + 1..]
        .iter()
        .find(|l| !is_blank(l))
        .map(|l| indent_of(l))
        .filter(|i| *i > 0)
        .unwrap_or(2);
    let mut out = lines.to_vec();
    out.push(format!("{}{key}:", " ".repeat(child)));
    Some(out)
}

/// `POST /api/inventory/host` —— 新增一台主机(在 `hosts:` 块末尾**插一行**)。
pub async fn host_add(Json(req): Json<HostAdd>) -> Response {
    if req.f.name.is_empty()
        || !req.f.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return err(StatusCode::BAD_REQUEST, "主机名只允许字母数字-_");
    }
    if req.f.address.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "地址不能为空");
    }
    let text = match read_text(&req.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let lines: Vec<&str> = text.lines().collect();
    if find_host_line(&lines, &req.f.name).is_some() {
        return err(StatusCode::CONFLICT, format!("主机 `{}` 已存在", req.f.name));
    }
    // 缺 hosts: 块就补一个 —— 这不是"看不懂的结构",只是还没写。
    let patched = ensure_block(&lines.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "hosts");
    let owned: Vec<String> = patched.unwrap_or_else(|| lines.iter().map(|s| s.to_string()).collect());
    let lines: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    let Some(b) = block_of(&lines, "hosts") else {
        return err(StatusCode::CONFLICT, "这份文件没有顶层 `inventory:` —— 请直接改文本");
    };

    let entry = render_host(&req.f, b.item_indent);
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
        Ok(()) => Json(json!({ "ok": true, "host": req.f.name })).into_response(),
        Err((c, m)) => err(c, m),
    }
}

/// `PUT /api/inventory/host` —— 编辑一台主机(**换一行**)。
///
/// 改名会连带改各组成员:组里留着旧名字,等于把机器悄悄踢出了组,
/// 而这种"改完之后少跑了一台"最难在事后看出来。
pub async fn host_update(Json(req): Json<HostUpdate>) -> Response {
    let text = match read_text(&req.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some(i) = find_host_line(&lines, &req.old_name) else {
        return err(StatusCode::NOT_FOUND, format!("找不到主机 `{}`", req.old_name));
    };
    if !is_single_line(&lines, i) {
        return err(StatusCode::CONFLICT, "这是块式条目(多行)—— 请直接改文本");
    }
    if req.host.name != req.old_name && find_host_line(&lines, &req.host.name).is_some() {
        return err(StatusCode::CONFLICT, format!("主机 `{}` 已存在", req.host.name));
    }
    if req.host.name.is_empty()
        || !req.host.name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return err(StatusCode::BAD_REQUEST, "主机名只允许字母数字-_");
    }
    if req.host.address.trim().is_empty() {
        return err(StatusCode::BAD_REQUEST, "地址不能为空");
    }
    // 行尾注释保真:`}` 之后的部分原样接回去。
    let tail = lines[i]
        .rfind('}')
        .map(|p| lines[i][p + 1..].to_string())
        .unwrap_or_default();
    let mut host = req.host;
    if req.keep_password && host.password.is_empty() && host.key.trim().is_empty() {
        host.password = password_of(lines[i]).unwrap_or_default();
    }
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    out[i] = format!("{}{}", render_host(&host, indent_of(lines[i])), tail);

    let mut manual: Vec<String> = Vec::new();
    if host.name != req.old_name {
        let refreshed: Vec<&str> = out.iter().map(|s| s.as_str()).collect();
        let targets: Vec<usize> = match block_of(&refreshed, "groups") {
            Some(b) => (b.key_line + 1..refreshed.len())
                .take_while(|&j| is_blank(refreshed[j]) || indent_of(refreshed[j]) > b.key_indent)
                .filter(|&j| !is_blank(refreshed[j]) && contains_member(refreshed[j], &req.old_name))
                .collect(),
            None => Vec::new(),
        };
        for j in targets {
            if !out[j].contains('[') {
                manual.push(out[j].trim().split(':').next().unwrap_or("?").to_string());
                continue;
            }
            out[j] = rename_member(&out[j], &req.old_name, &host.name);
        }
    }
    match write_text(&req.path, &join(&out, text.ends_with('\n'))) {
        Ok(()) => Json(json!({ "ok": true, "host": host.name, "manual_groups": manual }))
            .into_response(),
        Err((c, m)) => err(c, m),
    }
}

#[derive(Deserialize)]
pub struct HostUpdate {
    pub path: String,
    /// 改之前的名字 —— 定位靠它,改名才可能。
    pub old_name: String,
    /// 口令留空 = 沿用原来那个。
    ///
    /// 读接口**刻意不回传明文口令**(没必要为了改个端口就把口令铺到页面上),
    /// 所以客户端送不回来,只能由服务端从原行里取。
    #[serde(default)]
    pub keep_password: bool,
    #[serde(flatten)]
    pub host: HostFields,
}

/// 从一行 flow 主机条目里抠出口令原文(带引号的与裸写的都认)。
fn password_of(line: &str) -> Option<String> {
    let at = line.find("password:")? + "password:".len();
    let rest = line[at..].trim_start();
    if let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let body = &rest[1..];
        let end = body.find(q)?;
        return Some(body[..end].replace("\\\"", "\""));
    }
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

/// 在 flow 成员列表里把一个名字换成另一个,其余字节不动。
fn rename_member(line: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let Some(close_rel) = rest[open..].find(']') else { break };
        let close = open + close_rel;
        out.push_str(&rest[..=open]);
        let inner: Vec<String> = rest[open + 1..close]
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| if s == from || s.trim_matches('"') == from { to.to_string() } else { s.to_string() })
            .collect();
        out.push_str(&inner.join(", "));
        out.push(']');
        rest = &rest[close + 1..];
    }
    out.push_str(rest);
    out
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
    let lines0: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    // 只有 hosts 没有 groups 的 inventory 是常态 —— 缺块就补一个,
    // 而不是让人"先去手写一行 groups: 再回来点按钮"。
    let owned = ensure_block(&lines0, "groups").unwrap_or(lines0);
    let lines: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    let Some(b) = block_of(&lines, "groups") else {
        return err(StatusCode::CONFLICT, "这份文件没有顶层 `inventory:` —— 请直接改文本");
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

// ───────────────────── 连通性:后台探测 + 存活缓存 ─────────────────────

/// 存活缓存:键是**连接身份**(user@address:port),不是 inventory 里的名字。
///
/// 同一台机器常同时出现在好几份 inventory 里(库里的 k8s/middleware/rustfs
/// 示例都指着同一批实验机);按连接身份缓存,探一次全都亮,而不是按文件
/// 重复捅同一台机器。
#[derive(Clone)]
struct Live {
    ok: bool,
    detail: String,
    /// 上次出结果的时刻(epoch 秒);0 = 还没有过结果。
    ts: u64,
    /// 已经有一个后台探测在飞 —— 去重靠它,否则每次轮询都会再开一批。
    probing: bool,
}

fn cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, Live>> {
    static C: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Live>>> =
        std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// 并发闸:实验机往往是同一台物理机上的几个虚机,SSH 并发拉满只会互相拖慢。
fn gate() -> &'static tokio::sync::Semaphore {
    static S: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    S.get_or_init(|| tokio::sync::Semaphore::new(8))
}

/// 结果多久算旧。比页面轮询间隔大一个数量级 —— 轮询是为了取结果,不是为了触发探测。
const TTL: u64 = 45;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn conn_key(h: &crater_core::spec::Host) -> String {
    format!("{}@{}:{}", h.user, h.address, h.port)
}

/// 派一次后台探测(已在飞则不重复派)。**立刻返回** —— 调用方永远不等 SSH。
fn kick(h: crater_core::spec::Host) {
    let key = conn_key(&h);
    // 本机不走 SSH:连自己还要握手一次是白费的,而且本机永远"在线"。
    if h.is_local() {
        let mut c = cache().lock().unwrap();
        c.insert(key, Live { ok: true, detail: "本机".into(), ts: now(), probing: false });
        return;
    }
    {
        let mut c = cache().lock().unwrap();
        let e = c.entry(key.clone()).or_insert(Live {
            ok: false,
            detail: String::new(),
            ts: 0,
            probing: false,
        });
        if e.probing {
            return;
        }
        e.probing = true;
    }
    tokio::spawn(async move {
        let _permit = gate().acquire().await;
        // 超时兜住"连得上但不回话"的机器 —— 那种最耗时,也最该显示成不通。
        let r = tokio::time::timeout(std::time::Duration::from_secs(8), probe_one(h)).await;
        let (ok, detail) = match r {
            Err(_) => (false, "超时(8s)".to_string()),
            Ok(Err(e)) => (false, format!("{e:#}")),
            Ok(Ok(s)) => (true, s),
        };
        let mut c = cache().lock().unwrap();
        c.insert(key, Live { ok, detail, ts: now(), probing: false });
    });
}

/// `GET /api/inventory/liveness?path=…` —— 读缓存 + 顺手补探过期的。
///
/// **永不阻塞在 SSH 上**:页面拿到的是此刻已知的结果(附年龄),同时后台
/// 把过期的重探一遍,下一轮轮询就新了。没人看的 inventory 一次都不探 ——
/// 全局定时扫全库会把实验机 SSH 打满,而那些结果没人看。
pub async fn liveness(Query(q): Query<PathQ>) -> Response {
    let text = match read_text(&q.path) {
        Ok(t) => t,
        Err((c, m)) => return err(c, m),
    };
    let mut spec: crater_core::spec::CraterSpec = match serde_yaml::from_str(&text) {
        Ok(s) => s,
        Err(e) => return err(StatusCode::CONFLICT, format!("inventory 解析失败:{e}")),
    };
    spec.inventory.resolve();
    let n = now();
    let mut out = serde_json::Map::new();
    for h in &spec.inventory.hosts {
        let key = conn_key(h);
        let cur = cache().lock().unwrap().get(&key).cloned();
        let stale = match &cur {
            None => true,
            Some(l) => !l.probing && n.saturating_sub(l.ts) > TTL,
        };
        if stale {
            kick(h.clone());
        }
        // 重新读一次:本机那条在 kick 里就落定了。
        let l = cache().lock().unwrap().get(&key).cloned();
        out.insert(
            h.name.clone(),
            match l {
                Some(l) if l.ts > 0 => json!({
                    "state": if l.ok { "up" } else { "down" },
                    "detail": l.detail,
                    "age": n.saturating_sub(l.ts),
                    "probing": l.probing,
                }),
                _ => json!({ "state": "unknown", "detail": "", "age": null, "probing": true }),
            },
        );
    }
    Json(json!({ "hosts": out })).into_response()
}

#[derive(Deserialize)]
pub struct ProbeReq {
    pub path: String,
    #[serde(default)]
    pub name: String,
}

/// `POST /api/inventory/probe` —— 强制重探(把结果标旧,立刻派探测)。
///
/// 同样不等结果:点完按钮点亮的是"探测中",结果由下一轮 liveness 轮询接住。
/// 只有一条探测代码路径(kick),按钮与自动刷新不会各走各的、给出不同答案。
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
    for h in &targets {
        cache().lock().unwrap().remove(&conn_key(h));
        kick(h.clone());
    }
    Json(json!({ "ok": true, "kicked": targets.len() })).into_response()
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
  .dot{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:6px;
    vertical-align:middle;background:var(--unknown)}
  .dot.up{background:var(--ok);box-shadow:0 0 0 3px color-mix(in srgb,var(--ok) 22%,transparent)}
  .dot.down{background:var(--drift)}
  .dot.probing{background:var(--accent);animation:dpulse 1.1s ease-in-out infinite}
  @keyframes dpulse{0%,100%{opacity:1}50%{opacity:.25}}
  .st{white-space:nowrap;font-size:12.5px}
  .st .why{color:var(--faint);font-size:11.5px}
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
  .meta-hint{font-size:12px;color:var(--faint)}
  .inv-t tr.grow{cursor:pointer}
  .inv-t tr.grow:hover{background:var(--surface-2)}
  .inv-t tr.open{background:var(--surface-2)}
  .gedit{background:var(--surface-2)}
  .gedit td{padding:12px 14px}
  .gedit .row{display:flex;gap:10px;align-items:center;flex-wrap:wrap;margin-bottom:8px}
  .gedit .row b{font-size:12px;color:var(--muted);font-weight:600}
  .gedit input{background:var(--surface);color:var(--text);border:1px solid var(--border);
    border-radius:8px;padding:5px 9px;font:inherit;font-size:13px;width:150px}
  .chipx{display:inline-flex;align-items:center;gap:5px;padding:2px 6px 2px 10px;
    border-radius:99px;background:var(--surface);border:1px solid var(--border);font-size:12px}
  .chipx button{border:0;background:transparent;color:var(--faint);cursor:pointer;
    font-size:14px;line-height:1;padding:0 2px}
  .chipx button:hover{color:var(--drift)}
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
    <button class="btn" onclick="probeAll()">立即重探</button>
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
    <button class="btn primary" id="h-submit" onclick="hostSubmit()">新增主机</button>
    <button class="btn" id="h-cancel" style="display:none" onclick="hostCancel()">取消编辑</button>
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
    <button class="btn primary" onclick="groupNew()">新建空组</button>
    <span class="meta-hint">空组是合法拓扑(单节点的 worker),建完点开就能加成员</span>
  </div>
  <div id="inv-body"></div>
</section>
"##;

const HOSTS_JS: &str = r##"
  const body = document.getElementById('inv-body');
  // 存活状态由后台探测供血,页面只读缓存 —— 点开就有,不必点任何按钮。
  let live = {}, liveTimer = null;
  function dot(h){
    const l = live[h.name];
    if (!l || l.state === 'unknown')
      return '<span class="st"><span class="dot probing"></span>探测中…</span>';
    if (l.probing && l.state !== 'unknown')
      return '<span class="st"><span class="dot '+l.state+'"></span>重探中…</span>';
    const age = l.age == null ? '' : (l.age < 60 ? l.age+'s 前' : Math.floor(l.age/60)+'m 前');
    if (l.state === 'up')
      return '<span class="st"><span class="dot up"></span>在线'
        + '<span class="why"> · '+esc(l.detail).slice(0,26)+' · '+age+'</span></span>';
    return '<span class="st" title="'+esc(l.detail)+'"><span class="dot down"></span>不通'
      + '<span class="why"> · '+esc(l.detail).slice(0,30)+'</span></span>';
  }
  async function pollLive(){
    // 页面被 htmx 换走就自行了断 —— 否则每切一次视图都留下一个定时器。
    // 按**节点身份**判断,不按 id:主机组页也有一个 id="inv-body",
    // 用 getElementById 会认成"我还在",于是切走之后照样在捅机器。
    if (!document.contains(body)){ clearTimeout(liveTimer); return; }
    if (sel.value){
      try{
        const d = await (await fetch('/api/inventory/liveness?path='+encodeURIComponent(sel.value))).json();
        if (d.hosts){ live = d.hosts; paintDots(); }
      }catch(e){ /* 存活是增益:拿不到就保持上一次的显示 */ }
    }
    liveTimer = setTimeout(pollLive, 3000);
  }
  function paintDots(){
    for (const td of document.querySelectorAll('#inv-body td[data-live]'))
      td.innerHTML = dot({name: td.dataset.live});
  }
  window.load = async function(){
    if (!sel.value){ body.innerHTML = '<div class="inv-empty">工作区还没有 inventory 文件。<br><br>'
      + '点上面的【新建 inventory】开一份,再往里加主机。</div>'; return; }
    const d = await (await fetch('/api/inventory/read?path='+encodeURIComponent(sel.value))).json();
    if (d.error){ body.innerHTML = '<div class="inv-empty bad-d">'+esc(d.error)+'</div>'; return; }
    if (!d.hosts.length){
      body.innerHTML = '<div class="inv-empty">这份 inventory 还没有主机 —— 用上面的表单加第一台。</div>';
      return;
    }
    lastRows = d.hosts;
    body.innerHTML = '<table class="inv-t"><thead><tr><th>状态</th><th>名字</th><th>地址</th><th>端口</th>'
      + '<th>用户</th><th>认证</th><th>组</th><th>变量</th><th></th></tr></thead><tbody>'
      + d.hosts.map(h => {
        const q = JSON.stringify(h.name).replace(/"/g,'&quot;');
        const vars = Object.entries(h.vars||{}).map(([k,v])=>'<span class="tag">'+esc(k)+'='+esc(v)+'</span>').join('');
        const del = h.editable
          ? '<button class="btn" onclick="hostDel('+q+')">删除</button>'
          : '<span class="tag ro">块式条目,只读</span>';
        return '<tr><td data-live="'+esc(h.name)+'"></td><td class="mono">'+esc(h.name)+'</td>'
          + '<td class="mono">'+esc(h.address)+'</td>'
          + '<td>'+h.port+'</td><td>'+esc(h.user)+'</td><td>'+esc(h.auth)+'</td>'
          + '<td>'+(h.groups||[]).map(g=>'<span class="tag">'+esc(g)+'</span>').join('')+'</td>'
          + '<td>'+vars+'</td>'
          + '<td><button class="btn" onclick="probeOne('+q+')">重探</button> '
          + (h.editable ? '<button class="btn" onclick="hostEdit('+q+')">编辑</button> ' : '')
          + del + '</td></tr>';
      }).join('') + '</tbody></table>';
    paintDots();
  };
  // 同一个表单兼作新增与编辑 —— editing 非空即编辑态。
  let editing = null, lastRows = [];
  function formClear(){
    for (const i of ['h-name','h-addr','h-port','h-pass','h-key','h-vars'])
      document.getElementById(i).value='';
    editing = null;
    document.getElementById('h-submit').textContent = '新增主机';
    document.getElementById('h-cancel').style.display = 'none';
  }
  window.hostCancel = formClear;
  window.hostEdit = function(name){
    const h = lastRows.find(x=>x.name===name); if (!h) return;
    document.getElementById('h-name').value = h.name;
    document.getElementById('h-addr').value = h.address;
    document.getElementById('h-port').value = h.port;
    document.getElementById('h-user').value = h.user;
    document.getElementById('h-key').value  = h.key || '';
    document.getElementById('h-vars').value =
      Object.entries(h.vars||{}).map(([k,v])=>k+'='+v).join(',');
    // 口令不回填(文件里是明文,但没必要再把它铺到页面上);留空 = 不改。
    document.getElementById('h-pass').value = '';
    document.getElementById('h-pass').placeholder = '留空 = 不改动原口令';
    editing = name;
    document.getElementById('h-submit').textContent = '保存修改';
    document.getElementById('h-cancel').style.display = '';
    say('正在编辑 '+name);
  };
  window.hostSubmit = async function(){
    const g = id => document.getElementById(id).value.trim();
    const vars = g('h-vars').split(',').map(s=>s.trim()).filter(Boolean)
      .map(s=>{const i=s.indexOf('='); return [s.slice(0,i), s.slice(i+1)];});
    const prev = editing ? lastRows.find(x=>x.name===editing) : null;
    const pass = document.getElementById('h-pass').value;
    const payload = {path:sel.value, name:g('h-name'), address:g('h-addr'),
      port:g('h-port')?parseInt(g('h-port')):null, user:g('h-user'),
      password:pass, key:g('h-key'), vars};
    if (editing){
      // 口令留空 = 保持原样。做法是把原行的口令读回来 —— 但 read 接口
      // 刻意不回传明文口令,所以让服务端保留:留空且原来就是口令认证时,
      // 前端不覆盖 key/password 两项之外的判断,交给下面的 keep_password。
      payload.old_name = editing;
      payload.keep_password = !pass && prev && prev.auth === 'password';
    }
    const r = await fetch('/api/inventory/host',
      {method: editing ? 'PUT' : 'POST', headers:{'Content-Type':'application/json'},
       body:JSON.stringify(payload)});
    const d = await r.json();
    if (d.error){ say(d.error, true); return; }
    const was = editing;
    formClear();
    say(was ? ('已保存 '+d.host + ((d.manual_groups||[]).length
        ? ';这些块式组里的旧名字要手工改:'+d.manual_groups.join(', ') : ''))
            : ('已加入 '+d.host+'(探测中)'));
    load();
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
  // 手动只是"催一下":标旧 + 立刻派探测,结果仍由轮询接住 ——
  // 只有一条探测路径,按钮和自动刷新不会给出两种答案。
  async function nudge(name){
    const d = await (await fetch('/api/inventory/probe',{method:'POST',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({path:sel.value,name})})).json();
    if (d.error){ say(d.error, true); return false; }
    if (live[name]) live[name].probing = true; else for (const k in live) live[k].probing = true;
    paintDots();
    return true;
  }
  window.probeOne = async function(name){ if (await nudge(name)) say('重探 '+name+' …'); };
  window.probeAll = async function(){ if (await nudge('')) say('重探全部 …'); };
  fileList().then(load).then(pollLive);
"##;

const GROUPS_JS: &str = r##"
  const body = document.getElementById('inv-body');
  let hostNames = [], groups = [], open = null;
  window.load = async function(){
    if (!sel.value){ body.innerHTML = '<div class="inv-empty">工作区还没有 inventory 文件。</div>'; return; }
    const d = await (await fetch('/api/inventory/read?path='+encodeURIComponent(sel.value))).json();
    if (d.error){ body.innerHTML = '<div class="inv-empty bad-d">'+esc(d.error)+'</div>'; return; }
    // 可加的成员只来自这份 inventory 的真实主机 —— 打不出幽灵成员。
    hostNames = d.hosts.map(h=>h.name);
    if (!d.groups.length){
      body.innerHTML = '<div class="inv-empty">还没有组。<br><br>'
        + '组名要与蓝图的 <code>fleet.groups</code> 对得上 —— 对不上时 app 的 lint 会报。<br>'
        + '上面填个组名点【新建空组】,建完点开就能加成员。</div>';
      groups = [];
      return;
    }
    groups = d.groups;
    body.innerHTML = '<table class="inv-t"><thead><tr><th>组</th><th>成员</th><th>嵌套组</th><th></th></tr></thead><tbody>'
      + d.groups.map(g => {
        const q = JSON.stringify(g.name).replace(/"/g,'&quot;');
        const act = g.editable
          ? '<button class="btn" onclick="event.stopPropagation();groupDel('+q+')">删除</button>'
          : '<span class="tag ro">块式/嵌套组,只读</span>';
        const cls = g.editable ? 'grow' + (open===g.name?' open':'') : '';
        const click = g.editable ? ' onclick="groupToggle('+q+')"' : '';
        const rows = '<tr class="'+cls+'"'+click+'><td class="mono">'
          + (g.editable ? (open===g.name?'▾ ':'▸ ') : '')+esc(g.name)+'</td>'
          + '<td>'+((g.hosts||[]).length
              ? g.hosts.map(h=>'<span class="tag">'+esc(h)+'</span>').join('')
              : '<span class="tag ro">空组</span>')+'</td>'
          + '<td>'+(g.groups||[]).map(h=>'<span class="tag">@'+esc(h)+'</span>').join('')+'</td>'
          + '<td>'+act+'</td></tr>';
        return rows + (open===g.name ? editorRow(g) : '');
      }).join('') + '</tbody></table>';
  };
  // 展开行:成员增删(点 chip 上的 × 移除,勾选未入组的主机加入)、改名。
  function editorRow(g){
    const inn = hostNames.filter(n=>!g.hosts.includes(n));
    const q = JSON.stringify(g.name).replace(/"/g,'&quot;');
    return '<tr class="gedit"><td colspan="4">'
      + '<div class="row"><b>成员</b>'
      + (g.hosts.length ? g.hosts.map(h=>'<span class="chipx">'+esc(h)
          + '<button title="移出本组" onclick="memDrop('+q+','+JSON.stringify(h).replace(/"/g,'&quot;')+')">×</button></span>').join('')
        : '<span class="tag ro">还没有成员</span>')
      + '</div>'
      + '<div class="row"><b>加入</b>'
      + (inn.length ? inn.map(h=>'<button class="btn" onclick="memAdd('+q+','
          + JSON.stringify(h).replace(/"/g,'&quot;')+')">+ '+esc(h)+'</button>').join('')
        : '<span class="tag ro">这份 inventory 的主机都已在组内</span>')
      + '</div>'
      + '<div class="row"><b>改名</b><input id="g-rn" value="'+esc(g.name)+'">'
      + '<button class="btn" onclick="groupRename('+q+')">保存新名字</button></div>'
      + '</td></tr>';
  }
  window.groupToggle = function(name){ open = (open===name ? null : name); load(); };
  async function setMembers(name, hosts, note){
    const d = await (await fetch('/api/inventory/group',{method:'POST',
      headers:{'Content-Type':'application/json'},body:JSON.stringify({path:sel.value,name,hosts})})).json();
    if (d.error){ say(d.error, true); return false; }
    say(note); load(); return true;
  }
  window.memAdd = function(g, h){
    const cur = groups.find(x=>x.name===g); if (!cur) return;
    setMembers(g, cur.hosts.concat([h]), h+' 已加入 '+g);
  };
  window.memDrop = function(g, h){
    const cur = groups.find(x=>x.name===g); if (!cur) return;
    setMembers(g, cur.hosts.filter(x=>x!==h), h+' 已移出 '+g);
  };
  window.groupNew = async function(){
    const name = document.getElementById('g-name').value.trim();
    if (!name){ say('先填组名', true); return; }
    // 空组照建 —— 建完展开就能加成员,不必先凑齐机器。
    if (await setMembers(name, [], '已建组 '+name+'(空组,点开加成员)')){
      document.getElementById('g-name').value=''; open = name;
    }
  };
  window.groupRename = async function(oldName){
    const nn = document.getElementById('g-rn').value.trim();
    if (!nn || nn === oldName) return;
    const cur = groups.find(x=>x.name===oldName); if (!cur) return;
    // 先建新的再删旧的:反过来的话中间那一刻组没了,而删除是不可撤的。
    const d = await (await fetch('/api/inventory/group',{method:'POST',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({path:sel.value,name:nn,hosts:cur.hosts})})).json();
    if (d.error){ say(d.error, true); return; }
    const r = await (await fetch('/api/inventory/group',{method:'DELETE',
      headers:{'Content-Type':'application/json'},
      body:JSON.stringify({path:sel.value,name:oldName})})).json();
    if (r.error){ say('新组已建,旧组没删掉:'+r.error, true); }
    else say(oldName+' → '+nn+'(引用旧组名的蓝图会在 app 的 lint 里报)');
    open = nn; load();
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
