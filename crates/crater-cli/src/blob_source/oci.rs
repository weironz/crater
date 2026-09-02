//! `--closure oci://<ref>` —— 物料就是包里的层。
//!
//! 逻辑原封搬自 `pkg::blobs_of`。**不解包、不复制**:store 的 blob 本来就是
//! 内容寻址的文件,直接把路径交出去 —— 于是也没有临时目录要守着。
//!
//! 这是 D-119 说的"第二个 blob 后端",而它当初没让 `BlobSource` 多一个方法;
//! 现在把它搬进接口后面,四个方法仍然是四个方法。

use std::path::PathBuf;

use anyhow::{anyhow, bail, Result};
use crater_core::bundle::{BlobEntry, Manifest};
use crater_core::store::{ImageStore, ANN_MATERIAL_SOURCE, MT_MATERIAL};

use crate::material_ctx::BlobMap;

use super::BlobSource;

/// 本地 store 里一个已拉全的蓝图包。
pub(crate) struct OciSource {
    blobs: BlobMap,
    manifest: Manifest,
    origin: String,
}

impl OciSource {
    pub(crate) fn open(reference: &str) -> Result<OciSource> {
        let store = ImageStore::open()?;
        let m = store.resolve_manifest(reference)?;
        let mut blobs = BlobMap::new();
        let mut entries: Vec<BlobEntry> = Vec::new();
        let mut missing = Vec::new();
        for l in m["layers"].as_array().into_iter().flatten() {
            if l["mediaType"].as_str() != Some(MT_MATERIAL) {
                continue;
            }
            let Some(src) = l["annotations"][ANN_MATERIAL_SOURCE].as_str() else {
                continue;
            };
            let d = l["digest"]
                .as_str()
                .unwrap_or_default()
                .trim_start_matches("sha256:");
            let p = store.blob_path(d);
            // 瘦拉过的包物料层不在本地。报出来而不是静默少一条 —— 少一条的表现
            // 是"部署时目标机自己去联网下载",在断网现场就是装不上。
            if p.exists() {
                blobs.insert(src.to_string(), p);
                entries.push(BlobEntry {
                    source_url: src.to_string(),
                    sha256: d.to_string(),
                    size: l["size"].as_u64().unwrap_or(0),
                });
            } else {
                missing.push(src.to_string());
            }
        }
        if !missing.is_empty() {
            bail!(
                "{reference} 的 {} 份物料不在本地(瘦拉的包只有蓝图层)。\n\
                 先 `crater pkg pull {reference} --full`。缺:{}",
                missing.len(),
                missing.join(", ")
            );
        }
        // 包的 manifest 是 OCI 的那份 JSON,而部署侧读的是 `bundle::Manifest`。
        // 这里把物料层翻译过去 —— 不新造类型,两边描述的是同一件事。
        let manifest = Manifest {
            blobs: entries,
            ..super::empty_manifest(reference.to_string())
        };
        Ok(OciSource {
            blobs,
            manifest,
            // 报错与日志里要说得出"从哪拿的":包里的层与 tar 闭包不是一回事,
            // 找不到物料时这两个字决定了人该去 `pkg pull` 还是去重烤闭包。
            origin: format!("{reference}(包内物料)"),
        })
    }
}

impl BlobSource for OciSource {
    fn contains(&self, source: &str) -> bool {
        self.blobs.contains_key(source)
    }

    fn fetch(&self, source: &str) -> Result<PathBuf> {
        self.blobs
            .get(source)
            .cloned()
            .ok_or_else(|| anyhow!("包 {} 里没有物料 {source}", self.origin))
    }

    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn origin(&self) -> String {
        self.origin.clone()
    }
}
