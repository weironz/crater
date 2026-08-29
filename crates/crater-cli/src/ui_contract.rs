//! 契约 API —— UI 与编辑器赖以工作的三个端点。
//!
//! 它们不是"给 UI 加的功能",而是把**已有的机器可读契约**接出来:
//! 类型登记表、JSON Schema 生成器、lint 诊断,三者本就存在且互为一致。
//!
//! 这正是我们能做出比 AWX/Semaphore 强的 UI 的唯一原因。Ansible 的模块参数
//! 只存在于远端 Python 里,控制端从来不知道 `copy` 支持哪些字段 —— 没有契约,
//! UI 就只能退化成"一个文本框 + 一个运行按钮"。而我们这 34 个类型 / 121 个
//! 字段是一张表,CLI 字段卡、JSON Schema、lint 报错、UI 表单是它的四个消费者。
//!
//! **纪律**:UI 里不得硬编码任何字段。一旦手写一份字段列表,加类型就要改两处,
//! 两处迟早分家 —— 那时 UI 说的和引擎认的不是一回事,而用户只会相信 UI。

use axum::{
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Json},
};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

/// `GET /api/types` —— 全部类型;`?name=copy` 取单个。
///
/// 表单面板据此渲染:必选/可选、枚举下拉、互斥组单选、悬停帮助,
/// 以及"这个类型尚未实现"的显式标注。
pub async fn types(Query(q): Query<NameQuery>) -> impl IntoResponse {
    match q.name {
        None => Json(crater_ir::types::catalog_json()).into_response(),
        Some(n) => match crater_ir::types::type_json(&n) {
            Some(v) => Json(v).into_response(),
            // 未知类型也给拼写建议 —— 与 lint、CLI 用同一份纠错逻辑。
            None => (
                StatusCode::NOT_FOUND,
                Json(json!({
                    "error": format!("没有类型 `{n}`"),
                    "suggestion": crater_ir::types::suggest(&n),
                })),
            )
                .into_response(),
        },
    }
}

#[derive(Deserialize)]
pub struct NameQuery {
    pub name: Option<String>,
}

/// `POST /api/schema` —— JSON Schema(draft 2020-12)。
///
/// 请求体给了蓝图正文就**自特化**:该蓝图自己的物料名、自定义类型、选角名
/// 会进入补全候选。编辑器把它喂给 YAML 语言服务,就有了补全与悬停 ——
/// 而这些候选是这份蓝图特有的,通用 schema 给不出来。
pub async fn schema(body: String) -> impl IntoResponse {
    let bp = if body.trim().is_empty() {
        None
    } else {
        crater_ir::parse::blueprint_from_str(&body).ok()
    };
    Json(crater_ir::jsonschema::generate(bp.as_ref()))
}

/// `POST /api/lint` —— 正文是蓝图 YAML,返回带行号的诊断。
///
/// 与 `crater lint` **同一套**措辞、同一批行锚点、同一份拼写建议。
/// UI 校验与 CLI lint 一旦分家,必有一个在说谎 —— 而用户会相信眼前那个。
pub async fn lint(body: String) -> impl IntoResponse {
    // 解析失败本身就是一条诊断,不是 HTTP 错误:编辑器里 YAML 半截不合法是
    // 常态(用户正打字),返回 4xx 会让编辑器把它当成请求出错而不是内容有问题。
    let bp = match crater_ir::parse::blueprint_from_str(&body) {
        Ok(bp) => bp,
        Err(e) => {
            return Json(json!({
                "ok": false,
                "parsed": false,
                "diagnostics": [{
                    "severity": "error",
                    "at": "(解析)",
                    "line": parse_error_line(&e.to_string()),
                    "message": e.to_string(),
                }],
            }))
        }
    };

    let diags = crater_ir::lint::lint(&bp);
    let errors = diags.iter().filter(|d| d.is_error()).count();
    Json(json!({
        "ok": errors == 0,
        "parsed": true,
        // 概览:UI 顶栏据此显示"这份蓝图有多大",不必自己数。
        "summary": {
            "name": bp.name,
            "resources": bp.resources.len(),
            "materials": bp.materials.len(),
            "procedures": bp.procedures.len(),
            "custom_types": bp.types.len(),
            "health": bp.health.len(),
            "errors": errors,
            "warnings": diags.len() - errors,
        },
        // 机群契约单独给出来:UI 据此在**按运行之前**核对 inventory,
        // 而不是跑到一半才发现少台机器。
        "fleet": bp.fleet.groups.iter()
            .map(|(g, c)| json!({ "group": g, "min": c.min }))
            .collect::<Vec<_>>(),
        "params": bp.params.iter().map(|(n, p)| json!({
            "name": n,
            "required": p.default.is_none(),
            "default": p.default,
            "desc": p.desc,
        })).collect::<Vec<_>>(),
        "diagnostics": diags.iter().map(|d| json!({
            "severity": if d.is_error() { "error" } else { "warning" },
            "at": d.at,
            "line": d.line,
            "message": d.msg,
        })).collect::<Vec<_>>(),
    }))
}

/// serde_yaml 的报错里带 `at line N column M`,把行号抠出来给编辑器打标记。
/// 抠不出来就返回 None —— 宁可不标记,也不要标错行。
fn parse_error_line(msg: &str) -> Option<usize> {
    let i = msg.find("line ")? + 5;
    msg[i..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .ok()
}

/// `GET /api/inventory/skeleton?...` —— 按蓝图的机群契约生成 inventory 骨架。
///
/// 这条直接对着"AWX 要你事先准备好 inventory 文件"那个痛点:蓝图已经声明了
/// 它需要哪些组、每组最少几台,那份骨架本就该由机器生成,而不是让人照文档抄。
pub async fn inventory_skeleton(body: String) -> impl IntoResponse {
    let bp = match crater_ir::parse::blueprint_from_str(&body) {
        Ok(bp) => bp,
        Err(e) => {
            return (StatusCode::BAD_REQUEST, Json(json!({ "error": e.to_string() }))).into_response()
        }
    };
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut hosts: Vec<serde_json::Value> = Vec::new();
    let mut n = 0usize;
    for (g, c) in &bp.fleet.groups {
        let mut members = Vec::new();
        // min: 0 的组(如单节点拓扑的 worker)照样列出来但留空 ——
        // "声明过但为空"与"没有这个组"是两回事,骨架要把这个区别显出来。
        for _ in 0..c.min {
            n += 1;
            let name = format!("n{n}");
            hosts.push(json!({
                "name": name,
                "address": "192.0.2.1",
                "user": "root",
                "password": "改成真的口令,或改用 key",
            }));
            members.push(name);
        }
        groups.insert(g.clone(), members);
    }
    // **带注释的 YAML 文本**,不是 JSON 结构:JSON 化丢注释,而"这里为什么留空"
    // 恰恰要靠注释说 —— YAML 是模型这条原则对生成物同样成立。
    let mut y = String::from(
        "# 由蓝图的 fleet.groups 生成的 inventory 骨架。\n# min: 0 的组留空是合法拓扑(如单节点的 worker),不是漏填。\ninventory:\n  hosts:\n",
    );
    for h in &hosts {
        y.push_str(&format!(
            "    - {{ name: {}, address: 192.0.2.1, user: root, password: \"改成真口令,或改用 key:\" }}   # TODO 真实地址\n",
            h["name"].as_str().unwrap_or("n?")
        ));
    }
    y.push_str("  groups:\n");
    for (g, members) in &groups {
        if members.is_empty() {
            y.push_str(&format!("    {g}: {{ hosts: [] }}\n"));
        } else {
            y.push_str(&format!("    {g}: {{ hosts: [{}] }}\n", members.join(", ")));
        }
    }
    Json(json!({
        "inventory": { "hosts": hosts, "groups": groups },
        "yaml": y,
        "note": "按蓝图的 fleet.groups 生成;min: 0 的组留空是合法拓扑,不是漏填",
    }))
    .into_response()
}

/// `POST /api/blueprint/skeleton` —— 按所选类型生成**带注释**的蓝图骨架。
///
/// 注释全部来自登记表的 doc:这正是"UI 不硬编码任何字段"在生成物上的体现 ——
/// 加一个类型,骨架自动会写它的注释。
#[derive(Deserialize)]
pub struct SkeletonReq {
    pub name: String,
    #[serde(default)]
    pub types: Vec<String>,
}

pub async fn blueprint_skeleton(Json(req): Json<SkeletonReq>) -> impl IntoResponse {
    use crater_ir::types::{self, Req as FReq};
    let mut y = format!(
        "# {} —— 蓝图骨架(由类型登记表生成)。\n# 每个字段的注释来自 `crater types <类型>`;必填项已留 TODO。\nname: {}\nversion: \"1\"\n\nresources:\n",
        req.name, req.name
    );
    let mut missing: Vec<String> = Vec::new();
    for tname in &req.types {
        let Some(t) = types::builtin(tname) else {
            missing.push(tname.clone());
            continue;
        };
        y.push_str(&format!("\n  # ── {} —— {}\n", t.name, t.doc));
        y.push_str(&format!("  - {}:\n", t.name));
        for f in t.fields {
            let (mark, val) = match f.req {
                FReq::Required => ("必填", "TODO".to_string()),
                FReq::OneOf(g) => ("择一", format!("TODO(互斥组 {g},恰选一个)")),
                FReq::Optional => continue, // 可选项不进骨架 —— 骨架要短,字段卡负责发现
            };
            y.push_str(&format!("      {}: {}   # [{}] {}\n", f.name, val, mark, f.doc));
        }
    }
    Json(json!({ "yaml": y, "unknown_types": missing }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parse_error_line_is_extracted_for_the_editor() {
        assert_eq!(
            parse_error_line("YAML:did not find expected key at line 79 column 12"),
            Some(79)
        );
        // 抠不出来就别标 —— 标错行比不标更误导。
        assert_eq!(parse_error_line("something else entirely"), None);
    }

    #[test]
    fn the_catalog_covers_every_registered_type() {
        // UI 的字段面板完全由它驱动;少一个类型就等于 UI 里凭空少一种能力。
        let v = crater_ir::types::catalog_json();
        assert_eq!(v.as_array().unwrap().len(), crater_ir::types::BUILTINS.len());
    }

    #[test]
    fn every_field_carries_what_a_form_needs() {
        // 必选性、类型、枚举取值、互斥组、帮助文字 —— 缺一样表单就得猜。
        let v = crater_ir::types::catalog_json();
        for t in v.as_array().unwrap() {
            for f in t["fields"].as_array().unwrap() {
                assert!(f["name"].is_string(), "{t:?}");
                assert!(f["type"].is_string());
                assert!(f["required"].is_boolean());
                assert!(f["doc"].is_string());
            }
        }
    }

    #[test]
    fn unimplemented_types_are_flagged_for_the_ui() {
        // 让人在 UI 里选中一个类型、直到 apply 才撞墙,是最坏的发现方式。
        let v = crater_ir::types::catalog_json();
        for t in v.as_array().unwrap() {
            assert!(t["implemented"].is_boolean(), "{t:?}");
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────
// 阶段⑤:表单投影 —— 光标字段卡 与 span 级定点补丁。
//
// 两条端点共守一个纪律:**行级文本启发式,不是 YAML 解析器**。
// 解析器在文件坏一半时(编辑中的常态)什么都给不出;而"向上找资源头、
// 认出 `key:` 前缀"这种启发式,恰恰在半成品文本上最有用 —— 坏输入降级
// 成"不认识",永不报错(设计文档:坏输入降级)。

/// 光标行属于哪个资源类型:向上找缩进更浅的 `- <type>:` 资源头。
fn locate(lines: &[&str], cur: usize) -> Option<(String, Option<String>)> {
    let line = lines.get(cur)?;
    let indent = line.len() - line.trim_start().len();
    // 光标行本身:`- type:`(资源头)或 `field:`(字段行)。
    let head = |l: &str| -> Option<String> {
        let t = l.trim_start();
        let t = t.strip_prefix("- ")?;
        let (name, _) = t.split_once(':')?;
        (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .then(|| name.to_string())
    };
    let field = |l: &str| -> Option<String> {
        let t = l.trim_start().strip_prefix("- ").unwrap_or(l.trim_start());
        let (name, _) = t.split_once(':')?;
        (!name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_'))
            .then(|| name.to_string())
    };
    if let Some(ty) = head(line) {
        return Some((ty, None));
    }
    let f = field(line)?;
    // 向上扫:第一个缩进更浅的资源头就是所属类型;
    // 扫到顶级键(缩进 0 的 `resources:` 等)截止 —— 不跨区乱认。
    for i in (0..cur).rev() {
        let l = lines[i];
        if l.trim().is_empty() {
            continue;
        }
        let ind = l.len() - l.trim_start().len();
        if ind < indent {
            if let Some(ty) = head(l) {
                return Some((ty, Some(f)));
            }
            if ind == 0 {
                return None;
            }
        }
    }
    None
}

#[derive(Deserialize)]
pub struct CtxReq {
    pub text: String,
    /// 1 起数(与编辑器行号槽一致)。
    pub line: usize,
}

/// `POST /api/context` —— 光标处的字段卡(登记表投影到编辑现场)。
pub async fn context(Json(req): Json<CtxReq>) -> impl IntoResponse {
    let lines: Vec<&str> = req.text.lines().collect();
    let cur = req.line.saturating_sub(1);
    let Some((ty, field)) = locate(&lines, cur) else {
        return Json(json!({ "context": "none" }));
    };
    let Some(tj) = crater_ir::types::type_json(&ty) else {
        return Json(json!({
            "context": "custom",
            "type": ty,
            "note": "蓝图自定义类型(types: 段)—— 登记表不含它的字段卡",
        }));
    };
    match field {
        // 光标在资源头上:给类型卡(doc + 字段清单)。
        None => Json(json!({ "context": "type", "type": ty, "card": tj })),
        Some(f) => {
            let card = tj["fields"]
                .as_array()
                .and_then(|fs| fs.iter().find(|x| x["name"] == f.as_str()).cloned());
            match card {
                Some(c) => Json(json!({ "context": "field", "type": ty, "field": f, "card": c })),
                None => Json(json!({
                    "context": "unknown_field", "type": ty, "field": f,
                    "suggestion": crater_ir::types::suggest_field(&ty, &f),
                })),
            }
        }
    }
}

#[derive(Deserialize)]
pub struct PatchReq {
    pub text: String,
    pub line: usize,
    pub value: String,
}

/// `POST /api/patch` —— 单行 scalar 的定点补丁。
///
/// 只动 `key: value` 的 value 一段,行尾注释原样保留 —— 注释是这类文件
/// 一半的价值,表单化编辑绝不能吃掉它。碰到锚点/别名/flow 集合/块标量,
/// 拒绝并降级只读:那些形态一改就牵连别处,表单投影不该假装看得懂。
pub async fn patch(Json(req): Json<PatchReq>) -> axum::response::Response {
    let lines: Vec<&str> = req.text.lines().collect();
    let idx = req.line.saturating_sub(1);
    let Some(&line) = lines.get(idx) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "行号越界" }))).into_response();
    };
    let Some(colon) = line.find(':') else {
        return (StatusCode::CONFLICT, Json(json!({ "error": "这一行不是 `key: value`" }))).into_response();
    };
    let (prefix, rest) = line.split_at(colon + 1);
    let rest_trim = rest.trim_start();
    // 降级只读的形态:锚点(&)/别名(*)/flow 集合({ [)/块标量(| >)/空值(嵌套父键)。
    if rest_trim.is_empty()
        || matches!(rest_trim.chars().next(), Some('&' | '*' | '{' | '[' | '|' | '>'))
    {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "这个值不是简单标量(锚点/flow/块标量/嵌套)—— 请直接改文本" })),
        )
            .into_response();
    }
    // 行尾注释:带引号的值先跳过引号段再找 `#`;裸值找第一个 ` #`。
    let comment_at = if let Some(q) = rest_trim.chars().next().filter(|c| *c == '"' || *c == '\'') {
        rest_trim[1..]
            .find(q)
            .and_then(|close| rest_trim[1 + close + 1..].find('#').map(|h| 1 + close + 1 + h))
    } else {
        rest_trim.find(" #").map(|i| i + 1)
    };
    let comment = comment_at
        .map(|i| format!("   {}", rest_trim[i..].trim_end()))
        .unwrap_or_default();
    // 写回值:数字/布尔裸写;其余带引号 —— 引号永远合法,裸串则有一堆坑
    // (yes/no/on/off/1.0e3……),契约生成物宁可稳不可省。
    let v = req.value.trim();
    // 歧义裸词(YAML 1.1 的坑)必须引号;安全裸词(纯字母数字与 _-./)
    // 裸写 —— `state: started` 不该被写成 `state: "started"`。
    let ambiguous = matches!(
        v.to_ascii_lowercase().as_str(),
        "yes" | "no" | "on" | "off" | "null" | "~" | ""
    );
    let bare_safe = !v.is_empty()
        && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/'))
        && !v.starts_with('-');
    let rendered = if v.parse::<i64>().is_ok()
        || v.parse::<f64>().is_ok()
        || v == "true"
        || v == "false"
        || (bare_safe && !ambiguous)
    {
        v.to_string()
    } else {
        format!("\"{}\"", v.replace('\\', "\\\\").replace('"', "\\\""))
    };
    let leading = &rest[..rest.len() - rest_trim.len()];
    let new_line = format!("{prefix}{leading}{rendered}{comment}");
    let mut out: Vec<String> = lines.iter().map(|s| s.to_string()).collect();
    out[idx] = new_line;
    // 尾换行保真:原文有就保留。
    let mut text = out.join("\n");
    if req.text.ends_with('\n') {
        text.push('\n');
    }
    Json(json!({ "text": text, "line": req.line })).into_response()
}

#[cfg(test)]
mod projection_tests {
    use super::locate;

    const BP: &str = "name: demo\nversion: \"1\"\n\nresources:\n  - package:\n      packages:\n        debian: [nginx]\n  - service:\n      name: nginx   # 服务名\n      enabled: true\n";

    #[test]
    fn cursor_on_field_resolves_type_and_field() {
        let lines: Vec<&str> = BP.lines().collect();
        // L9 `name: nginx` 属于 service
        assert_eq!(
            locate(&lines, 8),
            Some(("service".into(), Some("name".into())))
        );
        // L6 `packages:` 属于 package
        assert_eq!(
            locate(&lines, 5),
            Some(("package".into(), Some("packages".into())))
        );
    }

    #[test]
    fn cursor_on_resource_head_gives_type_card() {
        let lines: Vec<&str> = BP.lines().collect();
        assert_eq!(locate(&lines, 4), Some(("package".into(), None)));
    }

    #[test]
    fn toplevel_keys_resolve_to_nothing() {
        let lines: Vec<&str> = BP.lines().collect();
        assert_eq!(locate(&lines, 0), None, "顶级键不该冒充字段");
    }
}
