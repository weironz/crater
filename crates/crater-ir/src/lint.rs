//! Lint —— **部署前**把能静态发现的错全找出来。
//!
//! 这是"YAML 是数据、表达式非图灵完备"换来的直接收益:CEL 可以在不连目标机的情况下
//! 做作用域检查,`substrate.famliy` 拼错在这里就报,而不是 Ansible 那样连上机器、跑到
//! 那一行才炸。`x lint` 应能把整个仓库扫一遍。

use crate::expr::CelExpr;
use crate::ir::*;
use crate::selector::Selector;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// 一定跑不对,拒绝部署。
    Error,
    /// 可疑,放行但要看见。
    Warn,
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    pub severity: Severity,
    /// 语义位置,如 `resources.copy1` —— 稳定、可搜索,与行号互补。
    pub at: String,
    /// 源码行号(1-based)。没有时说明这条诊断不绑定某一行(如 facts 产销失衡)。
    pub line: Option<usize>,
    pub msg: String,
}

impl Diagnostic {
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tag = match self.severity {
            Severity::Error => "error",
            Severity::Warn => "warn ",
        };
        match self.line {
            Some(l) => write!(f, "{tag} {}:{}: {}", self.at, l, self.msg),
            None => write!(f, "{tag} {}: {}", self.at, self.msg),
        }
    }
}

/// CEL 作用域白名单(ir-draft §4-3)。超出这些根变量 = 写错了。
const ROOT_SCOPES: &[&str] = &["params", "env", "substrate", "item", "facts", "observed"];

/// 只读探针函数白名单(裁定 A)。CEL 本身不允许用户定义函数;这里再把**宿主提供**的
/// 函数集也钉成封闭集合 —— 否则"非图灵完备"会被一个万能的 `exec()` 悄悄破坏。
const PROBE_FUNCS: &[&str] = &[
    "port_owner",   // 谁在监听这个端口(空串 = 没人)
    "path_exists",
    "cmd_ok",       // 只读命令退出码为 0
    "service_state",
    "has",          // CEL 标准宏
    "size",
];

fn check_funcs(
    e: &CelExpr,
    at: &str,
    line: Option<usize>,
    push: &mut impl FnMut(Severity, String, Option<usize>, String),
) {
    for f in e.funcs() {
        if !PROBE_FUNCS.contains(&f.as_str()) {
            push(
                Severity::Error,
                at.into(),
                line,
                format!("未知探针函数 `{f}()`(可用:{})", PROBE_FUNCS.join(", ")),
            );
        }
    }
}

pub fn lint(bp: &Blueprint) -> Vec<Diagnostic> {
    let mut d = Vec::new();
    // 所有诊断都经这一个出口,保证 at/line 成对出现。
    let mut push = |severity, at: String, line: Option<usize>, msg: String| {
        d.push(Diagnostic { severity, at, line, msg })
    };

    let material_names: BTreeSet<&str> = bp.materials.iter().map(|m| m.name.as_str()).collect();
    let custom_types: BTreeSet<&str> = bp.types.iter().map(|t| t.name.as_str()).collect();
    let param_names: BTreeSet<&str> = bp.params.keys().map(|s| s.as_str()).collect();

    // ---- params:default 必须符合声明的 type ----
    for (name, p) in &bp.params {
        if let Some(def) = &p.default {
            if let Err(e) = p.ty.check(def) {
                push(Severity::Error, format!("params.{name}"), None, format!("default {e}"));
            }
        }
        if p.required && p.default.is_some() {
            push(
                Severity::Warn,
                format!("params.{name}"),
                None,
                "同时声明了 required 和 default —— default 会让 required 永不触发".into(),
            );
        }
    }

    // ---- 资源 ----
    for r in &bp.resources {
        let at = format!("resources.{}", r.id);
        lint_type_and_args(&r.ty, &r.args, &custom_types, &material_names, &at, r.line, &mut push);
        lint_scope_of(
            &r.args, r.when.as_ref(), &r.on, r.each.is_some(), &at, r.line, &param_names, &mut push,
        );
    }

    // ---- 自定义类型:apply/destroy/upgrade 必须指向真实存在的 procedure ----
    for t in &bp.types {
        let at = format!("types.{}", t.name);
        if crate::types::is_builtin(&t.name) {
            push(Severity::Error, at.clone(), None, format!("`{}` 与内建类型重名", t.name));
        }
        for (field, target) in [
            ("apply", Some(&t.apply)),
            ("destroy", t.destroy.as_ref()),
            ("upgrade", t.upgrade.as_ref()),
        ] {
            if let Some(p) = target {
                if !bp.procedures.contains_key(p.trim_start_matches("procedure ").trim()) {
                    push(
                        Severity::Error,
                        at.clone(),
                        None,
                        format!("`{field}: {p}` 指向不存在的 procedure"),
                    );
                }
            }
        }
    }

    // ---- procedure:步骤 + 跨主机 fact 的产销平衡(k8s 裁定 B)----
    let mut exported: BTreeSet<String> = BTreeSet::new();
    let mut consumed: BTreeSet<String> = BTreeSet::new();
    for (pname, p) in &bp.procedures {
        // procedure 有**自己的** params(`upgrade` 的 `to:`)。不并进作用域的话,
        // 每个带参数的 procedure 都会被误报成"引用了未声明的参数"。
        let mut proc_params = param_names.clone();
        proc_params.extend(p.params.keys().map(|s| s.as_str()));
        for s in &p.steps {
            let at = format!("procedures.{pname}.{}", s.id);
            lint_type_and_args(&s.ty, &s.args, &custom_types, &material_names, &at, s.line, &mut push);
            lint_scope_of(
                &s.args, s.when.as_ref(), &s.on, s.each.is_some(), &at, s.line, &proc_params, &mut push,
            );
            exported.extend(s.exports.keys().cloned());
            collect_fact_refs(&s.args, &mut consumed);
            if let Some(w) = &s.when {
                collect_fact_names_from_expr(w, &mut consumed);
            }
            if s.ty == "shell" && !s.args.contains_key("check") {
                push(
                    Severity::Warn,
                    at.clone(),
                    s.line,
                    "裸 shell 没有 `check:` —— plan 里只能显示 `?unknown`,并计入模型化欠债".into(),
                );
            }
        }
    }
    for c in consumed.difference(&exported) {
        push(
            Severity::Error,
            "facts".into(),
            None,
            format!("`${{facts.{c}}}` 无人导出 —— 需要某一步 `exports: {{{c}: …}}`"),
        );
    }
    for e in exported.difference(&consumed) {
        push(
            Severity::Warn,
            "facts".into(),
            None,
            format!("fact `{e}` 被导出但无人消费"),
        );
    }

    // ---- 物料 ----
    for m in &bp.materials {
        if m.unzip.is_some() && m.kind != MaterialKind::File {
            push(
                Severity::Error,
                format!("materials.{}", m.name),
                m.line,
                "`unzip:` 只对 `file:` 物料有意义".into(),
            );
        }
    }
    let mut seen = BTreeSet::new();
    for m in &bp.materials {
        // 同名多条 = 多架构/多 flavor 变体,必须靠 `when:` 区分,否则解析期就有歧义。
        if !seen.insert(&m.name) && m.when.is_none() {
            push(
                Severity::Error,
                format!("materials.{}", m.name),
                m.line,
                "同名物料重复且没有 `when:` 区分 —— 变体必须可判定".into(),
            );
        }
    }

    // ---- 断言与健康探针 ----
    for (i, a) in bp.preflight.iter().enumerate() {
        check_scope(&a.expr, &format!("preflight[{i}]"), a.line, &param_names, false, &mut push);
    }
    for h in &bp.health {
        let at = format!("health.{}", h.ty);
        lint_type_and_args(&h.ty, &h.args, &custom_types, &material_names, &at, h.line, &mut push);
    }

    d.sort_by_key(|x| (x.severity, x.line.unwrap_or(usize::MAX)));
    d
}

/// 类型名是否已知 + 参数拼写/必填/互斥。
#[allow(clippy::too_many_arguments)]
fn lint_type_and_args(
    ty: &str,
    args: &Args,
    custom: &BTreeSet<&str>,
    materials: &BTreeSet<&str>,
    at: &str,
    line: Option<usize>,
    push: &mut impl FnMut(Severity, String, Option<usize>, String),
) {
    if custom.contains(ty) {
        return; // 自定义类型的参数由其 `args:` 契约管,交给 plan 期
    }
    let Some(b) = crate::types::builtin(ty) else {
        let hint = crate::types::suggest(ty)
            .map(|s| format!(",是不是想写 `{s}`?"))
            .unwrap_or_default();
        push(
            Severity::Error,
            at.into(),
            line,
            format!("未知资源类型 `{ty}`{hint}"),
        );
        return;
    };
    for req in b.required {
        if !args.contains_key(*req) {
            push(Severity::Error, at.into(), line, format!("缺少必填参数 `{req}`"));
        }
    }
    if !b.one_of.is_empty() {
        let hits: Vec<&&str> = b.one_of.iter().filter(|k| args.contains_key(**k)).collect();
        match hits.len() {
            1 => {}
            0 => push(
                Severity::Error,
                at.into(),
                line,
                format!("需要其中之一:{}", b.one_of.join(" / ")),
            ),
            _ => push(
                Severity::Error,
                at.into(),
                line,
                format!(
                    "`{}` 互斥,只能给一个",
                    hits.iter().map(|s| **s).collect::<Vec<_>>().join("` 与 `")
                ),
            ),
        }
    }
    let allowed: BTreeSet<&str> = b
        .required
        .iter()
        .chain(b.optional.iter())
        .chain(b.one_of.iter())
        .copied()
        .collect();
    for k in args.keys() {
        if !allowed.contains(k.as_str()) {
            push(
                Severity::Error,
                at.into(),
                line,
                format!("`{ty}` 没有参数 `{k}`(可用:{})", {
                    let mut v: Vec<&str> = allowed.iter().copied().collect();
                    v.sort_unstable();
                    v.join(", ")
                }),
            );
        }
    }
    // 引用的物料必须已声明(旧模型要等 apply 才报 unknown material)。
    for key in ["material", "from_material"] {
        if let Some(Value::Lit(serde_yaml::Value::String(name))) = args.get(key) {
            if !materials.contains(name.as_str()) {
                push(
                    Severity::Error,
                    at.into(),
                    line,
                    format!("引用了未声明的物料 `{name}`"),
                );
            }
        }
    }
    if let Some(Value::List(items)) = args.get("materials") {
        for it in items {
            if let Value::Lit(serde_yaml::Value::String(name)) = it {
                if !materials.contains(name.as_str()) {
                    push(Severity::Error, at.into(), line, format!("引用了未声明的物料 `{name}`"));
                }
            }
        }
    }
}

/// CEL 作用域检查:根变量必须在白名单内;`item` 只在 `each:` 下合法。
#[allow(clippy::too_many_arguments)]
fn lint_scope_of(
    args: &Args,
    when: Option<&CelExpr>,
    on: &Selector,
    has_each: bool,
    at: &str,
    line: Option<usize>,
    params: &BTreeSet<&str>,
    push: &mut impl FnMut(Severity, String, Option<usize>, String),
) {
    let mut roots = BTreeSet::new();
    for v in args.values() {
        v.roots(&mut roots);
    }
    if let Some(w) = when {
        roots.extend(w.roots().iter().cloned());
    }
    for e in on.exprs() {
        roots.extend(e.roots().iter().cloned());
    }
    for r in &roots {
        if !ROOT_SCOPES.contains(&r.as_str()) {
            push(
                Severity::Error,
                at.into(),
                line,
                format!("表达式引用了作用域外的 `{r}`(可用:{})", ROOT_SCOPES.join(", ")),
            );
        }
        if r == "item" && !has_each {
            push(
                Severity::Error,
                at.into(),
                line,
                "用了 `item` 但这一条没有 `each:`".into(),
            );
        }
    }
    // params.X 里的 X 必须被声明过 —— 这条最能救命(改了参数名,用处忘了改)。
    for v in args.values() {
        check_param_refs(v, at, line, params, push);
    }
    if let Some(w) = when {
        check_param_names(w.src(), at, line, params, push);
        check_funcs(w, at, line, push);
    }
    for e in on.exprs() {
        check_funcs(e, at, line, push);
    }
}

fn check_scope(
    e: &CelExpr,
    at: &str,
    line: Option<usize>,
    params: &BTreeSet<&str>,
    has_each: bool,
    push: &mut impl FnMut(Severity, String, Option<usize>, String),
) {
    for r in e.roots() {
        if !ROOT_SCOPES.contains(&r.as_str()) {
            push(
                Severity::Error,
                at.into(),
                line,
                format!("表达式引用了作用域外的 `{r}`"),
            );
        }
        if r == "item" && !has_each {
            push(Severity::Error, at.into(), line, "用了 `item` 但这一条没有 `each:`".into());
        }
    }
    check_param_names(e.src(), at, line, params, push);
    check_funcs(e, at, line, push);
}

fn check_param_refs(
    v: &Value,
    at: &str,
    line: Option<usize>,
    params: &BTreeSet<&str>,
    push: &mut impl FnMut(Severity, String, Option<usize>, String),
) {
    match v {
        Value::Tmpl(t) => {
            for p in t.parts() {
                if let crate::expr::Part::Expr(e) = p {
                    check_param_names(e.src(), at, line, params, push);
                }
            }
        }
        Value::List(items) => items.iter().for_each(|i| check_param_refs(i, at, line, params, push)),
        Value::Map(m) => m.values().for_each(|i| check_param_refs(i, at, line, params, push)),
        Value::Lit(_) => {}
    }
}

/// 从表达式源码里抓 `params.<name>`(CEL 只给根变量,字段名要自己扫)。
fn check_param_names(
    src: &str,
    at: &str,
    line: Option<usize>,
    params: &BTreeSet<&str>,
    push: &mut impl FnMut(Severity, String, Option<usize>, String),
) {
    for name in field_refs(src, "params.") {
        if !params.contains(name.as_str()) {
            let hint = closest_param(&name, params)
                .map(|s| format!(",是不是 `{s}`?"))
                .unwrap_or_default();
            push(
                Severity::Error,
                at.into(),
                line,
                format!("引用了未声明的参数 `params.{name}`{hint}"),
            );
        }
    }
}

fn collect_fact_refs(args: &Args, out: &mut BTreeSet<String>) {
    for v in args.values() {
        collect_fact_refs_value(v, out);
    }
}

fn collect_fact_refs_value(v: &Value, out: &mut BTreeSet<String>) {
    match v {
        Value::Tmpl(t) => {
            for p in t.parts() {
                if let crate::expr::Part::Expr(e) = p {
                    out.extend(field_refs(e.src(), "facts."));
                }
            }
        }
        Value::List(items) => items.iter().for_each(|i| collect_fact_refs_value(i, out)),
        Value::Map(m) => m.values().for_each(|i| collect_fact_refs_value(i, out)),
        Value::Lit(_) => {}
    }
}

fn collect_fact_names_from_expr(e: &CelExpr, out: &mut BTreeSet<String>) {
    out.extend(field_refs(e.src(), "facts."));
}

/// 扫源码里 `<prefix><ident>` 的 ident 部分。
fn field_refs(src: &str, prefix: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(i) = rest.find(prefix) {
        let after = &rest[i + prefix.len()..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.push(name);
        }
        rest = &rest[i + prefix.len()..];
    }
    out
}

fn closest_param<'a>(name: &str, pool: &BTreeSet<&'a str>) -> Option<&'a str> {
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

/// 便捷:只要 error 就返回 Err。
pub fn errors(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
    diags.iter().filter(|d| d.is_error()).collect()
}
