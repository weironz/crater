//! 表达式层:全 IR 只有**一种**表达式语义 —— CEL(裁定见 ir-draft §4)。
//!
//! `when:` 与 `${...}` 插值走同一个求值器:一个 `${expr}` 就是一段 CEL。CEL 非图灵完备
//! (无循环、无自定义函数、求值有上界),因此可在 **lint 期**完成语法与作用域检查 ——
//! 这正是 D-036「YAML 是数据」想要、而 Jinja2 给不了的东西。

use std::collections::BTreeSet;

/// 一段已通过语法检查的 CEL 表达式(保留原文用于报错与再序列化)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CelExpr {
    src: String,
    /// 根变量名(如 `params` / `substrate` / `item` / `facts`),供作用域 lint。
    roots: BTreeSet<String>,
    /// 调用到的函数名,供**封闭探针白名单**校验(裁定 A:CEL 可调的只读探针是封闭集合,
    /// 否则"非图灵完备"就被自定义函数偷偷破坏了)。
    funcs: BTreeSet<String>,
}

impl CelExpr {
    /// 编译并抽取根变量;语法错误带位置信息返回。
    pub fn compile(src: &str) -> Result<Self, String> {
        let program = cel::Program::compile(src).map_err(|e| e.to_string())?;
        let refs = program.references();
        let roots = refs.variables().into_iter().map(|s| s.to_string()).collect();
        // CEL 把运算符也算函数:中缀是 `_+_` / `_==_`,**前缀是 `!_` / `-_`**。
        // 早先只挡了 `_` 与 `@` 开头,于是 `!path_exists(x)` 里的逻辑非被报成
        // 「未知探针函数 `!_()`」—— 任何在 `when:` 里写 `!` 的蓝图都过不了 lint。
        //
        // 改成**只保留合法标识符**:运算符的名字里必然带 `_` 占位或符号,
        // 而真正的函数名一定是标识符。按形状判定,不用穷举算子表。
        let funcs = refs
            .functions()
            .into_iter()
            .filter(|f| is_ident(f))
            .map(|s| s.to_string())
            .collect();
        Ok(CelExpr { src: src.to_string(), roots, funcs })
    }

    pub fn src(&self) -> &str {
        &self.src
    }
    pub fn roots(&self) -> &BTreeSet<String> {
        &self.roots
    }
    pub fn funcs(&self) -> &BTreeSet<String> {
        &self.funcs
    }

    /// 这段表达式是不是一个**纯引用**(标识符 + 点路径),没有任何运算。
    ///
    /// 这是 D-117/A4 的核心限权:`when:` 可以写完整 CEL,但 `${}` **插值位置只许名词**。
    /// 病灶从来不是"CEL 能写三元",而是我们允许完整 CEL 出现在值位置 ——
    /// 条件属于 `when:`,值位置只该有名词。这是**位置**问题,不是**语言**问题,
    /// 所以修法是一条 lint 规则,而不是换掉整门语言(见 authoring-dsl-v1.md §A4)。
    ///
    /// 判据基于**源码形状**而非 CEL AST:纯引用的字符序列只能由标识符字符与点构成。
    /// 这样连 `params.a+params.b`、`params.a[0]`、`f(x)`、`?:`、字面量、字符串拼接
    /// 全部一次拦下,且不依赖 cel crate 是否暴露 AST。
    pub fn is_pure_ref(&self) -> bool {
        let src = self.src.trim();
        if src.is_empty() || src.starts_with('.') || src.ends_with('.') || src.contains("..") {
            return false;
        }
        src.split('.').all(is_ident)
    }
}

/// 合法标识符:字母或下划线开头,其后字母/数字/下划线。
///
/// 刻意**不允许连字符** —— CEL 里 `a-b` 是减法,允许它会让"纯引用"与"算术"
/// 在源码形状上无法区分,限权就失守了。物料名等需要连字符的地方走引用位而非表达式。
fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 一个字符串字面量里的插值片段序列:`"http://${params.vip}:8443"`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Lit(String),
    Expr(CelExpr),
}

impl Template {
    /// 扫描 `${...}`(支持嵌套花括号,如 `${ {'a':1}.a }`),其余为字面量。
    /// `$${` 转义出一个字面 `${`。
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut parts = Vec::new();
        let mut lit = String::new();
        let b: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < b.len() {
            if b[i] == '$' && i + 1 < b.len() && b[i + 1] == '$' {
                // `$${` → 字面 `${`;单独的 `$$` 也折叠成一个 `$`。
                lit.push('$');
                i += 2;
                continue;
            }
            if b[i] == '$' && i + 1 < b.len() && b[i + 1] == '{' {
                let mut depth = 1;
                let mut j = i + 2;
                let mut expr = String::new();
                while j < b.len() && depth > 0 {
                    match b[j] {
                        '{' => {
                            depth += 1;
                            expr.push('{');
                        }
                        '}' => {
                            depth -= 1;
                            if depth > 0 {
                                expr.push('}');
                            }
                        }
                        c => expr.push(c),
                    }
                    j += 1;
                }
                if depth != 0 {
                    return Err(format!("插值未闭合:`${{{expr}`"));
                }
                if !lit.is_empty() {
                    parts.push(Part::Lit(std::mem::take(&mut lit)));
                }
                parts.push(Part::Expr(CelExpr::compile(expr.trim())?));
                i = j;
                continue;
            }
            lit.push(b[i]);
            i += 1;
        }
        if !lit.is_empty() {
            parts.push(Part::Lit(lit));
        }
        Ok(Template { parts })
    }

    pub fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// 是否含至少一段表达式(纯字面量串不必进求值器)。
    pub fn is_dynamic(&self) -> bool {
        self.parts.iter().any(|p| matches!(p, Part::Expr(_)))
    }

    /// 全部表达式的根变量并集。
    pub fn roots(&self) -> BTreeSet<String> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Expr(e) => Some(e.roots().iter().cloned()),
                Part::Lit(_) => None,
            })
            .flatten()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_and_extracts_roots() {
        let e = CelExpr::compile("params.port > 1024 && substrate.arch == 'amd64'").unwrap();
        assert_eq!(
            e.roots().iter().cloned().collect::<Vec<_>>(),
            vec!["params".to_string(), "substrate".to_string()]
        );
    }

    #[test]
    fn extracts_probe_function_calls_but_not_operators() {
        let e = CelExpr::compile("port_owner(9000) in ['', 'rustfs.service']").unwrap();
        assert!(e.funcs().contains("port_owner"), "funcs={:?}", e.funcs());
        let plain = CelExpr::compile("params.a + params.b").unwrap();
        assert!(plain.funcs().is_empty(), "运算符不该算探针函数: {:?}", plain.funcs());
        // **前缀**运算符也是函数(`!_` / `-_`)—— 早先只挡了 `_`/`@` 开头,
        // 于是任何写了 `!` 的蓝图都被报成「未知探针函数 `!_()`」(D-134)。
        let neg = CelExpr::compile("!path_exists('/x') && -1 < 0").unwrap();
        assert_eq!(
            neg.funcs().iter().map(String::as_str).collect::<Vec<_>>(),
            vec!["path_exists"],
            "前缀运算符不该混进函数名: {:?}",
            neg.funcs()
        );
    }

    #[test]
    fn pure_reference_accepts_only_nouns() {
        // 值位置只许名词 —— 这是 A4 限权的执行者。
        for ok in ["params.port", "item", "facts.arch", "self.role", "exports.join_command"] {
            assert!(CelExpr::compile(ok).unwrap().is_pure_ref(), "{ok} 该被接受");
        }
    }

    #[test]
    fn pure_reference_rejects_every_way_of_smuggling_logic() {
        // 这些正是我们真正写出过、或作者被逼急了会写的形态。
        let smuggles = [
            r#"params.ha ? "--upload-certs" : """#, // 三元 —— D-115 里真实出现过
            "params.a + params.b",                  // 拼接/算术
            "params.port + 1",
            "has(params.x)",                        // 函数(它属于 when,不属于值位)
            "size(params.dirs)",
            "params.dirs[0]",                       // 下标
            "params.a == params.b",                 // 比较
            "'literal'",                            // 字面量
            "params.a || params.b",
            "!params.ha",
        ];
        for bad in smuggles {
            let e = CelExpr::compile(bad).expect("语法本身合法");
            assert!(!e.is_pure_ref(), "`{bad}` 不该被当作纯引用");
        }
    }

    #[test]
    fn pure_reference_rejects_malformed_paths() {
        for bad in ["params.", ".params", "params..port", "params.9lives", ""] {
            // 有些连 CEL 都编译不过;编译得过的必须被形状检查拦下。
            if let Ok(e) = CelExpr::compile(bad) {
                assert!(!e.is_pure_ref(), "`{bad}` 不该被当作纯引用");
            }
        }
    }

    #[test]
    fn a_condition_keeps_full_cel_expressiveness() {
        // 限权只针对插值位;`when:` 位置一切照旧 —— 这正是留用 CEL 的意义。
        let cond = CelExpr::compile("has(params.cp_endpoint) && params.ha || size(params.dirs) > 0")
            .unwrap();
        assert!(!cond.is_pure_ref(), "它当然不是纯引用 —— 但在 when: 里完全合法");
        assert!(cond.roots().contains("params"));
    }

    #[test]
    fn syntax_error_is_reported_not_panicked() {
        let err = CelExpr::compile("params.a +").unwrap_err();
        assert!(err.contains("Syntax error"), "got: {err}");
    }

    #[test]
    fn template_splits_literals_and_exprs() {
        let t = Template::parse("http://${params.vip}:${params.port}/health").unwrap();
        // Lit("http://") Expr Lit(":") Expr Lit("/health")
        assert_eq!(t.parts().len(), 5);
        assert!(t.is_dynamic());
        assert!(t.roots().contains("params"));
    }

    #[test]
    fn template_plain_string_is_static() {
        let t = Template::parse("/usr/local/bin/rustfs").unwrap();
        assert!(!t.is_dynamic());
        assert_eq!(t.parts().len(), 1);
    }

    #[test]
    fn template_handles_nested_braces_and_escape() {
        let t = Template::parse("${ {'a': 1}.a } and $${literal}").unwrap();
        assert!(t.is_dynamic());
        let lits: Vec<_> = t
            .parts()
            .iter()
            .filter_map(|p| match p {
                Part::Lit(s) => Some(s.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(lits, vec![" and ${literal}"]);
    }

    #[test]
    fn unclosed_interpolation_errors() {
        assert!(Template::parse("${params.a").is_err());
    }

    #[test]
    fn bad_expr_inside_template_errors() {
        assert!(Template::parse("x=${params.a +}").is_err());
    }
}
