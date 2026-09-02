//! 物料闭包的**解析**:名字 → 这台机器上该用哪个变体、从哪儿取。
//!
//! 这是 crater 相对 Ansible 的核心差异化落到代码里的地方:`materials:` 是一份
//! **声明的闭包**,同名多条靠 `when:` 区分成变体(多架构 / 多 flavor),
//! 由**目标侧事实**判定该用哪个 —— 而不是让作者在控制端猜。
//!
//! 本模块只做"解析成一份取用计划";真正取字节(在线下载 / 离线推 blob)是执行层的事,
//! 那里才知道有没有 OCI 闭包。

use crate::eval::{Scope, Yaml};
use crate::ir::{Blueprint, Material, MaterialKind, Value};

/// 一份物料在**当前这台目标**上的取用计划。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaterialPlan {
    pub name: String,
    pub kind: MaterialKind,
    /// `file`:渲染后的 URL(或本地相对路径);`image`:镜像 ref。
    pub source: String,
    /// 声明的内容摘要;有就必须校验(内容寻址是离线可信的根)。
    pub sha256: Option<String>,
    /// 下载物是 zip 时,取其中这个成员作为物料本体。
    pub unzip: Option<String>,
}

#[derive(Debug)]
pub enum ResolveError {
    /// blueprint 里压根没声明这个名字(lint 已能拦,这里是执行期兜底)。
    Undeclared(String),
    /// 声明了,但**没有一个变体**适配这台机器 —— 例如只打了 amd64 却部署到 arm64。
    /// 这必须是响亮的错误:静默跳过会让目标机装上"半套"。
    NoVariant {
        name: String,
        tried: usize,
        hint: String,
    },
    /// 多个变体同时成立 —— 作者的条件写重叠了,无法判定该用哪个。
    Ambiguous {
        name: String,
        count: usize,
    },
    Eval(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Undeclared(n) => write!(f, "物料 `{n}` 未在 materials: 里声明"),
            ResolveError::NoVariant { name, tried, hint } => write!(
                f,
                "物料 `{name}` 的 {tried} 个变体没有一个适配这台机器({hint})—— \
                 闭包不覆盖此环境,拒绝装半套"
            ),
            ResolveError::Ambiguous { name, count } => write!(
                f,
                "物料 `{name}` 有 {count} 个变体同时成立 —— `when:` 条件重叠,无法判定"
            ),
            ResolveError::Eval(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// 选出适配当前 scope(含目标侧事实)的那个变体,并渲染出取用计划。
pub fn resolve(bp: &Blueprint, name: &str, scope: &Scope) -> Result<MaterialPlan, ResolveError> {
    let candidates: Vec<&Material> = bp.materials.iter().filter(|m| m.name == name).collect();
    if candidates.is_empty() {
        return Err(ResolveError::Undeclared(name.to_string()));
    }

    let mut matched: Vec<&Material> = Vec::new();
    for m in &candidates {
        let keep = match &m.when {
            None => true,
            Some(cond) => scope.eval_bool(cond).map_err(ResolveError::Eval)?,
        };
        if keep {
            matched.push(m);
        }
    }

    let chosen = match matched.len() {
        1 => matched[0],
        // 无条件的单一变体是最常见情形,上面已覆盖;这里是"全部条件都不成立"。
        0 => {
            return Err(ResolveError::NoVariant {
                name: name.to_string(),
                tried: candidates.len(),
                hint: describe_substrate(scope),
            })
        }
        n => {
            return Err(ResolveError::Ambiguous {
                name: name.to_string(),
                count: n,
            })
        }
    };

    Ok(MaterialPlan {
        name: chosen.name.clone(),
        kind: chosen.kind,
        source: scope
            .resolve(&chosen.source)
            .map(|v| crate::eval::scalar_to_string(&v))
            .map_err(ResolveError::Eval)?,
        sha256: render_sha(scope, &chosen.sha256)?,
        unzip: chosen.unzip.clone(),
    })
}

/// 摘要与 URL 走**同一条**渲染路径。
///
/// 不这么做的后果很具体:URL 里能写 `${params.version}`,摘要里不能 —— 于是
/// 换版本时 URL 变了而摘要没变,落地必然摘要不符。两者本就是一对(某个版本
/// 的字节 + 那份字节的摘要),只让其中一个可参数化,等于让它们**必然**走散。
///
/// 这也是 `crater pkg push --set version=…` 能真正发出别的版本的前提
/// (issue #25):版本与摘要一起 `--set`,包才是自洽的。
fn render_sha(scope: &Scope, raw: &Option<Value>) -> Result<Option<String>, ResolveError> {
    match raw {
        None => Ok(None),
        Some(v) => scope
            .resolve(v)
            .map(|y| Some(crate::eval::scalar_to_string(&y)))
            .map_err(ResolveError::Eval),
    }
}

/// 报错时告诉作者"这台机器长什么样",否则 `NoVariant` 无从下手。
fn describe_substrate(scope: &Scope) -> String {
    let pick = |k: &str| {
        scope
            .substrate
            .get(k)
            .and_then(Yaml::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("?")
            .to_string()
    };
    format!(
        "arch={} distro={} version={}",
        pick("arch"),
        pick("distro"),
        pick("version")
    )
}

/// blueprint 在当前 scope 下**实际会用到**的全部物料(去重)。
/// build 侧据此决定要烤哪些进 OCI —— 闭包 = f(values × 目标事实)。
pub fn closure(bp: &Blueprint, scope: &Scope) -> Vec<Result<MaterialPlan, ResolveError>> {
    referenced_names(bp)
        .iter()
        .map(|n| resolve(bp, n, scope))
        .collect()
}

/// **烘焙闭包**:blueprint 引用到的物料的**全部变体**。
///
/// 与 [`closure`] 的分歧只在"变体怎么选":
/// - `closure` 在**部署期**按目标事实选一个 —— 那时机器就在眼前;
/// - `bake` 在**构建期**把每个变体都带上 —— 那时还不知道要装到哪台。
///
/// 多带几个架构的字节,换的是"现场绝不会装不上"。air-gap 场景里这个交换永远
/// 划算:少带一个变体的代价是**已经断网的现场装不上**,那时补救成本无限大。
///
/// `profile` 给了目标画像(`--for arch=amd64`)时,`when:` 会被求值以缩小范围;
/// 没给就一个不筛。
pub fn bake(bp: &Blueprint, scope: &Scope, filter_by_when: bool) -> Vec<BakeItem> {
    let mut out = Vec::new();
    for name in referenced_names(bp) {
        let variants: Vec<&Material> = bp.materials.iter().filter(|m| m.name == name).collect();
        if variants.is_empty() {
            out.push(BakeItem {
                name: name.clone(),
                plan: Err(ResolveError::Undeclared(name)),
                when: None,
            });
            continue;
        }
        for m in variants {
            if filter_by_when {
                if let Some(cond) = &m.when {
                    match scope.eval_bool(cond) {
                        Ok(false) => continue,
                        Ok(true) => {}
                        // 画像里没有这条件用到的事实 —— 宁可带上,不可漏。
                        Err(_) => {}
                    }
                }
            }
            let plan = scope
                .resolve(&m.source)
                .map(|v| crate::eval::scalar_to_string(&v))
                .map_err(ResolveError::Eval)
                .and_then(|source| {
                    Ok(MaterialPlan {
                        name: m.name.clone(),
                        kind: m.kind,
                        source,
                        sha256: render_sha(scope, &m.sha256)?,
                        unzip: m.unzip.clone(),
                    })
                });
            out.push(BakeItem {
                name: m.name.clone(),
                plan,
                when: m.when.as_ref().map(|c| c.src().to_string()),
            });
        }
    }
    out
}

/// 烘焙清单里的一条。`when` 保留下来是为了报错时说清"是哪个变体"。
#[derive(Debug)]
pub struct BakeItem {
    pub name: String,
    pub plan: Result<MaterialPlan, ResolveError>,
    pub when: Option<String>,
}

impl BakeItem {
    /// 人读的标识:`containerd` 或 `containerd (when: substrate.arch == 'arm64')`。
    pub fn label(&self) -> String {
        match &self.when {
            Some(w) => format!("{} (when: {w})", self.name),
            None => self.name.clone(),
        }
    }
}

/// 整份 blueprint 里按名字引用到的物料(保持声明顺序、去重)。
///
/// 这个清单就是 air-gap 要带走的东西,**漏一项就是现场装不上**。所以扫描要覆盖:
/// - 资源与 **procedure 步骤**(舞里也会 copy 物料);
/// - `material` / `from_material` / `materials[]` / `dropins[]` 四种引用位置;
/// - `each:` **字面列表**配合 `material: "${item}"` 的写法 —— 静态看是模板,
///   实则引用了列表里的每一项(k8s 的 kubeadm/kubelet/kubectl 三件套正是如此,
///   早期版本在这里漏掉了三个二进制)。
pub fn referenced_names(bp: &Blueprint) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut push = |n: &str| {
        if !n.is_empty() && !names.iter().any(|s| s == n) {
            names.push(n.to_string());
        }
    };

    let mut scan = |args: &crate::ir::Args, each: Option<&crate::ir::Each>| {
        // `each:` 的字面项:`material: "${item}"` 逐项都是一次引用。
        let item_names: Vec<String> = match each {
            Some(crate::ir::Each::List(items)) => items
                .iter()
                .filter_map(|v| match v {
                    crate::ir::Value::Lit(Yaml::String(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        for key in ["material", "from_material"] {
            match args.get(key) {
                Some(crate::ir::Value::Lit(Yaml::String(n))) => push(n),
                // 纯 `${item}` 形式 → 展开成 each 的每一项
                Some(crate::ir::Value::Tmpl(t)) if is_bare_item(t) => {
                    item_names.iter().for_each(|n| push(n))
                }
                _ => {}
            }
        }
        for key in ["materials", "dropins"] {
            if let Some(crate::ir::Value::List(items)) = args.get(key) {
                for it in items {
                    if let crate::ir::Value::Lit(Yaml::String(n)) = it {
                        push(n);
                    }
                }
            }
        }
    };

    for r in &bp.resources {
        scan(&r.args, r.each.as_ref());
    }
    for proc in bp.procedures.values() {
        for st in &proc.steps {
            scan(&st.args, st.each.as_ref());
        }
    }
    names
}

/// 整串恰好是 `${item}` —— 说明它引用的是 `each:` 的每一项。
fn is_bare_item(t: &crate::expr::Template) -> bool {
    matches!(t.parts(), [crate::expr::Part::Expr(e)] if e.src().trim() == "item")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::blueprint_from_str;
    use std::collections::BTreeMap;

    const BP: &str = r#"
name: t
params:
  version: { default: "1.2.3" }
materials:
  - name: bin
    file: "https://ex.com/v${params.version}/tool-linux-x86_64.tar.gz"
    sha256: "abc"
    when: substrate.arch == 'amd64'
  - name: bin
    file: "https://ex.com/v${params.version}/tool-linux-aarch64.tar.gz"
    when: substrate.arch == 'arm64'
  - name: cfg
    file: files/app.conf
resources:
  - copy: { material: bin, dest: /usr/local/bin/tool }
  - copy: { material: cfg, dest: /etc/app.conf }
"#;

    /// 摘要必须和 URL 走**同一条**渲染 —— 否则换版本时 URL 变了、摘要没变,
    /// 落地必然摘要不符。这条是 `pkg push --set version=…` 能真正发出别的
    /// 版本的前提(issue #25)。
    ///
    /// 回归价值很具体:漏渲染时的表现不是崩,是把字面量 `${params.sha}` 当成
    /// 期望摘要拿去比对 —— 报错里会赫然写着 `期望 ${params.sha_amd64}`。
    /// 实测撞见过。
    #[test]
    fn a_templated_sha256_is_rendered_like_the_url() {
        const T: &str = r#"
name: t
params:
  version: { default: "9.9.9" }
  sha: { default: "deadbeef" }
materials:
  - name: bin
    file: "https://ex.com/v${params.version}/tool"
    sha256: "${params.sha}"
resources:
  - copy: { material: bin, dest: /usr/local/bin/tool }
"#;
        let bp = blueprint_from_str(T).unwrap();
        let mut params = BTreeMap::new();
        params.insert("version".to_string(), Yaml::from("9.9.9"));
        params.insert("sha".to_string(), Yaml::from("cafebabe"));
        let mut substrate = BTreeMap::new();
        substrate.insert("arch".to_string(), Yaml::from("amd64"));
        let scope = Scope {
            params,
            substrate,
            ..Default::default()
        };
        let plan = resolve(&bp, "bin", &scope).unwrap();
        assert_eq!(plan.sha256.as_deref(), Some("cafebabe"), "摘要没被渲染");
        assert!(
            plan.source.contains("v9.9.9"),
            "URL 没被渲染:{}",
            plan.source
        );
    }

    /// 没写摘要仍然是 `None`,不能变成空串 —— 空串会被当成"声明了摘要",
    /// 于是每次落地都判成不符。
    #[test]
    fn an_absent_sha256_stays_none() {
        let bp = blueprint_from_str(BP).unwrap();
        let plan = resolve(&bp, "bin", &scope_with_arch("arm64")).unwrap();
        assert_eq!(plan.sha256, None);
    }

    fn scope_with_arch(arch: &str) -> Scope {
        let mut params = BTreeMap::new();
        params.insert("version".to_string(), Yaml::from("1.2.3"));
        let mut substrate = BTreeMap::new();
        substrate.insert("arch".to_string(), Yaml::from(arch));
        Scope {
            params,
            substrate,
            ..Default::default()
        }
    }

    #[test]
    fn the_target_arch_picks_the_variant_not_the_author() {
        let bp = blueprint_from_str(BP).unwrap();
        let amd = resolve(&bp, "bin", &scope_with_arch("amd64")).unwrap();
        assert!(
            amd.source.ends_with("tool-linux-x86_64.tar.gz"),
            "{}",
            amd.source
        );
        assert_eq!(amd.sha256.as_deref(), Some("abc"));

        let arm = resolve(&bp, "bin", &scope_with_arch("arm64")).unwrap();
        assert!(
            arm.source.ends_with("tool-linux-aarch64.tar.gz"),
            "{}",
            arm.source
        );
        assert_eq!(arm.sha256, None, "变体各自带自己的摘要");
    }

    #[test]
    fn params_are_rendered_into_the_url() {
        let bp = blueprint_from_str(BP).unwrap();
        let p = resolve(&bp, "bin", &scope_with_arch("amd64")).unwrap();
        assert!(p.source.contains("/v1.2.3/"), "{}", p.source);
    }

    #[test]
    fn an_uncovered_architecture_fails_loudly_with_the_machine_described() {
        // 只打了 amd64/arm64 却部署到 riscv —— 静默跳过会让机器装上半套。
        let bp = blueprint_from_str(BP).unwrap();
        let err = resolve(&bp, "bin", &scope_with_arch("riscv64")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("2 个变体"), "{msg}");
        assert!(
            msg.contains("arch=riscv64"),
            "报错要说清这台机器长什么样:{msg}"
        );
        assert!(msg.contains("拒绝装半套"), "{msg}");
    }

    #[test]
    fn overlapping_conditions_are_rejected_rather_than_silently_first_wins() {
        let bp = blueprint_from_str(
            r#"
name: t
materials:
  - { name: bin, file: "a", when: "true" }
  - { name: bin, file: "b", when: "true" }
"#,
        )
        .unwrap();
        let err = resolve(&bp, "bin", &scope_with_arch("amd64")).unwrap_err();
        assert!(err.to_string().contains("同时成立"), "{err}");
    }

    #[test]
    fn unconditional_materials_work_on_every_machine() {
        let bp = blueprint_from_str(BP).unwrap();
        for arch in ["amd64", "arm64", "riscv64"] {
            let p = resolve(&bp, "cfg", &scope_with_arch(arch)).unwrap();
            assert_eq!(p.source, "files/app.conf");
        }
    }

    #[test]
    fn undeclared_names_are_caught_even_at_runtime() {
        let bp = blueprint_from_str(BP).unwrap();
        assert!(matches!(
            resolve(&bp, "ghost", &scope_with_arch("amd64")),
            Err(ResolveError::Undeclared(_))
        ));
    }

    #[test]
    fn the_closure_covers_each_expanded_and_procedure_referenced_materials() {
        // 漏一项就是现场装不上 —— 早期版本漏掉了 each 展开的三件套。
        let bp = blueprint_from_str(
            r#"
name: t
materials:
  - { name: kubeadm, file: "a" }
  - { name: kubelet, file: "b" }
  - { name: kubectl, file: "c" }
  - { name: unit,    file: "d" }
  - { name: dropin,  file: "e" }
  - { name: manifest, file: "f" }
resources:
  - copy: { material: "${item}", dest: "/usr/local/bin/${item}" }
    each: ["kubeadm", "kubelet", "kubectl"]
  - systemd_unit: { name: kubelet, from_material: unit, dropins: [dropin] }
procedures:
  boot:
    steps:
      - copy: { material: manifest, dest: /etc/k.yml }
        target: all
"#,
        )
        .unwrap();
        let names = referenced_names(&bp);
        for want in [
            "kubeadm", "kubelet", "kubectl", "unit", "dropin", "manifest",
        ] {
            assert!(
                names.contains(&want.to_string()),
                "闭包漏了 {want}:{names:?}"
            );
        }
    }

    #[test]
    fn the_closure_is_a_function_of_values_and_target_facts() {
        // build 侧据此决定烤什么进 OCI:同一份 blueprint,不同架构 → 不同闭包。
        let bp = blueprint_from_str(BP).unwrap();
        let amd: Vec<String> = closure(&bp, &scope_with_arch("amd64"))
            .into_iter()
            .map(|r| r.unwrap().source)
            .collect();
        assert_eq!(amd.len(), 2, "只算真正被引用的物料:{amd:?}");
        assert!(amd[0].contains("x86_64"));

        let arm: Vec<String> = closure(&bp, &scope_with_arch("arm64"))
            .into_iter()
            .map(|r| r.unwrap().source)
            .collect();
        assert!(arm[0].contains("aarch64"), "{arm:?}");
    }
}
