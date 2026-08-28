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
        // CEL 把运算符也算函数(`_+_` / `_==_`),内建算子不参与白名单校验。
        let funcs = refs
            .functions()
            .into_iter()
            .filter(|f| !f.starts_with('_') && !f.starts_with('@'))
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
