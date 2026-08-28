//! `crater schema` —— 生成 JSON Schema,把可发现性从命令行推进到**编辑器**。
//!
//! `crater types` 是"查得到";schema 是"写的时候就看得见"。生成后在蓝图首行加一句
//! `# yaml-language-server: $schema=...`,VS Code / Neovim 等就能补全字段、
//! 悬停看说明、拼错就地飘红 —— 不必等到 lint,更不必等到 apply。

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// `crater schema [-f <blueprint>] [-o <path>] [--stdout]`
pub fn run(file: Option<&Path>, out: Option<&Path>, to_stdout: bool) -> Result<()> {
    // 给了蓝图就**自特化**:物料名、自定义类型进枚举,补全只提示你自己有的东西。
    let bp = match file {
        Some(p) => Some(
            crater_ir::parse::blueprint_from_path(p).map_err(|e| anyhow::anyhow!("{e}"))?,
        ),
        None => None,
    };
    let schema = crater_ir::jsonschema::generate(bp.as_ref());
    let text = serde_json::to_string_pretty(&schema)?;

    if to_stdout {
        println!("{text}");
        return Ok(());
    }

    let path = out.map(Path::to_path_buf).unwrap_or_else(default_path);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, &text).with_context(|| format!("写 {}", path.display()))?;

    println!("已写入 {} ({} 类型)", path.display(), crater_ir::types::BUILTINS.len());
    if let Some(b) = &bp {
        println!("已按 `{}` 自特化:{} 个物料名、{} 个自定义类型进入补全", b.name, b.materials.len(), b.types.len());
    } else {
        println!("通用 schema。加 `-f <blueprint>` 可按该蓝图自特化(补全你自己的物料名与类型)");
    }
    println!("\n在蓝图首行加上这一句即可接入编辑器:");
    println!("{}", crater_ir::jsonschema::language_server_hint(&display_ref(&path, file)));
    Ok(())
}

fn default_path() -> PathBuf {
    PathBuf::from(".crater").join("schema.json")
}

/// 蓝图里该写的 $schema 路径:相对蓝图所在目录,便于文件被搬走后仍然指得对。
fn display_ref(schema: &Path, blueprint: Option<&Path>) -> String {
    let Some(bp_dir) = blueprint.and_then(Path::parent) else {
        return schema.display().to_string();
    };
    let abs = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    match abs(schema).strip_prefix(abs(bp_dir)) {
        Ok(rel) => rel.display().to_string(),
        // 不在蓝图目录下就给绝对路径 —— 猜一串 ../.. 更容易出错。
        Err(_) => abs(schema).display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_reference_is_relative_to_the_blueprint_when_possible() {
        let d = tempfile::tempdir().unwrap();
        let bp = d.path().join("k8s.blueprint.yaml");
        std::fs::write(&bp, "name: t\n").unwrap();
        let schema = d.path().join(".crater").join("schema.json");
        std::fs::create_dir_all(schema.parent().unwrap()).unwrap();
        std::fs::write(&schema, "{}").unwrap();

        assert_eq!(display_ref(&schema, Some(&bp)), ".crater/schema.json");
    }

    #[test]
    fn an_unrelated_location_falls_back_to_an_absolute_path() {
        // 猜一串 ../.. 比给绝对路径更容易出错。
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let bp = a.path().join("x.yaml");
        std::fs::write(&bp, "name: t\n").unwrap();
        let schema = b.path().join("schema.json");
        std::fs::write(&schema, "{}").unwrap();

        let r = display_ref(&schema, Some(&bp));
        assert!(Path::new(&r).is_absolute(), "{r}");
    }
}
