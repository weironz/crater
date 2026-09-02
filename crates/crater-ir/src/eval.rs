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
use std::sync::Arc;

use crate::expr::{CelExpr, Part, Template};
use crate::ir::{Args, Value};

/// 跑一条探针命令;没有 prober 就**明确报错**,不编造返回值。
fn run(p: &Option<Prober>, cmd: &str) -> Result<String, cel::ExecutionError> {
    match p {
        Some(f) => f(cmd).map_err(|e| cel::ExecutionError::FunctionError {
            function: "probe".into(),
            message: e,
        }),
        None => Err(cel::ExecutionError::FunctionError {
            function: "probe".into(),
            message: "此上下文没有目标机可探(lint / 构建期):探针函数在这里用不了".into(),
        }),
    }
}

/// shell 单引号转义 —— 探针参数来自蓝图,不能直接拼进命令行。
fn shq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// 在目标机上跑一条**只读**命令 → stdout。
///
/// CEL 的探针函数(`port_owner(9000)` 之类)靠它落地。做成 `Arc<dyn Fn>`
/// 而不是把 `&dyn Ctx` 塞进 `Scope`,是被 `cel::Context::add_function` 的
/// `'static + Send + Sync` 约束逼出来的 —— 借用引用进不去(D-134)。
///
/// 语义上也对:探测本来就是「这台机器」的能力,而 `Scope` 正是 per-host 的。
pub type Prober = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

pub type Yaml = serde_yaml::Value;
/// 求值后的参数:类型已定,可直接喂给资源类型。
pub type ResolvedArgs = BTreeMap<String, Yaml>;

/// 一次求值可见的全部名字(与 lint 的 ROOT_SCOPES 一一对应)。
#[derive(Clone, Default)]
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
    /// 这台机器的只读探测能力 —— CEL 探针函数用(D-134)。
    ///
    /// `None` = 这个上下文探不了(lint、单测、构建期烘焙)。那时探针函数
    /// 会**明确报错**而不是返回一个编出来的值:说不清就说不清。
    pub prober: Option<Prober>,
}

impl std::fmt::Debug for Scope {
    /// 手写 Debug:`prober` 是个闭包,没法 derive,而它对排障也没有意义 ——
    /// 有没有探测能力用一个词就说清了。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scope")
            .field("params", &self.params)
            .field("substrate", &self.substrate)
            .field("env", &self.env)
            .field("facts", &self.facts)
            .field("item", &self.item)
            .field("host", &self.host)
            .field("prober", &if self.prober.is_some() { "yes" } else { "no" })
            .finish()
    }
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
        self.add_probes(&mut ctx);
        Ok(ctx)
    }

    /// 注册**封闭集合**里的探针函数(D-117/A、D-134)。
    ///
    /// 四个都只读,全部走同一个 [`Prober`]。没有 prober 时**照样注册** ——
    /// 让它们返回一句"此上下文探不了",比留一个 `Undeclared reference` 好:
    /// 后者看起来像"函数名写错了",而真相是"这里探不了"。
    fn add_probes(&self, ctx: &mut cel::Context<'_>) {
        use cel::Value as V;
        let probe = self.prober.clone();

        // 谁在监听这个端口 —— 空串 = 没人。
        //
        // 返回**服务名**而不是布尔,是 rustfs 裁定 A 定下的:语义是"没被
        // 别人占",而不是"端口空闲" —— 后者在已部署机器上重跑必然失败,
        // 与幂等承诺冲突。
        let p = probe.clone();
        ctx.add_function("port_owner", move |port: i64| -> Result<V, cel::ExecutionError> {
            // `ss` 的 users:(("nginx",pid=…)) 里抠出服务名。用 sed 而不是
            // grep -oE 链:后者要嵌套两层引号,在 Rust 字符串里几乎写不对。
            let out = run(&p, &format!(
                r#"ss -lntpH 'sport = :{port}' 2>/dev/null | sed -n 's/.*users:(("\([^"]*\)".*/\1/p' | head -1"#
            ))?;
            Ok(V::String(out.trim().to_string().into()))
        });

        let p = probe.clone();
        ctx.add_function("path_exists", move |path: Arc<String>| -> Result<V, cel::ExecutionError> {
            let out = run(&p, &format!("test -e {} && echo yes || echo no", shq(&path)))?;
            Ok(V::Bool(out.trim() == "yes"))
        });

        let p = probe.clone();
        ctx.add_function("cmd_ok", move |cmd: Arc<String>| -> Result<V, cel::ExecutionError> {
            // 退出码而不是输出:`cmd_ok` 问的是"成不成",不是"说了什么"。
            let out = run(&p, &format!("if {cmd} >/dev/null 2>&1; then echo yes; else echo no; fi"))?;
            Ok(V::Bool(out.trim() == "yes"))
        });

        let p = probe;
        ctx.add_function("service_state", move |name: Arc<String>| -> Result<V, cel::ExecutionError> {
            let out = run(&p, &format!(
                "systemctl is-active {} 2>/dev/null || echo unknown", shq(&name)
            ))?;
            Ok(V::String(out.trim().to_string().into()))
        });
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
