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

/// `ip -o -4 addr show` 的输出里,哪块网卡持有落在 `cidr` 内的地址。
///
/// 匹配不到返回**空串**,不编一个网卡名出来:编出来的话 keepalived 会起来
/// 但不工作,而那是最难查的一类故障 —— 一切进程都在跑,只是 VIP 不漂。
///
/// 纯函数,所以可测:网段算术的边界(/31、/32、跨字节前缀)不该靠真机试。
fn iface_in_cidr(out: &str, cidr: &str) -> String {
    let Some(net) = parse_cidr(cidr) else {
        return String::new();
    };
    for line in out.lines() {
        // `2: ens33    inet 10.219.111.111/24 brd ... scope global ens33`
        let f: Vec<&str> = line.split_whitespace().collect();
        let (Some(name), Some(addr)) = (
            f.get(1),
            f.iter()
                .position(|x| *x == "inet")
                .and_then(|i| f.get(i + 1)),
        ) else {
            continue;
        };
        let Some((ip, _)) = addr.split_once('/') else {
            continue;
        };
        let Ok(ip) = ip.parse::<std::net::Ipv4Addr>() else {
            continue;
        };
        if in_net(u32::from(ip), net) {
            return (*name).to_string();
        }
    }
    String::new()
}

/// `10.0.0.0/24` → (网络地址, 掩码)。写不出来就返回 None,由调用方报空。
fn parse_cidr(s: &str) -> Option<(u32, u32)> {
    let (a, p) = s.trim().split_once('/')?;
    let base = u32::from(a.parse::<std::net::Ipv4Addr>().ok()?);
    let bits: u32 = p.parse().ok()?;
    if bits > 32 {
        return None;
    }
    // `<< 32` 在 Rust 里是溢出而不是 0 —— /0 要单独处理。
    let mask = if bits == 0 {
        0
    } else {
        u32::MAX << (32 - bits)
    };
    Some((base & mask, mask))
}

fn in_net(ip: u32, (net, mask): (u32, u32)) -> bool {
    ip & mask == net
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
        self.substrate
            .insert("name".into(), Yaml::String(name.to_string()));
        self.substrate.insert(
            "roles".into(),
            Yaml::Sequence(roles.iter().cloned().map(Yaml::String).collect()),
        );
    }

    pub fn with_item(&self, item: Yaml) -> Scope {
        Scope {
            item: Some(item),
            ..self.clone()
        }
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
        ctx.add_function(
            "path_exists",
            move |path: Arc<String>| -> Result<V, cel::ExecutionError> {
                let out = run(
                    &p,
                    &format!("test -e {} && echo yes || echo no", shq(&path)),
                )?;
                Ok(V::Bool(out.trim() == "yes"))
            },
        );

        let p = probe.clone();
        ctx.add_function(
            "cmd_ok",
            move |cmd: Arc<String>| -> Result<V, cel::ExecutionError> {
                // 退出码而不是输出:`cmd_ok` 问的是"成不成",不是"说了什么"。
                let out = run(
                    &p,
                    &format!("if {cmd} >/dev/null 2>&1; then echo yes; else echo no; fi"),
                )?;
                Ok(V::Bool(out.trim() == "yes"))
            },
        );

        // 持有该网段地址的网卡名(D-136)。
        //
        // `substrate.iface` 给的是**默认路由**那块网卡,绝大多数 HA 部署够用
        // (VRRP 是 L2 协议,VIP 与主地址同网段本就是前提)。这个函数补的是
        // 专用 VRRP 网段的情形 —— 那时默认路由网卡是错的答案。
        //
        // 网段计算放在**控制端**:目标机上只跑一句 `ip -o -4 addr show`。
        // 在目标机上算需要 python3 或 ipcalc,而"目标零依赖"是硬约束 ——
        // 为一个网卡名去要求目标装 python,这笔交易不成立。
        let p = probe.clone();
        ctx.add_function(
            "iface_in",
            move |cidr: Arc<String>| -> Result<V, cel::ExecutionError> {
                let out = run(&p, "ip -o -4 addr show 2>/dev/null")?;
                Ok(V::String(iface_in_cidr(&out, &cidr).into()))
            },
        );

        let p = probe;
        ctx.add_function(
            "service_state",
            move |name: Arc<String>| -> Result<V, cel::ExecutionError> {
                let out = run(
                    &p,
                    &format!(
                        "systemctl is-active {} 2>/dev/null || echo unknown",
                        shq(&name)
                    ),
                )?;
                Ok(V::String(out.trim().to_string().into()))
            },
        );
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
                items
                    .iter()
                    .map(|i| self.resolve(i))
                    .collect::<Result<Vec<_>, _>>()?,
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
            crate::ir::Each::List(items) => items.iter().map(|i| self.resolve(i)).collect(),
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
            items
                .iter()
                .map(cel_to_yaml)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        C::Map(m) => {
            let mut out = serde_yaml::Mapping::new();
            for (k, val) in m.map.iter() {
                out.insert(
                    Yaml::String(format!("{k:?}").trim_matches('"').to_string()),
                    cel_to_yaml(val)?,
                );
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
        other => serde_yaml::to_string(other)
            .unwrap_or_default()
            .trim()
            .to_string(),
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
        params.insert(
            "dirs".to_string(),
            serde_yaml::from_str("[/data/a, /data/b]").unwrap(),
        );
        params.insert("debug".to_string(), Yaml::from(false));
        let mut substrate = BTreeMap::new();
        substrate.insert("arch".to_string(), Yaml::from("arm64"));
        Scope {
            params,
            substrate,
            ..Default::default()
        }
    }

    fn v(yaml: &str) -> Value {
        value_from_yaml(&serde_yaml::from_str(yaml).unwrap()).unwrap()
    }

    #[test]
    fn lone_expression_keeps_its_native_type() {
        // 旧模型把一切压成字符串,`timeout: "{{n}}"` 一路以字符串流到执行层。
        assert_eq!(
            scope().resolve(&v(r#""${params.port}""#)).unwrap(),
            Yaml::from(9000)
        );
        assert_eq!(
            scope().resolve(&v(r#""${params.debug}""#)).unwrap(),
            Yaml::from(false)
        );
        assert!(scope()
            .resolve(&v(r#""${params.dirs}""#))
            .unwrap()
            .is_sequence());
    }

    #[test]
    fn mixed_template_concatenates_to_a_string() {
        assert_eq!(
            scope()
                .resolve(&v(r#""http://${params.vip}:${params.port}/health""#))
                .unwrap(),
            Yaml::from("http://10.0.0.9:9000/health")
        );
    }

    #[test]
    fn literals_pass_through_untouched() {
        assert_eq!(
            scope().resolve(&v("/usr/local/bin/x")).unwrap(),
            Yaml::from("/usr/local/bin/x")
        );
        assert_eq!(scope().resolve(&v("420")).unwrap(), Yaml::from(420));
    }

    #[test]
    fn nested_containers_resolve_recursively() {
        let out = scope()
            .resolve(&v(
                r#"{ ports: ["${params.port}", 9001], host: "${params.vip}" }"#,
            ))
            .unwrap();
        let m = out.as_mapping().unwrap();
        assert_eq!(m[&Yaml::from("host")], Yaml::from("10.0.0.9"));
        assert_eq!(
            m[&Yaml::from("ports")].as_sequence().unwrap()[0],
            Yaml::from(9000)
        );
    }

    #[test]
    fn conditions_must_be_boolean() {
        let s = scope();
        assert!(s
            .eval_bool(&CelExpr::compile("params.port > 1024").unwrap())
            .unwrap());
        assert!(!s
            .eval_bool(&CelExpr::compile("substrate.arch == 'amd64'").unwrap())
            .unwrap());
        // 含糊真值不接受 —— 免得 `when: params.vip` 这种写法悄悄成立。
        let err = s
            .eval_bool(&CelExpr::compile("params.vip").unwrap())
            .unwrap_err();
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
        assert_eq!(
            s.resolve(&v("\"${substrate.name}\"")).unwrap(),
            Yaml::from("n11")
        );
        assert_eq!(
            s.resolve(&v("\"${substrate.hostname}\"")).unwrap(),
            Yaml::from("ubuntu")
        );
        assert_eq!(
            s.host.as_deref(),
            Some("n11"),
            "host 与 substrate.name 同源"
        );
        assert!(s
            .eval_bool(&CelExpr::compile("'controlplane' in substrate.roles").unwrap())
            .unwrap());
    }

    #[test]
    fn item_is_visible_only_after_expansion() {
        let s = scope();
        assert!(s.resolve(&v("\"${item}\"")).is_err());
        let s2 = s.with_item(Yaml::from("/data/a"));
        assert_eq!(
            s2.resolve(&v("\"${item}\"")).unwrap(),
            Yaml::from("/data/a")
        );
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

#[cfg(test)]
mod cidr_tests {
    use super::iface_in_cidr;

    const OUT: &str = "\
1: lo    inet 127.0.0.1/8 scope host lo
2: ens33    inet 10.219.111.111/24 brd 10.219.111.255 scope global dynamic ens33
3: ens34    inet 192.168.50.7/28 brd 192.168.50.15 scope global ens34
4: docker0    inet 172.17.0.1/16 brd 172.17.255.255 scope global docker0";

    #[test]
    fn picks_the_interface_holding_an_address_in_that_subnet() {
        assert_eq!(iface_in_cidr(OUT, "10.219.111.0/24"), "ens33");
        // 专用 VRRP 网段:默认路由网卡(ens33)是**错的**答案,这正是
        // 这个函数存在的理由(D-136)。
        assert_eq!(iface_in_cidr(OUT, "192.168.50.0/28"), "ens34");
        assert_eq!(iface_in_cidr(OUT, "172.17.0.0/16"), "docker0");
    }

    #[test]
    fn no_match_yields_empty_not_a_guess() {
        // 编一个网卡名出来的话,keepalived 会起来但不工作 —— 一切进程都在跑,
        // 只是 VIP 不漂,那是最难查的一类故障。
        assert_eq!(iface_in_cidr(OUT, "10.9.9.0/24"), "");
        assert_eq!(iface_in_cidr(OUT, "不是网段"), "");
        assert_eq!(iface_in_cidr(OUT, "10.0.0.0/99"), "");
    }

    #[test]
    fn prefix_arithmetic_holds_at_the_edges() {
        // 前缀边界:.7 在 /29(覆盖 .0-.7)里,不在 /30(.0-.3)里。
        assert_eq!(iface_in_cidr(OUT, "192.168.50.0/29"), "ens34");
        assert_eq!(iface_in_cidr(OUT, "192.168.50.0/30"), "");
        // /32 精确匹配单个地址。
        assert_eq!(iface_in_cidr(OUT, "10.219.111.111/32"), "ens33");
        assert_eq!(iface_in_cidr(OUT, "10.219.111.112/32"), "");
        // /0 匹配一切 —— 第一条(lo)胜出;`<< 32` 是溢出不是 0,单独处理过。
        assert_eq!(iface_in_cidr(OUT, "0.0.0.0/0"), "lo");
    }
}
