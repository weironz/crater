//! 参数契约(values schema)—— Blueprint 对外的**唯一**输入面。
//!
//! 对标 Helm `values.schema.json` / Terraform `variables.tf`:操作者只面对它,不看资源。
//! rustfs 试金石裁定 D/E:必须有 `type`(旧版把列表压成"空格分隔字符串")、`secret`
//! (孪生视图/日志/API 自动打码)、`stage: build|deploy`(旧名 `apply` 与七名词的动词冲突)。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 参数在哪个阶段生效。build 期参数会被**烤进** OCI 制品(改它必须重新 build);
/// deploy 期参数是环境配置,永远不进制品(承接 D-093 的 gate 分治)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Build,
    #[default]
    Deploy,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    #[default]
    String,
    Int,
    Bool,
    /// 语义化字符串类型:lint 期做格式校验,错的 IP/CIDR 不必等到部署才发现。
    Ip,
    Cidr,
    Version,
    Port,
    List(Box<ParamType>),
    Enum(Vec<String>),
}

impl ParamType {
    /// YAML 里三种写法:`type: int` / `type: [string]` / `type: {enum: [...]}`。
    pub fn from_yaml(v: &serde_yaml::Value) -> Result<Self, String> {
        match v {
            serde_yaml::Value::String(s) => match s.as_str() {
                "string" => Ok(ParamType::String),
                "int" => Ok(ParamType::Int),
                "bool" => Ok(ParamType::Bool),
                "ip" => Ok(ParamType::Ip),
                "cidr" => Ok(ParamType::Cidr),
                "version" => Ok(ParamType::Version),
                "port" => Ok(ParamType::Port),
                other => Err(format!("未知参数类型 `{other}`")),
            },
            serde_yaml::Value::Sequence(items) if items.len() == 1 => {
                Ok(ParamType::List(Box::new(ParamType::from_yaml(&items[0])?)))
            }
            serde_yaml::Value::Mapping(m) => {
                let vals = m
                    .get(serde_yaml::Value::String("enum".into()))
                    .and_then(|v| v.as_sequence())
                    .ok_or("map 形式的 type 只支持 `{enum: [...]}`")?;
                Ok(ParamType::Enum(
                    vals.iter().map(crate::parse::scalar_to_string).collect(),
                ))
            }
            _ => Err("type 应为字符串、单元素列表或 {enum: [...]}".into()),
        }
    }

    /// 校验一个具体值是否符合本类型(lint 校验 default,plan 期校验实参)。
    pub fn check(&self, v: &serde_yaml::Value) -> Result<(), String> {
        match self {
            ParamType::String => v.as_str().map(|_| ()).ok_or_else(|| "期望 string".into()),
            ParamType::Int => v.as_i64().map(|_| ()).ok_or_else(|| "期望 int".into()),
            ParamType::Bool => v.as_bool().map(|_| ()).ok_or_else(|| "期望 bool".into()),
            ParamType::Port => match v.as_i64() {
                Some(p) if (1..=65535).contains(&p) => Ok(()),
                Some(p) => Err(format!("端口 {p} 越界(1-65535)")),
                None => Err("期望 port(int)".into()),
            },
            ParamType::Ip => check_ip(v),
            ParamType::Cidr => check_cidr(v),
            ParamType::Version => v
                .as_str()
                .filter(|s| s.chars().next().is_some_and(|c| c.is_ascii_digit()))
                .map(|_| ())
                .ok_or_else(|| "期望 version(以数字开头,如 1.36.1)".into()),
            ParamType::List(inner) => {
                let seq = v.as_sequence().ok_or("期望列表")?;
                for (i, item) in seq.iter().enumerate() {
                    inner.check(item).map_err(|e| format!("第 {i} 项:{e}"))?;
                }
                Ok(())
            }
            ParamType::Enum(allowed) => {
                let s = v.as_str().ok_or("期望 enum(字符串)")?;
                allowed
                    .iter()
                    .any(|a| a == s)
                    .then_some(())
                    .ok_or_else(|| format!("`{s}` 不在 [{}] 内", allowed.join(", ")))
            }
        }
    }
}

fn check_ip(v: &serde_yaml::Value) -> Result<(), String> {
    let s = v.as_str().ok_or("期望 ip(字符串)")?;
    s.parse::<std::net::IpAddr>()
        .map(|_| ())
        .map_err(|_| format!("`{s}` 不是合法 IP"))
}

fn check_cidr(v: &serde_yaml::Value) -> Result<(), String> {
    let s = v.as_str().ok_or("期望 cidr(字符串)")?;
    let (addr, bits) = s
        .split_once('/')
        .ok_or_else(|| format!("`{s}` 缺少 /前缀长度"))?;
    let ip: std::net::IpAddr = addr.parse().map_err(|_| format!("`{addr}` 不是合法 IP"))?;
    let max = if ip.is_ipv4() { 32 } else { 128 };
    match bits.parse::<u8>() {
        Ok(b) if b <= max => Ok(()),
        _ => Err(format!("前缀长度 `{bits}` 越界(0-{max})")),
    }
}

/// 一个声明的参数。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamSpec {
    pub name: String,
    pub ty: ParamType,
    pub default: Option<serde_yaml::Value>,
    pub required: bool,
    /// 敏感值:孪生视图 / Run 日志 / API 响应里自动打码(裁定 D)。
    pub secret: bool,
    pub stage: Stage,
    pub desc: Option<String>,
}

/// 参数表(有序,便于 `inspect` 稳定输出)。
pub type Params = BTreeMap<String, ParamSpec>;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_yaml::Value as Y;

    fn y(s: &str) -> Y {
        serde_yaml::from_str(s).unwrap()
    }

    #[test]
    fn parses_scalar_list_and_enum_types() {
        assert_eq!(ParamType::from_yaml(&y("int")).unwrap(), ParamType::Int);
        assert_eq!(
            ParamType::from_yaml(&y("[string]")).unwrap(),
            ParamType::List(Box::new(ParamType::String))
        );
        assert_eq!(
            ParamType::from_yaml(&y("{enum: [a, b]}")).unwrap(),
            ParamType::Enum(vec!["a".into(), "b".into()])
        );
        assert!(ParamType::from_yaml(&y("frobnicate")).is_err());
    }

    #[test]
    fn semantic_types_catch_bad_defaults_at_lint_time() {
        assert!(ParamType::Ip.check(&y("192.168.73.14")).is_ok());
        assert!(ParamType::Ip.check(&y("192.168.73.999")).is_err());
        assert!(ParamType::Cidr.check(&y("10.244.0.0/16")).is_ok());
        assert!(ParamType::Cidr.check(&y("10.244.0.0/48")).is_err());
        assert!(ParamType::Cidr.check(&y("10.244.0.0")).is_err());
        assert!(ParamType::Port.check(&y("9000")).is_ok());
        assert!(ParamType::Port.check(&y("70000")).is_err());
    }

    #[test]
    fn list_type_reports_offending_index() {
        let err = ParamType::List(Box::new(ParamType::Int))
            .check(&y("[1, \"x\"]"))
            .unwrap_err();
        assert!(err.contains("第 1 项"), "got: {err}");
    }

    #[test]
    fn enum_rejects_unlisted_value() {
        let t = ParamType::Enum(vec!["control-plane".into(), "worker".into()]);
        assert!(t.check(&y("worker")).is_ok());
        assert!(t.check(&y("etcd")).is_err());
    }
}
