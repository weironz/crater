//! YAML 前端 —— 把作者写的 YAML 编译成 [`Blueprint`]。
//!
//! 核心规则一句话:**一条声明 = 若干"步骤关键字" + 恰好一个"模块名 key"**。
//!
//! ```yaml
//! - copy: { material: bin, dest: /usr/local/bin/rustfs, mode: "0755" }   # 模块名即 key
//!   on: role.controlplane                                                 # 步骤关键字
//!   when: params.enable_lb
//! ```
//!
//! 这条规则换来两件旧模型做不到的事:
//! 1. 少一行 `action:` 内部标签,且模块参数与步骤关键字在**视觉上分层**;
//! 2. **未知 key 在解析期就报错**(带拼写建议),而不是 Ansible 那样运行到才炸。

use crate::expr::{CelExpr, Template};
use crate::ir::*;
use crate::schema::{ParamSpec, ParamType, Params, Stage};
use crate::selector::Selector;
use crate::{Error, Result};
use serde_yaml::Value as Y;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// 资源条目允许的步骤关键字(其余 key 视为模块名)。
const RESOURCE_KEYWORDS: &[&str] = &["id", "name", "target", "when", "each", "deps"];
/// procedure 步骤额外允许的关键字。
const STEP_KEYWORDS: &[&str] = &[
    "id", "name", "target", "when", "each", "deps", "exports", "strategy",
];

pub fn blueprint_from_str(text: &str) -> Result<Blueprint> {
    let mut bp = parse_structure(text)?;
    assign_lines(&mut bp, &crate::loc::LineIndex::new(text));
    Ok(bp)
}

/// 可外置的顶层节(A1)。清单封闭 —— 能拆的就这几段,不接受任意划分。
pub const SPLITTABLE: &[&str] = &[
    "resources",
    "procedures",
    "types",
    "materials",
    "health",
    "preflight",
];

/// 从文件加载,并按根文件的 `parts:` 声明合并同目录约定文件(A1)。
///
/// 与被拒绝的 `include` 的**本质区别**:文件名由约定钉死(`<stem>.<节名>.yaml`)、
/// 无自由路径、无嵌套、无参数、无条件 —— 合并结果与把内容写在一个文件里**逐字节等价**。
/// 拒绝的是 Ansible 那种"三级跳读",不是拒绝多文件。
pub fn blueprint_from_path(path: &Path) -> Result<Blueprint> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::parse(format!("读 {}:{e}", path.display())))?;
    let merged = merge_parts(path, &text)?;
    blueprint_from_str(&merged)
}

/// 蓝图文件的 stem:`k8s.blueprint.yaml` → `k8s`(去掉 `.blueprint` 与扩展名)。
fn blueprint_stem(path: &Path) -> String {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    name.strip_suffix(".yaml")
        .or_else(|| name.strip_suffix(".yml"))
        .unwrap_or(name)
        .strip_suffix(".blueprint")
        .map(str::to_string)
        .unwrap_or_else(|| {
            name.strip_suffix(".yaml")
                .or_else(|| name.strip_suffix(".yml"))
                .unwrap_or(name)
                .to_string()
        })
}

/// 某个外置节的约定路径。**无自由度** —— 这是它与 include 的分界线。
pub fn part_path(root: &Path, section: &str) -> PathBuf {
    let dir = root.parent().unwrap_or(Path::new("."));
    dir.join(format!("{}.{section}.yaml", blueprint_stem(root)))
}

/// 读根文件的 `parts:`,把外置节并回一个文档。
fn merge_parts(path: &Path, text: &str) -> Result<String> {
    let root: Y = serde_yaml::from_str(text)?;
    let Some(m) = root.as_mapping() else {
        return Ok(text.to_string());
    };
    let declared: Vec<String> = match m.get(Y::from("parts")) {
        None | Some(Y::Null) => Vec::new(),
        Some(Y::Sequence(items)) => items.iter().map(scalar_to_string).collect(),
        Some(_) => return Err(Error::parse("`parts:` 应是列表")),
    };

    // 目录里存在约定名的文件却没声明 → 幽灵文件,静默不生效是最难查的一类问题。
    if let Some(dir) = path.parent() {
        for section in SPLITTABLE {
            let candidate = part_path(path, section);
            if candidate.exists() && !declared.iter().any(|d| d == section) {
                return Err(Error::parse(format!(
                    "E122 {} 存在,但根文件的 `parts:` 没声明 `{section}` —— \
                     它不会生效。要么加进 parts,要么删掉它",
                    candidate.strip_prefix(dir).unwrap_or(&candidate).display()
                )));
            }
        }
    }

    if declared.is_empty() {
        return Ok(text.to_string());
    }

    let mut out = root.clone();
    let out_map = out.as_mapping_mut().expect("checked");
    out_map.remove(Y::from("parts"));

    for section in &declared {
        if !SPLITTABLE.contains(&section.as_str()) {
            let hint = closest(section, SPLITTABLE)
                .map(|s| format!(",是不是想写 `{s}`?"))
                .unwrap_or_default();
            return Err(Error::parse(format!(
                "`parts:` 不能外置 `{section}`{hint}(可外置:{})",
                SPLITTABLE.join(", ")
            )));
        }
        // 同一节内联与外置二选一 —— 双定义时谁赢都是猜,不如拒绝。
        if m.contains_key(Y::from(section.as_str())) {
            return Err(Error::parse(format!(
                "E121 `{section}` 既写在根文件里,又声明为外置 part —— 二选一"
            )));
        }
        let part = part_path(path, section);
        let body = std::fs::read_to_string(&part).map_err(|_| {
            Error::parse(format!(
                "E120 `parts:` 声明了 `{section}`,但找不到 {} —— \
                 part 文件名由约定钉死:`<根文件 stem>.<节名>.yaml`",
                part.display()
            ))
        })?;
        let value: Y = serde_yaml::from_str(&body)
            .map_err(|e| Error::parse(format!("{}:{e}", part.display())))?;
        // part 文件的顶层**就是**该节的内容;不得再套一层 parts(无嵌套)。
        if let Some(pm) = value.as_mapping() {
            if pm.contains_key(Y::from("parts")) {
                return Err(Error::parse(format!(
                    "{}:part 文件不得再含 `parts:` —— 外置只有一层",
                    part.display()
                )));
            }
        }
        out_map.insert(Y::from(section.as_str()), value);
    }
    serde_yaml::to_string(&out).map_err(Into::into)
}

/// 把解析期记下的**序号**换成源码**行号**,让诊断可点击(`file.yaml:42`)。
fn assign_lines(bp: &mut Blueprint, idx: &crate::loc::LineIndex) {
    fn put(slot: &mut Option<usize>, lines: &[usize]) {
        // 解析期存的是序号;越界(极少数畸形缩进)时宁可留空也不报错行。
        *slot = slot.and_then(|i| lines.get(i).copied());
    }
    let res = idx.list_items(&["resources"]);
    bp.resources.iter_mut().for_each(|r| put(&mut r.line, &res));
    let mats = idx.list_items(&["materials"]);
    bp.materials
        .iter_mut()
        .for_each(|m| put(&mut m.line, &mats));
    let pre = idx.list_items(&["preflight"]);
    bp.preflight.iter_mut().for_each(|a| put(&mut a.line, &pre));
    let health = idx.list_items(&["health"]);
    bp.health.iter_mut().for_each(|h| put(&mut h.line, &health));
    for (name, proc) in bp.procedures.iter_mut() {
        let steps = idx.list_items(&["procedures", name, "steps"]);
        proc.steps
            .iter_mut()
            .for_each(|st| put(&mut st.line, &steps));
    }
}

fn parse_structure(text: &str) -> Result<Blueprint> {
    let root: Y = serde_yaml::from_str(text)?;
    let m = root
        .as_mapping()
        .ok_or_else(|| Error::parse("blueprint 顶层应是一个 map"))?;

    known_keys(
        m,
        &[
            "name",
            "version",
            "description",
            "params",
            "requires",
            "materials",
            "preflight",
            "types",
            "resources",
            "procedures",
            "health",
            "parts",
            "fleet",
            "cast",
            "facts",
        ],
        "blueprint",
    )?;

    let name = get_str(m, "name").ok_or_else(|| Error::parse("blueprint 缺少 `name:`"))?;
    let params = parse_params(m.get(Y::from("params")))?;
    let fleet = parse_fleet(m.get(Y::from("fleet")))?;
    // cast 必须先解析:后面所有 selector 都可能引用它。
    let cast = parse_cast(m.get(Y::from("cast")), &fleet)?;

    Ok(Blueprint {
        name,
        version: get_str(m, "version"),
        description: get_str(m, "description"),
        params,
        requires: parse_requires(m.get(Y::from("requires")))?,
        materials: parse_materials(m.get(Y::from("materials")))?,
        preflight: parse_preflight(m.get(Y::from("preflight")), &cast)?,
        facts: parse_derived_facts(m.get(Y::from("facts")))?,
        types: parse_types(m.get(Y::from("types")))?,
        resources: parse_resources(m.get(Y::from("resources")), &cast)?,
        procedures: parse_procedures(m.get(Y::from("procedures")), &cast)?,
        health: parse_health(m.get(Y::from("health")), &cast)?,
        fleet,
        cast,
    })
}

// ---------------------------------------------------------------- 条目解析

/// 从一个 map 条目里拆出 (模块名, 参数, 关键字表)。
fn split_entry<'a>(
    m: &'a serde_yaml::Mapping,
    keywords: &[&str],
    what: &str,
) -> Result<(String, Args, BTreeMap<String, &'a Y>)> {
    let mut kw = BTreeMap::new();
    let mut modules: Vec<(String, &Y)> = Vec::new();

    for (k, v) in m {
        // 与 known_keys 同一条纪律:`on` 在 YAML 1.1 里是布尔,不能静默当成模块名。
        if k.as_bool() == Some(true) || k.as_str() == Some("on") {
            return Err(Error::parse(format!(
                "{what}:`on:` 已改名为 `target:` —— `on` 在 YAML 1.1 里是布尔 `true`,\
                 某些工具(PyYAML / 部分 CI 与编辑器)会把这个键读成 `true:`"
            )));
        }
        let key = k
            .as_str()
            .ok_or_else(|| Error::parse(format!("{what}:key 必须是字符串")))?;
        if keywords.contains(&key) {
            kw.insert(key.to_string(), v);
        } else {
            modules.push((key.to_string(), v));
        }
    }

    match modules.len() {
        1 => {
            let (ty, raw) = modules.pop().expect("len checked");
            let args = module_args(&ty, raw)?;
            Ok((ty, args, kw))
        }
        0 => Err(Error::parse(format!(
            "{what}:没有模块名 —— 一条声明必须恰好有一个模块 key(如 `copy:` / `service:`)"
        ))),
        _ => {
            let names: Vec<&str> = modules.iter().map(|(n, _)| n.as_str()).collect();
            let hint = names
                .iter()
                .filter_map(|n| {
                    crate::types::suggest(n).map(|s| format!("`{n}` 是不是想写 `{s}`?"))
                })
                .collect::<Vec<_>>()
                .join(" ");
            Err(Error::parse(format!(
                "{what}:一条声明里出现了 {} 个模块 key({})——只允许一个;\
                 模块参数应写在该模块的 map 里(如 `shell: {{cmd: …, check: …}}`)。{hint}",
                names.len(),
                names.join(", ")
            )))
        }
    }
}

/// 模块参数:map 形式,或自由形式短写法(`- shell: "cmd"`)。
fn module_args(ty: &str, raw: &Y) -> Result<Args> {
    match raw {
        Y::Mapping(m) => {
            let mut args = Args::new();
            for (k, v) in m {
                let key = k
                    .as_str()
                    .ok_or_else(|| Error::parse(format!("模块 `{ty}`:参数名必须是字符串")))?;
                args.insert(key.to_string(), value_from_yaml(v)?);
            }
            Ok(args)
        }
        Y::Null => Ok(Args::new()),
        scalar_or_seq => {
            let field = crate::types::builtin(ty)
                .and_then(|b| b.freeform)
                .ok_or_else(|| {
                    Error::parse(format!(
                        "模块 `{ty}` 没有自由形式短写法,请写成 map:`{ty}: {{…}}`"
                    ))
                })?;
            let mut args = Args::new();
            args.insert(field.to_string(), value_from_yaml(scalar_or_seq)?);
            Ok(args)
        }
    }
}

/// 选角表:名字 → selector 串。
pub type Cast = BTreeMap<String, Selector>;

/// 解析 selector 字符串,先查选角表。
///
/// 展开发生在**解析期**:`target: seed` 直接变成 `first(role.controlplane)`,
/// 运行期零成本;拼错的角色名当场报错并给建议,而不是等到求值时说"没有这个组"。
fn resolve_selector(src: &str, cast: &Cast) -> Result<Selector> {
    if let Some(sel) = cast.get(src.trim()) {
        return Ok(sel.clone());
    }
    Selector::parse(src).map_err(|e| {
        // 看起来像个光秃秃的名字 → 多半是想引用选角表却拼错了。
        let hint = if !src.contains('.') && !src.contains('(') && !cast.is_empty() {
            closest(
                src.trim(),
                &cast.keys().map(String::as_str).collect::<Vec<_>>(),
            )
            .map(|c| format!(",是不是想写 cast 里的 `{c}`?"))
            .unwrap_or_default()
        } else {
            String::new()
        };
        Error::parse(format!("`target:` {e}{hint}"))
    })
}

fn parse_cast(v: Option<&Y>, fleet: &FleetContract) -> Result<Cast> {
    let m = match v {
        None | Some(Y::Null) => return Ok(Cast::new()),
        Some(Y::Mapping(m)) => m,
        Some(_) => return Err(Error::parse("`cast:` 应是 map(角色名 → selector)")),
    };
    let mut out = Cast::new();
    for (k, val) in m {
        let name = scalar_to_string(k);
        let src = scalar_to_string(val);
        // 选角表自己不能套选角表 —— 一层间接已经够,两层就开始要跳读了。
        let sel = Selector::parse(&src).map_err(|e| Error::parse(format!("cast `{name}`:{e}")))?;
        // 引用了 fleet 没声明的组 → 当场报错。这是契约存在的意义。
        if !fleet.groups.is_empty() {
            for role in sel.roles() {
                if !fleet.groups.contains_key(role) {
                    let hint = closest(
                        role,
                        &fleet.groups.keys().map(String::as_str).collect::<Vec<_>>(),
                    )
                    .map(|c| format!(",是不是 `{c}`?"))
                    .unwrap_or_default();
                    return Err(Error::parse(format!(
                        "cast `{name}` 引用了组 `{role}`,但 `fleet.groups` 没声明它{hint}"
                    )));
                }
            }
        }
        out.insert(name, sel);
    }
    Ok(out)
}

fn parse_fleet(v: Option<&Y>) -> Result<FleetContract> {
    let m = match v {
        None | Some(Y::Null) => return Ok(FleetContract::default()),
        Some(Y::Mapping(m)) => m,
        Some(_) => return Err(Error::parse("`fleet:` 应是 map")),
    };
    known_keys(m, &["groups"], "fleet")?;
    let mut groups = BTreeMap::new();
    if let Some(Y::Mapping(gm)) = m.get(Y::from("groups")) {
        for (k, val) in gm {
            let name = scalar_to_string(k);
            let body = val
                .as_mapping()
                .ok_or_else(|| Error::parse(format!("fleet.groups.{name}:应是 map")))?;
            known_keys(body, &["min"], &format!("fleet.groups.{name}"))?;
            groups.insert(
                name,
                GroupContract {
                    min: body
                        .get(Y::from("min"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(1) as usize,
                },
            );
        }
    }
    Ok(FleetContract { groups })
}

/// 解析 `cmd.flags:` —— 有序条目,条件是条目的属性。
///
/// `name` **禁止插值**:这是 lint 能静态枚举命令全部展开形态的前提。
/// 一旦允许 `name: "--${params.x}"`,展开集合就依赖运行期取值,plan 也就说不清
/// "哪些 flag 可能出现"。
pub fn parse_flags(v: &Y) -> Result<Vec<Flag>> {
    let Y::Sequence(items) = v else {
        return Err(Error::parse("`flags:` 应是列表"));
    };
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let m = item
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("flags[{i}]:应是 map")))?;
        known_keys(m, &["name", "value", "when"], &format!("flags[{i}]"))?;
        let name =
            get_str(m, "name").ok_or_else(|| Error::parse(format!("flags[{i}]:缺少 `name:`")))?;
        if name.contains("${") {
            return Err(Error::parse(format!(
                "flags[{i}] `name: {name}`:flag 名禁止插值 —— \
                 否则 lint 无法静态枚举命令的全部展开形态,plan 也说不清哪些 flag 会出现。\
                 要按条件决定是否出现,请用 `when:`"
            )));
        }
        out.push(Flag {
            name,
            value: match m.get(Y::from("value")) {
                Some(val) => Some(value_from_yaml(val)?),
                None => None,
            },
            when: match m.get(Y::from("when")) {
                Some(w) => Some(
                    CelExpr::compile(&scalar_to_string(w))
                        .map_err(|e| Error::parse(format!("flags[{i}] `when:` {e}")))?,
                ),
                None => None,
            },
        });
    }
    Ok(out)
}

/// YAML 值 → IR 值:字符串扫描 `${}` 编译成模板,容器递归。
pub fn value_from_yaml(v: &Y) -> Result<Value> {
    Ok(match v {
        Y::String(s) => {
            let t = Template::parse(s).map_err(|e| Error::parse(format!("`{s}`:{e}")))?;
            if t.is_dynamic() {
                Value::Tmpl(t)
            } else {
                Value::Lit(v.clone())
            }
        }
        Y::Sequence(items) => Value::List(
            items
                .iter()
                .map(value_from_yaml)
                .collect::<Result<Vec<_>>>()?,
        ),
        Y::Mapping(m) => {
            let mut out = BTreeMap::new();
            for (k, val) in m {
                out.insert(scalar_to_string(k), value_from_yaml(val)?);
            }
            Value::Map(out)
        }
        other => Value::Lit(other.clone()),
    })
}

/// 每条声明共有的"步骤关键字"解析结果。
struct Common {
    id: Option<String>,
    name: Option<String>,
    on: Selector,
    when: Option<CelExpr>,
    each: Option<Each>,
    deps: Vec<String>,
}

fn parse_common(kw: &BTreeMap<String, &Y>, cast: &Cast) -> Result<Common> {
    let id = kw.get("id").map(|v| scalar_to_string(v));
    let name = kw.get("name").map(|v| scalar_to_string(v));
    let on = match kw.get("target") {
        Some(v) => resolve_selector(&scalar_to_string(v), cast)?,
        None => Selector::All,
    };
    let when = match kw.get("when") {
        Some(v) => Some(
            CelExpr::compile(&scalar_to_string(v))
                .map_err(|e| Error::parse(format!("`when:` {e}")))?,
        ),
        None => None,
    };
    let each = match kw.get("each") {
        Some(Y::Sequence(items)) => Some(Each::List(
            items
                .iter()
                .map(value_from_yaml)
                .collect::<Result<Vec<_>>>()?,
        )),
        Some(scalar) => Some(Each::Expr(
            CelExpr::compile(&scalar_to_string(scalar))
                .map_err(|e| Error::parse(format!("`each:` {e}")))?,
        )),
        None => None,
    };
    let deps = kw
        .get("deps")
        .and_then(|v| v.as_sequence())
        .map(|s| s.iter().map(scalar_to_string).collect())
        .unwrap_or_default();
    Ok(Common {
        id,
        name,
        on,
        when,
        each,
        deps,
    })
}

fn parse_resources(v: Option<&Y>, cast: &Cast) -> Result<Vec<ResourceDecl>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => return Err(Error::parse("`resources:` 应是列表")),
    };
    let mut out: Vec<ResourceDecl> = Vec::new();
    let mut counters: BTreeMap<String, usize> = BTreeMap::new();
    for (i, item) in seq.iter().enumerate() {
        let m = item
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("resources[{i}]:应是 map")))?;
        let (ty, args, kw) = split_entry(m, RESOURCE_KEYWORDS, &format!("resources[{i}]"))?;
        let c = parse_common(&kw, cast)?;
        let n = counters.entry(ty.clone()).or_insert(0);
        let auto = if *n == 0 {
            ty.clone()
        } else {
            format!("{ty}{n}")
        };
        *n += 1;
        out.push(ResourceDecl {
            id: c.id.unwrap_or(auto),
            name: c.name,
            ty,
            args,
            on: c.on,
            when: c.when,
            each: c.each,
            deps: c.deps,
            line: Some(i),
        });
    }
    Ok(out)
}

fn parse_procedures(v: Option<&Y>, cast: &Cast) -> Result<BTreeMap<String, Procedure>> {
    let m = match v {
        None | Some(Y::Null) => return Ok(BTreeMap::new()),
        Some(Y::Mapping(m)) => m,
        Some(_) => return Err(Error::parse("`procedures:` 应是 map(名字 → 程序)")),
    };
    let mut out = BTreeMap::new();
    for (k, body) in m {
        let name = scalar_to_string(k);
        let body = body
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("procedure `{name}`:应是 map")))?;
        known_keys(
            body,
            &["params", "steps", "description"],
            &format!("procedure `{name}`"),
        )?;
        let params = parse_params(body.get(Y::from("params")))?;
        let steps = parse_steps(body.get(Y::from("steps")), &name, cast)?;
        out.insert(
            name.clone(),
            Procedure {
                name,
                params,
                steps,
            },
        );
    }
    Ok(out)
}

fn parse_steps(v: Option<&Y>, proc_name: &str, cast: &Cast) -> Result<Vec<Step>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => {
            return Err(Error::parse(format!(
                "procedure `{proc_name}`:`steps:` 应是列表"
            )))
        }
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let what = format!("procedure `{proc_name}` steps[{i}]");
        let m = item
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("{what}:应是 map")))?;
        let (ty, args, kw) = split_entry(m, STEP_KEYWORDS, &what)?;
        let c = parse_common(&kw, cast)?;
        let exports = match kw.get("exports") {
            Some(Y::Mapping(em)) => em
                .iter()
                .map(|(k, v)| (scalar_to_string(k), scalar_to_string(v)))
                .collect(),
            Some(_) => {
                return Err(Error::parse(format!(
                    "{what}:`exports:` 应是 map(fact 名 → 取值命令)"
                )))
            }
            None => BTreeMap::new(),
        };
        let strategy = match kw.get("strategy") {
            Some(Y::Mapping(sm)) => {
                known_keys(
                    sm,
                    &["throttle", "retries", "ignore_errors"],
                    &format!("{what} strategy"),
                )?;
                Strategy {
                    throttle: sm
                        .get(Y::from("throttle"))
                        .and_then(|v| v.as_u64())
                        .map(|n| n as usize),
                    retries: sm
                        .get(Y::from("retries"))
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as u32,
                    ignore_errors: sm
                        .get(Y::from("ignore_errors"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                }
            }
            Some(_) => return Err(Error::parse(format!("{what}:`strategy:` 应是 map"))),
            None => Strategy::default(),
        };
        out.push(Step {
            id: c.id.unwrap_or_else(|| format!("{ty}{i}")),
            name: c.name,
            ty,
            args,
            on: c.on,
            when: c.when,
            each: c.each,
            exports,
            strategy,
            line: Some(i),
        });
    }
    Ok(out)
}

fn parse_types(v: Option<&Y>) -> Result<Vec<TypeDef>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => return Err(Error::parse("`types:` 应是列表")),
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let m = item
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("types[{i}]:应是 map")))?;
        known_keys(
            m,
            &["name", "args", "observe", "apply", "destroy", "upgrade"],
            &format!("types[{i}]"),
        )?;
        let name =
            get_str(m, "name").ok_or_else(|| Error::parse(format!("types[{i}]:缺少 `name:`")))?;
        let ob = m
            .get(Y::from("observe"))
            .and_then(|v| v.as_mapping())
            .ok_or_else(|| {
                Error::parse(format!(
                    "type `{name}`:缺少 `observe:`(五动词里唯一强制的一个)"
                ))
            })?;
        known_keys(ob, &["cmd", "parse"], &format!("type `{name}` observe"))?;
        let observe = ObserveSpec {
            cmd: get_str(ob, "cmd")
                .ok_or_else(|| Error::parse(format!("type `{name}`:`observe.cmd` 必填")))?,
            parse: ob
                .get(Y::from("parse"))
                .and_then(|v| v.as_mapping())
                .map(|pm| {
                    pm.iter()
                        .map(|(k, v)| (scalar_to_string(k), scalar_to_string(v)))
                        .collect()
                })
                .unwrap_or_default(),
        };
        out.push(TypeDef {
            name: name.clone(),
            args: parse_params(m.get(Y::from("args")))?,
            observe,
            apply: get_str(m, "apply").ok_or_else(|| {
                Error::parse(format!("type `{name}`:缺少 `apply:`(procedure 名)"))
            })?,
            destroy: get_str(m, "destroy"),
            upgrade: get_str(m, "upgrade"),
        });
    }
    Ok(out)
}

fn parse_params(v: Option<&Y>) -> Result<Params> {
    let m = match v {
        None | Some(Y::Null) => return Ok(Params::new()),
        Some(Y::Mapping(m)) => m,
        Some(_) => return Err(Error::parse("`params:` 应是 map(名字 → 声明)")),
    };
    let mut out = Params::new();
    for (k, body) in m {
        let name = scalar_to_string(k);
        let b = body
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("param `{name}`:应是 map")))?;
        known_keys(
            b,
            &[
                "type",
                "default",
                "required",
                "secret",
                "stage",
                "desc",
                "description",
            ],
            &format!("param `{name}`"),
        )?;
        let ty = match b.get(Y::from("type")) {
            Some(tv) => {
                ParamType::from_yaml(tv).map_err(|e| Error::parse(format!("param `{name}`:{e}")))?
            }
            None => ParamType::default(),
        };
        let stage = match b.get(Y::from("stage")).and_then(|v| v.as_str()) {
            Some("build") => Stage::Build,
            Some("deploy") | None => Stage::Deploy,
            Some("apply") => {
                return Err(Error::parse(format!(
                "param `{name}`:`stage: apply` 已废止 —— apply 是动词,参数分期请写 `stage: deploy`"
            )))
            }
            Some(other) => {
                return Err(Error::parse(format!("param `{name}`:未知 stage `{other}`")))
            }
        };
        out.insert(
            name.clone(),
            ParamSpec {
                name,
                ty,
                default: b.get(Y::from("default")).cloned(),
                required: b
                    .get(Y::from("required"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                secret: b
                    .get(Y::from("secret"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                stage,
                desc: get_str(b, "desc").or_else(|| get_str(b, "description")),
            },
        );
    }
    Ok(out)
}

/// `facts:` —— 派生事实(D-136)。
///
/// 形状是 `名字: <CEL 表达式>`,与 `when:` 同一门语言。这里**允许完整 CEL**
/// (含探针函数),因为这是声明位置;资源参数那边仍然只许名词(E310)。
/// 一次计算、多处引用,正是 `cast:` 对 selector 做过的事。
fn parse_derived_facts(v: Option<&Y>) -> Result<BTreeMap<String, CelExpr>> {
    let Some(v) = v else {
        return Ok(BTreeMap::new());
    };
    let m = v
        .as_mapping()
        .ok_or_else(|| Error::parse("`facts:` 应是 map(名字: 表达式)"))?;
    let mut out = BTreeMap::new();
    for (k, val) in m {
        let name = k
            .as_str()
            .ok_or_else(|| Error::parse("`facts:` 的键应是字符串"))?
            .to_string();
        // 值可以写成 `${...}` 或裸表达式 —— 前者与蓝图别处一致,后者更短。
        // 两种都收,剥掉 `${}` 之后按同一条路编译。
        let raw = scalar_to_string(val);
        let src = raw
            .trim()
            .strip_prefix("${")
            .and_then(|r| r.strip_suffix('}'))
            .unwrap_or(raw.trim())
            .to_string();
        let expr = CelExpr::compile(&src).map_err(|e| Error::parse(format!("facts.{name}:{e}")))?;
        out.insert(name, expr);
    }
    Ok(out)
}

fn parse_materials(v: Option<&Y>) -> Result<Vec<Material>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => return Err(Error::parse("`materials:` 应是列表")),
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let m = item
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("materials[{i}]:应是 map")))?;
        known_keys(
            m,
            &[
                "name",
                "file",
                "image",
                "os_package",
                "sha256",
                "unzip",
                "when",
            ],
            &format!("materials[{i}]"),
        )?;
        let name = get_str(m, "name")
            .ok_or_else(|| Error::parse(format!("materials[{i}]:缺少 `name:`")))?;
        // 物料种类 = 哪个来源 key 出现(取代旧的 `kind:` 字段 + 分散的 url_tmpl/ref/packages)
        let kinds: Vec<(&str, MaterialKind)> = [
            ("file", MaterialKind::File),
            ("image", MaterialKind::Image),
            ("os_package", MaterialKind::OsPackage),
        ]
        .into_iter()
        .filter(|(k, _)| m.contains_key(Y::from(*k)))
        .collect();
        let (src_key, kind) = match kinds.len() {
            1 => kinds[0],
            0 => {
                return Err(Error::parse(format!(
                    "material `{name}`:需要恰好一个来源 key(`file:` / `image:` / `os_package:`)"
                )))
            }
            _ => {
                return Err(Error::parse(format!(
                    "material `{name}`:来源 key 出现了多个({})",
                    kinds.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(", ")
                )))
            }
        };
        out.push(Material {
            name,
            kind,
            source: value_from_yaml(m.get(Y::from(src_key)).expect("key present"))?,
            sha256: m.get(Y::from("sha256")).map(value_from_yaml).transpose()?,
            unzip: get_str(m, "unzip"),
            when: match m.get(Y::from("when")) {
                Some(w) => Some(
                    CelExpr::compile(&scalar_to_string(w))
                        .map_err(|e| Error::parse(format!("materials[{i}] `when:` {e}")))?,
                ),
                None => None,
            },
            line: Some(i),
        });
    }
    Ok(out)
}

fn parse_preflight(v: Option<&Y>, cast: &Cast) -> Result<Vec<Assertion>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => return Err(Error::parse("`preflight:` 应是列表")),
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let m = item
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("preflight[{i}]:应是 map")))?;
        known_keys(m, &["assert", "msg", "target"], &format!("preflight[{i}]"))?;
        let src = get_str(m, "assert")
            .ok_or_else(|| Error::parse(format!("preflight[{i}]:缺少 `assert:`")))?;
        out.push(Assertion {
            expr: CelExpr::compile(&src)
                .map_err(|e| Error::parse(format!("preflight[{i}] `assert:` {e}")))?,
            msg: get_str(m, "msg"),
            on: match get_str(m, "target") {
                Some(s) => resolve_selector(&s, cast)?,
                None => Selector::All,
            },
            line: Some(i),
        });
    }
    Ok(out)
}

fn parse_health(v: Option<&Y>, cast: &Cast) -> Result<Vec<HealthProbe>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => return Err(Error::parse("`health:` 应是列表")),
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let m = item
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("health[{i}]:应是 map")))?;
        let (ty, args, kw) =
            split_entry(m, &["target", "timeout", "name"], &format!("health[{i}]"))?;
        out.push(HealthProbe {
            ty,
            args,
            on: match kw.get("target") {
                Some(v) => resolve_selector(&scalar_to_string(v), cast)?,
                None => Selector::All,
            },
            timeout: kw.get("timeout").map(|v| scalar_to_string(v)),
            line: Some(i),
        });
    }
    Ok(out)
}

fn parse_requires(v: Option<&Y>) -> Result<Requires> {
    let m = match v {
        None | Some(Y::Null) => return Ok(Requires::default()),
        Some(Y::Mapping(m)) => m,
        Some(_) => return Err(Error::parse("`requires:` 应是 map")),
    };
    known_keys(m, &["os", "arch"], "requires")?;
    let os = m
        .get(Y::from("os"))
        .and_then(|v| v.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|e| e.as_mapping())
                .map(|em| OsRequire {
                    distro: get_str(em, "distro").unwrap_or_default(),
                    versions: em
                        .get(Y::from("versions"))
                        .and_then(|v| v.as_sequence())
                        .map(|s| s.iter().map(scalar_to_string).collect())
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();
    let arch = m
        .get(Y::from("arch"))
        .and_then(|v| v.as_sequence())
        .map(|s| s.iter().map(scalar_to_string).collect())
        .unwrap_or_default();
    Ok(Requires { os, arch })
}

// ---------------------------------------------------------------- 小工具

/// 未知 key 直接报错并给拼写建议 —— 静态可分析的用户体验落点。
pub(crate) fn known_keys(m: &serde_yaml::Mapping, allowed: &[&str], what: &str) -> Result<()> {
    let allow: BTreeSet<&str> = allowed.iter().copied().collect();
    for k in m.keys() {
        // `on` 在 **YAML 1.1** 里是布尔 true。crater 用 1.2 解析没问题,但 PyYAML、
        // 许多 CI 与编辑器插件仍是 1.1,会把这个键读成 `true:` —— 坑由作者承担,
        // 且极难查("我的 YAML 明明是对的")。所以不静默接受,而是教他改。
        if k.as_bool() == Some(true) || k.as_str() == Some("on") {
            return Err(Error::parse(format!(
                "{what}:`on:` 已改名为 `target:` —— `on` 在 YAML 1.1 里是布尔 `true`,\
                 某些工具(PyYAML / 部分 CI 与编辑器)会把这个键读成 `true:`。\
                 crater 自己解析无误,但这个坑会落到你头上"
            )));
        }
        let key = k.as_str().unwrap_or_default();
        if !allow.contains(key) {
            let hint = closest(key, allowed)
                .map(|s| format!(",是不是想写 `{s}`?"))
                .unwrap_or_default();
            return Err(Error::parse(format!(
                "{what}:未知字段 `{key}`{hint}(可用:{})",
                allowed.join(", ")
            )));
        }
    }
    Ok(())
}

fn closest<'a>(name: &str, pool: &[&'a str]) -> Option<&'a str> {
    pool.iter()
        .map(|c| (*c, lev(name, c)))
        .filter(|&(_, d)| d <= 2)
        .min_by_key(|&(_, d)| d)
        .map(|(c, _)| c)
}

fn lev(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

fn get_str(m: &serde_yaml::Mapping, key: &str) -> Option<String> {
    m.get(Y::from(key)).map(scalar_to_string)
}

/// 标量 → 字符串(YAML 里 `port: 9000` 与 `port: "9000"` 都该接受)。
pub fn scalar_to_string(v: &Y) -> String {
    match v {
        Y::String(s) => s.clone(),
        Y::Number(n) => n.to_string(),
        Y::Bool(b) => b.to_string(),
        Y::Null => String::new(),
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
    }
}

#[cfg(test)]
mod cast_fleet_tests {
    use super::*;

    fn bp(y: &str) -> Result<Blueprint> {
        blueprint_from_str(y)
    }

    const HEAD: &str = "name: t\nfleet:\n  groups:\n    controlplane: {min: 1}\n    worker: {min: 0}\ncast:\n  seed: first(role.controlplane)\n  rest_cp: rest(role.controlplane)\n";

    #[test]
    fn a_cast_name_expands_to_its_selector_at_parse_time() {
        let b = bp(&format!(
            "{HEAD}resources:\n  - service: {{name: kubelet, state: running}}\n    target: seed\n"
        ))
        .unwrap();
        // 展开发生在解析期:运行期看到的就是完整 selector,零间接。
        match &b.resources[0].on {
            Selector::First(inner) => {
                assert!(matches!(**inner, Selector::Role(ref r) if r == "controlplane"))
            }
            other => panic!("未展开:{other:?}"),
        }
    }

    #[test]
    fn a_misspelled_cast_name_is_caught_with_a_suggestion() {
        // 光秃秃的名字既不是 role.x 也不是 host.x —— 只可能是想引用选角表。
        let err = bp(&format!(
            "{HEAD}resources:\n  - service: {{name: k, state: running}}\n    target: sed\n"
        ))
        .unwrap_err()
        .to_string();
        assert!(err.contains("seed"), "{err}");
    }

    #[test]
    fn cast_may_not_reference_a_group_the_contract_never_declared() {
        // 契约一旦存在就是权威:选角表引用组外角色 = 契约和用法对不上,当场停。
        let err = bp("name: t\nfleet:\n  groups:\n    controlplane: {min: 1}\ncast:\n  seed: first(role.controlplan)\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("controlplane"), "{err}");
    }

    #[test]
    fn without_a_contract_cast_is_unconstrained() {
        // 没写 fleet: 的蓝图不该因为引入 cast 而突然被要求写 fleet:。
        let b = bp("name: t\ncast:\n  seed: first(role.anything)\n").unwrap();
        assert_eq!(b.cast.len(), 1);
    }

    #[test]
    fn a_group_contract_defaults_to_requiring_one_machine() {
        // `min` 省略时是 1,不是 0 —— 声明一个组却允许它为空是反直觉的。
        let b = bp("name: t\nfleet:\n  groups:\n    controlplane: {}\n").unwrap();
        assert_eq!(b.fleet.groups["controlplane"].min, 1);
    }

    #[test]
    fn cast_may_not_chain_into_another_cast_entry() {
        // 一层间接够用;两层就要跳读了。这条限制是刻意的。
        let err = bp("name: t\ncast:\n  a: role.x\n  b: first(a)\n")
            .unwrap_err()
            .to_string();
        assert!(!err.is_empty());
    }
}
