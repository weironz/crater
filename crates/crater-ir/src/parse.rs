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

/// 资源条目允许的步骤关键字(其余 key 视为模块名)。
const RESOURCE_KEYWORDS: &[&str] = &["id", "name", "on", "when", "each", "deps"];
/// procedure 步骤额外允许的关键字。
const STEP_KEYWORDS: &[&str] = &[
    "id", "name", "on", "when", "each", "deps", "exports", "strategy",
];

pub fn blueprint_from_str(text: &str) -> Result<Blueprint> {
    let mut bp = parse_structure(text)?;
    assign_lines(&mut bp, &crate::loc::LineIndex::new(text));
    Ok(bp)
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
    bp.materials.iter_mut().for_each(|m| put(&mut m.line, &mats));
    let pre = idx.list_items(&["preflight"]);
    bp.preflight.iter_mut().for_each(|a| put(&mut a.line, &pre));
    let health = idx.list_items(&["health"]);
    bp.health.iter_mut().for_each(|h| put(&mut h.line, &health));
    for (name, proc) in bp.procedures.iter_mut() {
        let steps = idx.list_items(&["procedures", name, "steps"]);
        proc.steps.iter_mut().for_each(|st| put(&mut st.line, &steps));
    }
}

fn parse_structure(text: &str) -> Result<Blueprint> {
    let root: Y = serde_yaml::from_str(text)?;
    let m = root.as_mapping().ok_or_else(|| Error::parse("blueprint 顶层应是一个 map"))?;

    known_keys(
        m,
        &[
            "name", "version", "description", "params", "requires", "materials",
            "preflight", "types", "resources", "procedures", "health",
        ],
        "blueprint",
    )?;

    let name = get_str(m, "name").ok_or_else(|| Error::parse("blueprint 缺少 `name:`"))?;
    let params = parse_params(m.get(Y::from("params")))?;

    Ok(Blueprint {
        name,
        version: get_str(m, "version"),
        description: get_str(m, "description"),
        params,
        requires: parse_requires(m.get(Y::from("requires")))?,
        materials: parse_materials(m.get(Y::from("materials")))?,
        preflight: parse_preflight(m.get(Y::from("preflight")))?,
        types: parse_types(m.get(Y::from("types")))?,
        resources: parse_resources(m.get(Y::from("resources")))?,
        procedures: parse_procedures(m.get(Y::from("procedures")))?,
        health: parse_health(m.get(Y::from("health")))?,
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
                .filter_map(|n| crate::types::suggest(n).map(|s| format!("`{n}` 是不是想写 `{s}`?")))
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
            items.iter().map(value_from_yaml).collect::<Result<Vec<_>>>()?,
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

fn parse_common(kw: &BTreeMap<String, &Y>) -> Result<Common> {
    let id = kw.get("id").map(|v| scalar_to_string(v));
    let name = kw.get("name").map(|v| scalar_to_string(v));
    let on = match kw.get("on") {
        Some(v) => Selector::parse(&scalar_to_string(v))
            .map_err(|e| Error::parse(format!("`on:` {e}")))?,
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
            items.iter().map(value_from_yaml).collect::<Result<Vec<_>>>()?,
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
    Ok(Common { id, name, on, when, each, deps })
}

fn parse_resources(v: Option<&Y>) -> Result<Vec<ResourceDecl>> {
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
        let c = parse_common(&kw)?;
        let n = counters.entry(ty.clone()).or_insert(0);
        let auto = if *n == 0 { ty.clone() } else { format!("{ty}{n}") };
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

fn parse_procedures(v: Option<&Y>) -> Result<BTreeMap<String, Procedure>> {
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
        known_keys(body, &["params", "steps", "description"], &format!("procedure `{name}`"))?;
        let params = parse_params(body.get(Y::from("params")))?;
        let steps = parse_steps(body.get(Y::from("steps")), &name)?;
        out.insert(name.clone(), Procedure { name, params, steps });
    }
    Ok(out)
}

fn parse_steps(v: Option<&Y>, proc_name: &str) -> Result<Vec<Step>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => return Err(Error::parse(format!("procedure `{proc_name}`:`steps:` 应是列表"))),
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let what = format!("procedure `{proc_name}` steps[{i}]");
        let m = item.as_mapping().ok_or_else(|| Error::parse(format!("{what}:应是 map")))?;
        let (ty, args, kw) = split_entry(m, STEP_KEYWORDS, &what)?;
        let c = parse_common(&kw)?;
        let exports = match kw.get("exports") {
            Some(Y::Mapping(em)) => em
                .iter()
                .map(|(k, v)| (scalar_to_string(k), scalar_to_string(v)))
                .collect(),
            Some(_) => return Err(Error::parse(format!("{what}:`exports:` 应是 map(fact 名 → 取值命令)"))),
            None => BTreeMap::new(),
        };
        let strategy = match kw.get("strategy") {
            Some(Y::Mapping(sm)) => {
                known_keys(sm, &["throttle", "retries", "ignore_errors"], &format!("{what} strategy"))?;
                Strategy {
                    throttle: sm.get(Y::from("throttle")).and_then(|v| v.as_u64()).map(|n| n as usize),
                    retries: sm.get(Y::from("retries")).and_then(|v| v.as_u64()).unwrap_or(0) as u32,
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
        let m = item.as_mapping().ok_or_else(|| Error::parse(format!("types[{i}]:应是 map")))?;
        known_keys(m, &["name", "args", "observe", "apply", "destroy", "upgrade"], &format!("types[{i}]"))?;
        let name = get_str(m, "name")
            .ok_or_else(|| Error::parse(format!("types[{i}]:缺少 `name:`")))?;
        let ob = m
            .get(Y::from("observe"))
            .and_then(|v| v.as_mapping())
            .ok_or_else(|| Error::parse(format!("type `{name}`:缺少 `observe:`(五动词里唯一强制的一个)")))?;
        known_keys(ob, &["cmd", "parse"], &format!("type `{name}` observe"))?;
        let observe = ObserveSpec {
            cmd: get_str(ob, "cmd")
                .ok_or_else(|| Error::parse(format!("type `{name}`:`observe.cmd` 必填")))?,
            parse: ob
                .get(Y::from("parse"))
                .and_then(|v| v.as_mapping())
                .map(|pm| pm.iter().map(|(k, v)| (scalar_to_string(k), scalar_to_string(v))).collect())
                .unwrap_or_default(),
        };
        out.push(TypeDef {
            name: name.clone(),
            args: parse_params(m.get(Y::from("args")))?,
            observe,
            apply: get_str(m, "apply")
                .ok_or_else(|| Error::parse(format!("type `{name}`:缺少 `apply:`(procedure 名)")))?,
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
            &["type", "default", "required", "secret", "stage", "desc", "description"],
            &format!("param `{name}`"),
        )?;
        let ty = match b.get(Y::from("type")) {
            Some(tv) => ParamType::from_yaml(tv)
                .map_err(|e| Error::parse(format!("param `{name}`:{e}")))?,
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
            Some(other) => return Err(Error::parse(format!("param `{name}`:未知 stage `{other}`"))),
        };
        out.insert(
            name.clone(),
            ParamSpec {
                name,
                ty,
                default: b.get(Y::from("default")).cloned(),
                required: b.get(Y::from("required")).and_then(|v| v.as_bool()).unwrap_or(false),
                secret: b.get(Y::from("secret")).and_then(|v| v.as_bool()).unwrap_or(false),
                stage,
                desc: get_str(b, "desc").or_else(|| get_str(b, "description")),
            },
        );
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
        let m = item.as_mapping().ok_or_else(|| Error::parse(format!("materials[{i}]:应是 map")))?;
        known_keys(
            m,
            &["name", "file", "image", "os_package", "sha256", "unzip", "when"],
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
            sha256: get_str(m, "sha256"),
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

fn parse_preflight(v: Option<&Y>) -> Result<Vec<Assertion>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => return Err(Error::parse("`preflight:` 应是列表")),
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let m = item.as_mapping().ok_or_else(|| Error::parse(format!("preflight[{i}]:应是 map")))?;
        known_keys(m, &["assert", "msg", "on"], &format!("preflight[{i}]"))?;
        let src = get_str(m, "assert")
            .ok_or_else(|| Error::parse(format!("preflight[{i}]:缺少 `assert:`")))?;
        out.push(Assertion {
            expr: CelExpr::compile(&src)
                .map_err(|e| Error::parse(format!("preflight[{i}] `assert:` {e}")))?,
            msg: get_str(m, "msg"),
            on: match get_str(m, "on") {
                Some(s) => Selector::parse(&s)
                    .map_err(|e| Error::parse(format!("preflight[{i}] `on:` {e}")))?,
                None => Selector::All,
            },
            line: Some(i),
        });
    }
    Ok(out)
}

fn parse_health(v: Option<&Y>) -> Result<Vec<HealthProbe>> {
    let seq = match v {
        None | Some(Y::Null) => return Ok(vec![]),
        Some(Y::Sequence(s)) => s,
        Some(_) => return Err(Error::parse("`health:` 应是列表")),
    };
    let mut out = Vec::new();
    for (i, item) in seq.iter().enumerate() {
        let m = item.as_mapping().ok_or_else(|| Error::parse(format!("health[{i}]:应是 map")))?;
        let (ty, args, kw) = split_entry(m, &["on", "timeout", "name"], &format!("health[{i}]"))?;
        out.push(HealthProbe {
            ty,
            args,
            on: match kw.get("on") {
                Some(v) => Selector::parse(&scalar_to_string(v))
                    .map_err(|e| Error::parse(format!("health[{i}] `on:` {e}")))?,
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
fn known_keys(m: &serde_yaml::Mapping, allowed: &[&str], what: &str) -> Result<()> {
    let allow: BTreeSet<&str> = allowed.iter().copied().collect();
    for k in m.keys() {
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
        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    }
}
