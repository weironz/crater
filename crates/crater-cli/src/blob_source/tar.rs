//! 单文件带走一切 —— `crater build -f bp.yaml -o closure.tar` 的另一端。
//!
//! 逻辑原封搬自 `closure::load`:解到临时目录、读 manifest、**一次性校验全部
//! blob**。慢几秒,换的是"部署到一半才发现字节坏了"永远不会发生。

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context as _, Result};
use crater_core::bundle::{self, Manifest};

use crate::material_ctx::{BlobMap, ClosureImage, ImageMap};

use super::BlobSource;

/// 一个已解开并校验过的 tar 闭包。
pub(crate) struct TarClosure {
    /// 解包目录。**必须活到部署结束** —— blob 就在里面,提前 drop 会让所有
    /// 物料在推送时凭空消失。所以它由来源持有,而不是让调用方自己记着守住。
    _dir: tempfile::TempDir,
    /// 键是**源 URL** 而不是物料名:同名物料按 `when:` 分成多个变体,各有各的
    /// URL。按名字索引会让"多架构"这个最常见的场景取到错误的字节 ——
    /// 而且是**静默**取错。
    blobs: BlobMap,
    manifest: Manifest,
    origin: String,
}

impl TarClosure {
    /// 打开一个闭包文件。
    ///
    /// 镜像与 blob 一起被读出来,但**不**进 `BlobSource`:镜像不是一份字节
    /// (config + 若干层 + manifest,共享同一个 blob 池),给 trait 加一个只有
    /// 这个后端答得上来的方法,窄接口就开始变宽了。
    pub(crate) fn open(path: &Path) -> Result<(TarClosure, ImageMap)> {
        let dir = tempfile::tempdir()?;
        let stage = bundle::unpack(path, dir.path())
            .with_context(|| format!("解包闭包 {}", path.display()))?;
        let manifest = stage
            .read_manifest()
            .with_context(|| format!("{} 不像是 crater 闭包(读不到 manifest)", path.display()))?;
        // 校验一次全部 blob。慢几秒,换的是"部署到一半才发现字节坏了"永远不会发生。
        stage.verify(&manifest).context("闭包完整性校验")?;

        let blobs: BlobMap = manifest
            .blobs
            .iter()
            .map(|b| (b.source_url.clone(), stage.blob_path(&b.sha256)))
            .collect();
        // 镜像按 **ref** 索引(不是物料名):同名物料按 `when:` 分变体,各有各的 ref。
        let images: ImageMap = manifest
            .images
            .iter()
            .map(|i| {
                (
                    i.reference.clone(),
                    ClosureImage {
                        manifest_digest: i.manifest_digest.clone(),
                        blobs_dir: stage.blobs_dir(),
                    },
                )
            })
            .collect();

        let origin = path.display().to_string();
        Ok((
            TarClosure {
                _dir: dir,
                blobs,
                manifest,
                origin,
            },
            images,
        ))
    }
}

impl BlobSource for TarClosure {
    fn contains(&self, source: &str) -> bool {
        self.blobs.contains_key(source)
    }

    fn fetch(&self, source: &str) -> Result<PathBuf> {
        self.blobs
            .get(source)
            .cloned()
            .ok_or_else(|| anyhow!("闭包 {} 里没有物料 {source}", self.origin))
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn origin(&self) -> String {
        self.origin.clone()
    }
}
