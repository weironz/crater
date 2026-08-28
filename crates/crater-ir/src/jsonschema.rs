//! 从类型注册表生成 **JSON Schema**(draft 2020-12)。
//!
//! 这是可发现性的最后一步:`crater types` 让人**查得到**,schema 让人**写的时候就看得见** ——
//! 任何支持 yaml-language-server 的编辑器据此做补全、悬停说明与实时飘红。
//!
//! 与 [`crate::types`]、lint 报错同源一张表,所以三者永不矛盾。
//!
//! **自特化**:传入一份 blueprint 时,schema 会把它自己的东西枚举进去 ——
//! `material:` 位只补全**你声明过的**物料名,自定义 `types:` 作为新的判别键进入
//! resource 的 oneOf。也就是说 schema 不只懂"crater 有什么",还懂"你这份文件有什么"。
//!
//! 结构上的关键选择:资源条目用**判别键 oneOf**(`required: [service]` +
//! `additionalProperties: false`)。这正是"模块名即 key"这个语法决定换来的回报 ——
//! 编辑器一看见 `service:` 就知道接下来该补 `state`/`enabled`,拼错字段就地飘红。

use serde_json::{json, Map, Value};

use crate::ir::Blueprint;
use crate::types::{self, BuiltinType, Field, Kind, Ty};

/// 生成 schema。`bp` 为 `None` 时生成通用 schema(只含内建类型)。
pub fn generate(bp: Option<&Blueprint>) -> Value {
    let mut defs = Map::new();
    defs.insert("selector".into(), selector_def());
    defs.insert("condition".into(), condition_def());
    defs.insert("material_ref".into(), material_ref_def(bp));
    defs.insert("mode".into(), mode_def());
    defs.insert(
        "meta_each".into(),
        json!({ "description": "循环展开:列表字面量,或求值为列表的 CEL 表达式" }),
    );

    // 每个内建类型一个 $def;自定义类型同等对待 —— 它们在编辑器里应当没有二等感。
    for t in types::BUILTINS {
        defs.insert(format!("type_{}", t.name), type_def(t));
    }
    let custom: Vec<&str> = bp
        .map(|b| b.types.iter().map(|t| t.name.as_str()).collect())
        .unwrap_or_default();
    for name in &custom {
        defs.insert(format!("type_{name}"), custom_type_def(name));
    }

    defs.insert("resource".into(), entry_def(Some(Kind::Resource), &custom));
    defs.insert("step".into(), entry_def(None, &custom)); // 步骤位:任何类型都可能出现
    defs.insert("probe".into(), entry_def(Some(Kind::Probe), &[]));

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": bp.map(|b| format!("crater blueprint · {}", b.name))
                   .unwrap_or_else(|| "crater blueprint".into()),
        "type": "object",
        "required": ["name"],
        "additionalProperties": false,
        "properties": top_level_properties(),
        "$defs": Value::Object(defs),
    })
}

fn top_level_properties() -> Value {
    json!({
        "name":        { "type": "string", "description": "蓝图名(制品标识)" },
        "version":     { "type": "string", "description": "蓝图自身版本号" },
        "description": { "type": "string", "description": "一句话说明" },
        "requires":    { "type": "object", "description": "环境准入:支持的 distro/version/arch" },
        "params":      { "type": "object", "description": "类型化参数契约",
                         "additionalProperties": param_def() },
        "materials":   { "type": "array",  "description": "离线闭包物料", "items": material_def() },
        "preflight":   { "type": "array",  "description": "只读准入断言:任一失败则整个部署不开始" },
        "types":       { "type": "array",  "description": "自定义资源类型(L2):探针 + procedure 补齐五动词" },
        "resources":   { "type": "array",  "description": "期望态资源,按声明序收敛",
                         "items": { "$ref": "#/$defs/resource" } },
        "procedures":  { "type": "object", "description": "机群级工作流(舞):名 → 步骤列表",
                         "additionalProperties": {
                             "type": "object",
                             "properties": {
                                 "params": { "type": "object" },
                                 "steps":  { "type": "array", "items": { "$ref": "#/$defs/step" } }
                             }
                         } },
        "health":      { "type": "array",  "description": "只读健康探针:verify 与漂移检测的依据",
                         "items": { "$ref": "#/$defs/probe" } },
    })
}

/// 一个条目 = 判别类型键 + 元字段。这是"模块名即 key"换来的补全能力。
fn entry_def(kind: Option<Kind>, custom: &[&str]) -> Value {
    let mut branches: Vec<Value> = types::BUILTINS
        .iter()
        .filter(|t| kind.is_none_or(|k| t.kind == k))
        .map(|t| branch(t.name, t.freeform.is_some()))
        .collect();
    // `cmd` 是**双位置**类型:动作位用 argv+flags,探针位用 run(见其注册表 note)。
    // 按 Kind 单一归类会把 `health: [- cmd: {...}]` 误判成非法 —— 真实 blueprint 实测踩到。
    if kind == Some(Kind::Probe) {
        if let Some(t) = types::builtin("cmd") {
            branches.push(branch(t.name, t.freeform.is_some()));
        }
    }
    // 自定义类型只出现在资源位(它们的弥合是机群级的舞)。
    if kind != Some(Kind::Probe) {
        branches.extend(custom.iter().map(|n| branch(n, false)));
    }
    json!({ "oneOf": branches })
}

fn branch(type_name: &str, freeform: bool) -> Value {
    let body = if freeform {
        // 短写法:`- shell: "cmd"` 与 map 形式等价,schema 两者都收。
        json!({ "oneOf": [ { "$ref": format!("#/$defs/type_{type_name}") },
                           { "type": ["string", "number", "boolean", "array"] } ] })
    } else {
        json!({ "$ref": format!("#/$defs/type_{type_name}") })
    };
    let mut props = Map::new();
    props.insert(type_name.into(), body);
    for (k, v) in meta_fields() {
        props.insert(k, v);
    }
    json!({
        "type": "object",
        "required": [type_name],
        // 关键:未知字段就地飘红,而不是等到 lint 或 apply。
        "additionalProperties": false,
        "properties": Value::Object(props),
    })
}

/// 所有条目通用的元字段。
fn meta_fields() -> Vec<(String, Value)> {
    vec![
        ("name".into(), json!({ "type": "string", "description": "人类标签(纯注释,不参与语义)" })),
        ("id".into(), json!({ "type": "string", "description": "显式 id;不写则按类型自动编号" })),
        ("on".into(), json!({ "$ref": "#/$defs/selector" })),
        ("when".into(), json!({ "$ref": "#/$defs/condition" })),
        ("each".into(), json!({ "$ref": "#/$defs/meta_each" })),
        ("deps".into(), json!({ "type": "array", "items": { "type": "string" },
                                "description": "显式依赖;默认按声明顺序建边" })),
        ("exports".into(), json!({ "type": "object",
                                   "description": "步骤位:导出跨主机 fact(名 → 取值命令)" })),
        ("strategy".into(), json!({ "type": "object",
                                    "description": "步骤位:throttle / retries / ignore_errors",
                                    "additionalProperties": false,
                                    "properties": { "throttle": { "type": "integer", "minimum": 1 },
                                                    "retries": { "type": "integer", "minimum": 0 },
                                                    "ignore_errors": { "type": "boolean" } } })),
        ("timeout".into(), json!({ "type": ["string", "integer"], "description": "探针位:超时" })),
    ]
}

/// 某个内建类型的字段体。说明文字直接来自注册表 —— 编辑器悬停即可读到。
fn type_def(t: &BuiltinType) -> Value {
    let mut props = Map::new();
    for f in t.fields {
        props.insert(f.name.into(), field_def(f));
    }
    let mut obj = Map::new();
    obj.insert("type".into(), json!("object"));
    obj.insert("description".into(), json!(describe(t)));
    obj.insert("additionalProperties".into(), json!(false));
    obj.insert("properties".into(), Value::Object(props));

    let required = t.required();
    if !required.is_empty() {
        obj.insert("required".into(), json!(required));
    }
    // 互斥组 → 每组一个 oneOf(恰择其一)。
    let groups = t.one_of_groups();
    if !groups.is_empty() {
        let all: Vec<Value> = groups
            .iter()
            .map(|(_, members)| {
                json!({ "oneOf": members.iter()
                                        .map(|m| json!({ "required": [m] }))
                                        .collect::<Vec<_>>() })
            })
            .collect();
        obj.insert("allOf".into(), Value::Array(all));
    }
    Value::Object(obj)
}

/// 自定义类型:参数由作者的 `args:` 契约管,这里不锁死字段。
fn custom_type_def(name: &str) -> Value {
    json!({
        "type": "object",
        "description": format!("{name} —— 本蓝图自定义类型(L2);字段见其 `types:` 里的 args 契约"),
    })
}

fn field_def(f: &Field) -> Value {
    let mut obj = Map::new();
    obj.insert("description".into(), json!(f.doc));
    match f.ty {
        Ty::Enum => {
            obj.insert("enum".into(), json!(f.values));
        }
        Ty::Mode => {
            obj.insert("$ref".into(), json!("#/$defs/mode"));
        }
        Ty::Material => {
            obj.insert("$ref".into(), json!("#/$defs/material_ref"));
        }
        Ty::Int => {
            obj.insert("type".into(), json!("integer"));
        }
        Ty::Bool => {
            obj.insert("type".into(), json!("boolean"));
        }
        Ty::List => {
            obj.insert("type".into(), json!("array"));
        }
        Ty::Map => {
            obj.insert("type".into(), json!("object"));
        }
        Ty::Str | Ty::Path => {
            obj.insert("type".into(), json!("string"));
        }
    }
    Value::Object(obj)
}

fn describe(t: &BuiltinType) -> String {
    let mut s = format!("{} — {}", t.name, t.doc);
    if let Some(note) = t.note {
        s.push_str("\n\n");
        s.push_str(note);
    }
    s
}

// ---------------------------------------------------------------- 共用 $defs

fn selector_def() -> Value {
    json!({
        "type": "string",
        "description": "定址:all | role.<组> | host.<名> | first(<sel>) | rest(<sel>) | <sel> where <CEL>",
        "examples": ["all", "role.controlplane", "first(role.controlplane)", "rest(role.controlplane)"],
    })
}

fn condition_def() -> Value {
    json!({
        "type": "string",
        "description": "条件(CEL 布尔表达式)。可用:params.* / substrate.* / facts.* / item\n\
                        注意:值位置的 ${} 只许纯引用 —— 条件写在这里,不要把三元塞进字符串",
        "examples": ["params.ha", "has(params.cp_endpoint)", "substrate.arch == 'amd64'"],
    })
}

/// 自特化的核心:枚举**本蓝图声明过的**物料名。
fn material_ref_def(bp: Option<&Blueprint>) -> Value {
    let mut names: Vec<&str> = bp
        .map(|b| b.materials.iter().map(|m| m.name.as_str()).collect())
        .unwrap_or_default();
    names.sort_unstable();
    names.dedup();
    if names.is_empty() {
        return json!({ "type": "string", "description": "materials: 里声明的物料名" });
    }
    // 枚举**加**插值兜底:`each:` 展开时写 `material: "${item}"` 完全合法,
    // 只枚举字面名会把这种正当写法误判成错误(真实 blueprint 实测踩到)。
    json!({
        "description": "materials: 里声明的物料名(或求值到物料名的插值)",
        "anyOf": [
            { "type": "string", "enum": names },
            { "type": "string", "pattern": "\\$\\{" }
        ],
    })
}

fn mode_def() -> Value {
    json!({
        "type": "string",
        // 不带引号会被 YAML 当十进制整数 —— 经典脚枪,schema 层就挡掉。
        "pattern": "^0[0-7]{3,4}$",
        "description": "权限:必须是带引号的八进制字符串,如 \"0755\"",
        "examples": ["0644", "0755", "0600"],
    })
}

/// 编辑器接入用的头注释。
pub fn language_server_hint(schema_path: &str) -> String {
    format!("# yaml-language-server: $schema={schema_path}")
}

fn param_def() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "type": { "description": "string|int|bool|ip|cidr|port|version|[T]|{enum:[…]}" },
            "default": { "description": "默认值;有 default 即非必填" },
            "required": { "type": "boolean" },
            "secret": { "type": "boolean", "description": "孪生视图/日志/API 自动打码" },
            "stage": { "enum": ["build", "deploy"],
                       "description": "build 期烤进制品;deploy 期部署时给" },
            "desc": { "type": "string" },
            "description": { "type": "string" },
        }
    })
}

fn material_def() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["name"],
        "properties": {
            "name":   { "type": "string" },
            "file":   { "type": "string", "description": "远端 URL,或随蓝图走的本地相对路径" },
            "image":  { "type": "string", "description": "容器镜像 ref" },
            "os_package": { "description": "OS 包闭包" },
            "sha256": { "type": "string", "pattern": "^[0-9a-f]{64}$",
                        "description": "内容摘要;有它 plan 期才能内容寻址比对" },
            "unzip":  { "type": "string", "description": "下载物是 zip 时取其中这个成员" },
            "when":   { "$ref": "#/$defs/condition" },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::blueprint_from_str;

    fn generic() -> Value {
        generate(None)
    }

    fn defs(schema: &Value) -> &Map<String, Value> {
        schema["$defs"].as_object().unwrap()
    }

    #[test]
    fn every_catalog_type_gets_a_definition_and_a_branch() {
        let s = generic();
        for t in types::BUILTINS {
            assert!(defs(&s).contains_key(&format!("type_{}", t.name)), "{} 缺 $def", t.name);
        }
        // 资源位的分支数 = 资源类型数(探针与过程性原语不在 resources 里补全)
        let branches = s["$defs"]["resource"]["oneOf"].as_array().unwrap();
        let resources = types::BUILTINS.iter().filter(|t| t.kind == Kind::Resource).count();
        assert_eq!(branches.len(), resources);
    }

    #[test]
    fn a_branch_is_keyed_on_the_type_name_and_rejects_unknown_fields() {
        // 判别键 oneOf 正是"模块名即 key"换来的补全能力。
        let s = generic();
        let branch = s["$defs"]["resource"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["required"][0] == "service")
            .expect("service 分支");
        assert_eq!(branch["additionalProperties"], json!(false), "未知字段要就地飘红");
        let props = branch["properties"].as_object().unwrap();
        assert!(props.contains_key("service") && props.contains_key("on") && props.contains_key("when"));
    }

    #[test]
    fn field_metadata_reaches_the_schema() {
        let s = generic();
        let svc = &s["$defs"]["type_service"];
        assert_eq!(svc["required"], json!(["name"]));
        assert_eq!(svc["properties"]["state"]["enum"], json!(["started", "stopped", "restarted"]));
        // 说明文字直接来自注册表 —— 编辑器悬停即可读到
        assert!(svc["properties"]["enabled"]["description"].as_str().unwrap().contains("开机自启"));
        assert!(svc["description"].as_str().unwrap().contains("传导"), "note 应进 description");
    }

    #[test]
    fn mutually_exclusive_groups_become_one_of_constraints() {
        let s = generic();
        let all = s["$defs"]["type_copy"]["allOf"].as_array().unwrap();
        assert_eq!(all.len(), 1, "copy 只有一个互斥组");
        assert_eq!(all[0]["oneOf"].as_array().unwrap().len(), 3, "content / src / material");
    }

    #[test]
    fn mode_is_pinned_to_quoted_octal() {
        // 不带引号会被 YAML 当十进制整数 —— 经典脚枪,schema 层就挡掉。
        let s = generic();
        assert_eq!(s["$defs"]["mode"]["pattern"], json!("^0[0-7]{3,4}$"));
        assert_eq!(s["$defs"]["type_file"]["properties"]["mode"]["$ref"], json!("#/$defs/mode"));
    }

    #[test]
    fn freeform_types_accept_both_shorthand_and_map_form() {
        let s = generic();
        let shell = s["$defs"]["step"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["required"][0] == "shell")
            .expect("shell 分支");
        assert!(shell["properties"]["shell"]["oneOf"].is_array(), "短写法与 map 都要收");
    }

    #[test]
    fn a_generic_schema_leaves_material_names_open() {
        let s = generic();
        assert!(s["$defs"]["material_ref"]["anyOf"].is_null(), "没有蓝图就无从枚举");
        assert_eq!(s["$defs"]["material_ref"]["type"], json!("string"));
    }

    #[test]
    fn a_material_position_still_accepts_an_interpolation() {
        // `each:` 展开时 `material: "${item}"` 是正当写法 ——
        // 只枚举字面名会把它误判成错误(真实 blueprint 实测踩到)。
        let bp = blueprint_from_str(
            "name: t\nmaterials:\n  - { name: kubeadm, file: \"https://x\" }\n",
        )
        .unwrap();
        let s = generate(Some(&bp));
        let alts = s["$defs"]["material_ref"]["anyOf"].as_array().unwrap();
        assert_eq!(alts.len(), 2, "枚举 + 插值兜底");
        assert!(alts.iter().any(|a| a["enum"].is_array()));
        assert!(alts.iter().any(|a| a["pattern"].as_str() == Some(r"\$\{")));
    }

    #[test]
    fn cmd_is_available_in_probe_positions_too() {
        // cmd 是双位置类型:动作位 argv+flags,探针位 run。
        // 按 Kind 单一归类会把 `health: [- cmd: {...}]` 误判成非法。
        let s = generic();
        let in_probe = s["$defs"]["probe"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["required"][0] == "cmd");
        assert!(in_probe, "health: 段就用 cmd");
    }

    #[test]
    fn self_specialisation_enumerates_this_blueprints_own_things() {
        // schema 不只懂"crater 有什么",还懂"你这份文件有什么"。
        let bp = blueprint_from_str(
            r#"
name: demo
materials:
  - { name: caddy-bin, file: "https://ex.com/caddy" }
  - { name: cfg, file: files/caddy.conf }
types:
  - name: cluster_member
    observe: { cmd: "test -f /x" }
    apply: boot
procedures:
  boot:
    steps: []
resources:
  - copy: { material: cfg, dest: /etc/x }
"#,
        )
        .unwrap();
        let s = generate(Some(&bp));

        assert_eq!(s["$defs"]["material_ref"]["anyOf"][0]["enum"], json!(["caddy-bin", "cfg"]));
        assert!(s["title"].as_str().unwrap().contains("demo"));
        // 自定义类型进入资源位的 oneOf —— 在编辑器里不该有二等感
        let has_custom = s["$defs"]["resource"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["required"][0] == "cluster_member");
        assert!(has_custom, "自定义类型应可被补全");
    }

    #[test]
    fn custom_types_never_appear_in_probe_positions() {
        // 它们的弥合是机群级的舞,不是只读探针。
        let bp = blueprint_from_str(
            "name: t\ntypes:\n  - name: thing\n    observe: { cmd: \"test -f /x\" }\n    apply: b\nprocedures:\n  b:\n    steps: []\n",
        )
        .unwrap();
        let s = generate(Some(&bp));
        let in_probe = s["$defs"]["probe"]["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .any(|b| b["required"][0] == "thing");
        assert!(!in_probe);
    }

    #[test]
    fn the_document_is_a_valid_2020_12_schema_shell() {
        let s = generic();
        assert_eq!(s["$schema"], json!("https://json-schema.org/draft/2020-12/schema"));
        assert_eq!(s["additionalProperties"], json!(false), "顶层未知键也要飘红");
        assert_eq!(s["required"], json!(["name"]));
        for key in ["resources", "procedures", "params", "materials", "health", "types"] {
            assert!(s["properties"][key].is_object(), "顶层缺 {key}");
        }
    }

    #[test]
    fn the_editor_hint_is_a_yaml_comment() {
        let hint = language_server_hint(".crater/schema.json");
        assert!(hint.starts_with("# yaml-language-server: $schema="), "{hint}");
    }
}
