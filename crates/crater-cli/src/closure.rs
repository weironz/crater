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

#[cfg(test)]
use crate::material_ctx::BlobMap;

/// 一次烘焙的累积状态 —— 栈级 bake 靠它把多份蓝图的闭包并成一个。
///
/// 去重发生在两层:`seen` 挡住重复的 URL(不重复下载),`store_blob` 按
/// sha256 落盘(相同字节共用一个文件)。所以"各蓝图闭包的并集"是内容寻址的
/// 自然结果,不需要额外一遍比对。
struct Baker {
    stage: BundleStage,
    blobs: Vec<crater_core::bundle::BlobEntry>,
    images: Vec<crater_core::bundle::ImageRef>,
    seen: BTreeMap<String, ()>,
    skipped: Vec<String>,
}

impl Baker {
    fn new(root: PathBuf) -> Result<Self> {
        Ok(Baker {
            stage: BundleStage::new(root)?,
            blobs: Vec::new(),
            images: Vec::new(),
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
        let (baked, images, skipped) = bake_bytes(
            bp_path,
            profile,
            extra_params,
            &mut self.seen,
            Some(&self.stage),
        )
        .await?;
        self.skipped.extend(skipped);
        let taken = baked.len() + images.len();
        self.images.extend(images);
        for b in baked {
            // `store_blob` 内容寻址、幂等:系统包那一路已经存过一次,这里
            // 再存只是把 BlobEntry 收进清单,不会写第二份字节。
            let entry = self.stage.store_blob(&b.source, &b.bytes)?;
            if !b.source.starts_with(OS_PKG_SCHEME) {
                println!(
                    "  ✓ {:<28} {:>9}  {}",
                    b.name,
                    human(entry.size),
                    &entry.sha256[..12]
                );
            }
            self.blobs.push(entry);
        }
        Ok(taken)
    }

    /// 收尾:写 manifest、打包。
    fn seal(self, name: &str, out: &Path) -> Result<()> {
        for s in &self.skipped {
            println!("  · 跳过 {s}");
        }
        if self.blobs.is_empty() && self.images.is_empty() {
            bail!("没有一个物料被烤进闭包 —— 检查 `materials:` 是不是都是 os_package 类型");
        }
        let total: u64 = self.blobs.iter().map(|b| b.size).sum();
        let count = self.blobs.len();
        let nimg = self.images.len();
        self.stage.write_manifest(&Manifest {
            format_version: bundle::BUNDLE_FORMAT_VERSION,
            name: name.to_string(),
            components: Vec::new(),
            blobs: self.blobs,
            images: self.images,
            rootfs: Vec::new(),
        })?;
        if let Some(dir) = out.parent() {
            if !dir.as_os_str().is_empty() {
                std::fs::create_dir_all(dir)?;
            }
        }
        bundle::pack(self.stage.root.as_path(), out)?;
        let imgs = if nimg > 0 {
            format!(",{nimg} 个镜像")
        } else {
            String::new()
        };
        println!(
            "\n闭包 → {} ({count} 份物料{imgs},{})",
            out.display(),
            human(total)
        );
        println!(
            "现场用法:crater apply -f <蓝图或栈> --closure {}",
            out.display()
        );
        Ok(())
    }
}

/// `crater build -f <blueprint> -o <out.tar> [--for k=v]…`
pub async fn build(bp_path: &Path, out: &Path, profile: &[String], sets: &[String]) -> Result<()> {
    let bp = crater_ir::parse::blueprint_from_path(bp_path)?;
    let tmp = tempfile::tempdir()?;
    let mut baker = Baker::new(tmp.path().to_path_buf())?;
    let overrides = parse_params(sets)?;
    if baker.absorb(bp_path, profile, &overrides).await? == 0 && baker.skipped.is_empty() {
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
pub async fn build_stack(
    stack_path: &Path,
    out: &Path,
    profile: &[String],
    sets: &[String],
) -> Result<()> {
    let st = crater_ir::stack::from_path(stack_path)?;
    let dir = stack_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let tmp = tempfile::tempdir()?;
    let mut baker = Baker::new(tmp.path().to_path_buf())?;

    println!(
        "烘焙栈 `{}` 的离线闭包 —— {} 份蓝图\n",
        st.name,
        st.uses.len()
    );
    for (i, u) in st.uses.iter().enumerate() {
        let bp_path = crater_ir::stack::resolve_ref(&u.blueprint, &dir)?;
        println!("── [{}/{}] {} ──", i + 1, st.uses.len(), u.label());
        // 栈的作者侧参数要带进来:物料 URL 常按 `${params.version}` 插值,
        // 漏掉它会**静默**烤错版本。命令行 `--set` 排在后面,可以盖过它。
        let mut params = u.params.clone();
        params.extend(parse_params(sets)?);
        baker.absorb(&bp_path, profile, &params).await?;
    }
    println!();
    baker.seal(&st.name, out)
}

// 装载一侧(把 closure.tar 解开、校验、摊成 blob 表)搬去了
// `blob_source::tar::TarClosure` —— 那里与 `oci://` 包共用同一个 `BlobSource`,
// 于是 `open_closure()` 不再需要按来源分叉(D-119)。这个文件只剩**烘焙**。

/// 装载一个刚烤好的闭包 —— 只给本文件的测试用。
///
/// 装载一侧已经搬走,但**烤 → 装**这条往返仍然要整条走完:这些用例问的正是
/// "写进去的字节能不能原样取回来",只测其中一半等于没测。
///
/// 返回的 `BlobSource` 必须被调用方持有 —— blob 在它管的临时目录里。
#[cfg(test)]
fn load(
    path: &Path,
) -> Result<(
    Box<dyn crate::blob_source::BlobSource>,
    BlobMap,
    crate::material_ctx::ImageMap,
)> {
    let (src, images) = crate::blob_source::open(Some(path))?;
    let map = crate::blob_source::blob_map(src.as_ref())?;
    Ok((src, map, images))
}

/// `--set k=v` → 参数覆盖。
///
/// 烘焙期的参数选择直接决定**闭包里装的是哪个版本的字节** ——
/// `crater build --set version=1.37.0` 与 `--set version=1.36.1` 产出两个
/// 内容不同的制品。升级在 crater 的模型里就是**换一个闭包**:
/// 版本连同它的摘要一起属于制品,而不是部署时才拼出来的一个字符串。
fn parse_params(sets: &[String]) -> Result<BTreeMap<String, serde_yaml::Value>> {
    let mut out = BTreeMap::new();
    for kv in sets {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("`--set` 要写成 k=v,收到 `{kv}`"))?;
        // 走 YAML 标量解析,`true`/`3` 才不会变成字符串。
        let val: serde_yaml::Value =
            serde_yaml::from_str(v).unwrap_or_else(|_| serde_yaml::Value::String(v.to_string()));
        out.insert(k.to_string(), val);
    }
    Ok(out)
}

/// 烤好的一份物料 —— 字节还在手里,放哪由调用方决定。
///
/// 闭包把它落进 `BundleStage`,`crater build/push/pull` 把它做成一个 OCI 层。两条出口
/// 共用这一次下载与这一次校验:摘要核对写两遍,迟早会有一遍写松。
pub(crate) struct Baked {
    pub name: String,
    pub source: String,
    pub bytes: Vec<u8>,
}

// 镜像烤出来就是一个 `bundle::ImageRef`(reference + manifest 摘要)。
// 不再包一层:`ImageRef.reference` 已经是部署侧查找用的键,物料名在那一侧
// 用不上 —— 同名物料按 `when:` 分变体,各有各的 ref,按名字查会取错。

/// 烤一份蓝图在某个画像下引用到的全部物料字节。
///
/// `seen` 由调用方持有并跨调用复用 —— 栈里多份蓝图共用 containerd 是常态,
/// 多架构打包时两个画像也常有共用物料。同一个 URL 只下载一次。
pub(crate) async fn bake_bytes(
    bp_path: &Path,
    profile: &[String],
    extra_params: &BTreeMap<String, serde_yaml::Value>,
    seen: &mut BTreeMap<String, ()>,
    stage: Option<&BundleStage>,
) -> Result<(Vec<Baked>, Vec<crater_core::bundle::ImageRef>, Vec<String>)> {
    let bp = crater_ir::parse::blueprint_from_path(bp_path)?;
    let base = bp_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut scope = bake_scope(&bp, profile)?;
    for (k, v) in extra_params {
        scope.params.insert(k.clone(), v.clone());
    }
    let items = materials::bake(&bp, &scope, !profile.is_empty());
    println!("烘焙 `{}` —— {} 个物料变体", bp.name, items.len());

    let mut out = Vec::new();
    let mut images = Vec::new();
    let mut skipped = Vec::new();
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
        // 镜像:整棵 OCI 树进闭包的共享 blob 池(D-131)。没有 stage 的调用方
        // (`crater build/push/pull` 的物料层)暂时收不了镜像 —— 如实跳过而不是假装收了。
        if plan.kind == MaterialKind::Image {
            let Some(stage) = stage else {
                skipped.push(format!("{}(镜像:此出口尚不支持)", item.label()));
                continue;
            };
            if seen.insert(plan.source.clone(), ()).is_some() {
                println!("  ↩ {:<28} (已在闭包中)", item.name);
                continue;
            }
            let img = stage
                .pull_image(&plan.source)
                .await
                .with_context(|| format!("烘焙镜像 {}({})", item.label(), plan.source))?;
            println!(
                "  ✓ {:<28} {:>9}  {}",
                item.name,
                "镜像",
                &img.manifest_digest[..12]
            );
            images.push(img);
            continue;
        }
        // 系统包:在**同族容器**里跑一次下载,连依赖一起烤进闭包(D-132)。
        if plan.kind == MaterialKind::OsPackage {
            let Some(stage) = stage else {
                skipped.push(format!("{}(系统包:此出口尚不支持)", item.label()));
                continue;
            };
            let debs = bake_os_package(&bp, &item.name, profile).await?;
            for (file, bytes) in debs {
                // 键里带物料名:部署时按前缀一次取全这个物料的所有包文件。
                let key = format!("{OS_PKG_SCHEME}{}/{file}", item.name);
                if seen.insert(key.clone(), ()).is_some() {
                    continue;
                }
                let entry = stage.store_blob(&key, &bytes)?;
                println!(
                    "  ✓ {:<28} {:>9}  {}",
                    file,
                    human(entry.size),
                    &entry.sha256[..12]
                );
                out.push(Baked {
                    name: item.name.clone(),
                    source: key,
                    bytes,
                });
            }
            continue;
        }
        if plan.kind != MaterialKind::File {
            skipped.push(format!("{} ({:?} 类型)", item.label(), plan.kind));
            continue;
        }
        // 同一个 URL 只取一次 —— 跨蓝图、跨架构共享物料都是常态。
        if seen.insert(plan.source.clone(), ()).is_some() {
            println!("  ↩ {:<28} (已在闭包中)", item.name);
            continue;
        }

        let bytes = fetch_bytes(&plan.source, &base)
            .await
            .with_context(|| format!("烘焙物料 {}", item.label()))?;
        // 声明了摘要就当场核对:烤进闭包的字节错了,现场是查不出来的。
        // 注意核对发生在解包**之前** —— 上游发布的 checksum 是对下载物
        // (zip)的,不是对里面那个成员的。
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
        // `unzip:` 在**这里**兑现(D-103:目标机零依赖,解包只能在控制端)。
        // 闭包里存的是解出来的成员,不是 zip —— 否则推到目标机的就是个
        // 没人解得开的压缩包,闭包等于白建。
        let bytes = match &plan.unzip {
            Some(member) => crater_core::zip::extract_member(&bytes, member)
                .with_context(|| format!("物料 {}:从 zip 抽取 `{member}`", item.label()))?,
            None => bytes,
        };
        out.push(Baked {
            name: item.name.clone(),
            source: plan.source.clone(),
            bytes,
        });
    }
    Ok((out, images, skipped))
}

/// 系统包 blob 的键前缀。部署时按 `os-pkg://<物料名>/` 一次取全。
pub(crate) const OS_PKG_SCHEME: &str = "os-pkg://";

/// 在**同族容器**里把一个 `os_package` 物料连依赖一起下载下来。
///
/// 为什么必须用容器:依赖解析是发行版包管理器的活,自己实现等于重写 apt 的
/// 求解器。而解析结果与"在什么样的系统上解"强相关 —— 只有跑在同族同版本的
/// 环境里,拿到的依赖集才对得上目标机。
///
/// 这给**控制端**加了一个依赖(docker 或 podman)。目标机仍然零依赖,而且
/// 只有真的要烤系统包时才需要 —— 这是"离线装 nginx"与"根本装不了"之间的
/// 交换。缺了就明说缺什么、怎么补,不静默降级成"只下一个包不带依赖"。
async fn bake_os_package(
    bp: &Blueprint,
    name: &str,
    profile: &[String],
) -> Result<Vec<(String, Vec<u8>)>> {
    let image = profile
        .iter()
        .find_map(|kv| kv.strip_prefix("os_image="))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "物料 `{name}` 是系统包 —— 需要知道**在什么系统上解依赖**才能烤。\n\
             加一句 `--for os_image=ubuntu:24.04`(或 rockylinux:9 等),\n\
             用与目标机同族同版本的镜像,否则依赖集对不上。"
            )
        })?
        .to_string();

    let runner = ["docker", "podman"]
        .into_iter()
        .find(|c| {
            std::process::Command::new(c)
                .arg("--version")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "烤系统包需要控制端有 docker 或 podman —— 依赖解析只能交给发行版\n\
             自己的包管理器,在同族容器里跑一次。\n\
             (目标机不需要它们;这只是构建期的事。)"
            )
        })?;

    // 家族按镜像里有什么命令判定,不按镜像名猜 —— `ubuntu`/`debian`/`ghcr.io/...`
    // 各种写法都有,猜名字必然漏。
    let m = bp
        .materials
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow::anyhow!("物料 `{name}` 不见了"))?;
    // `os_package:` 的来源是按 family 的包名表 —— 它是**字面量**,不插值:
    // 包名依赖目标机的家族,而家族在构建期还不知道,所以两边都写清楚才对。
    let table = match &m.source {
        crater_ir::ir::Value::Map(t) => t,
        _ => bail!("物料 `{name}`:`os_package:` 应是按 family 的包名表,如 `{{debian: [nginx]}}`"),
    };

    let tmp = tempfile::tempdir()?;
    let out = tmp.path().to_path_buf();
    let mut got: Vec<(String, Vec<u8>)> = Vec::new();

    for (family, key, script) in [
        ("debian", "debian", DEB_SCRIPT),
        ("rhel", "rhel", RPM_SCRIPT),
    ] {
        let Some(list) = table.get(key) else { continue };
        let pkgs: Vec<String> = match list {
            crater_ir::ir::Value::List(xs) => xs
                .iter()
                .filter_map(|v| match v {
                    crater_ir::ir::Value::Lit(y) => Some(crater_ir::eval::scalar_to_string(y)),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        if pkgs.is_empty() {
            continue;
        }
        println!(
            "  · {name}({family}):在 {image} 里解 {} 个包的依赖",
            pkgs.len()
        );
        let status = std::process::Command::new(runner)
            .args(["run", "--rm", "-v"])
            .arg(format!("{}:/out", out.display()))
            .arg(&image)
            .args(["sh", "-c", &script.replace("__PKGS__", &pkgs.join(" "))])
            .status()?;
        if !status.success() {
            bail!(
                "在 {image} 里下载 {name} 的包失败 —— 检查包名与镜像是否同族。\n\
                 (镜像里的家族由它自己有哪个包管理器决定,与镜像名无关)"
            );
        }
    }

    for e in std::fs::read_dir(&out)?.flatten() {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let fname = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        if !(fname.ends_with(".deb") || fname.ends_with(".rpm")) {
            continue;
        }
        got.push((fname, std::fs::read(&p)?));
    }
    got.sort_by(|a, b| a.0.cmp(&b.0)); // 可复现
    if got.is_empty() {
        bail!("物料 `{name}`:一个包文件都没下到 —— 检查包名与 `--for os_image=`");
    }
    Ok(got)
}

/// 只下载不安装,连依赖一起。`-o Dir::Cache::archives` 把 .deb 直接落到挂载目录。
const DEB_SCRIPT: &str = "set -e; export DEBIAN_FRONTEND=noninteractive; \
  apt-get update -qq; \
  apt-get install -y --download-only --reinstall -o Dir::Cache::archives=/out __PKGS__; \
  chmod -R a+r /out";

/// dnf/yum 的等价物。`--resolve` 才会把依赖一起拉下来。
const RPM_SCRIPT: &str = "set -e; \
  (command -v dnf >/dev/null && dnf install -y --downloadonly --downloaddir=/out --resolve __PKGS__) \
  || (yum install -y --downloadonly --downloaddir=/out --resolve __PKGS__); \
  chmod -R a+r /out";

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
        build(&bp, &out, &[], &[]).await.unwrap();
        (dir, bp, out)
    }

    #[tokio::test]
    async fn a_closure_carries_every_variant_so_the_field_can_never_miss_one() {
        // 构建期不知道要装到哪台 —— 两个架构的字节都得在里面。
        let (_d, _bp, out) = bake_fixture().await;
        let (_tmp, map, _imgs) = load(&out).unwrap();
        assert_eq!(map.len(), 2, "变体没带全:{map:?}");
        assert!(map.keys().any(|k| k.ends_with("tool.bin")));
        assert!(map.keys().any(|k| k.ends_with("arm.bin")));
    }

    #[tokio::test]
    async fn the_blobs_are_keyed_by_source_url_not_by_material_name() {
        // 按名字索引会让一台 arm64 机器静默拿到 amd64 的字节。
        let (_d, _bp, out) = bake_fixture().await;
        let (_tmp, map, _imgs) = load(&out).unwrap();
        assert!(!map.contains_key("tool"), "键成了物料名:{map:?}");
    }

    #[tokio::test]
    async fn binary_bytes_survive_the_round_trip_intact() {
        // 闭包运的就是二进制。少一个字节,现场装上的就是坏文件。
        let (_d, _bp, out) = bake_fixture().await;
        let (_tmp, map, _imgs) = load(&out).unwrap();
        let blob = map
            .values()
            .find(|p| std::fs::read(p).unwrap().len() == 5)
            .unwrap();
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
        // `BlobSource` 是 trait object,没有 `Debug` —— 用 `err()` 取错误本身,
        // 而不是 `unwrap_err()`(它要求成功值可打印)。
        let err = load(&repacked).err().expect("坏闭包必须被拒").to_string();
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
        let err = build(&bp, &dir.path().join("c.tar"), &[], &[])
            .await
            .unwrap_err()
            .to_string();
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
        build_stack(&d.path().join("s.stack.yaml"), &out, &[], &[])
            .await
            .unwrap();
        let (_tmp, map, _imgs) = load(&out).unwrap();
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
        build_stack(&d.path().join("s.stack.yaml"), &out, &[], &[])
            .await
            .unwrap();
        let (_tmp, map, _imgs) = load(&out).unwrap();
        assert!(
            map.keys().any(|k| k.ends_with("v-2.0.bin")),
            "栈的 version=2.0 没进 URL:{map:?}"
        );
        assert!(
            !map.keys().any(|k| k.ends_with("v-1.0.bin")),
            "烤成了蓝图默认值"
        );
    }

    #[tokio::test]
    async fn a_stack_closure_is_loadable_like_any_other() {
        // 部署侧不需要知道闭包是从蓝图还是从栈烤出来的 —— 同一个格式。
        let d = stack_fixture();
        let out = d.path().join("c.tar");
        build_stack(&d.path().join("s.stack.yaml"), &out, &[], &[])
            .await
            .unwrap();
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

#[cfg(test)]
mod unzip_tests {
    use super::*;

    /// 手造一个单成员、无压缩(stored)的最小 zip。
    /// zip 的写侧是刻意不发行的(crater 只读不写),测试就地拼字节。
    fn make_zip(member: &str, content: &[u8]) -> Vec<u8> {
        let name = member.as_bytes();
        let n = content.len() as u32;
        let mut out = Vec::new();
        // Local File Header
        out.extend_from_slice(&0x04034b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0]); // ver, flags, method=0(stored)
        out.extend_from_slice(&[0; 8]); // time, date, crc(读侧按 CD 尺寸取,不验 crc)
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(name);
        out.extend_from_slice(content);
        // Central Directory
        let cd_off = out.len() as u32;
        out.extend_from_slice(&0x02014b50u32.to_le_bytes());
        out.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0]); // made-by, needed, flags, method=0
        out.extend_from_slice(&[0; 8]); // time, date, crc
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&n.to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0; 12]); // extra/comment/disk/attrs
        out.extend_from_slice(&0u32.to_le_bytes()); // LFH offset
        out.extend_from_slice(name);
        let cd_size = out.len() as u32 - cd_off;
        // End Of Central Directory
        out.extend_from_slice(&0x06054b50u32.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&cd_size.to_le_bytes());
        out.extend_from_slice(&cd_off.to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out
    }

    #[tokio::test]
    async fn an_unzip_material_is_extracted_at_bake_time_not_shipped_as_a_zip() {
        // 此前 build 只存原样 zip 字节 —— 推到目标机的是个没人解得开的压缩包,
        // 而部署路径还叫你"先 build 闭包":两头都对,合起来是死路。
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        let zip_bytes = make_zip("rustfs", b"i-am-the-binary");
        std::fs::write(dir.path().join("files/rustfs.zip"), &zip_bytes).unwrap();
        let bp = dir.path().join("bp.yaml");
        std::fs::write(
            &bp,
            [
                "name: t",
                "materials:",
                "  - name: tool",
                "    file: files/rustfs.zip",
                "    unzip: rustfs",
                "resources:",
                "  - copy: { material: tool, dest: /usr/local/bin/rustfs }",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        let out = dir.path().join("c.tar");
        build(&bp, &out, &[], &[]).await.unwrap();
        let (_tmp, map, _imgs) = load(&out).unwrap();
        let blob = map.values().next().unwrap();
        assert_eq!(
            std::fs::read(blob).unwrap(),
            b"i-am-the-binary",
            "闭包里应是解出来的成员,不是 zip"
        );
    }

    #[tokio::test]
    async fn the_declared_sha_is_checked_against_the_zip_before_extraction() {
        // 上游 checksum 是对下载物(zip)发布的 —— 核对必须在解包之前、
        // 针对 zip 字节;搞反了每个带摘要的 unzip 物料都烤不出来。
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("files")).unwrap();
        let zip_bytes = make_zip("tool", b"payload");
        let zip_sha = crater_core::bundle::sha256_hex(&zip_bytes);
        std::fs::write(dir.path().join("files/t.zip"), &zip_bytes).unwrap();
        let bp = dir.path().join("bp.yaml");
        std::fs::write(
            &bp,
            [
                "name: t",
                "materials:",
                "  - name: tool",
                "    file: files/t.zip",
                &format!("    sha256: \"{zip_sha}\""),
                "    unzip: tool",
                "resources:",
                "  - copy: { material: tool, dest: /usr/bin/t }",
                "",
            ]
            .join("\n"),
        )
        .unwrap();
        let out = dir.path().join("c.tar");
        build(&bp, &out, &[], &[])
            .await
            .expect("zip 摘要对得上就该烤成");
        let (_tmp, map, _imgs) = load(&out).unwrap();
        assert_eq!(
            std::fs::read(map.values().next().unwrap()).unwrap(),
            b"payload"
        );
    }
}
