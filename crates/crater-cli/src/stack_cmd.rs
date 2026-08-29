//! `crater plan/apply/verify -f <栈>.stack.yaml` —— 按序驱动多份蓝图。
//!
//! 栈只管**顺序**与**接线**(组名、参数),不管蓝图内部。执行路径与单蓝图
//! 完全同一条,栈只是给每份蓝图多戴一副[透镜](crate::blueprint::Lens)。
//!
//! 三条语义写死在这里:
//! 1. **全部契约先过一遍,再动第一台机器**。栈的价值是"一次装好一套",
//!    装到第三份才发现第五份的 inventory 不满足,是最差的失败时机。
//! 2. **失败即停**。栈是有序的:containerd 没装上,k8s 装了也不会好。
//!    继续往下只会把一个清晰的失败变成一堆难解的失败。
//! 3. **跨蓝图 export 不可见**。蓝图自包含,要传值就升成参数(A2 边界)。

use std::path::Path;

use anyhow::{Context as _, Result};
use crater_ir::stack::{self, Stack};

use crate::blueprint::{self, Lens, StackMode};
use crate::target::TargetOpts;

/// 这个文件是不是一份 stack。
pub fn is_stack_file(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
        .map(|v| stack::is_stack(&v))
        .unwrap_or(false)
}

pub async fn run(path: &Path, target: &TargetOpts, sets: &[String], mode: StackMode) -> Result<()> {
    let st = stack::from_path(path)?;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

    // 先把引用全部解析掉。少一份蓝图这种错,不该等到第三步才暴露。
    let mut entries = Vec::new();
    for u in &st.uses {
        let bp_path = stack::resolve_ref(&u.blueprint, &dir)
            .with_context(|| format!("栈 `{}`", st.name))?;
        entries.push((u, bp_path));
    }

    println!("栈 `{}` —— {} 份蓝图\n", st.name, entries.len());
    for (i, (u, p)) in entries.iter().enumerate() {
        println!("  {}. {:<20} {}", i + 1, u.label(), p.display());
    }
    println!();

    for (i, (u, bp_path)) in entries.iter().enumerate() {
        println!("══ [{}/{}] {} ══", i + 1, entries.len(), u.label());
        let lens = Lens {
            groups: u.groups.clone(),
            params: u.params.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        };
        blueprint::run_lensed(bp_path, target, sets, mode, &lens)
            .await
            // 失败即停:栈是有序的,继续往下只会把一个清晰的失败变成一堆。
            .with_context(|| {
                format!(
                    "栈 `{}` 在第 {}/{} 份蓝图 `{}` 上中止 —— 其后的 {} 份未执行",
                    st.name,
                    i + 1,
                    entries.len(),
                    u.label(),
                    entries.len() - i - 1
                )
            })?;
        println!();
    }
    println!("栈 `{}` 全部 {} 份蓝图完成。", st.name, entries.len());
    Ok(())
}

/// `crater destroy <栈> [--yes]` —— **逆序**退役。
///
/// 顺序反过来不是对称的美学:栈是有依赖的,`k8s` 建在 `containerd` 上。
/// 正序拆会先把 containerd 抽走,然后 k8s 的资源连观察都观察不了。
///
/// 与蓝图级 destroy 一样**默认只预演**;`--yes` 才真拆。
pub async fn destroy(path: &Path, target: &TargetOpts, sets: &[String], yes: bool) -> Result<()> {
    let st = stack::from_path(path)?;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();

    let mut entries = Vec::new();
    for u in &st.uses {
        entries.push((u, stack::resolve_ref(&u.blueprint, &dir)?));
    }
    entries.reverse();

    println!(
        "栈 `{}` —— {} 份蓝图,{}(逆序:{})\n",
        st.name,
        entries.len(),
        if yes { "将逐个退役" } else { "退役预演" },
        entries.iter().map(|(u, _)| u.label()).collect::<Vec<_>>().join(" → ")
    );

    for (i, (u, bp_path)) in entries.iter().enumerate() {
        println!("══ [{}/{}] {} ══", i + 1, entries.len(), u.label());
        let lens = Lens {
            groups: u.groups.clone(),
            params: u.params.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        };
        // 退役**不**在失败处停:一份拆不掉不该让其余的继续留在机器上。
        // 与 apply 的"失败即停"相反,因为方向反了 —— 半拆的栈比全拆的栈更糟。
        if let Err(e) = blueprint::destroy_lensed(bp_path, target, sets, yes, &lens).await {
            eprintln!("蓝图 `{}` 退役失败 —— {e:#}\n(继续退役其余蓝图)\n", u.label());
        }
        println!();
    }
    if !yes {
        println!("以上为**预演**。确认无误后加 `--yes` 执行。");
    }
    Ok(())
}

/// `crater lint <栈>` —— 只做静态检查:引用解析得开吗、蓝图各自 lint 得过吗。
pub fn lint(path: &Path) -> Result<Stack> {
    let st = stack::from_path(path)?;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    for u in &st.uses {
        let p = stack::resolve_ref(&u.blueprint, &dir)?;
        let bp = crater_ir::parse::blueprint_from_path(&p)
            .with_context(|| format!("栈 `{}` 引用的 {}", st.name, p.display()))?;
        // 栈给的参数必须是那份蓝图真有的 —— 拼错的参数名会静默失效,
        // 而"静默失效的参数"是配置类 bug 里最难查的一种。
        for name in u.params.keys() {
            if !bp.params.contains_key(name) {
                let hint = closest(name, &bp.params.keys().map(String::as_str).collect::<Vec<_>>());
                anyhow::bail!(
                    "栈给蓝图 `{}` 传了参数 `{name}`,但它没有这个参数{}",
                    u.blueprint,
                    hint.map(|c| format!(",是不是 `{c}`?")).unwrap_or_default()
                );
            }
        }
        // 组名重映射的左侧必须是那份蓝图声明过的组(有契约时才能判)。
        for bp_group in u.groups.keys() {
            if !bp.fleet.groups.is_empty() && !bp.fleet.groups.contains_key(bp_group) {
                anyhow::bail!(
                    "栈把 `{bp_group}` 映射给蓝图 `{}`,但它的 `fleet.groups` 没声明这个组",
                    u.blueprint
                );
            }
        }
    }
    Ok(st)
}

/// 与 lint/types 同一套拼写建议(编辑距离 ≤2 且不超过词长一半)。
fn closest<'a>(word: &str, candidates: &[&'a str]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (*c, distance(word, c)))
        .filter(|(c, d)| *d <= 2 && *d * 2 <= c.len().max(word.len()))
        .min_by_key(|(_, d)| *d)
        .map(|(c, _)| c)
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个能自洽的小栈:两份蓝图 + 组名映射 + 参数。
    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("base.blueprint.yaml"),
            "name: base\nresources:\n  - file: { path: /opt/base, state: directory }\n",
        )
        .unwrap();
        std::fs::write(
            d.path().join("app.blueprint.yaml"),
            [
                "name: app",
                "params:",
                "  ha: { type: bool, default: false }",
                "fleet:",
                "  groups:",
                "    controlplane: {min: 1}",
                "resources:",
                "  - file: { path: /opt/app, state: directory }",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        std::fs::write(
            d.path().join("s.stack.yaml"),
            [
                "stack: demo",
                "uses:",
                "  - blueprint: base",
                "  - blueprint: app",
                "    params: { ha: true }",
                "    groups: { controlplane: k8s_masters }",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        d
    }

    #[test]
    fn a_stack_file_is_told_apart_from_a_blueprint() {
        let d = fixture();
        assert!(is_stack_file(&d.path().join("s.stack.yaml")));
        assert!(!is_stack_file(&d.path().join("app.blueprint.yaml")));
    }

    #[test]
    fn linting_a_stack_resolves_every_reference() {
        let d = fixture();
        let st = lint(&d.path().join("s.stack.yaml")).unwrap();
        assert_eq!(st.uses.len(), 2);
    }

    #[test]
    fn a_missing_blueprint_is_caught_before_anything_runs() {
        let d = fixture();
        std::fs::remove_file(d.path().join("base.blueprint.yaml")).unwrap();
        let err = lint(&d.path().join("s.stack.yaml")).unwrap_err().to_string();
        assert!(err.contains("base"), "{err}");
    }

    #[test]
    fn a_misspelled_param_is_refused_with_a_suggestion() {
        // 静默失效的参数是配置类 bug 里最难查的一种:一切正常,只是没生效。
        let d = fixture();
        let p = d.path().join("s.stack.yaml");
        let s = std::fs::read_to_string(&p).unwrap().replace("ha: true", "h: true");
        std::fs::write(&p, s).unwrap();
        let err = lint(&p).unwrap_err().to_string();
        assert!(err.contains("是不是 `ha`"), "{err}");
    }

    #[test]
    fn remapping_a_group_the_blueprint_never_declared_is_refused() {
        let d = fixture();
        let p = d.path().join("s.stack.yaml");
        let s = std::fs::read_to_string(&p)
            .unwrap()
            .replace("controlplane: k8s_masters", "controlplan: k8s_masters");
        std::fs::write(&p, s).unwrap();
        let err = lint(&p).unwrap_err().to_string();
        assert!(err.contains("controlplan"), "{err}");
    }

    #[test]
    fn destroy_walks_the_stack_in_reverse() {
        // 顺序反过来不是对称的美学:k8s 建在 containerd 上,正序拆会先抽走
        // containerd,然后 k8s 的资源连观察都观察不了。
        let d = fixture();
        let st = stack::from_path(&d.path().join("s.stack.yaml")).unwrap();
        let mut order: Vec<&str> = st.uses.iter().map(|u| u.label()).collect();
        assert_eq!(order, vec!["base", "app"], "apply 是声明序");
        order.reverse();
        assert_eq!(order, vec!["app", "base"], "destroy 是逆序");
    }

    #[test]
    fn a_blueprint_without_a_contract_accepts_any_remapping() {
        // 没写 fleet: 的蓝图不该因为进了栈就被要求补 fleet:。
        let d = fixture();
        let p = d.path().join("s.stack.yaml");
        let s = std::fs::read_to_string(&p)
            .unwrap()
            .replace("  - blueprint: base", "  - blueprint: base\n    groups: { anything: x }");
        std::fs::write(&p, s).unwrap();
        assert!(lint(&p).is_ok());
    }
}
