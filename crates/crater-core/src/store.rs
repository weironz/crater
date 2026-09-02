//! Local OCI image store + registry client (D-018 ②: pull/push/images).
//!
//! The store is an accumulating OCI Image Layout under `~/.crater/store`
//! (override with `$CRATER_HOME`): `oci-layout` + `index.json` (one manifest per
//! tagged ref) + `blobs/sha256/`. Pull writes images here; `crater images` lists
//! it; `crater apply <ref>` resolves from here (pulling on miss). Registry I/O is
//! pure-Rust via `oci-client` (rustls); creds live in `~/.crater/auth.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::bundle::sha256_hex;

const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const ANN_REF: &str = "org.opencontainers.image.ref.name";
// B 类 artifact layer typing (D-087) — kept in sync with bundle.rs, used to
// skip `dependency` material layers on a thin pull.
pub const MT_MATERIAL: &str = "application/vnd.crater.material.v1";
pub const ANN_MATERIAL_FETCH: &str = "org.crater.material.fetch";
/// 物料层的来源 URL —— 部署侧的 `BlobMap` 按它索引(不是按物料名:
/// 同名物料按 `when:` 分成多个变体,各有各的 URL)。
pub const ANN_MATERIAL_SOURCE: &str = "org.crater.material.source";
pub const ANN_MATERIAL_NAME: &str = "org.crater.material.name";
// Project artifact typing (D-098) — kept in sync with bundle.rs.
const AT_PROJECT: &str = "application/vnd.crater.project.v1";
const MT_RECIPE: &str = "application/vnd.crater.recipe.v1+yaml";
// 蓝图包(D-123)。刻意**不设 `artifactType`**:制品身份靠 `config.mediaType`,
// 那是 OCI 1.0 时代的老约定,也是唯一跨得过 ACR / Docker Hub / GHCR / Harbor /
// zot 的写法 —— `artifactType` + 空 config 描述符是 1.1 写法,ACR 会拒收。
pub const MT_PKG_CONFIG: &str = "application/vnd.crater.blueprint.config.v1+json";
pub const MT_PKG_LAYER: &str = "application/vnd.crater.blueprint.v1.tar+gzip";

#[derive(Debug, Clone)]
pub struct StoredImage {
    pub reference: String,
    pub digest: String,
    /// The manifest descriptor size (index entry). Kept for back-compat.
    pub size: u64,
    /// Logical content size = config + all layers (what a registry transfers).
    pub content_size: u64,
    /// Actual on-disk bytes of this artifact's blobs (manifest + config +
    /// layers). For crater (uncompressed material layers) ≈ content_size.
    pub disk_usage: u64,
}

pub struct ImageStore {
    pub root: PathBuf,
}

/// Serializes index.json read-modify-write (D-078④): the blob store is
/// content-addressed (concurrent writes are naturally safe), but the index is
/// a whole-file rewrite — two concurrent pulls tagging at once would lose one
/// entry. Process-wide; the critical section is a few sync fs ops, no awaits.
fn index_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

impl ImageStore {
    /// `$CRATER_HOME` or `~/.crater`.
    pub fn home() -> PathBuf {
        if let Ok(h) = std::env::var("CRATER_HOME") {
            return PathBuf::from(h);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
        PathBuf::from(home).join(".crater")
    }

    pub fn open() -> crate::Result<Self> {
        let root = Self::home().join("store");
        std::fs::create_dir_all(root.join("blobs").join("sha256"))?;
        let layout = root.join("oci-layout");
        if !layout.exists() {
            std::fs::write(&layout, br#"{"imageLayoutVersion":"1.0.0"}"#)?;
        }
        let idx = root.join("index.json");
        if !idx.exists() {
            std::fs::write(
                &idx,
                br#"{"schemaVersion":2,"mediaType":"application/vnd.oci.image.index.v1+json","manifests":[]}"#,
            )?;
        }
        Ok(Self { root })
    }

    pub fn blob_path(&self, digest: &str) -> PathBuf {
        self.root.join("blobs").join("sha256").join(digest)
    }

    fn store_raw(&self, data: &[u8]) -> crate::Result<(String, u64)> {
        let s = sha256_hex(data);
        std::fs::write(self.blob_path(&s), data)?;
        Ok((s, data.len() as u64))
    }

    fn read_index(&self) -> crate::Result<serde_json::Value> {
        Ok(serde_json::from_slice(&std::fs::read(self.root.join("index.json"))?)?)
    }
    fn write_index(&self, v: &serde_json::Value) -> crate::Result<()> {
        // Atomic replace (D-078④): `fs::write` truncates THEN writes, so a
        // concurrent reader could see a torn/empty file. tmp + rename gives
        // readers a complete snapshot always; index_lock guards lost updates.
        let tmp = self.root.join("index.json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(v)?)?;
        std::fs::rename(&tmp, self.root.join("index.json"))?;
        Ok(())
    }

    pub fn list(&self) -> crate::Result<Vec<StoredImage>> {
        let idx = self.read_index()?;
        let mut out = Vec::new();
        if let Some(ms) = idx["manifests"].as_array() {
            for m in ms {
                let digest = m["digest"].as_str().unwrap_or("").to_string();
                let (content_size, disk_usage) = self.artifact_sizes(&digest).unwrap_or((0, 0));
                out.push(StoredImage {
                    reference: m["annotations"][ANN_REF].as_str().unwrap_or("<untagged>").to_string(),
                    digest,
                    size: m["size"].as_u64().unwrap_or(0),
                    content_size,
                    disk_usage,
                });
            }
        }
        Ok(out)
    }

    /// Sum an artifact's real sizes from its manifest: `content_size` = config +
    /// layers (declared); `disk_usage` = on-disk bytes of manifest + config +
    /// layer blobs (what it actually costs locally).
    fn artifact_sizes(&self, manifest_digest: &str) -> crate::Result<(u64, u64)> {
        let on_disk = |digest: &str| -> u64 {
            std::fs::metadata(self.blob_path(strip(digest)))
                .map(|m| m.len())
                .unwrap_or(0)
        };
        let man_path = self.blob_path(strip(manifest_digest));
        let man: serde_json::Value = serde_json::from_slice(&std::fs::read(&man_path)?)?;
        let mut content = man["config"]["size"].as_u64().unwrap_or(0);
        let mut disk = on_disk(manifest_digest)
            + man["config"]["digest"].as_str().map(on_disk).unwrap_or(0);
        if let Some(layers) = man["layers"].as_array() {
            for l in layers {
                content += l["size"].as_u64().unwrap_or(0);
                disk += l["digest"].as_str().map(on_disk).unwrap_or(0);
            }
        }
        Ok((content, disk))
    }

    pub fn has(&self, reference: &str) -> bool {
        self.list()
            .map(|l| l.iter().any(|s| s.reference == reference))
            .unwrap_or(false)
    }

    /// `crater tag <src> <dst>`: point a new reference at an existing stored
    /// image/artifact's manifest. Blobs are content-addressed and shared, so no
    /// data is copied — only a new index entry is added (like `docker tag`).
    pub fn retag(&self, src: &str, dst: &str) -> crate::Result<()> {
        let idx = self.read_index()?;
        let entry = idx["manifests"]
            .as_array()
            .and_then(|a| a.iter().find(|m| m["annotations"][ANN_REF].as_str() == Some(src)))
            .ok_or_else(|| anyhow::anyhow!("image '{src}' not in local store (pull/build/load it first)"))?;
        let digest = strip(entry["digest"].as_str().unwrap_or("")).to_string();
        let size = entry["size"].as_u64().unwrap_or(0);
        self.tag(dst, &digest, size)
    }

    /// Tag (add/replace) a manifest in the index under `reference`.
    /// Read-modify-write under [`index_lock`] — concurrent pulls (D-078④
    /// parallel image fetch) must not lose each other's entries.
    fn tag(&self, reference: &str, manifest_digest: &str, size: u64) -> crate::Result<()> {
        let _g = index_lock().lock().unwrap();
        let mut idx = self.read_index()?;
        let arr = idx["manifests"].as_array_mut().unwrap();
        arr.retain(|m| m["annotations"][ANN_REF].as_str() != Some(reference));
        arr.push(json!({
            "mediaType": MT_MANIFEST,
            "digest": format!("sha256:{manifest_digest}"),
            "size": size,
            "annotations": { ANN_REF: reference }
        }));
        self.write_index(&idx)
    }

    /// `crater rmi <ref>`: drop the reference from the index (like `docker
    /// rmi`). Blobs stay — content-addressed and possibly shared with other
    /// refs; `gc()` sweeps the ones nothing references anymore.
    pub fn remove(&self, reference: &str) -> crate::Result<bool> {
        let _g = index_lock().lock().unwrap();
        let mut idx = self.read_index()?;
        let arr = idx["manifests"].as_array_mut().unwrap();
        let before = arr.len();
        arr.retain(|m| m["annotations"][ANN_REF].as_str() != Some(reference));
        let removed = arr.len() < before;
        if removed {
            self.write_index(&idx)?;
        }
        Ok(removed)
    }

    /// Garbage-collect unreferenced blobs (mark-and-sweep). Mark: every digest
    /// reachable from the index — manifest blobs, their `config` + `layers`,
    /// recursing through nested indexes (`manifests`, multi-arch). Sweep: files
    /// under `blobs/sha256/` nobody references. `dry_run` reports only.
    /// Returns (blobs swept, bytes freed).
    pub fn gc(&self, dry_run: bool) -> crate::Result<(usize, u64)> {
        let idx = self.read_index()?;
        let mut keep: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let mut queue: Vec<String> = idx["manifests"]
            .as_array()
            .map(|a| a.iter().filter_map(|m| m["digest"].as_str()).map(|d| strip(d).to_string()).collect())
            .unwrap_or_default();
        while let Some(d) = queue.pop() {
            if !keep.insert(d.clone()) {
                continue; // already marked
            }
            // A reachable blob that parses as JSON may reference further blobs
            // (manifest: config+layers; nested index: manifests).
            let Ok(bytes) = std::fs::read(self.blob_path(&d)) else { continue };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
            if let Some(c) = v["config"]["digest"].as_str() {
                queue.push(strip(c).to_string());
            }
            for arr in [&v["layers"], &v["manifests"]] {
                if let Some(items) = arr.as_array() {
                    queue.extend(items.iter().filter_map(|l| l["digest"].as_str()).map(|s| strip(s).to_string()));
                }
            }
        }
        let mut swept = 0usize;
        let mut freed = 0u64;
        for entry in std::fs::read_dir(self.blobs_dir())?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if keep.contains(&name) {
                continue;
            }
            freed += entry.metadata().map(|m| m.len()).unwrap_or(0);
            swept += 1;
            if !dry_run {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok((swept, freed))
    }

    /// Pull an image/artifact from a registry into the store (pure Rust,
    /// oci-client). We store the **raw manifest bytes** (via `pull_manifest_raw`)
    /// so `artifactType` + custom layer mediaTypes survive the round-trip — the
    /// high-level `pull()` would synthesize a plain image-manifest and drop them
    /// (D-033). Blob *data* still comes from `pull()`; their sha256 match the
    /// digests the raw manifest references (content-addressed).
    pub async fn pull(&self, reference: &str) -> crate::Result<()> {
        self.pull_layers(reference, false).await
    }

    /// Thin pull (D-087): manifest + config + recipe + `embedded` material layers
    /// only. `dependency` layers (have url/ref/pkgs) are left in the registry and
    /// fetched online at apply — the basis for thin-online deploy of a ref.
    pub async fn pull_thin(&self, reference: &str) -> crate::Result<()> {
        self.pull_layers(reference, true).await
    }

    /// True iff every blob (config + all layers) this artifact's manifest
    /// references is present locally (D-087): distinguishes a full local copy
    /// from a thin pull, so an `--offline` apply can re-pull in full if needed.
    pub fn has_all_layers(&self, reference: &str) -> bool {
        let Ok(m) = self.resolve_manifest(reference) else { return false };
        let cfg_ok = m["config"]["digest"]
            .as_str()
            .map(|d| self.blob_path(strip(d)).exists())
            .unwrap_or(true);
        let layers_ok = m["layers"].as_array().map(|ls| {
            ls.iter().all(|l| {
                l["digest"].as_str().map(|d| self.blob_path(strip(d)).exists()).unwrap_or(false)
            })
        }).unwrap_or(true);
        cfg_ok && layers_ok
    }

    async fn pull_layers(&self, reference: &str, thin: bool) -> crate::Result<()> {
        use oci_client::Reference;

        let r: Reference = reference
            .parse()
            .map_err(|e| anyhow::anyhow!("bad image ref '{reference}': {e}"))?;
        let client = registry_client();
        let auth = auth_for(reference);
        let accepted = accepted_media_types();
        let (raw, _digest) = client
            .pull_manifest_raw(&r, &auth, &accepted)
            .await
            .map_err(|e| anyhow::anyhow!("pull manifest '{reference}': {e}"))?;

        let top: serde_json::Value = serde_json::from_slice(&raw)?;
        // Multi-arch images are a manifest list / image index — resolve to a
        // single-platform image manifest (default linux/amd64) so we pack a
        // concrete, importable image (D-061). crater B 类 artifacts are single
        // manifests (no `manifests:` array) → used as-is, unchanged.
        let manifest_raw = if top.get("manifests").and_then(|v| v.as_array()).is_some() {
            let entries = top["manifests"].as_array().unwrap();
            // 架构优先级:本机 → amd64 → 任意一条(D-127)。
            //
            // 原来只认 amd64(D-061),在 arm64 机器上会静默装错架构的字节 ——
            // 而"静默"正是这里最贵的部分:摘要对得上(那是 amd64 那份的摘要),
            // 直到目标机上 exec 才报 Exec format error。
            let want = crate::arch::detect_local().as_str();
            let pick = |a: &str| {
                entries.iter().find(|e| {
                    e["platform"]["architecture"].as_str() == Some(a)
                        && e["platform"]["os"].as_str().unwrap_or("linux") == "linux"
                })
            };
            let sub = pick(want)
                .or_else(|| pick("amd64"))
                .or_else(|| entries.iter().find(|e| e["platform"]["architecture"].is_string()))
                .ok_or_else(|| anyhow::anyhow!(
                    "'{reference}' 的 index 里没有 linux/{want},也没有任何带架构的条目"
                ))?;
            let sub_dig = sub["digest"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("manifest list entry missing digest"))?
                .to_string();
            // A sub-manifest lives on the MANIFESTS endpoint, not /blobs — fetch
            // it by a digest-pinned reference (docker.io is lenient, but aliyun/
            // harbor 404 on /blobs/<manifest-digest>).
            let digest_ref: Reference = format!("{}/{}@{}", r.registry(), r.repository(), sub_dig)
                .parse()
                .map_err(|e| anyhow::anyhow!("bad digest ref for {reference}: {e}"))?;
            let (sub_raw, _) = client
                .pull_manifest_raw(&digest_ref, &auth, &accepted)
                .await
                .map_err(|e| anyhow::anyhow!("pull sub-manifest {sub_dig} of '{reference}': {e}"))?;
            sub_raw.to_vec()
        } else {
            raw.to_vec()
        };

        // Fetch config + each layer blob by digest. We use `pull_blob` (not the
        // high-level `pull`, which rejects non-image layer mediaTypes like our
        // crater.recipe/material) so artifacts pull cleanly.
        let m: serde_json::Value = serde_json::from_slice(&manifest_raw)?;
        let mut digests: Vec<String> = Vec::new();
        if let Some(d) = m["config"]["digest"].as_str() {
            digests.push(d.to_string());
        }
        if let Some(ls) = m["layers"].as_array() {
            for l in ls {
                // Thin pull (D-087): skip `dependency` material layers (fetched
                // online at apply). recipe + `embedded` (self-authored) + any
                // non-material layer are always pulled. Missing fetch annotation
                // ⇒ embedded (old artifacts pull in full — safe).
                if thin {
                    let mt_l = l["mediaType"].as_str().unwrap_or("");
                    let fetch = l["annotations"][ANN_MATERIAL_FETCH].as_str().unwrap_or("embedded");
                    if mt_l == MT_MATERIAL && fetch == "dependency" {
                        continue;
                    }
                }
                if let Some(d) = l["digest"].as_str() {
                    digests.push(d.to_string());
                }
            }
        }
        for d in &digests {
            // Incremental pull (D-078): blobs are content-addressed, so a blob
            // already in the store IS this digest's content — skip the network
            // fetch. The manifest itself is always re-fetched (cheap), so a moved
            // tag still picks up new layers; only unchanged layers are skipped.
            if self.blob_path(strip(d)).exists() {
                continue;
            }
            let mut buf: Vec<u8> = Vec::new();
            client
                .pull_blob(&r, d.as_str(), &mut buf)
                .await
                .map_err(|e| anyhow::anyhow!("pull blob {d} of '{reference}': {e}"))?;
            self.store_raw(&buf)?;
        }
        let (md, ms) = self.store_raw(&manifest_raw)?;
        self.tag(reference, &md, ms)?;
        Ok(())
    }

    /// Push a stored image/artifact to a registry (oci-client). Re-serializes the
    /// stored manifest through `OciImageManifest` so `artifactType` (B 类 marker)
    /// + custom layer mediaTypes are preserved on the wire (D-033).
    ///
    /// An image **index** (multi-arch, D-127) is pushed sub-manifests first:
    /// a registry rejects an index whose children it has never seen, and each
    /// child must be reachable by digest before the index names it.
    pub async fn push(&self, reference: &str) -> crate::Result<()> {
        let manifest_blob = self.manifest_blob(reference)?;
        let top: serde_json::Value = serde_json::from_slice(&manifest_blob)?;
        if top.get("manifests").and_then(|v| v.as_array()).is_some() {
            return self.push_index(reference, &top, &manifest_blob).await;
        }
        self.push_manifest_blob(reference, &manifest_blob).await
    }

    /// Push one image manifest and every blob it names.
    async fn push_manifest_blob(&self, reference: &str, manifest_blob: &[u8]) -> crate::Result<()> {
        use oci_client::manifest::{OciImageManifest, OciManifest};
        use oci_client::{Reference, RegistryOperation};

        let im: OciImageManifest = serde_json::from_slice(manifest_blob)?;
        let r: Reference = reference
            .parse()
            .map_err(|e| anyhow::anyhow!("bad image ref '{reference}': {e}"))?;
        let client = registry_client();
        let auth = auth_for(reference);
        client
            .auth(&r, &auth, RegistryOperation::Push)
            .await
            .map_err(|e| anyhow::anyhow!("auth '{reference}': {e}"))?;

        // push config + each layer blob, then the manifest itself.
        let cfg = std::fs::read(self.blob_path(strip(&im.config.digest)))?;
        client
            .push_blob(&r, cfg, &im.config.digest)
            .await
            .map_err(|e| anyhow::anyhow!("push config '{reference}': {e}"))?;
        for l in &im.layers {
            let data = std::fs::read(self.blob_path(strip(&l.digest)))?;
            client
                .push_blob(&r, data, &l.digest)
                .await
                .map_err(|e| anyhow::anyhow!("push layer '{reference}': {e}"))?;
        }
        client
            .push_manifest(&r, &OciManifest::Image(im))
            .await
            .map_err(|e| anyhow::anyhow!("push '{reference}': {e}"))?;
        Ok(())
    }

    /// 存一份 blob,返回 (sha256, size)。内容寻址,重复写入是幂等的。
    pub fn put_blob(&self, data: &[u8]) -> crate::Result<(String, u64)> {
        self.store_raw(data)
    }

    /// 存下一份 manifest 并打上引用 —— `pkg push` 之前把制品落进本地 store,
    /// 于是"推上去的"与"本地留着的"是同一份字节,而不是各算一遍。
    pub fn put_manifest(&self, reference: &str, manifest: &[u8]) -> crate::Result<String> {
        let (d, sz) = self.store_raw(manifest)?;
        self.tag(reference, &d, sz)?;
        Ok(d)
    }

    /// 只取 manifest 与 config,**一层都不下载**,也不写本地 store。
    ///
    /// `pkg inspect` 与 UI 的远端目录靠它:契约(参数/机群/物料清单)全在
    /// config 里,几百字节就能回答"这东西要我给什么"。多架构时按 platform
    /// 挑一份子 manifest —— 契约与架构无关,取哪份都一样。
    /// 返回 (子 manifest, config 字节, 支持的 `os/arch` 清单)。
    /// 单 manifest 的包 platforms 为空 —— 它不按架构分变体。
    pub async fn fetch_contract(
        reference: &str,
    ) -> crate::Result<(serde_json::Value, Vec<u8>, Vec<String>)> {
        use oci_client::Reference;
        let r: Reference = reference
            .parse()
            .map_err(|e| anyhow::anyhow!("bad ref '{reference}': {e}"))?;
        let client = registry_client();
        let auth = auth_for(reference);
        let (raw, _) = client
            .pull_manifest_raw(&r, &auth, &accepted_media_types())
            .await
            .map_err(|e| anyhow::anyhow!("pull manifest '{reference}': {e}"))?;
        let top: serde_json::Value = serde_json::from_slice(&raw)?;
        let platforms = platforms_of(&top);
        let m: serde_json::Value = if top.get("manifests").and_then(|v| v.as_array()).is_some() {
            let sub = top["manifests"]
                .as_array()
                .unwrap()
                .first()
                .ok_or_else(|| anyhow::anyhow!("'{reference}' 的 index 里一个 manifest 都没有"))?;
            let dig = sub["digest"].as_str().unwrap_or_default();
            let dref: Reference = format!("{}/{}@{}", r.registry(), r.repository(), dig)
                .parse()
                .map_err(|e| anyhow::anyhow!("bad digest ref for {reference}: {e}"))?;
            let (sub_raw, _) = client
                .pull_manifest_raw(&dref, &auth, &accepted_media_types())
                .await
                .map_err(|e| anyhow::anyhow!("pull sub-manifest of '{reference}': {e}"))?;
            serde_json::from_slice(&sub_raw)?
        } else {
            top
        };
        let cd = m["config"]["digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("'{reference}' 的 manifest 没有 config"))?
            .to_string();
        let mut cfg: Vec<u8> = Vec::new();
        client
            .pull_blob(&r, cd.as_str(), &mut cfg)
            .await
            .map_err(|e| anyhow::anyhow!("pull config {cd} of '{reference}': {e}"))?;
        Ok((m, cfg, platforms))
    }

    /// 远端有哪些版本 —— `/v2/<repo>/tags/list`。
    ///
    /// OCI 只定义了这一个内容发现端点(`_catalog` 根本不在规范里,Docker Hub
    /// 也有意不提供),所以"这个包有哪几版"只能这么问,"registry 里有哪些包"
    /// 则问不出来 —— 那要靠索引文件(D-123 第七节)。
    pub async fn list_tags(reference: &str) -> crate::Result<Vec<String>> {
        use oci_client::Reference;
        let r: Reference = reference
            .parse()
            .map_err(|e| anyhow::anyhow!("bad ref '{reference}': {e}"))?;
        let client = registry_client();
        let auth = auth_for(reference);
        let resp = client
            .list_tags(&r, &auth, None, None)
            .await
            .map_err(|e| anyhow::anyhow!("list tags '{reference}': {e}"))?;
        Ok(resp.tags)
    }

    /// Push an image index: every child manifest (by digest) first, then the
    /// index under the tag.
    async fn push_index(
        &self,
        reference: &str,
        index: &serde_json::Value,
        index_blob: &[u8],
    ) -> crate::Result<()> {
        use oci_client::Reference;
        let r: Reference = reference
            .parse()
            .map_err(|e| anyhow::anyhow!("bad image ref '{reference}': {e}"))?;
        for child in index["manifests"].as_array().into_iter().flatten() {
            let d = child["digest"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("index entry missing digest"))?;
            let blob = std::fs::read(self.blob_path(strip(d)))?;
            // 子 manifest 按 digest 推(不占 tag)—— tag 留给 index 本身。
            let by_digest = format!("{}/{}@{}", r.registry(), r.repository(), d);
            self.push_manifest_blob(&by_digest, &blob).await?;
        }
        // index 本身没有 blob,只有一份 JSON —— 走原始 PUT,不经
        // `OciImageManifest`(它会把 `manifests` 丢掉,索引就成了空壳)。
        let client = registry_client();
        let auth = auth_for(reference);
        use oci_client::manifest::OciImageIndex;
        use oci_client::manifest::OciManifest;
        use oci_client::RegistryOperation;
        client
            .auth(&r, &auth, RegistryOperation::Push)
            .await
            .map_err(|e| anyhow::anyhow!("auth '{reference}': {e}"))?;
        let idx: OciImageIndex = serde_json::from_slice(index_blob)?;
        client
            .push_manifest(&r, &OciManifest::ImageIndex(idx))
            .await
            .map_err(|e| anyhow::anyhow!("push index '{reference}': {e}"))?;
        Ok(())
    }

    fn manifest_blob(&self, reference: &str) -> crate::Result<Vec<u8>> {
        let idx = self.read_index()?;
        let md = idx["manifests"]
            .as_array()
            .and_then(|a| a.iter().find(|m| m["annotations"][ANN_REF].as_str() == Some(reference)))
            .and_then(|m| m["digest"].as_str())
            .ok_or_else(|| anyhow::anyhow!("image '{reference}' not in local store"))?
            .to_string();
        Ok(std::fs::read(self.blob_path(strip(&md)))?)
    }

    /// Import one unpacked index entry: copy its manifest/config/layer blobs in
    /// and tag it. Tag = `override_ref` if given, else the entry's `ref.name`.
    fn import_entry(
        &self,
        tmp: &std::path::Path,
        entry: &serde_json::Value,
        override_ref: Option<&str>,
    ) -> crate::Result<String> {
        let reference = match override_ref {
            Some(r) => r.to_string(),
            None => entry["annotations"][ANN_REF]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("archive manifest has no ref.name; pass a tag"))?
                .to_string(),
        };
        let mdig_full = entry["digest"].as_str().unwrap_or("").to_string();
        let mdig = strip(&mdig_full);
        let src_blob = |d: &str| tmp.join("blobs").join("sha256").join(strip(d));
        let copy_in = |d: &str| -> crate::Result<()> {
            let data = std::fs::read(src_blob(d))?;
            self.store_raw(&data)?;
            Ok(())
        };
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(src_blob(mdig))?)?;
        copy_in(mdig)?;
        if let Some(c) = manifest["config"]["digest"].as_str() {
            copy_in(c)?;
        }
        if let Some(ls) = manifest["layers"].as_array() {
            for l in ls {
                if let Some(d) = l["digest"].as_str() {
                    copy_in(d)?;
                }
            }
        }
        let msize = entry["size"].as_u64().unwrap_or(0);
        self.tag(&reference, mdig, msize)?;
        Ok(reference)
    }

    /// Import an oci-archive (e.g. `crater save` / `build` output) into the store
    /// and tag it. `as_ref` overrides the tag; `None` uses the archive's embedded
    /// `image.ref.name`. Returns the reference used.
    pub fn import_oci_archive(
        &self,
        archive: &std::path::Path,
        as_ref: Option<&str>,
    ) -> crate::Result<String> {
        let tmp = std::env::temp_dir().join(format!("crater-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        crate::bundle::unpack(archive, &tmp)?;
        let index: serde_json::Value = serde_json::from_slice(&std::fs::read(tmp.join("index.json"))?)?;
        let entry = index["manifests"]
            .as_array()
            .and_then(|a| a.iter().find(|m| m["annotations"][ANN_REF].as_str().is_some()))
            .ok_or_else(|| anyhow::anyhow!("{}: no image manifest (ref.name) in archive", archive.display()))?;
        let reference = self.import_entry(&tmp, entry, as_ref)?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(reference)
    }

    /// Import EVERY tagged manifest from an oci-archive (a `crater build` output
    /// may carry multiple component artifacts). Returns all references imported.
    pub fn import_all(&self, archive: &std::path::Path) -> crate::Result<Vec<String>> {
        let tmp = std::env::temp_dir().join(format!("crater-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        crate::bundle::unpack(archive, &tmp)?;
        let index: serde_json::Value = serde_json::from_slice(&std::fs::read(tmp.join("index.json"))?)?;
        let mut refs = Vec::new();
        if let Some(ms) = index["manifests"].as_array() {
            for entry in ms.iter().filter(|m| m["annotations"][ANN_REF].as_str().is_some()) {
                refs.push(self.import_entry(&tmp, entry, None)?);
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);
        if refs.is_empty() {
            anyhow::bail!("{}: no tagged manifest in archive", archive.display());
        }
        Ok(refs)
    }

    /// Export a stored image/artifact to an oci-archive file (`crater save`):
    /// the inverse of `import`. Preserves `artifactType` on the index entry so
    /// the file round-trips through `load`/`apply <file>`.
    pub fn export_oci_archive(&self, reference: &str, out: &std::path::Path) -> crate::Result<()> {
        let idx = self.read_index()?;
        let find = |reference: &str| -> crate::Result<(String, u64)> {
            let entry = idx["manifests"]
                .as_array()
                .and_then(|a| a.iter().find(|m| m["annotations"][ANN_REF].as_str() == Some(reference)))
                .ok_or_else(|| anyhow::anyhow!("image '{reference}' not in local store"))?;
            Ok((
                entry["digest"].as_str().unwrap_or("").to_string(),
                entry["size"].as_u64().unwrap_or(0),
            ))
        };
        // Closure to export: the ref itself, PLUS — for a project artifact
        // (D-098) — every play's locked task artifact ref, so one .oci ships
        // the whole environment (blobs are content-addressed → shared layers
        // across tasks land once).
        let (mdig_full, msize) = find(reference)?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.blob_path(strip(&mdig_full)))?)?;
        let mut refs: Vec<(String, String, u64)> = vec![(reference.to_string(), mdig_full, msize)];
        if manifest["artifactType"].as_str() == Some(AT_PROJECT) {
            let project = self.project_recipe(&manifest)?;
            for play in &project.plays {
                if refs.iter().any(|(r, _, _)| r == &play.source) {
                    continue; // two plays sharing one task artifact → ship once
                }
                let (d, s) = find(&play.source).map_err(|e| {
                    anyhow::anyhow!("project play source '{}' 不在本地库:{e}(先 crater build -f <project>.yaml)", play.source)
                })?;
                refs.push((play.source.clone(), d, s));
            }
        }

        let tmp = std::env::temp_dir().join(format!("crater-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let blobs = tmp.join("blobs").join("sha256");
        std::fs::create_dir_all(&blobs)?;
        std::fs::write(tmp.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#)?;
        let copy = |d: &str| -> crate::Result<()> {
            // copy is idempotent across shared blobs (content-addressed names).
            std::fs::copy(self.blob_path(strip(d)), blobs.join(strip(d)))?;
            Ok(())
        };
        let mut entries: Vec<serde_json::Value> = Vec::new();
        for (r, mdig_full, msize) in &refs {
            let mdig = strip(mdig_full);
            let man: serde_json::Value =
                serde_json::from_slice(&std::fs::read(self.blob_path(mdig))?)?;
            copy(mdig)?;
            if let Some(c) = man["config"]["digest"].as_str() {
                copy(c)?;
            }
            if let Some(ls) = man["layers"].as_array() {
                for l in ls {
                    if let Some(d) = l["digest"].as_str() {
                        copy(d)?;
                    }
                }
            }
            let mut m_entry = json!({
                "mediaType": MT_MANIFEST,
                "digest": mdig_full,
                "size": msize,
                "annotations": { ANN_REF: r }
            });
            if let Some(at) = man["artifactType"].as_str() {
                m_entry["artifactType"] = json!(at);
            }
            entries.push(m_entry);
        }
        let index = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": entries
        });
        std::fs::write(tmp.join("index.json"), serde_json::to_vec_pretty(&index)?)?;
        // docker-archive 兼容垫片(plain image only):经典存储(非 containerd
        // image store)的 `docker load` 不认 OCI layout,会走 legacy v1 路径找
        // `<dir>/json` 而报 `blobs/json: no such file`。补一个 manifest.json
        // 指向同一批 blob(buildx `type=docker` 同款双格式)——老 docker 读它,
        // 新 docker/ctr/nerdctl 读 index.json,互不干扰。B 类 artifact
        // (artifactType)永远不会被 docker load,不加。
        if manifest["artifactType"].as_str().is_none() {
            if let (Some(cfg), Some(ls)) =
                (manifest["config"]["digest"].as_str(), manifest["layers"].as_array())
            {
                let layers: Vec<String> = ls
                    .iter()
                    .filter_map(|l| l["digest"].as_str())
                    .map(|d| format!("blobs/sha256/{}", strip(d)))
                    .collect();
                let docker_manifest = json!([{
                    "Config": format!("blobs/sha256/{}", strip(cfg)),
                    "RepoTags": [reference],
                    "Layers": layers,
                }]);
                std::fs::write(tmp.join("manifest.json"), serde_json::to_vec_pretty(&docker_manifest)?)?;
            }
        }
        crate::bundle::pack(&tmp, out)?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok(())
    }

    /// The parsed OCI manifest of a stored image (to detect crater artifacts).
    pub fn resolve_manifest(&self, reference: &str) -> crate::Result<serde_json::Value> {
        Ok(serde_json::from_slice(&self.manifest_blob(reference)?)?)
    }

    /// Parse the LOCKED project out of a project-artifact manifest (D-098):
    /// its recipe layer is the project yaml with play `source`s = task refs.
    pub fn project_recipe(&self, manifest: &serde_json::Value) -> crate::Result<crate::project::Project> {
        let d = manifest["layers"]
            .as_array()
            .and_then(|ls| ls.iter().find(|l| l["mediaType"].as_str() == Some(MT_RECIPE)))
            .and_then(|l| l["digest"].as_str())
            .ok_or_else(|| anyhow::anyhow!("project artifact has no recipe layer"))?;
        let bytes = std::fs::read(self.blob_path(strip(d)))?;
        Ok(serde_yaml::from_slice(&bytes)?)
    }

    /// blobs/sha256 dir (for materializing artifact layers).
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs").join("sha256")
    }

    /// Ordered fs-layer blob paths of a stored image (for apply/extract).
    pub fn resolve_layers(&self, reference: &str) -> crate::Result<Vec<PathBuf>> {
        let m: serde_json::Value = serde_json::from_slice(&self.manifest_blob(reference)?)?;
        let mut paths = Vec::new();
        if let Some(ls) = m["layers"].as_array() {
            for l in ls {
                if let Some(d) = l["digest"].as_str() {
                    paths.push(self.blob_path(strip(d)));
                }
            }
        }
        Ok(paths)
    }
}

fn strip(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap_or(digest)
}

/// 一个 manifest / index 支持哪些 `os/arch`。单 manifest 返回空 —— 它不分变体,
/// 说"支持 linux/amd64"是编出来的(制品里没有这个事实)。
pub fn platforms_of(top: &serde_json::Value) -> Vec<String> {
    top["manifests"]
        .as_array()
        .map(|ms| {
            ms.iter()
                .filter_map(|e| {
                    let a = e["platform"]["architecture"].as_str()?;
                    let o = e["platform"]["os"].as_str().unwrap_or("linux");
                    Some(format!("{o}/{a}"))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// 拉 manifest 时声明能收哪些类型。
///
/// 自定义类型(crater 的 recipe / material / blueprint 层)不在这张表里 ——
/// 表管的是 **manifest** 的 Accept 头,层的类型只出现在 manifest 正文里,
/// 靠 `pull_blob` 按 digest 取,不经过内容协商(D-033)。
fn accepted_media_types() -> Vec<&'static str> {
    use oci_client::manifest as mt;
    vec![
        mt::IMAGE_MANIFEST_MEDIA_TYPE,
        mt::IMAGE_MANIFEST_LIST_MEDIA_TYPE,
        mt::OCI_IMAGE_MEDIA_TYPE,       // ghcr & others use OCI manifest
        mt::OCI_IMAGE_INDEX_MEDIA_TYPE, // ...and OCI image index (multi-arch)
        mt::IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE,
        mt::IMAGE_LAYER_GZIP_MEDIA_TYPE,
        mt::IMAGE_LAYER_MEDIA_TYPE,
        mt::IMAGE_CONFIG_MEDIA_TYPE,
        mt::IMAGE_DOCKER_CONFIG_MEDIA_TYPE,
    ]
}

/// An oci-client honoring `$CRATER_INSECURE_REGISTRIES` (comma-separated hosts
/// served over plain HTTP, e.g. a temp zot at `192.168.73.5:5000`).
pub(crate) fn registry_client() -> oci_client::Client {
    use oci_client::client::{ClientConfig, ClientProtocol};
    let mut cfg = ClientConfig::default();
    if let Ok(list) = std::env::var("CRATER_INSECURE_REGISTRIES") {
        let regs: Vec<String> = list
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !regs.is_empty() {
            cfg.protocol = ClientProtocol::HttpsExcept(regs);
        }
    }
    oci_client::Client::new(cfg)
}

// ---------------------------------------------------------------------------
// Registry auth (~/.crater/auth.json): { "<registry>": {username, password} }
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize, Serialize)]
struct AuthFile {
    #[serde(default)]
    registries: BTreeMap<String, Cred>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct Cred {
    username: String,
    password: String,
}

fn auth_path() -> PathBuf {
    ImageStore::home().join("auth.json")
}

/// The registry host of a reference (e.g. `docker.io` from `docker.io/library/x`).
/// Also drives D-101 closure push/pull: each locked task's remote twin is
/// `<this>/<bare-lock>`.
pub fn registry_of(reference: &str) -> String {
    let first = reference.split('/').next().unwrap_or("");
    if first.contains('.') || first.contains(':') || first == "localhost" {
        first.to_string()
    } else {
        "docker.io".to_string()
    }
}

/// `~/.docker/config.json` 里这个 registry 的凭据。
///
/// 只认明文的 `auth` 字段(base64 的 `user:password`)。凭据助手
/// (`credsStore` / `credHelpers`)不认:那要 exec 一个外部程序,而 crater
/// 的气质是静态单二进制、不依赖宿主上装了什么 —— 装了助手的人用
/// `crater registry login` 一句话就能补上。
fn docker_login(registry: &str) -> Option<(String, String)> {
    let home = std::env::var("HOME").ok()?;
    let path = std::path::Path::new(&home).join(".docker").join("config.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    docker_login_in(&v, registry)
}

/// 上面那条的纯函数内核 —— 与文件系统无关,于是可测。
fn docker_login_in(v: &serde_json::Value, registry: &str) -> Option<(String, String)> {
    use base64::Engine as _;
    let auths = v.get("auths")?.as_object()?;
    // docker.io 在那个文件里的键是历史遗留的 v1 端点,不是主机名。
    let keys: Vec<String> = if registry == "docker.io" {
        ["https://index.docker.io/v1/", "index.docker.io", "docker.io"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    } else {
        vec![registry.to_string(), format!("https://{registry}")]
    };
    let entry = keys.iter().find_map(|k| auths.get(k))?;
    let raw = entry.get("auth")?.as_str()?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(raw).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (u, p) = text.split_once(':')?;
    Some((u.to_string(), p.to_string()))
}

#[cfg(test)]
mod docker_auth_tests {
    use super::docker_login_in;
    use serde_json::json;

    /// `dXNlcjpwYXNz` = base64("user:pass")。
    fn cfg() -> serde_json::Value {
        json!({"auths": {
            "https://index.docker.io/v1/": {"auth": "dXNlcjpwYXNz"},
            "registry.cn-shenzhen.aliyuncs.com": {"auth": "dXNlcjpwYXNz"}
        }})
    }

    #[test]
    fn docker_io_is_found_under_its_legacy_v1_key() {
        // docker.io 在那个文件里的键是历史遗留的 v1 端点,不是主机名 ——
        // 按主机名直查会**静默**退成匿名,表现为莫名其妙的 401。
        assert_eq!(
            docker_login_in(&cfg(), "docker.io"),
            Some(("user".into(), "pass".into()))
        );
    }

    #[test]
    fn a_plain_host_is_found_as_is() {
        assert_eq!(
            docker_login_in(&cfg(), "registry.cn-shenzhen.aliyuncs.com"),
            Some(("user".into(), "pass".into()))
        );
    }

    #[test]
    fn an_unknown_registry_yields_nothing_not_a_wrong_credential() {
        assert_eq!(docker_login_in(&cfg(), "ghcr.io"), None);
    }

    #[test]
    fn a_creds_helper_entry_is_not_mistaken_for_a_password() {
        // credsStore 的条目没有 `auth` 字段 —— 认不出来就该退成匿名,
        // 而不是拿一个空口令去撞认证。
        let v = json!({"auths": {"x.io": {}}, "credsStore": "pass"});
        assert_eq!(docker_login_in(&v, "x.io"), None);
    }
}

/// Persist credentials for a registry (`crater registry login`).
pub fn save_login(registry: &str, username: &str, password: &str) -> crate::Result<()> {
    let path = auth_path();
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let mut f: AuthFile = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    f.registries.insert(
        registry.to_string(),
        Cred { username: username.to_string(), password: password.to_string() },
    );
    std::fs::write(&path, serde_json::to_vec_pretty(&f)?)?;
    Ok(())
}

/// Resolve registry auth: crater 自己的 `auth.json` → `~/.docker/config.json`
/// → 匿名。
///
/// 认 docker 的登录不是图省事,是因为**别的 OCI 工具都认**(helm、oras、
/// skopeo、buildah)。让已经 `docker login` 过的人再跑一遍 `crater registry
/// login`,等于要他把口令在第二个地方再写一遍 —— 而口令被抄写的次数,
/// 就是它泄漏的机会次数。crater 只读不写那个文件。
pub(crate) fn auth_for(reference: &str) -> oci_client::secrets::RegistryAuth {
    use oci_client::secrets::RegistryAuth;
    let reg = registry_of(reference);
    let f: Option<AuthFile> = std::fs::read(auth_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(c) = f.and_then(|f| f.registries.get(&reg).cloned()) {
        return RegistryAuth::Basic(c.username, c.password);
    }
    if let Some((u, p)) = docker_login(&reg) {
        return RegistryAuth::Basic(u, p);
    }
    {
        RegistryAuth::Anonymous
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal store on disk: index → manifest → {config, layer}; one orphan.
    fn fake_store(dir: &std::path::Path) -> ImageStore {
        let store = ImageStore { root: dir.to_path_buf() };
        std::fs::create_dir_all(store.blobs_dir()).unwrap();
        // 名字只为读代码时看清这是哪个 blob;内容寻址不需要它。
        let put = |_name: &str, data: &[u8]| {
            let d = crate::bundle::sha256_hex(data);
            std::fs::write(store.blobs_dir().join(&d), data).unwrap();
            d
        };
        let config = put("config", b"{\"cfg\":true}");
        let layer = put("layer", b"layer-bytes");
        let manifest = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "config": { "digest": format!("sha256:{config}") },
            "layers": [ { "digest": format!("sha256:{layer}") } ],
        }))
        .unwrap();
        let mdigest = put("manifest", &manifest);
        put("orphan", b"unreferenced-bytes");
        let idx = json!({ "schemaVersion": 2, "manifests": [ {
            "mediaType": MT_MANIFEST,
            "digest": format!("sha256:{mdigest}"),
            "size": manifest.len(),
            "annotations": { ANN_REF: "t/x:1" }
        } ] });
        std::fs::write(store.root.join("index.json"), serde_json::to_vec(&idx).unwrap()).unwrap();
        store
    }

    /// D-078④: concurrent index tagging must not lose entries — N parallel
    /// retags of one manifest all land (the index_lock serializes the
    /// read-modify-write; without it this test loses entries reliably).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_retags_all_land() {
        let dir = std::env::temp_dir().join(format!("crater-idxlock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = std::sync::Arc::new(fake_store(&dir));
        let mut handles = Vec::new();
        for i in 0..10 {
            let st = store.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                st.retag("t/x:1", &format!("t/alias:{i}")).unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        let refs: Vec<String> = store.list().unwrap().into_iter().map(|s| s.reference).collect();
        for i in 0..10 {
            assert!(refs.contains(&format!("t/alias:{i}")), "alias {i} lost; got {refs:?}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D-097: gc keeps everything the index reaches (manifest → config/layers)
    /// and sweeps the rest; dry-run deletes nothing; after `remove(ref)` the
    /// whole chain becomes sweepable.
    #[test]
    fn gc_mark_and_sweep_with_rmi() {
        let dir = std::env::temp_dir().join(format!("crater-store-gc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = fake_store(&dir);
        let n_blobs = || std::fs::read_dir(store.blobs_dir()).unwrap().count();
        assert_eq!(n_blobs(), 4); // manifest + config + layer + orphan

        let (swept, freed) = store.gc(true).unwrap(); // dry-run
        assert_eq!((swept, n_blobs()), (1, 4), "dry-run reports but keeps");
        assert!(freed > 0);

        let (swept, _) = store.gc(false).unwrap();
        assert_eq!((swept, n_blobs()), (1, 3), "orphan swept, chain kept");

        assert!(store.remove("t/x:1").unwrap());
        assert!(!store.remove("t/x:1").unwrap(), "second remove is a no-op");
        let (swept, _) = store.gc(false).unwrap();
        assert_eq!((swept, n_blobs()), (3, 0), "unreferenced chain swept");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
