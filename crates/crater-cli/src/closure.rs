//! 离线闭包 —— 把一份 blueprint 需要的**全部字节**烤成一个 OCI 归档,
//! 现场断网也能装。
//!
//! 两条命令,一个不变量:
//! - `crater build -f bp.yaml -o closure.tar` —— 在**有网的地方**取齐字节;
//! - `crater apply -f bp.yaml --closure closure.tar` —— 在**没网的地方**用它们。
//!
//! 不变量是**内容寻址**:blob 以 sha256 命名,manifest 记着"哪个 URL 对应哪个
//! 摘要"。于是装载时的校验不是可选项,而是查表的副产品 —— 对不上就找不到文件。
//!
//! 与部署期选变体的分歧写在 [`crater_ir::materials::bake`]:构建时还不知道
//! 要装到哪台机器,所以**每个变体都带上**。多几兆字节,换的是"现场绝不会
//! 装不上" —— 断网之后补救的成本是无限大。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use crater_core::bundle::{self, BundleStage, Manifest};
use crater_ir::eval::Scope;
use crater_ir::ir::MaterialKind;
use crater_ir::materials;
use crater_ir::Blueprint;

use crate::material_ctx::BlobMap;

/// 一次烘焙的累积状态 —— 栈级 bake 靠它把多份蓝图的闭包并成一个。
///
/// 去重发生在两层:`seen` 挡住重复的 URL(不重复下载),`store_blob` 按
/// sha256 落盘(相同字节共用一个文件)。所以"各蓝图闭包的并集"是内容寻址的
/// 自然结果,不需要额外一遍比对。
struct Baker {
    stage: BundleStage,
    blobs: Vec<crater_core::bundle::BlobEntry>,
    seen: BTreeMap<String, ()>,
    skipped: Vec<String>,
}

impl Baker {
    fn new(root: PathBuf) -> Result<Self> {
        Ok(Baker {
            stage: BundleStage::new(root)?,
            blobs: Vec::new(),
            seen: BTreeMap::new(),
            skipped: Vec::new(),
        })
    }

    /// 把一份蓝图引用到的字节收进袋子。
    ///
    /// `extra_params` 来自栈的 `uses[].params` —— 物料 URL 里可能插值
    /// `${params.version}`,不把它带进来就会烤错版本(而且是**静默**烤错)。
    async fn absorb(
        &mut self,
        bp_path: &Path,
        profile: &[String],
        extra_params: &BTreeMap<String, serde_yaml::Value>,
    ) -> Result<usize> {
        let bp = crater_ir::parse::blueprint_from_path(bp_path)?;
        let base = bp_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let mut scope = bake_scope(&bp, profile)?;
        for (k, v) in extra_params {
            scope.params.insert(k.clone(), v.clone());
        }
        let items = materials::bake(&bp, &scope, !profile.is_empty());
        println!("烘焙 `{}` —— {} 个物料变体", bp.name, items.len());

        let mut taken = 0usize;
        for item in &items {
            let plan = match &item.plan {
                Ok(p) => p,
                // URL 本身依赖目标事实(`.../${substrate.arch}/tool`)时,不给画像
                // 就渲染不出来。这不是内部错误,是**作者需要补一句 `--for`**。
                Err(e) => bail!(
                    "物料 {} 无法在构建期定型:{e}\n\
                     提示:它的 URL 引用了目标事实,请用 `--for arch=amd64` 之类给出\
                     要烘焙的目标画像(可给多次)",
                    item.label()
                ),
            };
            // 镜像与系统包不是"一份字节":前者要整棵 OCI 树,后者根本在 apt/yum 那边。
            if plan.kind != MaterialKind::File {
                self.skipped.push(format!("{} ({:?} 类型)", item.label(), plan.kind));
                continue;
            }
            // 同一个 URL 只取一次 —— 跨蓝图共享物料是栈里的常态。
            if self.seen.insert(plan.source.clone(), ()).is_some() {
                println!("  ↩ {:<28} (已在闭包中)", item.name);
                continue;
            }

            let bytes = fetch_bytes(&plan.source, &base)
                .await
                .with_context(|| format!("烘焙物料 {}", item.label()))?;
            // 声明了摘要就当场核对:烤进闭包的字节错了,现场是查不出来的。
            if let Some(want) = &plan.sha256 {
                let got = bundle::sha256_hex(&bytes);
                if &got != want {
                    bail!(
                        "物料 {} 摘要不符 —— 声明 {want},实得 {got}\n源:{}",
                        item.label(),
                        plan.source
                    );
                }
            }
            let entry = self.stage.store_blob(&plan.source, &bytes)?;
            println!(
                "  ✓ {:<28} {:>9}  {}",
                item.name,
                human(entry.size),
                &entry.sha256[..12]
            );
            self.blobs.push(entry);
            taken += 1;
        }
        Ok(taken)
    }

    /// 收尾:写 manifest、打包。
    fn seal(self, name: &str, out: &Path) -> Result<()> {
        for s in &self.skipped {
            println!("  · 跳过 {s}");
        }
        if self.blobs.is_empty() {
            bail!("没有一个物料被烤进闭包 —— 检查 `materials:` 是否都是 image/os_package 类型");
        }
        let total: u64 = self.blobs.iter().map(|b| b.size).sum();
        let count = self.blobs.len();
        self.stage.write_manifest(&Manifest {
            format_version: bundle::BUNDLE_FORMAT_VERSION,
            name: name.to_string(),
            components: Vec::new(),
            blobs: self.blobs,
            images: Vec::new(),
            rootfs: Vec::new(),
        })?;
        if let Some(dir) = out.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        bundle::pack(self.stage.root.as_path(), out)?;
        println!("\n闭包 → {} ({count} 份物料,{})", out.display(), human(total));
        println!("现场用法:crater apply -f <蓝图或栈> --closure {}", out.display());
        Ok(())
    }
}

/// `crater build -f <blueprint> -o <out.tar> [--for k=v]…`
pub async fn build(bp_path: &Path, out: &Path, profile: &[String]) -> Result<()> {
    let bp = crater_ir::parse::blueprint_from_path(bp_path)?;
    let tmp = tempfile::tempdir()?;
    let mut baker = Baker::new(tmp.path().to_path_buf())?;
    if baker.absorb(bp_path, profile, &BTreeMap::new()).await? == 0 && baker.skipped.is_empty() {
        bail!(
            "blueprint `{}` 没有引用任何物料 —— 没有可烘焙的东西。\n\
             (物料是 `materials:` 里声明、被 copy/template/unarchive 引用的字节)",
            bp.name
        );
    }
    baker.seal(&bp.name, out)
}

/// `crater build -f <栈>.stack.yaml -o <out.tar>` —— **一个**制品装下整个栈。
///
/// 现场只需要拿一个文件走。各蓝图的闭包按内容寻址取并集:同一个 URL 只下载
/// 一次,相同字节只落盘一份 —— 栈里多份蓝图共用 containerd 是常态,
/// 逐个 build 会把它复制 N 遍。
pub async fn build_stack(stack_path: &Path, out: &Path, profile: &[String]) -> Result<()> {
    let st = crater_ir::stack::from_path(stack_path)?;
    let dir = stack_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let tmp = tempfile::tempdir()?;
    let mut baker = Baker::new(tmp.path().to_path_buf())?;

    println!("烘焙栈 `{}` 的离线闭包 —— {} 份蓝图\n", st.name, st.uses.len());
    for (i, u) in st.uses.iter().enumerate() {
        let bp_path = crater_ir::stack::resolve_ref(&u.blueprint, &dir)?;
        println!("── [{}/{}] {} ──", i + 1, st.uses.len(), u.label());
        // 栈的作者侧参数要带进来:物料 URL 常按 `${params.version}` 插值,
        // 漏掉它会**静默**烤错版本。
        baker.absorb(&bp_path, profile, &u.params).await?;
    }
    println!();
    baker.seal(&st.name, out)
}

/// 装载一个闭包 → (解包目录, 按**源 URL** 索引的 blob 表)。
///
/// 键是源 URL 而不是物料名:同名物料按 `when:` 分成多个变体,各有各的 URL。
/// 按名字索引会让"多架构"这个最常见的场景取到错误的字节 —— 而且是**静默**取错。
///
/// 返回的 `TempDir` 必须活到部署结束:blob 就在里面。
pub fn load(path: &Path) -> Result<(tempfile::TempDir, BlobMap)> {
    let tmp = tempfile::tempdir()?;
    let stage = bundle::unpack(path, tmp.path())
        .with_context(|| format!("解包闭包 {}", path.display()))?;
    let manifest = stage
        .read_manifest()
        .with_context(|| format!("{} 不像是 crater 闭包(读不到 manifest)", path.display()))?;
    // 校验一次全部 blob。慢几秒,换的是"部署到一半才发现字节坏了"永远不会发生。
    stage.verify(&manifest).context("闭包完整性校验")?;

    let map: BlobMap = manifest
        .blobs
        .iter()
        .map(|b| (b.source_url.clone(), stage.blob_path(&b.sha256)))
        .collect();
    Ok((tmp, map))
}

/// 构建期的求值作用域:参数默认值 ⊕ `--for` 给出的目标画像。
fn bake_scope(bp: &Blueprint, profile: &[String]) -> Result<Scope> {
    let mut scope = crater_ir::plan::scope_from_defaults(bp);
    for kv in profile {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("`--for` 要写成 k=v,收到 `{kv}`"))?;
        scope
            .substrate
            .insert(k.to_string(), serde_yaml::Value::String(v.to_string()));
    }
    Ok(scope)
}

/// 取字节:远端 URL 走网络(带镜像回退),否则按 blueprint 目录解析本地文件。
async fn fetch_bytes(source: &str, base: &Path) -> Result<Vec<u8>> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let (bytes, used) = crater_core::source::fetch_best(source).await?;
        if used != source {
            println!("    (经镜像 {used})");
        }
        return Ok(bytes);
    }
    if let Some(rest) = source.strip_prefix("file://") {
        return Ok(std::fs::read(rest)?);
    }
    let p = base.join(source);
    std::fs::read(&p).with_context(|| format!("读本地物料 {}", p.display()))
}

fn human(n: u64) -> String {
    const U: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn sizes_are_rendered_for_humans() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(2048), "2.0 KiB");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MiB");
    }

    #[test]
    fn a_profile_must_be_key_equals_value() {
        let bp = crater_ir::parse::blueprint_from_str("name: t\n").unwrap();
        let err = bake_scope(&bp, &["arch".into()]).unwrap_err().to_string();
        assert!(err.contains("k=v"), "{err}");
        let s = bake_scope(&bp, &["arch=arm64".into()]).unwrap();
        assert_eq!(s.substrate["arch"].as_str(), Some("arm64"));
    }

    /// 端到端:烤一个含**二进制**与多变体的闭包,再装载回来用。
    ///
    /// 这条测试守的是闭包唯一的存在理由 —— 现场断网也能装上。
    async fn bake_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        // 真二进制:含 NUL,不是 UTF-8。文本通道会在这里断掉。
        std::fs::write(dir.path().join("files/tool.bin"), [0u8, 1, 2, 255, 0]).unwrap();
        std::fs::write(dir.path().join("files/arm.bin"), [9u8, 8, 7]).unwrap();
        let bp = dir.path().join("bp.yaml");
        std::fs::write(
            &bp,
            "name: t\n\
             materials:\n\
             \x20 - name: tool\n\
             \x20   file: files/tool.bin\n\
             \x20   when: \"substrate.arch == 'amd64'\"\n\
             \x20 - name: tool\n\
             \x20   file: files/arm.bin\n\
             \x20   when: \"substrate.arch == 'arm64'\"\n\
             resources:\n\
             \x20 - copy: { material: tool, dest: /usr/bin/tool }\n",
        )
        .unwrap();
        let out = dir.path().join("c.tar");
        build(&bp, &out, &[]).await.unwrap();
        (dir, bp, out)
    }

    #[tokio::test]
    async fn a_closure_carries_every_variant_so_the_field_can_never_miss_one() {
        // 构建期不知道要装到哪台 —— 两个架构的字节都得在里面。
        let (_d, _bp, out) = bake_fixture().await;
        let (_tmp, map) = load(&out).unwrap();
        assert_eq!(map.len(), 2, "变体没带全:{map:?}");
        assert!(map.keys().any(|k| k.ends_with("tool.bin")));
        assert!(map.keys().any(|k| k.ends_with("arm.bin")));
    }

    #[tokio::test]
    async fn the_blobs_are_keyed_by_source_url_not_by_material_name() {
        // 按名字索引会让一台 arm64 机器静默拿到 amd64 的字节。
        let (_d, _bp, out) = bake_fixture().await;
        let (_tmp, map) = load(&out).unwrap();
        assert!(!map.contains_key("tool"), "键成了物料名:{map:?}");
    }

    #[tokio::test]
    async fn binary_bytes_survive_the_round_trip_intact() {
        // 闭包运的就是二进制。少一个字节,现场装上的就是坏文件。
        let (_d, _bp, out) = bake_fixture().await;
        let (_tmp, map) = load(&out).unwrap();
        let blob = map.values().find(|p| std::fs::read(p).unwrap().len() == 5).unwrap();
        assert_eq!(std::fs::read(blob).unwrap(), vec![0u8, 1, 2, 255, 0]);
    }

    #[tokio::test]
    async fn a_corrupted_closure_is_rejected_at_load_not_mid_deploy() {
        // 校验发生在连机器之前。字节坏了要在这里知道,不是推到一半。
        let (_d, _bp, out) = bake_fixture().await;
        let scratch = tempfile::tempdir().unwrap();
        let stage = bundle::unpack(&out, &scratch.path().join("stage")).unwrap();
        let m = stage.read_manifest().unwrap();
        std::fs::write(stage.blob_path(&m.blobs[0].sha256), b"tampered").unwrap();
        // 重打包的目标必须**在 stage 之外** —— 否则 tar 会把自己卷进去。
        let repacked = scratch.path().join("bad.tar");
        bundle::pack(stage.root.as_path(), &repacked).unwrap();
        let err = load(&repacked).unwrap_err().to_string();
        assert!(err.contains("完整性") || err.contains("checksum"), "{err}");
    }

    #[tokio::test]
    async fn a_material_url_needing_target_facts_asks_for_a_profile() {
        let dir = tempfile::tempdir().unwrap();
        let bp = dir.path().join("bp.yaml");
        let yaml = [
            "name: t",
            "materials:",
            "  - name: tool",
            "    file: \"https://x/${substrate.arch}/t\"",
            "resources:",
            "  - copy: { material: tool, dest: /usr/bin/t }",
            "",
        ]
        .join("\n");
        std::fs::write(&bp, yaml).unwrap();
        let err = build(&bp, &dir.path().join("c.tar"), &[]).await.unwrap_err().to_string();
        assert!(err.contains("--for"), "报错要给出下一步动作:{err}");
    }

    /// 一个栈:两份蓝图共用 `shared`,另各有一份自己的物料。
    fn stack_fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("files");
        std::fs::create_dir_all(&f).unwrap();
        std::fs::write(f.join("shared.bin"), b"shared").unwrap();
        std::fs::write(f.join("only-a.bin"), b"a").unwrap();
        std::fs::write(f.join("v-2.0.bin"), b"v2").unwrap();
        for (name, extra) in [("a", "only-a.bin"), ("b", "v-${params.version}.bin")] {
            std::fs::write(
                d.path().join(format!("{name}.blueprint.yaml")),
                [
                    &format!("name: {name}"),
                    "params:",
                    "  version: { type: string, default: \"1.0\" }",
                    "materials:",
                    "  - name: shared",
                    "    file: files/shared.bin",
                    "  - name: own",
                    &format!("    file: \"files/{extra}\""),
                    "resources:",
                    "  - copy: { material: shared, dest: /opt/s }",
                    "  - copy: { material: own, dest: /opt/o }",
                    "",
                ]
                .join("\n"),
            )
            .unwrap();
        }
        std::fs::write(
            d.path().join("s.stack.yaml"),
            [
                "stack: demo",
                "uses:",
                "  - blueprint: a",
                "  - blueprint: b",
                "    params: { version: \"2.0\" }",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        d
    }

    #[tokio::test]
    async fn a_stack_bakes_into_one_artifact_with_shared_bytes_stored_once() {
        // 栈里多份蓝图共用 containerd 是常态;逐个 build 会把它复制 N 遍。
        let d = stack_fixture();
        let out = d.path().join("c.tar");
        build_stack(&d.path().join("s.stack.yaml"), &out, &[]).await.unwrap();
        let (_tmp, map) = load(&out).unwrap();
        // shared + a 自己的 + b 自己的 = 3,而不是 4。
        assert_eq!(map.len(), 3, "共用物料被存了不止一份:{map:?}");
        assert_eq!(map.keys().filter(|k| k.contains("shared")).count(), 1);
    }

    #[tokio::test]
    async fn stack_params_reach_the_material_urls() {
        // 物料 URL 常按 `${params.version}` 插值。漏掉栈参数会**静默**烤错版本
        // —— 闭包看起来完整,现场装上的是另一个版本。
        let d = stack_fixture();
        let out = d.path().join("c.tar");
        build_stack(&d.path().join("s.stack.yaml"), &out, &[]).await.unwrap();
        let (_tmp, map) = load(&out).unwrap();
        assert!(
            map.keys().any(|k| k.ends_with("v-2.0.bin")),
            "栈的 version=2.0 没进 URL:{map:?}"
        );
        assert!(!map.keys().any(|k| k.ends_with("v-1.0.bin")), "烤成了蓝图默认值");
    }

    #[tokio::test]
    async fn a_stack_closure_is_loadable_like_any_other() {
        // 部署侧不需要知道闭包是从蓝图还是从栈烤出来的 —— 同一个格式。
        let d = stack_fixture();
        let out = d.path().join("c.tar");
        build_stack(&d.path().join("s.stack.yaml"), &out, &[]).await.unwrap();
        assert!(load(&out).is_ok());
    }

    #[tokio::test]
    async fn a_local_material_is_read_relative_to_the_blueprint() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        std::fs::write(dir.path().join("files/x.conf"), b"body").unwrap();
        let got = fetch_bytes("files/x.conf", dir.path()).await.unwrap();
        assert_eq!(got, b"body");
    }
}
