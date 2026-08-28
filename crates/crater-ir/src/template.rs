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
/// 唯一的差别是 `fleet`(机群视角)不进来:它是定址用的,渲染里没有意义。
fn ctx(scope: &Scope) -> BTreeMap<&'static str, Yaml> {
    let mut m = BTreeMap::new();
    m.insert("params", map_to_yaml(&scope.params));
    m.insert("substrate", map_to_yaml(&scope.substrate));
    m.insert("env", map_to_yaml(&scope.env));
    m.insert("facts", map_to_yaml(&scope.facts));
    if let Some(item) = &scope.item {
        m.insert("item", item.clone());
    }
    m
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
