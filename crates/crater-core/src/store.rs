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
        Ok(serde_json::from_slice(&std::fs::read(
            self.root.join("index.json"),
        )?)?)
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
                    reference: m["annotations"][ANN_REF]
                        .as_str()
                        .unwrap_or("<untagged>")
                        .to_string(),
                    digest,
                    size: m["size"].as_u64().unwrap_or(0),
                    content_size,
                    disk_usage,
                });
            }
        }
        Ok(out)
    }

    /// Sum an artifact's real sizes from its manifest tree: `content_size` =
    /// config + layers (declared, across every sub-manifest); `disk_usage` =
    /// on-disk bytes of every blob in the closure.
    fn artifact_sizes(&self, manifest_digest: &str) -> crate::Result<(u64, u64)> {
        let on_disk = |digest: &str| -> u64 {
            std::fs::metadata(self.blob_path(strip(digest)))
                .map(|m| m.len())
                .unwrap_or(0)
        };
        let c = self.closure_of(manifest_digest)?;
        let mut content = 0u64;
        for (_, man) in &c.manifests {
            content += man["config"]["size"].as_u64().unwrap_or(0);
            if let Some(layers) = man["layers"].as_array() {
                for l in layers {
                    content += l["size"].as_u64().unwrap_or(0);
                }
            }
        }
        let disk = c.blobs.iter().map(|d| on_disk(d)).sum();
        Ok((content, disk))
    }

    /// 一件制品的**全部 blob**,从本地 store 读清单树算出来。
    /// 见 [`closure_walk`] —— 那里写了为什么这件事必须只有一份实现。
    fn closure_of(&self, manifest_digest: &str) -> crate::Result<Closure> {
        closure_walk(manifest_digest, &|d| std::fs::read(self.blob_path(d)).ok())
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
            .and_then(|a| {
                a.iter()
                    .find(|m| m["annotations"][ANN_REF].as_str() == Some(src))
            })
            .ok_or_else(|| {
                anyhow::anyhow!("image '{src}' not in local store (pull/build/load it first)")
            })?;
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
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["digest"].as_str())
                    .map(|d| strip(d).to_string())
                    .collect()
            })
            .unwrap_or_default();
        while let Some(d) = queue.pop() {
            if !keep.insert(d.clone()) {
                continue; // already marked
            }
            // A reachable blob that parses as JSON may reference further blobs
            // (manifest: config+layers; nested index: manifests).
            let Ok(bytes) = std::fs::read(self.blob_path(&d)) else {
                continue;
            };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            if let Some(c) = v["config"]["digest"].as_str() {
                queue.push(strip(c).to_string());
            }
            for arr in [&v["layers"], &v["manifests"]] {
                if let Some(items) = arr.as_array() {
                    queue.extend(
                        items
                            .iter()
                            .filter_map(|l| l["digest"].as_str())
                            .map(|s| strip(s).to_string()),
                    );
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

    /// True iff every blob this artifact's manifest **tree** references is
    /// present locally (D-087): distinguishes a full local copy from a thin
    /// pull, so an `--offline` apply can re-pull in full if needed.
    ///
    /// 走整棵树而不是一层:多架构包的顶层 index 没有 config 也没有 layers,
    /// 按"没有就算通过"的老写法它**永远回答 true** —— 一个只有索引 blob、
    /// 一个字节物料都没有的 store 也算"闭包完整"。issue #3 的空 tar 之所以
    /// 能一路装作成功,这是其中一环。
    pub fn has_all_layers(&self, reference: &str) -> bool {
        let Ok(digest) = self.manifest_digest(reference) else {
            return false;
        };
        let Ok(c) = self.closure_of(&digest) else {
            return false;
        };
        c.unreadable.is_empty() && c.blobs.iter().all(|d| self.blob_path(d).exists())
    }

    /// 本机架构那份清单的 (摘要, 字节数) —— 仅当它的闭包**整个在本地**。
    ///
    /// 多架构包只需要本机那一份的字节就能装,所以这里问的是子树而不是整棵树:
    /// 拿整棵树当判据,会因为另一个架构的物料没搬过来就判定"要联网",而那份
    /// 字节这台机器根本用不上。
    fn local_platform_manifest(&self, reference: &str) -> Option<(String, u64)> {
        let root = self.manifest_digest(reference).ok()?;
        let (d, s) = match self.platform_child(&root).ok()? {
            Some(child) => child,
            None => (root, 0),
        };
        let c = self.closure_of(&d).ok()?;
        if !c.unreadable.is_empty() || !c.blobs.iter().all(|b| self.blob_path(b).exists()) {
            return None;
        }
        let size = if s > 0 {
            s
        } else {
            std::fs::metadata(self.blob_path(&d)).ok()?.len()
        };
        Some((d, size))
    }

    /// registry 够不着时的报错 —— **先说字节在不在本地**,再说网络怎么了。
    ///
    /// 断网机房里裸露的 `pull manifest '...': error sending request` 把人引到
    /// 错的方向去查(防火墙?证书?),而真正该做的动作是"把包 load 进来"。
    /// 本地已经有一部分(瘦拉过、或 tar 缺了层)与"一个字节都没有"要分开说:
    /// 前者是搬运漏了东西,后者是根本没搬。
    fn unreachable(&self, reference: &str, e: impl std::fmt::Display) -> anyhow::Error {
        let partial = self.has(reference);
        let hint = if partial {
            "本地有这条引用,但闭包不完整(瘦拉的,或 tar 里缺了层)"
        } else {
            "字节不在本地"
        };
        anyhow::anyhow!(
            "{reference}:{hint},而 registry 也够不着({e})。\n\
             断网现场先把包搬进来:`crater pkg save {reference} -o <包>.pkg.tar`(联网机)\n\
             → 拷过去 → `crater pkg load <包>.pkg.tar`(本机),再重来。"
        )
    }

    async fn pull_layers(&self, reference: &str, thin: bool) -> crate::Result<()> {
        use oci_client::Reference;

        // **字节已经全在本地就不连 registry。**
        //
        // 断网机房里这一条是成立与否的分界:`crater pkg load yq.pkg.tar` 之后
        // 闭包就在盘上了,而 install 仍然无条件先去拉一次 manifest —— 在断网
        // 机上那是一次长超时,然后失败。字节是内容寻址的,本地那份和 registry
        // 那份按定义相同,这一趟网络除了失败没有别的产出(issue #3)。
        //
        // 代价说清楚:tag 是可移动的,`latest` 指到了新 digest 时这里不会发现。
        // 所以要**说出来**,并给一条强制重取的路 —— 静默地用旧字节才是更坏的
        // 那个。同一笔取舍 `ensure_pulled(--offline)` 早就在做。
        if let Some((d, s)) = self.local_platform_manifest(reference) {
            // 顺手把引用落到本机架构那份子清单上 —— 这正是走网络那条路
            // 结尾做的事(D-127)。不做的话 `install` 拿到的是一份没有
            // config 的 index,而它本来只是想读契约。纯本地操作,不联网。
            if self.manifest_digest(reference).ok().as_deref() != Some(d.as_str()) {
                self.tag(reference, &d, s)?;
            }
            tracing::info!("{reference}: closure already local — not contacting the registry (force a refetch with `crater rmi {reference}`)");
            return Ok(());
        }

        let r: Reference = reference
            .parse()
            .map_err(|e| anyhow::anyhow!("bad image ref '{reference}': {e}"))?;
        let client = registry_client()?;
        let auth = auth_for(reference);
        let accepted = accepted_media_types();
        let (raw, _digest) = client
            .pull_manifest_raw(&r, &auth, &accepted)
            .await
            .map_err(|e| self.unreachable(reference, e))?;

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
                .or_else(|| {
                    entries
                        .iter()
                        .find(|e| e["platform"]["architecture"].is_string())
                })
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "'{reference}' 的 index 里没有 linux/{want},也没有任何带架构的条目"
                    )
                })?;
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
                .map_err(|e| {
                    anyhow::anyhow!("pull sub-manifest {sub_dig} of '{reference}': {e}")
                })?;
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
                    let fetch = l["annotations"][ANN_MATERIAL_FETCH]
                        .as_str()
                        .unwrap_or("embedded");
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
        let client = registry_client()?;
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
        let client = registry_client()?;
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
        let client = registry_client()?;
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
        let client = registry_client()?;
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

    /// 这条引用在本地 store 里指向哪个清单摘要(已剥 `sha256:`)。
    fn manifest_digest(&self, reference: &str) -> crate::Result<String> {
        let idx = self.read_index()?;
        let md = idx["manifests"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|m| m["annotations"][ANN_REF].as_str() == Some(reference))
            })
            .and_then(|m| m["digest"].as_str())
            .ok_or_else(|| anyhow::anyhow!("image '{reference}' not in local store"))?;
        Ok(strip(md).to_string())
    }

    fn manifest_blob(&self, reference: &str) -> crate::Result<Vec<u8>> {
        Ok(std::fs::read(
            self.blob_path(&self.manifest_digest(reference)?),
        )?)
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
        // 走整棵清单树 —— 多架构包的顶层 index 没有 config / layers,只按
        // 一层收会把子清单连同全部物料一起丢掉,而且**一声不吭**(issue #3)。
        let c = closure_walk(mdig, &|d| std::fs::read(src_blob(d)).ok())?;
        if !c.unreadable.is_empty() {
            anyhow::bail!(
                "归档里缺 {} 份清单({})—— 这份包是残的,不导进来。\n\
                 多半是用修复前的 `crater save` 打的多架构包(只装了顶层索引):\n\
                 回联网机上重打一次。",
                c.unreadable.len(),
                c.unreadable
                    .iter()
                    .map(|d| &d[..12.min(d.len())])
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        for d in &c.blobs {
            copy_in(d)?;
        }

        // **落位得和在线 `pull` 落得一模一样。**
        //
        // 多架构包的根是 index。`pull_layers` 把 index 折到本机架构那一份
        // 子清单上再 tag(D-127),于是 store 里这条引用指向一份**有 config、
        // 有 layers** 的具体清单 —— `install` 读契约、找蓝图层都指着它。
        // 而 import 原来直接 tag 了 index:字节全都进来了,引用却指着一份
        // 没有 config 的东西,`install` 当场报 "manifest 没有 config"。
        //
        // 同一个 store,联网装得上、U 盘搬过去装不上,差的就是这一下(issue #3)。
        // 全部架构的 blob 都已经收进来了,这里只决定这条引用**指向谁**。
        let (tag_dig, tag_size) = match self.platform_child(mdig)? {
            Some(child) => child,
            None => (mdig.to_string(), entry["size"].as_u64().unwrap_or(0)),
        };
        self.tag(&reference, &tag_dig, tag_size)?;
        Ok(reference)
    }

    /// index → 本机该用的那份子清单 (摘要, 字节数);不是 index 就是 None。
    ///
    /// 架构优先级与 `pull_layers` 一致(D-127):本机 → amd64 → 任意一条带
    /// 架构的。两处必须同序,否则同一个包在线拉和离线搬会落到不同架构上,
    /// 而这件事要到目标机 exec 报 `Exec format error` 才看得见。
    fn platform_child(&self, manifest_digest: &str) -> crate::Result<Option<(String, u64)>> {
        let man: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.blob_path(strip(manifest_digest)))?)?;
        let Some(subs) = man["manifests"].as_array() else {
            return Ok(None);
        };
        let want = crate::arch::detect_local();
        let want = want.as_str();
        let pick = |a: &str| {
            subs.iter().find(|e| {
                e["platform"]["architecture"].as_str() == Some(a)
                    && e["platform"]["os"].as_str().unwrap_or("linux") == "linux"
            })
        };
        let sub = pick(want)
            .or_else(|| pick("amd64"))
            .or_else(|| {
                subs.iter()
                    .find(|e| e["platform"]["architecture"].is_string())
            })
            .ok_or_else(|| {
                anyhow::anyhow!("这个包的 index 里没有 linux/{want},也没有任何带架构的条目")
            })?;
        let d = sub["digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("index 条目没有 digest"))?;
        let size = sub["size"].as_u64().unwrap_or_else(|| {
            std::fs::metadata(self.blob_path(strip(d)))
                .map(|m| m.len())
                .unwrap_or(0)
        });
        Ok(Some((strip(d).to_string(), size)))
    }

    /// Import an oci-archive (e.g. `crater save` / `build` output) into the store
    /// and tag it. `as_ref` overrides the tag; `None` uses the archive's embedded
    /// `image.ref.name`. Returns the reference used.
    pub fn import_oci_archive(
        &self,
        archive: &std::path::Path,
        as_ref: Option<&str>,
    ) -> crate::Result<String> {
        Ok(self.import_oci_archive_rooted(archive, as_ref)?.0)
    }

    /// 同 [`Self::import_oci_archive`],外加归档的**根清单摘要**。
    ///
    /// 多架构包收进来之后引用是 tag 在子清单上的(与在线 `pull` 一致),
    /// 于是"这个 tar 里装了几个架构"就只剩根清单知道 —— `pkg load` 要把它
    /// 报出来,不然搬错架构要到目标机上才发现。
    pub fn import_oci_archive_rooted(
        &self,
        archive: &std::path::Path,
        as_ref: Option<&str>,
    ) -> crate::Result<(String, String)> {
        let tmp = std::env::temp_dir().join(format!("crater-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        crate::bundle::unpack(archive, &tmp)?;
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.join("index.json"))?)?;
        let entry = index["manifests"]
            .as_array()
            .and_then(|a| {
                a.iter()
                    .find(|m| m["annotations"][ANN_REF].as_str().is_some())
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{}: no image manifest (ref.name) in archive",
                    archive.display()
                )
            })?;
        let root = strip(entry["digest"].as_str().unwrap_or_default()).to_string();
        let reference = self.import_entry(&tmp, entry, as_ref)?;
        let _ = std::fs::remove_dir_all(&tmp);
        Ok((reference, root))
    }

    /// Import EVERY tagged manifest from an oci-archive (a `crater build` output
    /// may carry multiple component artifacts). Returns all references imported.
    pub fn import_all(&self, archive: &std::path::Path) -> crate::Result<Vec<String>> {
        let tmp = std::env::temp_dir().join(format!("crater-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        crate::bundle::unpack(archive, &tmp)?;
        let index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(tmp.join("index.json"))?)?;
        let mut refs = Vec::new();
        if let Some(ms) = index["manifests"].as_array() {
            for entry in ms
                .iter()
                .filter(|m| m["annotations"][ANN_REF].as_str().is_some())
            {
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
                .and_then(|a| {
                    a.iter()
                        .find(|m| m["annotations"][ANN_REF].as_str() == Some(reference))
                })
                .ok_or_else(|| anyhow::anyhow!("image '{reference}' not in local store"))?;
            Ok((
                entry["digest"].as_str().unwrap_or("").to_string(),
                entry["size"].as_u64().unwrap_or(0),
            ))
        };
        // 导出这一个引用。旧 task 管线的 project artifact(D-098)会在这里
        // 把每个 play 锁定的 task 制品一并带上 —— 那条随旧管线一起删了(D-151)。
        let (mdig_full, msize) = find(reference)?;
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.blob_path(strip(&mdig_full)))?)?;
        let refs: Vec<(String, String, u64)> = vec![(reference.to_string(), mdig_full, msize)];

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
            // 整棵清单树,不是一层。多架构包顶层是 image index:没有 config、
            // 没有 layers,只有 `manifests`。照着"config + layers"收,得到的是
            // **空集而不是错误** —— 于是 save 写出一个只有索引 blob 的 6 KB
            // tar,打印 "saved",退出 0。U 盘插到断网机上才发现 18 MB 物料一个
            // 字节都没跟过来(issue #3)。
            let c = self.closure_of(mdig)?;
            if !c.unreadable.is_empty() {
                anyhow::bail!(
                    "{r}:本地缺 {} 份清单({})—— 导出的会是个残包。\n\
                     先 `crater pkg pull {r} --full` 补齐再 save。",
                    c.unreadable.len(),
                    c.unreadable
                        .iter()
                        .map(|d| &d[..12.min(d.len())])
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            for d in &c.blobs {
                copy(d)?;
            }
            let mut m_entry = json!({
                // 多架构包的根是 index,不是 manifest —— 照实写,别让归档
                // 自称一件它不是的东西。
                "mediaType": man["mediaType"].as_str().unwrap_or(MT_MANIFEST),
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
            if let (Some(cfg), Some(ls)) = (
                manifest["config"]["digest"].as_str(),
                manifest["layers"].as_array(),
            ) {
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
                std::fs::write(
                    tmp.join("manifest.json"),
                    serde_json::to_vec_pretty(&docker_manifest)?,
                )?;
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

/// 一件制品的 blob 闭包:清单树 + 每份清单的 config 与 layers。
pub(crate) struct Closure {
    /// 全部 blob 摘要(已剥 `sha256:`),根清单在最前。
    pub blobs: Vec<String>,
    /// 树里**读得出来**的每一份清单:(摘要, 解析结果)。
    pub manifests: Vec<(String, serde_json::Value)>,
    /// 树里读不出来的清单摘要 —— 多架构包缺子清单时就是它。
    pub unreadable: Vec<String>,
}

/// 走一件制品的清单树,收齐它引用的全部 blob。
///
/// **这个函数存在的唯一理由是"只看 config + layers"这个假设会静默失真。**
/// 多架构包的顶层是 image index:它既没有 `config` 也没有 `layers`,只有
/// `manifests`。于是每一处照着"config + layers"写的遍历,在多架构包上都
/// **收到空集而不是报错** —— `save` 写出一个只装着索引本身的 6 KB tar 并打印
/// "saved"、`load` 收下它并打印 "loaded"、`has_all_layers` 回答 true、
/// `images` 报 0B。四处各写一份同样的假设,就一起错了四次,而且全程退出 0
/// (issue #3:U 盘搬到断网机上才发现字节根本没跟过来)。
///
/// `read` 只会被用在**清单**上(根与子清单,都是几百字节),不会去读 layer ——
/// 存不存在由调用方按需查,免得为了数一数就把几百兆读进内存。
pub(crate) fn closure_walk(
    root: &str,
    read: &dyn Fn(&str) -> Option<Vec<u8>>,
) -> crate::Result<Closure> {
    let mut c = Closure {
        blobs: Vec::new(),
        manifests: Vec::new(),
        unreadable: Vec::new(),
    };
    let mut seen = std::collections::BTreeSet::new();
    let mut queue = vec![strip(root).to_string()];
    while let Some(d) = queue.pop() {
        if !seen.insert(d.clone()) {
            continue;
        }
        c.blobs.push(d.clone());
        let Some(bytes) = read(&d) else {
            c.unreadable.push(d);
            continue;
        };
        let Ok(man) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            c.unreadable.push(d);
            continue;
        };
        // index → 继续往下走子清单;image manifest → config 与 layers 是叶子。
        if let Some(subs) = man["manifests"].as_array() {
            for s in subs {
                if let Some(sd) = s["digest"].as_str() {
                    queue.push(strip(sd).to_string());
                }
            }
        }
        let leaves = man["config"]["digest"]
            .as_str()
            .into_iter()
            .chain(
                man["layers"]
                    .as_array()
                    .map(|v| v.as_slice())
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|l| l["digest"].as_str()),
            )
            .map(|d| strip(d).to_string())
            .collect::<Vec<_>>();
        for l in leaves {
            if seen.insert(l.clone()) {
                c.blobs.push(l);
            }
        }
        c.manifests.push((d, man));
    }
    Ok(c)
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
///
/// **返回 `Result`,不用 `oci_client::Client::new`**(D-145)。那个构造器在
/// 建不出客户端时 `unwrap_or_else` 退回一个 `Default` 客户端,而
/// `Default` 走的是 `reqwest::Client::new()` —— 后者在 TLS 后端起不来时
/// **panic**。于是一台没装 `ca-certificates` 的机器上,`crater pull` 报的是
/// 一句 Rust 堆栈,而不是"这台机器没有 CA 证书"。
///
/// 这不是边角情形:`ubuntu:24.04` 这类基础镜像**本身就不带 ca-certificates**,
/// 而内网机器裁掉它更是常态 —— 正是 crater 的主场景。
pub(crate) fn registry_client() -> crate::Result<oci_client::Client> {
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
    oci_client::Client::try_from(cfg).map_err(|e| {
        // 真因在 source 链里:reqwest 的顶层 Display 只说 "builder error",
        // 而"没有 CA 证书"那句在下一层。只报顶层等于什么都没说 —— 人会去
        // 查网络和代理,而真正要做的是装一个包,或者根本不该联网。
        let mut chain = e.to_string();
        let mut cur: Option<&dyn std::error::Error> = std::error::Error::source(&e);
        while let Some(c) = cur {
            chain.push_str(&format!(":{c}"));
            cur = c.source();
        }
        let hint = if chain.contains("CA certificate") {
"\n这台机器上没有 CA 证书。装 `ca-certificates`,\n或者本来就该走离线:`crater pkg pull --full` 在有网的机器上备好包,\n搬过去 `crater pkg load`,再 `--closure` 部署。"
        } else {
            ""
        };
        anyhow::anyhow!("建不出 registry 客户端:{chain}{hint}")
    })
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
    let path = std::path::Path::new(&home)
        .join(".docker")
        .join("config.json");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    docker_login_in(&v, registry)
}

/// 上面那条的纯函数内核 —— 与文件系统无关,于是可测。
fn docker_login_in(v: &serde_json::Value, registry: &str) -> Option<(String, String)> {
    use base64::Engine as _;
    let auths = v.get("auths")?.as_object()?;
    // docker.io 在那个文件里的键是历史遗留的 v1 端点,不是主机名。
    let keys: Vec<String> = if registry == "docker.io" {
        [
            "https://index.docker.io/v1/",
            "index.docker.io",
            "docker.io",
        ]
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
mod client_tests {
    /// `registry_client()` 必须返回 `Result` 而不是在建不出客户端时 panic。
    ///
    /// 钉住的是**类型**,不是某一次运行的结果:在有证书的开发机上这条永远
    /// 成功,真正的判据在一台没有 `ca-certificates` 的机器上(D-145 用
    /// `ubuntu:24.04` 容器验过 —— 那个基础镜像本身就不带)。而一旦有人把它
    /// 改回 `oci_client::Client::new()`,这里连编译都过不去:那个构造器返回
    /// 的是 `Client` 不是 `Result<Client>`。
    #[test]
    fn building_a_client_is_fallible_not_a_panic() {
        let c: crate::Result<oci_client::Client> = super::registry_client();
        // 开发机上有证书,应当成功;没有也不该 panic —— 两种都由 Result 表达。
        assert!(c.is_ok() || c.is_err());
    }
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
        Cred {
            username: username.to_string(),
            password: password.to_string(),
        },
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
        let store = ImageStore {
            root: dir.to_path_buf(),
        };
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
        std::fs::write(
            store.root.join("index.json"),
            serde_json::to_vec(&idx).unwrap(),
        )
        .unwrap();
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
        let refs: Vec<String> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|s| s.reference)
            .collect();
        for i in 0..10 {
            assert!(
                refs.contains(&format!("t/alias:{i}")),
                "alias {i} lost; got {refs:?}"
            );
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

    // ───────────────── 多架构包的离线搬运(issue #3) ─────────────────

    /// 一个**多架构**蓝图包:index → 两份子清单 → 各自的 config + 物料层。
    ///
    /// 本机架构一定在里面 —— `platform_child` 按本机架构挑,写死 amd64 的话
    /// 这些用例在 arm64 机器上就测的是另一条分支。
    fn fake_multiarch_store(dir: &std::path::Path) -> (ImageStore, Vec<String>) {
        let store = ImageStore {
            root: dir.to_path_buf(),
        };
        std::fs::create_dir_all(store.blobs_dir()).unwrap();
        let put = |data: &[u8]| {
            let d = crate::bundle::sha256_hex(data);
            std::fs::write(store.blobs_dir().join(&d), data).unwrap();
            d
        };
        let local = crate::arch::detect_local().as_str().to_string();
        let other = if local == "amd64" { "arm64" } else { "amd64" };
        let mut all = Vec::new();
        let mut subs = Vec::new();
        for a in [local.as_str(), other] {
            let cfg = put(format!("{{\"name\":\"yq\",\"arch\":\"{a}\"}}").as_bytes());
            let mat = put(format!("material-bytes-for-{a}").as_bytes());
            let man = serde_json::to_vec(&json!({
                "schemaVersion": 2,
                "mediaType": MT_MANIFEST,
                "config": { "digest": format!("sha256:{cfg}"), "mediaType": MT_PKG_CONFIG },
                "layers": [ { "digest": format!("sha256:{mat}"), "mediaType": MT_MATERIAL } ],
            }))
            .unwrap();
            let md = put(&man);
            subs.push(json!({
                "mediaType": MT_MANIFEST,
                "digest": format!("sha256:{md}"),
                "size": man.len(),
                "platform": { "architecture": a, "os": "linux" },
            }));
            all.extend([cfg, mat, md]);
        }
        let index_blob = serde_json::to_vec(&json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": subs,
        }))
        .unwrap();
        let idig = put(&index_blob);
        all.push(idig.clone());
        let idx = json!({ "schemaVersion": 2, "manifests": [ {
            "mediaType": MT_MANIFEST,
            "digest": format!("sha256:{idig}"),
            "size": index_blob.len(),
            "annotations": { ANN_REF: "reg/t/yq:4.44.3" }
        } ] });
        std::fs::write(
            store.root.join("index.json"),
            serde_json::to_vec(&idx).unwrap(),
        )
        .unwrap();
        (store, all)
    }

    /// **issue #3 的封条。** 多架构包 save → load 必须把**全部字节**搬过去,
    /// 而且落位得和在线 pull 一样(引用指向本机架构那份子清单)。
    ///
    /// 修复前:export 只看顶层的 `config`/`layers`,而 index 两者皆无 ——
    /// 于是导出一个只装着索引 blob 的 tar,打印 "saved",退出 0;import 收下
    /// 它,打印 "loaded",退出 0;`install` 到断网机上才发现一个字节都没来。
    /// 全程没有任何一处报错,这正是它能活到验收之后的原因。
    #[test]
    fn a_multiarch_package_survives_save_and_load() {
        let dir = std::env::temp_dir().join(format!("crater-mularch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, blobs) = fake_multiarch_store(&dir.join("A"));

        let tar = dir.join("yq.pkg.tar");
        a.export_oci_archive("reg/t/yq:4.44.3", &tar).unwrap();

        // ① tar 里必须有全部 7 个 blob:index + 2 子清单 + 2 config + 2 物料。
        let unpacked = dir.join("unpacked");
        crate::bundle::unpack(&tar, &unpacked).unwrap();
        let got: std::collections::BTreeSet<String> =
            std::fs::read_dir(unpacked.join("blobs").join("sha256"))
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
        assert_eq!(
            got.len(),
            7,
            "tar 里只有 {} 个 blob,应有 7 个:{got:?}",
            got.len()
        );
        for b in &blobs {
            assert!(got.contains(b), "tar 里缺 blob {b}");
        }

        // ② load 进一个全新的 store,字节一个不少。
        let b = ImageStore {
            root: dir.join("B"),
        };
        std::fs::create_dir_all(b.blobs_dir()).unwrap();
        std::fs::write(
            b.root.join("index.json"),
            br#"{"schemaVersion":2,"manifests":[]}"#,
        )
        .unwrap();
        let r = b.import_oci_archive(&tar, None).unwrap();
        assert_eq!(r, "reg/t/yq:4.44.3");
        for blob in &blobs {
            assert!(b.blob_path(blob).exists(), "load 之后缺 blob {blob}");
        }
        assert!(b.has_all_layers(&r), "闭包应判定为完整");

        // ③ 落位与在线 pull 一致:引用指向**有 config 的**那份子清单,
        //    不是 index。否则 `install` 一上来就是 "manifest 没有 config"。
        let m = b.resolve_manifest(&r).unwrap();
        assert!(
            m["manifests"].as_array().is_none(),
            "引用还指着 index,install 读不到契约"
        );
        assert_eq!(m["config"]["mediaType"].as_str(), Some(MT_PKG_CONFIG));
        let cfg: serde_json::Value = serde_json::from_slice(
            &std::fs::read(b.blob_path(strip(m["config"]["digest"].as_str().unwrap()))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            cfg["arch"].as_str(),
            Some(crate::arch::detect_local().as_str()),
            "挑错了架构 —— 装上去要到目标机 exec 才报 Exec format error"
        );

        // ④ **反证**:字节残缺的 store 导出必须报错,不再静默产出残 tar。
        //    (与 ①②③ 同一个用例,是因为 save/import 的临时目录按 PID 命名,
        //    同进程里并行跑两个导出会互相删掉对方的临时目录。)
        for blob in std::fs::read_dir(a.blobs_dir()).unwrap().flatten() {
            let n = blob.file_name().to_string_lossy().into_owned();
            if n != a.manifest_digest("reg/t/yq:4.44.3").unwrap() {
                std::fs::remove_file(blob.path()).unwrap();
            }
        }
        let e = a
            .export_oci_archive("reg/t/yq:4.44.3", &dir.join("gap.tar"))
            .unwrap_err();
        assert!(format!("{e:#}").contains("残包"), "错误没说清是残包:{e:#}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **反证**:多架构包缺了子清单的字节,`has_all_layers` 必须说 false。
    ///
    /// 老写法对 index 问的是它自己的 `config`/`layers` —— 两个都不存在,于是
    /// "没有就算通过",一个只有索引 blob 的空 store 也被判成闭包完整。那正是
    /// 空 tar 能一路装作成功、直到断网机上才炸的那一环。
    #[test]
    fn a_multiarch_package_missing_its_bytes_is_not_complete() {
        let dir = std::env::temp_dir().join(format!("crater-mularch-gap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (store, _) = fake_multiarch_store(&dir);
        assert!(
            store.has_all_layers("reg/t/yq:4.44.3"),
            "对照组:齐的时候要说 true"
        );

        // 只留索引 blob —— 这正是修复前的 `crater save` 打出来的那种残包。
        let root = store.manifest_digest("reg/t/yq:4.44.3").unwrap();
        for e in std::fs::read_dir(store.blobs_dir()).unwrap().flatten() {
            if e.file_name().to_string_lossy() != root {
                std::fs::remove_file(e.path()).unwrap();
            }
        }
        assert!(
            !store.has_all_layers("reg/t/yq:4.44.3"),
            "只剩一个索引 blob 却说闭包完整 —— 空包就是这样混过验收的"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
