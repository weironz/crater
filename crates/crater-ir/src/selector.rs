//! Selector —— **定址**:一条声明作用在哪些 substrate 上(k8s 试金石裁定 A)。
//!
//! 取代 ansible / 旧 crater 的 `when_role:` + `run_once:` 组合。那套组合表达不了
//! 「除首台之外的其余 control-plane」——旧 k8s-ha 只能"全组跑 + check 守卫跳过首台",
//! 一行代码配三行注释。选择器直接说人话:`rest(role.controlplane)`。
//!
//! ```text
//! all | role.<ident> | host.<ident> | first(<sel>) | rest(<sel>) | <sel> where <cel>
//! ```
//! `when:` 与之正交:selector 管**在谁身上**,`when:` 管**要不要做**。

use crate::expr::CelExpr;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Selector {
    /// 全部 substrate(默认)。
    #[default]
    All,
    /// 属于某组/角色。
    Role(String),
    /// 指名某台。
    Host(String),
    /// 子集的第一台(稳定序:inventory 声明序)——取代 `run_once`。
    First(Box<Selector>),
    /// 子集去掉第一台之后的其余(HA join 场景的关键)。
    Rest(Box<Selector>),
    /// 子集上再加 CEL 过滤(`substrate.*` 事实可用)。
    Where(Box<Selector>, CelExpr),
}

impl Selector {
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("selector 为空".into());
        }
        // `where` 优先级最低,且在顶层(不在括号内)才算分隔。
        if let Some(idx) = find_top_level_where(s) {
            let base = &s[..idx];
            let cond = s[idx + "where".len()..].trim();
            if cond.is_empty() {
                return Err("`where` 后缺少条件表达式".into());
            }
            let expr = CelExpr::compile(cond).map_err(|e| format!("where 条件:{e}"))?;
            return Ok(Selector::Where(Box::new(Selector::parse(base)?), expr));
        }
        for (kw, wrap) in [
            ("first", Selector::First as fn(Box<Selector>) -> Selector),
            ("rest", Selector::Rest as fn(Box<Selector>) -> Selector),
        ] {
            if let Some(inner) = strip_call(s, kw) {
                return Ok(wrap(Box::new(Selector::parse(inner)?)));
            }
        }
        if s == "all" {
            return Ok(Selector::All);
        }
        if let Some(r) = s.strip_prefix("role.") {
            return ident(r).map(Selector::Role);
        }
        if let Some(h) = s.strip_prefix("host.") {
            return ident(h).map(Selector::Host);
        }
        Err(format!(
            "无法解析 selector `{s}`;可用形式:all / role.X / host.X / first(sel) / rest(sel) / sel where <cel>"
        ))
    }

    /// 递归收集其中 CEL 表达式(供作用域 lint)。
    pub fn exprs(&self) -> Vec<&CelExpr> {
        match self {
            Selector::All | Selector::Role(_) | Selector::Host(_) => vec![],
            Selector::First(i) | Selector::Rest(i) => i.exprs(),
            Selector::Where(i, e) => {
                let mut v = i.exprs();
                v.push(e);
                v
            }
        }
    }

    /// 引用到的组名(供 lint 校验 environment 是否定义了这些组)。
    pub fn roles(&self) -> Vec<&str> {
        match self {
            Selector::Role(r) => vec![r.as_str()],
            Selector::All | Selector::Host(_) => vec![],
            Selector::First(i) | Selector::Rest(i) => i.roles(),
            Selector::Where(i, _) => i.roles(),
        }
    }
}

impl std::fmt::Display for Selector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Selector::All => write!(f, "all"),
            Selector::Role(r) => write!(f, "role.{r}"),
            Selector::Host(h) => write!(f, "host.{h}"),
            Selector::First(i) => write!(f, "first({i})"),
            Selector::Rest(i) => write!(f, "rest({i})"),
            Selector::Where(i, e) => write!(f, "{i} where {}", e.src()),
        }
    }
}

fn ident(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.is_empty() || !s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
        return Err(format!("非法组/主机名 `{s}`"));
    }
    Ok(s.to_string())
}

/// `first( ... )` → 内层;要求括号在整串首尾配平。
fn strip_call<'a>(s: &'a str, kw: &str) -> Option<&'a str> {
    let rest = s.strip_prefix(kw)?.trim_start();
    let inner = rest.strip_prefix('(')?;
    let inner = inner.strip_suffix(')')?;
    // 确认这对括号是最外层配对(排除 `first(a)+first(b)` 这种畸形)。
    let mut depth = 0;
    for c in inner.chars() {
        match c {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    return None;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    (depth == 0).then_some(inner)
}

/// 顶层(括号外、引号外)的 ` where ` 位置。
fn find_top_level_where(s: &str) -> Option<usize> {
    let b: Vec<char> = s.chars().collect();
    let (mut depth, mut quote) = (0i32, None::<char>);
    let mut byte = 0usize;
    for (i, &c) in b.iter().enumerate() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'') | (None, '"') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, 'w') if depth == 0 => {
                let word: String = b[i..].iter().take(5).collect();
                let prev_space = i > 0 && b[i - 1].is_whitespace();
                if prev_space && word == "where" && b.get(i + 5).is_some_and(|c| c.is_whitespace()) {
                    return Some(byte);
                }
            }
            _ => {}
        }
        byte += c.len_utf8();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_forms() {
        assert_eq!(Selector::parse("all").unwrap(), Selector::All);
        assert_eq!(
            Selector::parse("role.controlplane").unwrap(),
            Selector::Role("controlplane".into())
        );
        assert_eq!(Selector::parse("host.n11").unwrap(), Selector::Host("n11".into()));
    }

    #[test]
    fn parses_first_and_rest() {
        let s = Selector::parse("first(role.controlplane)").unwrap();
        assert_eq!(s, Selector::First(Box::new(Selector::Role("controlplane".into()))));
        assert_eq!(s.roles(), vec!["controlplane"]);
        assert!(matches!(Selector::parse("rest(role.cp)").unwrap(), Selector::Rest(_)));
    }

    #[test]
    fn parses_where_clause() {
        let s = Selector::parse("role.worker where substrate.arch == 'amd64'").unwrap();
        match &s {
            Selector::Where(inner, e) => {
                assert_eq!(**inner, Selector::Role("worker".into()));
                assert!(e.roots().contains("substrate"));
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(s.exprs().len(), 1);
    }

    #[test]
    fn where_inside_first_still_parses() {
        let s = Selector::parse("first(role.cp where substrate.name != 'n0')").unwrap();
        assert!(matches!(s, Selector::First(_)));
        assert_eq!(s.roles(), vec!["cp"]);
    }

    #[test]
    fn round_trips_via_display() {
        for src in ["all", "role.cp", "first(role.cp)", "rest(host.n1)"] {
            assert_eq!(Selector::parse(src).unwrap().to_string(), src);
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(Selector::parse("controlplane").is_err());
        assert!(Selector::parse("").is_err());
        assert!(Selector::parse("role.cp where").is_err());
        assert!(Selector::parse("first(role.cp)+first(role.w)").is_err());
    }
}
