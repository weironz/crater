//! 模板渲染 —— 在**控制端**把模板变成字节。
//!
//! 为什么在控制端而不是目标机:目标机零依赖是硬约束(D-103)。渲染完成后
//! 模板与 `copy` 再无区别 —— 同样按内容寻址判幂等,同样能在 plan 期说清
//! "这个文件会不会变"。**这正是渲染必须发生在 observe 之前的原因**:
//! 渲染结果的摘要是控制端事实,拿得到它,`template` 的 diff 才不是 `?`。
//!
//! 语法用 minijinja(Jinja2 兼容),不是新发明的 —— 写过 Ansible/Helm 的人
//! 直接就会,这在"人来写"这件事上比语法优雅重要得多。

use std::collections::BTreeMap;

use minijinja::{Environment, UndefinedBehavior};

use crate::eval::{Scope, Yaml};

/// 渲染一份模板文本。
///
/// **未定义变量是错误,不是空串。** Jinja 默认把 `{{ typo }}` 悄悄渲染成空 ——
/// 那会生成一份语法上合法、语义上错误的配置文件,然后服务带着它启动。
/// 宁可在控制端炸,也不要把这种文件推到机器上。
pub fn render(text: &str, scope: &Scope) -> anyhow::Result<String> {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    // Jinja2 默认吃掉结尾换行 —— 对网页片段无所谓,对**文件**是实打实的差异:
    // 同一份源经 `copy` 和经 `template` 会产出不同字节,内容寻址随之失真。
    env.set_keep_trailing_newline(true);
    env.add_template("t", text)
        .map_err(|e| anyhow::anyhow!("模板语法错误:{}", chain(&e)))?;
    let tmpl = env.get_template("t").expect("刚加进去");
    tmpl.render(ctx(scope))
        .map_err(|e| anyhow::anyhow!("模板渲染失败:{}", chain(&e)))
}

/// 暴露给模板的变量面 —— **与 CEL 条件同一套名字**。
///
/// 作者不该记两套:`when: params.ha` 和 `{{ params.ha }}` 指的是同一个东西。
///
/// `fleet` 也在里面。早期版本刻意把它排除在外,理由是"fleet 是定址用的,
/// 渲染里没有意义" —— 那个判断是**错的**,而且是被真机打回来的:haproxy 的
/// 后端列表、etcd 的 peer 列表、任何"这一台要知道其余台是谁"的配置,
/// 恰恰只能从机群视角渲染。没有它,这类文件根本无法声明式地写出来。
fn ctx(scope: &Scope) -> BTreeMap<&'static str, Yaml> {
    let mut m = BTreeMap::new();
    m.insert("params", map_to_yaml(&scope.params));
    m.insert("substrate", map_to_yaml(&scope.substrate));
    m.insert("env", map_to_yaml(&scope.env));
    m.insert("facts", map_to_yaml(&scope.facts));
    if let Some(item) = &scope.item {
        m.insert("item", item.clone());
    }
    if let Some(fleet) = &scope.fleet {
        m.insert("fleet", fleet_to_yaml(fleet));
    }
    m
}

/// 机群按**组名**索引:`fleet.controlplane` 是一个有序成员列表,每项有
/// `name` / `address` / `roles` / `vars`。
///
/// 按组索引而不是给一个扁平列表,是因为模板里真正要问的问题就是
/// "controlplane 有哪几台" —— 让作者自己 filter 一遍既啰嗦又容易写错。
/// 顺序取成员在 inventory 里的顺序:配置文件的行序应当可复现。
fn fleet_to_yaml(fleet: &crate::fleet::Fleet) -> Yaml {
    let mut by_group: BTreeMap<String, Vec<Yaml>> = BTreeMap::new();
    for role in &fleet.declared_roles {
        by_group.insert(role.clone(), Vec::new());
    }
    for m in &fleet.members {
        let mut entry = serde_yaml::Mapping::new();
        entry.insert(Yaml::from("name"), Yaml::String(m.name.clone()));
        entry.insert(Yaml::from("address"), Yaml::String(m.address.clone()));
        entry.insert(
            Yaml::from("roles"),
            Yaml::Sequence(m.roles.iter().map(|r| Yaml::String(r.clone())).collect()),
        );
        entry.insert(Yaml::from("vars"), map_to_yaml(
            &m.vars.iter().map(|(k, v)| (k.clone(), Yaml::String(v.clone()))).collect(),
        ));
        let y = Yaml::Mapping(entry);
        for role in &m.roles {
            by_group.entry(role.clone()).or_default().push(y.clone());
        }
    }
    Yaml::Mapping(
        by_group
            .into_iter()
            .map(|(k, v)| (Yaml::String(k), Yaml::Sequence(v)))
            .collect(),
    )
}

fn map_to_yaml(m: &BTreeMap<String, Yaml>) -> Yaml {
    Yaml::Mapping(
        m.iter()
            .map(|(k, v)| (Yaml::String(k.clone()), v.clone()))
            .collect(),
    )
}

/// minijinja 把"哪一行出错"放在 source 链里,只打最外层等于扔掉定位信息。
fn chain(e: &minijinja::Error) -> String {
    let mut s = e.to_string();
    let mut cur: &dyn std::error::Error = e;
    while let Some(next) = cur.source() {
        s.push_str(&format!(" ← {next}"));
        cur = next;
    }
    if let Some(line) = e.line() {
        s.push_str(&format!("(第 {line} 行)"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> Scope {
        let mut s = Scope::default();
        s.params.insert("port".into(), Yaml::from(8080));
        s.params.insert("ha".into(), Yaml::from(true));
        s.params
            .insert("peers".into(), serde_yaml::from_str("[a, b, c]").unwrap());
        s.substrate.insert("name".into(), Yaml::from("n1"));
        s
    }

    #[test]
    fn a_template_sees_the_same_names_as_a_cel_condition() {
        // 作者不该记两套名字:`when: params.ha` 与 `{{ params.ha }}` 同源。
        let out = render("listen {{ params.port }} on {{ substrate.name }}", &scope()).unwrap();
        assert_eq!(out, "listen 8080 on n1");
    }

    #[test]
    fn loops_and_conditionals_work_as_in_jinja2() {
        // 写过 Ansible/Helm 的人直接就会 —— 这比语法优雅重要。
        let out = render(
            "{% for p in params.peers %}{{ p }}{% if not loop.last %},{% endif %}{% endfor %}",
            &scope(),
        )
        .unwrap();
        assert_eq!(out, "a,b,c");
    }

    #[test]
    fn a_typo_in_a_variable_name_is_an_error_not_an_empty_string() {
        // 默认 Jinja 会把 `{{ prot }}` 渲染成空串,生成一份语法合法、语义错误的
        // 配置,然后服务带着它启动。宁可在控制端炸。
        let err = render("listen {{ params.prot }}", &scope()).unwrap_err().to_string();
        assert!(err.contains("渲染失败"), "{err}");
    }

    #[test]
    fn a_syntax_error_names_the_line() {
        let err = render("ok\nok\n{% if %}", &scope()).unwrap_err().to_string();
        assert!(err.contains("语法错误") && err.contains("3 行"), "{err}");
    }

    #[test]
    fn rendering_is_deterministic_so_content_addressing_holds() {
        // 幂等的全部前提:同样的输入渲染出同样的字节。
        let t = "{% for p in params.peers %}{{ p }}={{ params.port }}\n{% endfor %}";
        assert_eq!(render(t, &scope()).unwrap(), render(t, &scope()).unwrap());
    }

    #[test]
    fn the_trailing_newline_survives() {
        // Jinja2 默认吃掉它。对文件来说这是实打实的差异:同一份源经 `copy`
        // 和经 `template` 会产出不同字节,内容寻址随之失真。
        let raw = "[Unit]\nDescription=x\n";
        assert_eq!(render(raw, &scope()).unwrap(), raw);
    }
}

#[cfg(test)]
mod fleet_ctx_tests {
    use super::*;
    use crate::fleet::{Fleet, Member};

    fn scope_with_fleet() -> Scope {
        let mut vars = BTreeMap::new();
        vars.insert("ip".to_string(), "10.0.0.11".to_string());
        let fleet = Fleet::new(vec![
            Member::new("cp1", &["controlplane"]).with_vars(vars),
            Member::new("cp2", &["controlplane"]).with_address("10.0.0.12"),
            Member::new("w1", &["worker"]).with_address("10.0.0.21"),
        ]);
        let mut s = Scope { fleet: Some(fleet), ..Default::default() };
        s.params.insert("port".into(), Yaml::from(6443));
        s
    }

    #[test]
    fn a_template_can_render_a_backend_list_from_the_fleet() {
        // 这是把 fleet 放进渲染上下文的**全部理由**:haproxy 后端、etcd peer
        // 列表这类配置,只能从机群视角写出来。早期版本把 fleet 排除在外,
        // 于是这类文件根本无法声明式表达 —— 是真机部署把它打回来的。
        let out = render(
            "{% for n in fleet.controlplane %}server {{ n.name }} {{ n.address }}:{{ params.port }}\n{% endfor %}",
            &scope_with_fleet(),
        )
        .unwrap();
        assert_eq!(out, "server cp1 10.0.0.11:6443\nserver cp2 10.0.0.12:6443\n");
    }

    #[test]
    fn host_vars_ip_overrides_the_connection_address() {
        // 走跳板/隧道时,inventory 的 address 是**控制端视角**,对同伴毫无意义。
        // 把 127.0.0.1 写进 apiserver 后端会得到一个谁都连不上的集群。
        let mut vars = BTreeMap::new();
        vars.insert("ip".to_string(), "192.168.73.11".to_string());
        let m = Member::new("cp1", &["controlplane"])
            .with_address("127.0.0.1")
            .with_vars(vars);
        assert_eq!(m.address, "192.168.73.11");
    }

    #[test]
    fn groups_are_indexed_by_name_so_the_author_need_not_filter() {
        let out = render("{{ fleet.worker | length }}/{{ fleet.controlplane | length }}", &scope_with_fleet()).unwrap();
        assert_eq!(out, "1/2");
    }

    #[test]
    fn member_order_follows_the_inventory_so_configs_are_reproducible() {
        // 配置文件的行序必须可复现,否则每次 apply 都会"内容变了"。
        let s = scope_with_fleet();
        let t = "{% for n in fleet.controlplane %}{{ n.name }},{% endfor %}";
        assert_eq!(render(t, &s).unwrap(), "cp1,cp2,");
        assert_eq!(render(t, &s).unwrap(), render(t, &s).unwrap());
    }

    #[test]
    fn a_scope_without_a_fleet_simply_has_no_fleet_variable() {
        // 单机语境(如 crater plan --local)不该因为引入 fleet 而报错;
        // 用到它的模板会在 strict 模式下明确失败,那正是想要的。
        let err = render("{{ fleet.controlplane }}", &Scope::default()).unwrap_err().to_string();
        assert!(err.contains("渲染失败"), "{err}");
    }

    #[test]
    fn host_vars_are_reachable_for_anything_beyond_the_address() {
        let mut vars = BTreeMap::new();
        vars.insert("rack".to_string(), "r2".to_string());
        let fleet = Fleet::new(vec![Member::new("n1", &["db"]).with_vars(vars)]);
        let s = Scope { fleet: Some(fleet), ..Default::default() };
        assert_eq!(render("{{ fleet.db[0].vars.rack }}", &s).unwrap(), "r2");
    }
}
