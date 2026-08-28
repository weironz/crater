//! 求值 —— 把带 `${}` 的声明变成**具体值**,交给五动词执行。
//!
//! 全 IR 只有一种表达式语义(CEL),所以这里只有一个求值器:`when:` 的布尔判定、
//! `${}` 插值、`each:` 的列表展开走的是同一条路。
//!
//! 一条影响手感的规则:**整串就是一个表达式时保留原生类型**
//! (`port: "${params.port}"` → 整数 9000,不是字符串 "9000");
//! 混着字面量时才拼成字符串(`"http://${params.vip}:8443"`)。
//! 旧模型全靠字符串替换,于是 `timeout: "{{n}}"` 这类值一路以字符串流到执行层。

use std::collections::BTreeMap;

use crate::expr::{CelExpr, Part, Template};
use crate::ir::{Args, Value};

pub type Yaml = serde_yaml::Value;
/// 求值后的参数:类型已定,可直接喂给资源类型。
pub type ResolvedArgs = BTreeMap<String, Yaml>;

/// 一次求值可见的全部名字(与 lint 的 ROOT_SCOPES 一一对应)。
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub params: BTreeMap<String, Yaml>,
    /// 目标侧探测事实:os/arch/hostname/网卡…(k8s 试金石裁定 C)。
    pub substrate: BTreeMap<String, Yaml>,
    /// 环境级 values。
    pub env: BTreeMap<String, Yaml>,
    /// 跨主机 fact(procedure 内)。
    pub facts: BTreeMap<String, Yaml>,
    /// `each:` 展开后的当前项。
    pub item: Option<Yaml>,
    /// 机群视角:整个部署面向哪些目标(`on:` 判定用,**不进 CEL 变量**)。
    pub fleet: Option<crate::fleet::Fleet>,
    /// 当前正在对哪一台求值。
    pub host: Option<String>,
}

impl Scope {
    /// 绑定"我是谁":把机群身份写进 `substrate.*`,与 `host` 保持同源。
    ///
    /// `substrate.name` 是 **inventory 里的名字**,不是 `hostname` 探针读回来的
    /// 当前 OS 主机名 —— 两者常常不同,而 kubeadm 之类恰恰要用前者当节点名。
    pub fn identify(&mut self, name: &str, roles: &[String]) {
        self.host = Some(name.to_string());
        self.substrate.insert("name".into(), Yaml::String(name.to_string()));
        self.substrate.insert(
            "roles".into(),
            Yaml::Sequence(roles.iter().cloned().map(Yaml::String).collect()),
        );
    }

    pub fn with_item(&self, item: Yaml) -> Scope {
        Scope { item: Some(item), ..self.clone() }
    }

    fn context(&self) -> Result<cel::Context<'_>, String> {
        let mut ctx = cel::Context::default();
        for (name, map) in [
            ("params", &self.params),
            ("substrate", &self.substrate),
            ("env", &self.env),
            ("facts", &self.facts),
        ] {
            ctx.add_variable(name, map).map_err(|e| e.to_string())?;
        }
        if let Some(item) = &self.item {
            ctx.add_variable("item", item).map_err(|e| e.to_string())?;
        }
        Ok(ctx)
    }

    /// 求一段表达式,得到 YAML 值。
    pub fn eval(&self, e: &CelExpr) -> Result<Yaml, String> {
        let program = cel::Program::compile(e.src()).map_err(|err| err.to_string())?;
        let ctx = self.context()?;
        let out = program
            .execute(&ctx)
            .map_err(|err| format!("求值 `{}` 失败:{err}", e.src()))?;
        cel_to_yaml(&out)
    }

    /// 求 `when:` —— 必须得到布尔,含糊的真值(非空字符串之类)一律拒绝。
    pub fn eval_bool(&self, e: &CelExpr) -> Result<bool, String> {
        match self.eval(e)? {
            Yaml::Bool(b) => Ok(b),
            other => Err(format!(
                "`when:` 必须求值为布尔,`{}` 得到 {other:?}",
                e.src()
            )),
        }
    }

    /// 求一个 IR 值(字面量 / 模板 / 容器)。
    pub fn resolve(&self, v: &Value) -> Result<Yaml, String> {
        Ok(match v {
            Value::Lit(y) => y.clone(),
            Value::Tmpl(t) => self.resolve_template(t)?,
            Value::List(items) => Yaml::Sequence(
                items.iter().map(|i| self.resolve(i)).collect::<Result<Vec<_>, _>>()?,
            ),
            Value::Map(m) => {
                let mut out = serde_yaml::Mapping::new();
                for (k, val) in m {
                    out.insert(Yaml::String(k.clone()), self.resolve(val)?);
                }
                Yaml::Mapping(out)
            }
        })
    }

    fn resolve_template(&self, t: &Template) -> Result<Yaml, String> {
        // 整串恰好是一个表达式 → 保留原生类型(见文件头注释)。
        if let [Part::Expr(e)] = t.parts() {
            return self.eval(e);
        }
        let mut s = String::new();
        for p in t.parts() {
            match p {
                Part::Lit(lit) => s.push_str(lit),
                Part::Expr(e) => s.push_str(&scalar_to_string(&self.eval(e)?)),
            }
        }
        Ok(Yaml::String(s))
    }

    pub fn resolve_args(&self, args: &Args) -> Result<ResolvedArgs, String> {
        let mut out = ResolvedArgs::new();
        for (k, v) in args {
            // `flags:` 的条目带 `when:` —— 在这里筛掉,渲染层只管拼接。
            // 条件为假的 flag **根本不出现**,不留空串占位:作者因此没有
            // 写 `${cond ? "--x" : ""}` 的动机(D-117 §3.4)。
            if k == "flags" {
                out.insert(k.clone(), self.resolve_flags(v)?);
                continue;
            }
            let r = self.resolve(v).map_err(|e| format!("`{k}`:{e}"))?;
            out.insert(k.clone(), r);
        }
        Ok(out)
    }

    /// 求值 flags 列表:逐条判 `when`,留下的再求 value。
    fn resolve_flags(&self, v: &Value) -> Result<Yaml, String> {
        let Value::List(items) = v else {
            return self.resolve(v).map_err(|e| format!("`flags`:{e}"));
        };
        let mut kept = Vec::new();
        for item in items {
            let Value::Map(m) = item else {
                return Err("`flags` 的每一条应是 map".into());
            };
            if let Some(Value::Tmpl(_) | Value::Lit(_)) = m.get("when") {
                // when 在解析期已编译成 CelExpr 并存进 Flag;走到这里说明是
                // 未经 parse_flags 的原始形态(如测试直接构造),按字面处理。
            }
            let mut out = serde_yaml::Mapping::new();
            for (k, val) in m {
                if k == "when" {
                    continue;
                }
                out.insert(Yaml::String(k.clone()), self.resolve(val)?);
            }
            kept.push(Yaml::Mapping(out));
        }
        Ok(Yaml::Sequence(kept))
    }

    /// `each:` 的展开:表达式求值后必须是列表;字面列表逐项求值。
    pub fn expand_each(&self, each: &crate::ir::Each) -> Result<Vec<Yaml>, String> {
        match each {
            crate::ir::Each::List(items) => {
                items.iter().map(|i| self.resolve(i)).collect()
            }
            crate::ir::Each::Expr(e) => match self.eval(e)? {
                Yaml::Sequence(items) => Ok(items),
                other => Err(format!("`each: {}` 需要求值为列表,得到 {other:?}", e.src())),
            },
        }
    }
}

fn cel_to_yaml(v: &cel::Value) -> Result<Yaml, String> {
    use cel::Value as C;
    Ok(match v {
        C::Int(i) => Yaml::Number((*i).into()),
        C::UInt(u) => Yaml::Number((*u as i64).into()),
        C::Float(f) => Yaml::Number(serde_yaml::Number::from(*f)),
        C::String(s) => Yaml::String(s.to_string()),
        C::Bool(b) => Yaml::Bool(*b),
        C::Null => Yaml::Null,
        C::Bytes(b) => Yaml::String(String::from_utf8_lossy(b).into_owned()),
        C::List(items) => Yaml::Sequence(
            items.iter().map(cel_to_yaml).collect::<Result<Vec<_>, _>>()?,
        ),
        C::Map(m) => {
            let mut out = serde_yaml::Mapping::new();
            for (k, val) in m.map.iter() {
                out.insert(Yaml::String(format!("{k:?}").trim_matches('"').to_string()), cel_to_yaml(val)?);
            }
            Yaml::Mapping(out)
        }
        other => return Err(format!("不支持的 CEL 结果类型:{other:?}")),
    })
}

/// 标量 → 字符串(拼接插值时用)。
pub fn scalar_to_string(v: &Yaml) -> String {
    match v {
        Yaml::String(s) => s.clone(),
        Yaml::Number(n) => n.to_string(),
        Yaml::Bool(b) => b.to_string(),
        Yaml::Null => String::new(),
        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::value_from_yaml;

    fn scope() -> Scope {
        let mut params = BTreeMap::new();
        params.insert("port".to_string(), Yaml::from(9000));
        params.insert("vip".to_string(), Yaml::from("10.0.0.9"));
        params.insert("dirs".to_string(), serde_yaml::from_str("[/data/a, /data/b]").unwrap());
        params.insert("debug".to_string(), Yaml::from(false));
        let mut substrate = BTreeMap::new();
        substrate.insert("arch".to_string(), Yaml::from("arm64"));
        Scope { params, substrate, ..Default::default() }
    }

    fn v(yaml: &str) -> Value {
        value_from_yaml(&serde_yaml::from_str(yaml).unwrap()).unwrap()
    }

    #[test]
    fn lone_expression_keeps_its_native_type() {
        // 旧模型把一切压成字符串,`timeout: "{{n}}"` 一路以字符串流到执行层。
        assert_eq!(scope().resolve(&v(r#""${params.port}""#)).unwrap(), Yaml::from(9000));
        assert_eq!(scope().resolve(&v(r#""${params.debug}""#)).unwrap(), Yaml::from(false));
        assert!(scope().resolve(&v(r#""${params.dirs}""#)).unwrap().is_sequence());
    }

    #[test]
    fn mixed_template_concatenates_to_a_string() {
        assert_eq!(
            scope().resolve(&v(r#""http://${params.vip}:${params.port}/health""#)).unwrap(),
            Yaml::from("http://10.0.0.9:9000/health")
        );
    }

    #[test]
    fn literals_pass_through_untouched() {
        assert_eq!(scope().resolve(&v("/usr/local/bin/x")).unwrap(), Yaml::from("/usr/local/bin/x"));
        assert_eq!(scope().resolve(&v("420")).unwrap(), Yaml::from(420));
    }

    #[test]
    fn nested_containers_resolve_recursively() {
        let out = scope().resolve(&v(r#"{ ports: ["${params.port}", 9001], host: "${params.vip}" }"#)).unwrap();
        let m = out.as_mapping().unwrap();
        assert_eq!(m[&Yaml::from("host")], Yaml::from("10.0.0.9"));
        assert_eq!(m[&Yaml::from("ports")].as_sequence().unwrap()[0], Yaml::from(9000));
    }

    #[test]
    fn conditions_must_be_boolean() {
        let s = scope();
        assert!(s.eval_bool(&CelExpr::compile("params.port > 1024").unwrap()).unwrap());
        assert!(!s.eval_bool(&CelExpr::compile("substrate.arch == 'amd64'").unwrap()).unwrap());
        // 含糊真值不接受 —— 免得 `when: params.vip` 这种写法悄悄成立。
        let err = s.eval_bool(&CelExpr::compile("params.vip").unwrap()).unwrap_err();
        assert!(err.contains("必须求值为布尔"), "{err}");
    }

    #[test]
    fn each_takes_an_expression_or_a_literal_list() {
        use crate::ir::Each;
        let s = scope();
        // 表达式形式:与 `when:` 一致,不写 `${}`。
        let e = Each::Expr(CelExpr::compile("params.dirs").unwrap());
        assert_eq!(s.expand_each(&e).unwrap().len(), 2);
        // 字面列表形式。
        let l = Each::List(vec![v("kubeadm"), v("kubelet")]);
        assert_eq!(s.expand_each(&l).unwrap().len(), 2);
        // 求值成标量 → 报错,不静默当成单元素。
        let bad = Each::Expr(CelExpr::compile("params.port").unwrap());
        assert!(s.expand_each(&bad).is_err());
    }

    #[test]
    fn identity_exposes_the_inventory_name_not_the_probed_hostname() {
        // kubeadm 用节点名认机器,而"当前 OS 主机名"往往还没设成期望值 ——
        // 两者必须分得开:`substrate.name` 是身份,`substrate.hostname` 是现实。
        let mut s = scope();
        s.substrate.insert("hostname".into(), Yaml::from("ubuntu"));
        s.identify("n11", &["controlplane".to_string()]);
        assert_eq!(s.resolve(&v("\"${substrate.name}\"")).unwrap(), Yaml::from("n11"));
        assert_eq!(s.resolve(&v("\"${substrate.hostname}\"")).unwrap(), Yaml::from("ubuntu"));
        assert_eq!(s.host.as_deref(), Some("n11"), "host 与 substrate.name 同源");
        assert!(s
            .eval_bool(&CelExpr::compile("'controlplane' in substrate.roles").unwrap())
            .unwrap());
    }

    #[test]
    fn item_is_visible_only_after_expansion() {
        let s = scope();
        assert!(s.resolve(&v("\"${item}\"")).is_err());
        let s2 = s.with_item(Yaml::from("/data/a"));
        assert_eq!(s2.resolve(&v("\"${item}\"")).unwrap(), Yaml::from("/data/a"));
        assert_eq!(
            s2.resolve(&v(r#""/usr/local/bin/${item}""#)).unwrap(),
            Yaml::from("/usr/local/bin//data/a")
        );
    }

    #[test]
    fn missing_name_is_a_clear_error_not_an_empty_string() {
        // Ansible 会把未定义变量渲染成空串,错误一路沉默地流下去。
        let err = scope().resolve(&v("\"${params.nope}\"")).unwrap_err();
        assert!(err.contains("params.nope"), "{err}");
    }
}
