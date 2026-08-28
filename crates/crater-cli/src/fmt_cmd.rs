//! `crater fmt --split <节> / --join` —— A1 分节的**机械**双向转换。
//!
//! `lint --stats` 会提示"可 `crater fmt --split procedures` 外置" ——
//! 这个模块让那句提示兑现,而不是一张空头支票。
//!
//! 纪律:**转换必须是机械的、可逆的、语义等价的**。拆开与合回,引擎看到的东西
//! 必须一模一样 —— 这正是 A1 与被拒的 `include` 的分界线。所以每次写盘后都会
//! 重新解析并与转换前比对结构,对不上就响亮失败。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use crater_ir::parse::{self, SPLITTABLE};

/// `crater fmt <blueprint> [--split <节>] [--join]`
pub fn run(path: &Path, split: Option<&str>, join: bool) -> Result<()> {
    match (split, join) {
        (Some(_), true) => bail!("`--split` 与 `--join` 不能同时给"),
        (Some(section), false) => do_split(path, section),
        (None, true) => do_join(path),
        (None, false) => bail!(
            "需要 `--split <节>` 或 `--join`(可外置的节:{})",
            SPLITTABLE.join(", ")
        ),
    }
}

// ---------------------------------------------------------------- split

fn do_split(root: &Path, section: &str) -> Result<()> {
    if !SPLITTABLE.contains(&section) {
        bail!("不能外置 `{section}`(可外置:{})", SPLITTABLE.join(", "));
    }
    let before = load_shape(root)?;
    let text = std::fs::read_to_string(root).with_context(|| format!("读 {}", root.display()))?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&text)?;
    let map = doc
        .as_mapping_mut()
        .ok_or_else(|| anyhow::anyhow!("蓝图顶层应是 map"))?;

    let key = serde_yaml::Value::from(section);
    let Some(body) = map.remove(&key) else {
        bail!("`{}` 里没有 `{section}:` 这一节", root.display());
    };
    let part = parse::part_path(root, section);
    if part.exists() {
        bail!("{} 已存在 —— 先处理它,以免覆盖", part.display());
    }

    // `parts:` 排在最前:读者一进文件就知道"还有别的文件参与"。
    let mut declared = existing_parts(map);
    if !declared.iter().any(|d| d == section) {
        declared.push(section.to_string());
    }
    declared.sort();
    let mut out = serde_yaml::Mapping::new();
    out.insert(
        serde_yaml::Value::from("parts"),
        serde_yaml::Value::Sequence(
            declared
                .iter()
                .map(|s| serde_yaml::Value::from(s.as_str()))
                .collect(),
        ),
    );
    for (k, v) in map.iter() {
        if k.as_str() == Some("parts") {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }

    let root_text = serde_yaml::to_string(&serde_yaml::Value::Mapping(out))?;
    let part_text = serde_yaml::to_string(&body)?;

    atomic_write(&part, &part_text)?;
    atomic_write(root, &root_text)?;
    // 同样是事务性:校验不过就把根文件写回、把刚建的 part 删掉。
    if let Err(e) = verify(root, &before, "--split") {
        let _ = std::fs::write(root, &text);
        let _ = std::fs::remove_file(&part);
        return Err(e.context("已回滚,文件保持原样"));
    }

    println!("已外置 `{section}` → {}", part.display());
    println!("根文件现声明: parts: [{}]", declared.join(", "));
    Ok(())
}

// ---------------------------------------------------------------- join

fn do_join(root: &Path) -> Result<()> {
    let before = load_shape(root)?;
    let text = std::fs::read_to_string(root).with_context(|| format!("读 {}", root.display()))?;
    let doc: serde_yaml::Value = serde_yaml::from_str(&text)?;
    let map = doc
        .as_mapping()
        .ok_or_else(|| anyhow::anyhow!("蓝图顶层应是 map"))?;
    let declared = existing_parts(map);
    if declared.is_empty() {
        println!("{} 没有外置的节,无需合并", root.display());
        return Ok(());
    }

    let mut out = serde_yaml::Mapping::new();
    for (k, v) in map.iter() {
        if k.as_str() == Some("parts") {
            continue;
        }
        out.insert(k.clone(), v.clone());
    }
    let mut parts = Vec::new();
    for section in &declared {
        let part = parse::part_path(root, section);
        let body =
            std::fs::read_to_string(&part).with_context(|| format!("读 {}", part.display()))?;
        let value: serde_yaml::Value = serde_yaml::from_str(&body)?;
        out.insert(serde_yaml::Value::from(section.as_str()), value);
        parts.push(part);
    }

    let joined = serde_yaml::to_string(&serde_yaml::Value::Mapping(out))?;

    // 事务性:part 文件必须在校验**之前**删掉(留着会被 E122 幽灵检查拦下),
    // 所以先把原样存在内存里,校验不过就整体回滚 —— 绝不留下半合并的状态。
    let backup: Vec<(PathBuf, String)> = parts
        .iter()
        .map(|p| Ok((p.clone(), std::fs::read_to_string(p)?)))
        .collect::<Result<_>>()?;
    atomic_write(root, &joined)?;
    for p in &parts {
        std::fs::remove_file(p).with_context(|| format!("删 {}", p.display()))?;
    }
    if let Err(e) = verify(root, &before, "--join") {
        let _ = std::fs::write(root, &text);
        for (p, body) in &backup {
            let _ = std::fs::write(p, body);
        }
        return Err(e.context("已回滚,文件保持原样"));
    }

    println!("已合并 {} 个外置节回 {}", parts.len(), root.display());
    for p in &parts {
        println!("  移除 {}", p.display());
    }
    Ok(())
}

// ---------------------------------------------------------------- 共用

fn existing_parts(map: &serde_yaml::Mapping) -> Vec<String> {
    match map.get(serde_yaml::Value::from("parts")) {
        Some(serde_yaml::Value::Sequence(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// 转换前后用来比对的结构指纹。比对**解析结果**而非文本 ——
/// 格式化本就会改动文本;能改的只有排版,不能改的是语义。
#[derive(PartialEq, Eq, Debug)]
struct Shape {
    name: String,
    resources: usize,
    materials: usize,
    procedures: Vec<String>,
    types: Vec<String>,
    health: usize,
    preflight: usize,
}

fn load_shape(root: &Path) -> Result<Shape> {
    let bp = parse::blueprint_from_path(root).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Shape {
        name: bp.name.clone(),
        resources: bp.resources.len(),
        materials: bp.materials.len(),
        procedures: bp.procedures.keys().cloned().collect(),
        types: bp.types.iter().map(|t| t.name.clone()).collect(),
        health: bp.health.len(),
        preflight: bp.preflight.len(),
    })
}

/// 写盘:先写临时文件再改名,中途失败不留半成品。
fn atomic_write(path: &Path, text: &str) -> Result<()> {
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("写 {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("改名 {}", path.display()))?;
    Ok(())
}

/// 转换后重新解析并与转换前比对 —— 对不上就是 bug,必须响亮失败。
fn verify(root: &Path, before: &Shape, what: &str) -> Result<()> {
    let after = load_shape(root)
        .with_context(|| format!("{what} 之后重新解析失败 —— 这是 crater 的 bug,请报告"))?;
    if &after != before {
        bail!(
            "{what} 改变了蓝图语义 —— 这是 crater 的 bug,请报告\n  之前: {before:?}\n  之后: {after:?}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BP: &str = r#"
name: demo
params:
  port: { type: port, default: 9000 }
resources:
  - file: { path: /data, state: directory }
  - service: { name: app, state: started }
procedures:
  boot:
    steps:
      - shell: { cmd: "true", check: "test -f /x" }
  reset:
    steps:
      - shell: { cmd: "false", check: "test -f /y" }
health:
  - service_active: app
"#;

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("demo.blueprint.yaml");
        std::fs::write(&root, BP).unwrap();
        (d, root)
    }

    #[test]
    fn split_then_join_round_trips_to_the_same_semantics() {
        // 整个机制的地基:拆开与合回,引擎看到的必须一模一样。
        let (_d, root) = setup();
        let before = load_shape(&root).unwrap();

        run(&root, Some("procedures"), false).unwrap();
        assert!(parse::part_path(&root, "procedures").exists());
        assert_eq!(load_shape(&root).unwrap(), before, "拆开后语义应不变");

        run(&root, None, true).unwrap();
        assert!(
            !parse::part_path(&root, "procedures").exists(),
            "合回后 part 应被移除"
        );
        assert_eq!(load_shape(&root).unwrap(), before, "合回后语义应不变");
    }

    #[test]
    fn the_root_file_declares_parts_first() {
        // 读者一进文件就该知道"还有别的文件参与"。
        let (_d, root) = setup();
        run(&root, Some("procedures"), false).unwrap();
        let text = std::fs::read_to_string(&root).unwrap();
        assert!(text.trim_start().starts_with("parts:"), "{text}");
        assert!(!text.contains("\nprocedures:"), "根文件不该再有这一节");
    }

    #[test]
    fn splitting_twice_accumulates_instead_of_replacing() {
        let (_d, root) = setup();
        run(&root, Some("procedures"), false).unwrap();
        run(&root, Some("health"), false).unwrap();
        assert!(parse::part_path(&root, "health").exists());
        // 两个都外置后语义仍然不变
        let bp = parse::blueprint_from_path(&root).unwrap();
        assert_eq!(bp.procedures.len(), 2);
        assert_eq!(bp.health.len(), 1);
    }

    #[test]
    fn splitting_a_section_that_is_not_there_is_refused() {
        let (_d, root) = setup();
        let err = run(&root, Some("preflight"), false).unwrap_err().to_string();
        assert!(err.contains("没有 `preflight:`"), "{err}");
    }

    #[test]
    fn splitting_a_non_splittable_section_lists_what_is_allowed() {
        let (_d, root) = setup();
        let err = run(&root, Some("params"), false).unwrap_err().to_string();
        assert!(err.contains("不能外置 `params`") && err.contains("可外置:"), "{err}");
    }

    #[test]
    fn an_existing_part_file_is_never_overwritten() {
        // 幽灵文件检查(E122)会先一步拦下 —— 那条错更有信息量(它解释了为什么
        // 一个存在的 part 文件却不生效)。无论哪条先触发,文件都不能被覆盖。
        let (_d, root) = setup();
        let part = parse::part_path(&root, "procedures");
        std::fs::write(&part, "已有内容\n").unwrap();
        assert!(run(&root, Some("procedures"), false).is_err());
        assert_eq!(std::fs::read_to_string(&part).unwrap(), "已有内容\n", "原文件必须原封不动");
    }

    #[test]
    fn a_broken_part_file_aborts_join_without_deleting_anything() {
        // 事务性的真实场景:part 文件坏了 —— 宁可什么都不做,
        // 也不能出现"根文件已改、part 已删"的半合并状态。
        let (_d, root) = setup();
        run(&root, Some("procedures"), false).unwrap();
        let part = parse::part_path(&root, "procedures");
        let root_before = std::fs::read_to_string(&root).unwrap();
        std::fs::write(&part, "{{{ 这不是合法 YAML\n").unwrap();

        assert!(run(&root, None, true).is_err(), "坏的 part 必须让 join 失败");
        assert!(part.exists(), "失败时不能删 part");
        assert_eq!(
            std::fs::read_to_string(&root).unwrap(),
            root_before,
            "失败时根文件必须原封不动"
        );
    }

    #[test]
    fn verify_guards_fmt_s_own_bugs_not_the_users_edits() {
        // 说明性测试:verify 比对的是**本次转换**的前后。用户在两次 fmt 之间
        // 手改了 part 文件,那是他的合法编辑 —— fmt 不该、也无法把它当成错误。
        let (_d, root) = setup();
        run(&root, Some("procedures"), false).unwrap();
        std::fs::write(parse::part_path(&root, "procedures"), "boot:\n  steps: []\n").unwrap();

        run(&root, None, true).unwrap();
        let bp = parse::blueprint_from_path(&root).unwrap();
        assert_eq!(bp.procedures.len(), 1, "用户的编辑被如实合回");
    }

    #[test]
    fn joining_a_single_file_blueprint_is_a_noop() {
        let (_d, root) = setup();
        let before = load_shape(&root).unwrap();
        run(&root, None, true).unwrap();
        assert_eq!(load_shape(&root).unwrap(), before);
    }

    #[test]
    fn split_and_join_are_mutually_exclusive() {
        let (_d, root) = setup();
        assert!(run(&root, Some("procedures"), true).is_err());
    }

    #[test]
    fn no_temp_files_are_left_behind() {
        let (d, root) = setup();
        run(&root, Some("procedures"), false).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "留下了临时文件");
    }
}
