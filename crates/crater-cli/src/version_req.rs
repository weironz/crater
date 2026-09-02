//! 版本范围:`yq:4.*`、`yq:^4.44`、`oci://reg/ns/yq:~1.2`。
//!
//! 为什么要有:装包时钉死一个补丁号(`4.44.3`)意味着每次上游发版都要改一次
//! 蓝图;而完全不写版本又会跟着 latest 漂。范围是这两者之间那个能用的档位 ——
//! "4.44 这条线上的最新",升级由 registry 上有什么决定,而不是由谁记得改。
//!
//! helm 有这个(`helm pull --version '0.0.*'`),而且它是
//! <https://github.com/helm/helm/issues/11000> 里点名的用例之一。
//!
//! **建在 `pkg::semver_key` 上,不引 `semver` crate。** 理由是真实 registry 上
//! 的 tag 不都是严格 semver:`1`、`4.40`、`v1.2`、`1.31.0-rc1` 都常见,而
//! `semver::Version::parse` 对前两个直接失败。`semver_key` 是宽松的 —— 缺的
//! 段补 0,解不出的段记 -1 —— 拿它做比较,不认识的 tag 会**排在后面**而不是
//! 让整次解析崩掉。
//!
//! 支持的写法(以及**刻意不支持**的):
//!
//! | 写法 | 含义 |
//! | --- | --- |
//! | `4.44.3` | 精确 |
//! | `*` / 空 | 任意(即最新) |
//! | `4.*` / `4.44.*` | 前缀通配 |
//! | `^4.44.3` | >=4.44.3,< 5.0.0(不跨主版本) |
//! | `~4.44.3` | >=4.44.3,< 4.45.0(不跨次版本) |
//! | `>=4.44`、`>4.44`、`<=5`、`<5` | 比较 |
//! | 逗号或空格分隔 | **全部**满足(`>=4.44, <5`) |
//!
//! 不支持 `||`(或)。它要的是一棵表达式树,而至今没有一个真实需求 —— 加了
//! 就要一直背着。真需要时再加,那时它会有一个具体的例子。

use crate::pkg::semver_key;

/// 一条版本范围。`matches` 判断某个 tag 合不合。
#[derive(Debug, Clone)]
pub(crate) struct VersionReq {
    parts: Vec<Predicate>,
}

#[derive(Debug, Clone)]
enum Predicate {
    Any,
    /// 精确相等 —— 比的是**原字符串**,不是 key。
    ///
    /// 用 key 比会让 `4.44` 与 `4.44.0` 相等,而它们在 registry 上是两个不同
    /// 的 tag,拉下来是两份不同的字节。"精确"就该是精确。
    Exact(String),
    /// 前缀通配:`4.44.*` → 段前缀 `[4, 44]`
    Prefix(Vec<i64>),
    /// `>= lo` 且 `< hi`,用于 `^` 与 `~`
    Between {
        lo: Vec<i64>,
        hi: Vec<i64>,
    },
    Cmp(Ord2, Vec<i64>),
}

#[derive(Debug, Clone, Copy)]
enum Ord2 {
    Ge,
    Gt,
    Le,
    Lt,
}

impl VersionReq {
    /// 解析一条范围。语法错(如 `^` 后面没东西)会**报错**,不会退化成
    /// "精确匹配这串字符" —— 后者的表现是"仓库里没有 `^`",而真正的原因是
    /// 范围写错了。
    pub(crate) fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() || s == "*" {
            return Ok(Self {
                parts: vec![Predicate::Any],
            });
        }
        let mut parts = Vec::new();
        for raw in s.split([',', ' ']).filter(|x| !x.trim().is_empty()) {
            parts.push(Predicate::parse(raw.trim())?);
        }
        if parts.is_empty() {
            return Err(format!("`{s}` 解不出任何版本条件"));
        }
        Ok(Self { parts })
    }

    /// 这条范围是不是"就一个精确版本"。
    ///
    /// 用处很实际:精确版本可以直接拿去当 tag 用,不必先去 registry 列一遍
    /// 版本 —— 少一次网络往返,而且在只给了 pull 权限、列不了 tag 的仓库上
    /// 仍然能用。
    pub(crate) fn as_exact(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [Predicate::Exact(v)] => Some(v),
            _ => None,
        }
    }

    pub(crate) fn matches(&self, version: &str) -> bool {
        self.parts.iter().all(|p| p.matches(version))
    }

    /// 从一堆版本里挑**最高的合格者**。
    ///
    /// 自己排序,不假设入参已排好 —— `tags/list` 返回的顺序是 registry 说了算,
    /// 而 Docker Hub 与 zot 给的就不一样。依赖调用方排过序,是一种在换了
    /// registry 之后才会暴露的错。
    pub(crate) fn best<'a, I: IntoIterator<Item = &'a str>>(&self, versions: I) -> Option<String> {
        versions
            .into_iter()
            .filter(|v| self.matches(v))
            .max_by_key(|v| semver_key(v))
            .map(str::to_string)
    }
}

impl Predicate {
    fn parse(s: &str) -> Result<Self, String> {
        let bad = |what: &str| Err(format!("版本范围 `{s}` {what}"));
        if let Some(rest) = s.strip_prefix(">=") {
            return num(rest)
                .map(|k| Self::Cmp(Ord2::Ge, k))
                .ok_or_else(|| format!("版本范围 `{s}`:`>=` 后面要跟一个版本号"));
        }
        if let Some(rest) = s.strip_prefix("<=") {
            return num(rest)
                .map(|k| Self::Cmp(Ord2::Le, k))
                .ok_or_else(|| format!("版本范围 `{s}`:`<=` 后面要跟一个版本号"));
        }
        if let Some(rest) = s.strip_prefix('>') {
            return num(rest)
                .map(|k| Self::Cmp(Ord2::Gt, k))
                .ok_or_else(|| format!("版本范围 `{s}`:`>` 后面要跟一个版本号"));
        }
        if let Some(rest) = s.strip_prefix('<') {
            return num(rest)
                .map(|k| Self::Cmp(Ord2::Lt, k))
                .ok_or_else(|| format!("版本范围 `{s}`:`<` 后面要跟一个版本号"));
        }
        if let Some(rest) = s.strip_prefix('^') {
            // ^4.44.3 → [4.44.3, 5.0.0)。第一个**非零**段决定上界,这是 semver
            // 的规矩:0.x 里次版本就是破坏性边界,^0.2.3 不该放行 0.3.0。
            let lo = num(rest).ok_or("^ 后面要跟一个版本号")?;
            let hi = bump_at(&lo, first_nonzero(&lo));
            return Ok(Self::Between { lo, hi });
        }
        if let Some(rest) = s.strip_prefix('~') {
            // ~4.44.3 → [4.44.3, 4.45.0):只放行补丁号
            let lo = num(rest).ok_or("~ 后面要跟一个版本号")?;
            let hi = bump_at(&lo, 1.min(lo.len().saturating_sub(1)));
            return Ok(Self::Between { lo, hi });
        }
        if let Some(pfx) = s.strip_suffix(".*").or_else(|| s.strip_suffix(".x")) {
            let segs = num(pfx).ok_or_else(|| format!("版本范围 `{s}`:`*` 前面要跟版本号"))?;
            if segs.is_empty() {
                return bad("的前缀是空的");
            }
            return Ok(Self::Prefix(segs));
        }
        if s.contains('*') {
            // `4.*.3` 这种中缀通配没有实现,而**静默当成精确匹配**会表现为
            // "仓库里没有 4.*.3" —— 一条把人引向仓库的错误。
            return bad("只支持结尾的 `.*`(如 `4.44.*`),不支持中间的 `*`");
        }
        Ok(Self::Exact(s.to_string()))
    }

    fn matches(&self, v: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Exact(want) => want == v,
            Self::Prefix(pfx) => {
                let k = core_segs(v);
                pfx.len() <= k.len() && k[..pfx.len()] == pfx[..]
            }
            Self::Between { lo, hi } => {
                let k = semver_key(v);
                k >= semver_key(&join(lo)) && k < semver_key(&join(hi))
            }
            Self::Cmp(o, bound) => {
                let k = semver_key(v);
                let b = semver_key(&join(bound));
                match o {
                    Ord2::Ge => k >= b,
                    Ord2::Gt => k > b,
                    Ord2::Le => k <= b,
                    Ord2::Lt => k < b,
                }
            }
        }
    }
}

/// `"4.44.3"` → `[4, 44, 3]`;有一段不是数字就整体作废(返回 `None`)。
fn num(s: &str) -> Option<Vec<i64>> {
    let s = s.trim().trim_start_matches('v');
    if s.is_empty() {
        return None;
    }
    s.split('.').map(|p| p.parse::<i64>().ok()).collect()
}

/// tag 的核心段(丢掉 `-rc1` 这类后缀),用于前缀通配。
fn core_segs(v: &str) -> Vec<i64> {
    let v = v.trim_start_matches('v');
    let core = match v.find(['-', '+', '_']) {
        Some(i) => &v[..i],
        None => v,
    };
    core.split('.')
        .map(|p| p.parse::<i64>().unwrap_or(-1))
        .collect()
}

fn join(segs: &[i64]) -> String {
    segs.iter()
        .map(|x| x.to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// 第几段是第一个非零 —— `^` 的上界从这里抬。
fn first_nonzero(segs: &[i64]) -> usize {
    segs.iter().position(|&x| x != 0).unwrap_or(0)
}

/// 把第 `i` 段加一、后面清零:`([4,44,3], 0)` → `[5,0,0]`。
fn bump_at(segs: &[i64], i: usize) -> Vec<i64> {
    let mut out = segs.to_vec();
    if out.is_empty() {
        return vec![1];
    }
    let i = i.min(out.len() - 1);
    out[i] += 1;
    for x in out.iter_mut().skip(i + 1) {
        *x = 0;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(req: &str, v: &str) -> bool {
        VersionReq::parse(req).unwrap().matches(v)
    }

    /// issue helm#11000 里的原例:`0.0.*`。
    #[test]
    fn the_wildcard_from_the_helm_issue_works() {
        assert!(m("0.0.*", "0.0.17"));
        assert!(m("0.0.*", "0.0.1"));
        assert!(!m("0.0.*", "0.1.2"));
        assert!(m("0.1.*", "0.1.2"));
    }

    #[test]
    fn a_prefix_wildcard_matches_by_segment_not_by_string() {
        // 字符串前缀会让 `4.4.*` 命中 `4.44.0` —— 段比较不会
        assert!(m("4.4.*", "4.4.9"));
        assert!(!m("4.4.*", "4.44.0"));
        assert!(m("4.*", "4.44.3"));
        assert!(!m("4.*", "5.0.0"));
    }

    #[test]
    fn caret_does_not_cross_the_major() {
        assert!(m("^4.44.3", "4.44.3"));
        assert!(m("^4.44.3", "4.45.0"));
        assert!(m("^4.44.3", "4.99.99"));
        assert!(!m("^4.44.3", "5.0.0"));
        assert!(!m("^4.44.3", "4.44.2"));
    }

    /// semver 的规矩:0.x 里次版本就是破坏性边界。
    /// `^0.2.3` 放行 0.2.9 但**不**放行 0.3.0 —— 写错这条会让人在 0.x 的库上
    /// 自动升到一个不兼容的版本。
    #[test]
    fn caret_on_a_zero_major_pins_the_minor() {
        assert!(m("^0.2.3", "0.2.9"));
        assert!(!m("^0.2.3", "0.3.0"));
        assert!(m("^0.0.3", "0.0.3"));
        assert!(!m("^0.0.3", "0.1.0"));
    }

    #[test]
    fn tilde_only_allows_the_patch_to_move() {
        assert!(m("~4.44.3", "4.44.9"));
        assert!(!m("~4.44.3", "4.45.0"));
        assert!(!m("~4.44.3", "4.44.2"));
    }

    #[test]
    fn comparators_can_be_combined_with_and() {
        assert!(m(">=4.44, <5", "4.44.3"));
        assert!(!m(">=4.44, <5", "5.0.1"));
        assert!(!m(">=4.44, <5", "4.43.9"));
        // 空格分隔与逗号等价
        assert!(m(">=4.44 <5", "4.99.0"));
    }

    /// 精确匹配比的是**原字符串**:`4.44` 与 `4.44.0` 在 registry 上是两个
    /// 不同的 tag,拉下来是两份不同的字节。
    #[test]
    fn exact_compares_the_tag_text_not_the_parsed_number() {
        assert!(m("4.44.0", "4.44.0"));
        assert!(!m("4.44", "4.44.0"));
        assert!(!m("4.44.0", "4.44"));
    }

    #[test]
    fn star_or_empty_means_any() {
        assert!(m("*", "4.44.3"));
        assert!(m("", "whatever"));
        assert!(VersionReq::parse("*").unwrap().as_exact().is_none());
    }

    /// 精确版本要能被认出来 —— 那样可以省掉一次列 tag 的网络往返,而且在
    /// 只给了 pull 权限的仓库上仍然能用。
    #[test]
    fn an_exact_version_is_recognised_as_such() {
        assert_eq!(
            VersionReq::parse("4.44.3").unwrap().as_exact(),
            Some("4.44.3")
        );
        assert!(VersionReq::parse("^4.44").unwrap().as_exact().is_none());
        assert!(VersionReq::parse("4.*").unwrap().as_exact().is_none());
    }

    /// 挑最高的合格者,而且**自己排序** —— registry 返回的 tag 顺序不可信。
    #[test]
    fn best_picks_the_highest_match_regardless_of_input_order() {
        let tags = ["4.9.0", "4.44.3", "4.10.0", "5.0.0", "4.44.1"];
        assert_eq!(
            VersionReq::parse("4.*").unwrap().best(tags).as_deref(),
            Some("4.44.3")
        );
        assert_eq!(
            VersionReq::parse("*").unwrap().best(tags).as_deref(),
            Some("5.0.0")
        );
        // 字典序会挑 4.9.0
        assert_eq!(
            VersionReq::parse("<4.44").unwrap().best(tags).as_deref(),
            Some("4.10.0")
        );
        assert_eq!(VersionReq::parse("9.*").unwrap().best(tags), None);
    }

    /// 不认识的 tag(`latest`、`probe`)不能让整次解析崩掉,也不该被挑中。
    #[test]
    fn unparseable_tags_are_ignored_not_fatal() {
        let tags = ["latest", "probe", "4.44.3", "stable"];
        assert_eq!(
            VersionReq::parse("4.*").unwrap().best(tags).as_deref(),
            Some("4.44.3")
        );
        // 精确匹配仍然认得出它们 —— 那是合法的 tag 名
        assert!(m("latest", "latest"));
    }

    /// 写错的范围要**报错**,不能退化成"精确匹配这串字符" —— 后者的表现是
    /// "仓库里没有 `^`",而真正的原因是范围写错了。
    #[test]
    fn a_malformed_range_is_an_error_not_a_literal_match() {
        assert!(VersionReq::parse("^").is_err());
        assert!(VersionReq::parse(">=").is_err());
        assert!(VersionReq::parse("4.*.3").is_err(), "中缀通配应当报错");
        assert!(VersionReq::parse("^abc").is_err());
    }
}
