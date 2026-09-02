//! `crater inspect <蓝图|栈>` —— 交付前先问清楚"这东西要我给什么"。
//!
//! 这条命令回答的是**输入契约**:哪些参数必须给、给了会怎样、它需要什么样的
//! 机群、能跳哪几支舞。没有它,唯一的办法是通读 YAML —— 而一份蓝图动辄几百行,
//! 其中绝大多数是实现细节,与"我要怎么调用它"无关。

use anyhow::Result;
use crater_ir::ir::Blueprint;
use std::path::Path;

pub fn run(path: &Path) -> Result<()> {
    if crate::stack_cmd::is_stack_file(path) {
        return inspect_stack(path);
    }
    let bp = crater_ir::parse::blueprint_from_path(path)?;
    print_blueprint(&bp);
    Ok(())
}

fn print_blueprint(bp: &Blueprint) {
    println!(
        "蓝图 {}{}",
        bp.name,
        bp.version
            .as_deref()
            .map(|v| format!("  v{v}"))
            .unwrap_or_default()
    );
    if let Some(d) = &bp.description {
        println!("{d}");
    }

    // 参数按"必须给"排在前面 —— 读的人最先要知道的是"我非填不可的是什么"。
    if !bp.params.is_empty() {
        println!("\n参数:");
        let mut names: Vec<&String> = bp.params.keys().collect();
        names.sort_by_key(|n| {
            let p = &bp.params[*n];
            (p.default.is_some(), (*n).clone())
        });
        let w = names.iter().map(|n| n.len()).max().unwrap_or(0);
        for n in names {
            let p = &bp.params[n];
            let need = match &p.default {
                None => "必填".to_string(),
                Some(v) => format!("默认 {}", compact(v)),
            };
            println!(
                "  {:<w$}  {:<16}  {}",
                n,
                need,
                p.desc.as_deref().unwrap_or(""),
                w = w
            );
        }
    }

    // 机群契约:调用方要准备什么样的 inventory。
    if !bp.fleet.groups.is_empty() {
        println!("\n需要的机群:");
        for (g, c) in &bp.fleet.groups {
            let n = if c.min == 0 {
                "可为空".to_string()
            } else {
                format!("至少 {} 台", c.min)
            };
            println!("  {g:<16}  {n}");
        }
    }
    if !bp.cast.is_empty() {
        println!("\n选角表(蓝图内部定址,调用方通常不必关心):");
        for (name, sel) in &bp.cast {
            println!("  {name:<16}  {sel}");
        }
    }

    if !bp.procedures.is_empty() {
        println!("\n可跳的舞(crater procedure <名> -f …):");
        for (name, p) in &bp.procedures {
            let ps: Vec<String> = p
                .params
                .iter()
                .map(|(n, s)| {
                    if s.default.is_none() {
                        format!("--set {n}=<必填>")
                    } else {
                        format!("[--set {n}=…]")
                    }
                })
                .collect();
            println!("  {:<16}  {} 步  {}", name, p.steps.len(), ps.join(" "));
        }
    }

    println!(
        "\n资源 {} 项 · 物料 {} 份 · 自定义类型 {} 个 · 健康探针 {} 条",
        bp.resources.len(),
        bp.materials.len(),
        bp.types.len(),
        bp.health.len()
    );
}

/// 单行渲染一个默认值。
///
/// `scalar_to_string` 对列表会铺成多行 YAML,把表格撑坏 —— 而这张表的价值
/// 全在"一眼扫完",一个多行值就足以让人放弃扫。
fn compact(v: &crater_ir::eval::Yaml) -> String {
    use crater_ir::eval::Yaml;
    match v {
        Yaml::Sequence(items) => {
            format!(
                "[{}]",
                items.iter().map(compact).collect::<Vec<_>>().join(", ")
            )
        }
        Yaml::Mapping(m) => format!(
            "{{{}}}",
            m.iter()
                .map(|(k, val)| format!("{}: {}", compact(k), compact(val)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => crater_ir::eval::scalar_to_string(other),
    }
}

fn inspect_stack(path: &Path) -> Result<()> {
    let st = crater_ir::stack::from_path(path)?;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    println!(
        "栈 {} —— {} 份蓝图,apply 自上而下、destroy 逆序\n",
        st.name,
        st.uses.len()
    );
    for (i, u) in st.uses.iter().enumerate() {
        let p = crater_ir::stack::resolve_ref(&u.blueprint, &dir)?;
        println!(
            "── [{}/{}] {} ({})",
            i + 1,
            st.uses.len(),
            u.label(),
            p.display()
        );
        if !u.params.is_empty() {
            let kv: Vec<String> = u
                .params
                .iter()
                .map(|(k, v)| format!("{k}={}", compact(v)))
                .collect();
            println!("   栈给的参数: {}", kv.join(" "));
        }
        if !u.groups.is_empty() {
            let kv: Vec<String> = u.groups.iter().map(|(k, v)| format!("{k}→{v}")).collect();
            println!("   组名映射:   {}", kv.join(" "));
        }
        let bp = crater_ir::parse::blueprint_from_path(&p)?;
        println!();
        print_blueprint(&bp);
        println!();
    }
    Ok(())
}
