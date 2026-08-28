//! 诊断能力回归 —— 「静态可分析」的兑现清单。
//!
//! 每条测试对应一类**在 Ansible 里要连上目标机、跑到那一行才会炸**的错误。
//! 它们在这里全部零连接、毫秒级报出。这是新 DSL 的核心卖点,不是附赠品。

use crater_ir::{lint, parse};

/// 解析必须失败,且错误里提到这些关键词。
fn parse_err(yaml: &str, expect: &[&str]) -> String {
    let e = parse::blueprint_from_str(yaml)
        .err()
        .unwrap_or_else(|| panic!("本应解析失败:\n{yaml}"))
        .to_string();
    for kw in expect {
        assert!(e.contains(kw), "错误信息缺 `{kw}`:{e}");
    }
    e
}

/// 解析成功但 lint 报 error,且信息里提到这些关键词。
fn lint_err(yaml: &str, expect: &[&str]) -> String {
    let bp = parse::blueprint_from_str(yaml).expect("应能解析");
    let diags = lint::lint(&bp);
    let errs = lint::errors(&diags);
    assert!(!errs.is_empty(), "本应 lint 报错:\n{yaml}");
    let joined = errs.iter().map(|d| d.to_string()).collect::<Vec<_>>().join("\n");
    for kw in expect {
        assert!(joined.contains(kw), "诊断缺 `{kw}`:\n{joined}");
    }
    joined
}

const HEAD: &str = "name: t\nparams:\n  port: { type: port, default: 9000 }\n";

#[test]
fn typo_in_module_name_is_caught_with_a_suggestion() {
    lint_err(
        &format!("{HEAD}resources:\n  - servce: {{ name: x }}\n"),
        &["未知资源类型", "servce", "service"],
    );
}

#[test]
fn typo_in_argument_name_is_caught() {
    lint_err(
        &format!("{HEAD}resources:\n  - service: {{ name: x, stat: started }}\n"),
        &["没有参数 `stat`"],
    );
}

#[test]
fn typo_in_param_reference_is_caught_with_a_suggestion() {
    // 改了参数名却忘了改引用 —— Ansible 会安静地渲染成空字符串。
    lint_err(
        &format!("{HEAD}resources:\n  - file: {{ path: \"/x/${{params.prot}}\", state: directory }}\n"),
        &["未声明的参数", "params.prot", "port"],
    );
}

#[test]
fn out_of_scope_root_variable_is_caught() {
    lint_err(
        &format!("{HEAD}resources:\n  - file: {{ path: \"/x/${{hostvars.a}}\", state: directory }}\n"),
        &["作用域外", "hostvars"],
    );
}

#[test]
fn item_without_each_is_caught() {
    lint_err(
        &format!("{HEAD}resources:\n  - file: {{ path: \"${{item}}\", state: directory }}\n"),
        &["`item`", "each"],
    );
}

#[test]
fn undeclared_material_reference_is_caught() {
    lint_err(
        &format!("{HEAD}resources:\n  - copy: {{ material: nope, dest: /x }}\n"),
        &["未声明的物料", "nope"],
    );
}

#[test]
fn missing_required_arg_and_broken_one_of_are_caught() {
    lint_err(
        &format!("{HEAD}resources:\n  - copy: {{ dest: /x }}\n"),
        &["需要其中之一", "content", "material"],
    );
    lint_err(
        &format!("{HEAD}resources:\n  - copy: {{ dest: /x, content: a, material: b }}\n"),
        &["互斥"],
    );
    lint_err(
        &format!("{HEAD}resources:\n  - file: {{ path: /x }}\n"),
        &["缺少必填参数 `state`"],
    );
}

#[test]
fn bad_default_value_is_caught_against_declared_type() {
    lint_err(
        "name: t\nparams:\n  vip: { type: ip, default: \"192.168.1.999\" }\n",
        &["default", "不是合法 IP"],
    );
    lint_err(
        "name: t\nparams:\n  net: { type: cidr, default: \"10.0.0.0/48\" }\n",
        &["前缀长度"],
    );
}

#[test]
fn unknown_probe_function_is_rejected() {
    // 封闭白名单:不许出现一个万能的 exec(),否则"非图灵完备"名存实亡。
    lint_err(
        &format!("{HEAD}preflight:\n  - assert: \"exec('rm -rf /') == 0\"\n"),
        &["未知探针函数", "exec"],
    );
    // 白名单内的照常放行。
    let ok = parse::blueprint_from_str(&format!(
        "{HEAD}preflight:\n  - assert: \"port_owner(9000) == ''\"\n"
    ))
    .unwrap();
    assert!(lint::errors(&lint::lint(&ok)).is_empty());
}

#[test]
fn fact_consumed_without_producer_is_caught() {
    lint_err(
        &format!(
            "{HEAD}procedures:\n  boot:\n    steps:\n      - shell: {{ cmd: \"${{facts.join}} x\", check: t }}\n"
        ),
        &["facts.join", "无人导出"],
    );
}

#[test]
fn bare_shell_without_check_is_flagged_as_modelling_debt() {
    let bp = parse::blueprint_from_str(&format!(
        "{HEAD}procedures:\n  boot:\n    steps:\n      - shell: \"do-something\"\n"
    ))
    .unwrap();
    let diags = lint::lint(&bp);
    // 接住,不羞辱:是 warn 不是 error —— 但必须可见。
    assert!(lint::errors(&diags).is_empty(), "裸 shell 不该阻断部署");
    let warns = diags.iter().filter(|d| d.severity == lint::Severity::Warn).count();
    assert!(warns >= 1);
    assert!(diags.iter().any(|d| d.msg.contains("模型化欠债")));
}

#[test]
fn custom_type_pointing_at_a_missing_procedure_is_caught() {
    lint_err(
        &format!(
            "{HEAD}types:\n  - name: thing\n    observe: {{ cmd: \"test -f /x\" }}\n    apply: nowhere\nresources:\n  - thing: {{}}\n"
        ),
        &["nowhere", "不存在的 procedure"],
    );
}

#[test]
fn duplicate_material_without_a_flavor_condition_is_caught() {
    // 同名物料只在带 `when:` 时才是合法的多架构/多 flavor 变体。
    lint_err(
        "name: t\nmaterials:\n  - { name: bin, file: \"http://a\" }\n  - { name: bin, file: \"http://b\" }\n",
        &["同名物料重复"],
    );
}

// ---------------------------------------------------------------- 解析期错误

#[test]
fn two_module_keys_in_one_entry_is_rejected() {
    // 最容易犯的形状错误:把模块参数写在了模块外面。
    parse_err(
        &format!("{HEAD}resources:\n  - shell: \"cmd\"\n    check: \"probe\"\n"),
        &["个模块 key", "只允许一个"],
    );
}

#[test]
fn entry_without_any_module_key_is_rejected() {
    parse_err(
        &format!("{HEAD}resources:\n  - on: all\n"),
        &["没有模块名"],
    );
}

#[test]
fn unknown_top_level_field_is_rejected_with_a_suggestion() {
    parse_err("name: t\nresource:\n  - file: {}\n", &["未知字段", "resources"]);
}

#[test]
fn legacy_stage_apply_is_rejected_by_name() {
    // 七名词里 apply 是动词;参数分期只有 build / deploy(裁定 E)。
    parse_err(
        "name: t\nparams:\n  vip: { stage: apply }\n",
        &["stage: apply", "stage: deploy"],
    );
}

#[test]
fn freeform_shorthand_only_where_declared() {
    // `- shell: "cmd"` 合法;`- copy: "…"` 无意义,应报错而不是猜。
    assert!(parse::blueprint_from_str(&format!("{HEAD}resources:\n  - shell: \"x\"\n")).is_ok());
    parse_err(
        &format!("{HEAD}resources:\n  - copy: \"/etc/x\"\n"),
        &["没有自由形式短写法"],
    );
}

#[test]
fn bad_selector_is_rejected_at_parse_time() {
    parse_err(
        &format!("{HEAD}resources:\n  - file: {{ path: /x, state: directory }}\n    on: controlplane\n"),
        &["无法解析 selector", "role.X"],
    );
}

#[test]
fn material_without_exactly_one_source_is_rejected() {
    parse_err("name: t\nmaterials:\n  - { name: a }\n", &["恰好一个来源 key"]);
    parse_err(
        "name: t\nmaterials:\n  - { name: a, file: \"http://x\", image: y }\n",
        &["来源 key 出现了多个"],
    );
}

#[test]
fn custom_type_must_have_an_observe_probe() {
    // observe 是五动词里唯一强制的一个 —— 没有它,plan 与 drift 都无从谈起。
    parse_err(
        &format!("{HEAD}types:\n  - name: thing\n    apply: boot\n"),
        &["缺少 `observe:`"],
    );
}

#[test]
fn cel_syntax_error_points_at_the_column() {
    let e = parse_err(
        &format!("{HEAD}resources:\n  - file: {{ path: \"${{params.port +}}\", state: directory }}\n"),
        &["Syntax error"],
    );
    assert!(e.contains('^'), "CEL 错误应带位置标记:{e}");
}

// ---------------------------------------------------------------- E310:值位置只许名词

#[test]
fn the_exact_mistake_we_made_is_now_caught() {
    // D-115 的 blueprint 里真实出现过这一行 —— 逻辑进了字符串,
    // 而这正是整套设计声称要消灭的东西。现在它过不了 lint。
    lint_err(
        &format!(
            "{HEAD}resources:\n  - shell: {{ cmd: \"kubeadm init ${{params.port > 0 ? 'a' : 'b'}}\", check: t }}\n"
        ),
        &["E310", "条件属于 `when:`", "flags"],
    );
}

#[test]
fn every_flavour_of_smuggled_logic_is_refused_with_a_targeted_hint() {
    // 报错要给**针对性**改写方向 —— 泛泛一句"请用结构化写法"帮不上忙。
    let cases = [
        ("\"${params.port == 9000}\"", "比较与布尔运算属于 `when:`"),
        ("\"${size(params.port)}\"", "函数调用属于 `when:`"),
        ("\"${params.port[0]}\"", "要遍历列表请用 `each:`"),
    ];
    for (expr, hint) in cases {
        lint_err(
            &format!("{HEAD}resources:\n  - file: {{ path: {expr}, state: directory }}\n"),
            &["E310", hint],
        );
    }
}

#[test]
fn plain_references_and_mixed_literals_stay_legal() {
    // 限权只针对运算,不针对插值本身 —— 常规写法一个都不能误伤。
    let bp = parse::blueprint_from_str(&format!(
        "{HEAD}resources:\n  - file: {{ path: \"/data/${{params.port}}\", state: directory }}\n  \
         - file: {{ path: \"${{item}}\", state: directory }}\n    each: [\"/a\", \"/b\"]\n"
    ))
    .unwrap();
    assert!(
        lint::errors(&lint::lint(&bp)).is_empty(),
        "{:#?}",
        lint::lint(&bp)
    );
}

#[test]
fn conditions_keep_full_cel_expressiveness() {
    // A4 的另一半:`when:` 位置一切照旧 —— 这正是留用 CEL 而非自造语言的意义。
    let bp = parse::blueprint_from_str(&format!(
        "{HEAD}resources:\n  - file: {{ path: /x, state: directory }}\n    \
         when: \"params.port > 1024 && has(params.port)\"\n"
    ))
    .unwrap();
    assert!(lint::errors(&lint::lint(&bp)).is_empty(), "{:#?}", lint::lint(&bp));
}
