//! 物料字节**从哪来** —— D-119 定下的窄接口。
//!
//! 在此之前,这个分叉住在 `blueprint.rs::open_closure()` 的一个 `if` 里:
//! 本地 tar 闭包一条路,`oci://` 包一条路,没给 `--closure` 就是空表(目标机
//! 自己联网取)。加第三个后端(rustfs / S3,多站点共享闭包)意味着在那个 `if`
//! 上再长一个分支 —— 于是"加一个后端"变成"改一层"。
//!
//! 现在的形态是:**加一个后端 = 加一个文件 + 工厂里一行**。
//!
//! ## 为什么只有四个方法
//!
//! `contains` / `fetch` / `manifest` / `origin`。没有 `list`、没有 `delete`、
//! 没有 `gc` —— 每个方法都要能说出"谁在调它"。两类看起来"该进来"的东西
//! 恰恰印证了这条纪律:
//!
//! - **镜像**不是"一份字节":它是 config + 若干层 + manifest,共享同一个 blob
//!   池(一套 k8s 十几个镜像共用基础层是常态)。它走 [`ImageMap`],由打开闭包
//!   时一并交出来,不给 trait 加一个只有 tar 后端答得上来的 `images()`。
//! - **系统包**是一个物料对应一批文件(本体 + 依赖),部署侧按 blob 键前缀
//!   `os-pkg://<物料名>/` 一次取全 —— 那是 [`BlobMap`] 上的操作,不是来源的操作。

use std::path::{Path, PathBuf};

use anyhow::Result;
use crater_core::bundle::Manifest;

use crate::material_ctx::{BlobMap, ImageMap};

mod oci;
mod tar;

/// 物料字节的来源。**四个方法** —— 窄到加一个后端是加一个文件,而不是改一层。
pub(crate) trait BlobSource: Send + Sync {
    /// 这个来源里有没有这份物料(按蓝图声明的、渲染后的 source URL 查)。
    fn contains(&self, source: &str) -> bool;

    /// 取字节,返回**本地可读路径**。
    ///
    /// 刻意不返回流:部署侧要把它分块推到目标机,需要的是能反复读、能问大小
    /// 的东西。远端来源在这里负责下载到本地缓存并返回缓存路径 —— 调用方不必
    /// 知道它来自哪。
    fn fetch(&self, source: &str) -> Result<PathBuf>;

    /// 清单:哪些 URL 有字节、各自的摘要与大小。[`blob_map`] 靠它枚举。
    fn manifest(&self) -> &Manifest;

    /// 人可读的来源标识,**只**用于报错与日志("从哪拿的"要说得出来)。
    fn origin(&self) -> String;
}

/// 打开一个来源:`--closure` 给的是什么,就构造什么。
///
/// 三条来源共用一个返回形状,所以调用方(`open_closure`)不再需要按类型分叉。
/// `ImageMap` 单独返回而不是塞进 trait,理由见模块头。
pub(crate) fn open(closure: Option<&Path>) -> Result<(Box<dyn BlobSource>, ImageMap)> {
    // 没给 `--closure`:目标机自己按 URL 取。空来源不是"退化情况",它就是
    // 在线部署的正常形态 —— 让它也是一个 `BlobSource`,调用方才不必区分。
    let Some(path) = closure else {
        return Ok((Box::new(TargetFetches::new()), ImageMap::new()));
    };
    // `oci://<ref>`:物料就是包里的层,已在本地 store 里按 sha256 躺着。
    if let Some(r) = path.to_string_lossy().strip_prefix("oci://") {
        return Ok((Box::new(oci::OciSource::open(r)?), ImageMap::new()));
    }
    let (t, images) = tar::TarClosure::open(path)?;
    Ok((Box::new(t), images))
}

/// 摊平成部署侧那张 `源 URL → 本地路径` 的表。
///
/// **表里只放取得到的**:清单里有、字节却不在本地(瘦拉的包就是这样),那条
/// 就不该进表 —— 塞一个取不到的路径进去,表现是部署到一半才读不到文件。
/// 判断由来源自己做,因为"在不在本地"是后端的知识。
pub(crate) fn blob_map(src: &dyn BlobSource) -> Result<BlobMap> {
    let mut map = BlobMap::new();
    for b in &src.manifest().blobs {
        if !src.contains(&b.source_url) {
            continue;
        }
        map.insert(b.source_url.clone(), src.fetch(&b.source_url)?);
    }
    Ok(map)
}

/// 没有闭包 —— 物料由**目标机自己**按 URL 取(agentless:控制端只编排)。
struct TargetFetches {
    manifest: Manifest,
}

impl TargetFetches {
    fn new() -> Self {
        TargetFetches { manifest: empty_manifest(String::new()) }
    }
}

impl BlobSource for TargetFetches {
    fn contains(&self, _source: &str) -> bool {
        false
    }

    fn fetch(&self, source: &str) -> Result<PathBuf> {
        anyhow::bail!("没有离线闭包 —— 物料 {source} 的字节不在控制端")
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn origin(&self) -> String {
        "目标机自取(未给 --closure)".to_string()
    }
}

/// 一个空清单 —— 给"不是闭包"的来源(空来源、OCI 包)当骨架用。
///
/// `Manifest` / `BlobEntry` 直接复用 `crater_core::bundle` 的,不新造类型:
/// 两边描述的是同一件事(哪个 URL 对应哪个摘要),造第二个只会让它们漂。
fn empty_manifest(name: String) -> Manifest {
    Manifest {
        format_version: crater_core::bundle::BUNDLE_FORMAT_VERSION,
        name,
        components: Vec::new(),
        blobs: Vec::new(),
        images: Vec::new(),
        rootfs: Vec::new(),
    }
}
