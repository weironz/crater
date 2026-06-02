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
        std::fs::write(self.root.join("index.json"), serde_json::to_vec_pretty(v)?)?;
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
    fn tag(&self, reference: &str, manifest_digest: &str, size: u64) -> crate::Result<()> {
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

    /// Pull an image/artifact from a registry into the store (pure Rust,
    /// oci-client). We store the **raw manifest bytes** (via `pull_manifest_raw`)
    /// so `artifactType` + custom layer mediaTypes survive the round-trip — the
    /// high-level `pull()` would synthesize a plain image-manifest and drop them
    /// (D-033). Blob *data* still comes from `pull()`; their sha256 match the
    /// digests the raw manifest references (content-addressed).
    pub async fn pull(&self, reference: &str) -> crate::Result<()> {
        use oci_client::manifest as mt;
        use oci_client::Reference;

        let r: Reference = reference
            .parse()
            .map_err(|e| anyhow::anyhow!("bad image ref '{reference}': {e}"))?;
        let client = registry_client();
        let auth = auth_for(reference);
        let accepted = vec![
            mt::IMAGE_MANIFEST_MEDIA_TYPE,
            mt::IMAGE_MANIFEST_LIST_MEDIA_TYPE,
            mt::OCI_IMAGE_MEDIA_TYPE,       // ghcr & others use OCI manifest
            mt::OCI_IMAGE_INDEX_MEDIA_TYPE, // ...and OCI image index (multi-arch)
            mt::IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE,
            mt::IMAGE_LAYER_GZIP_MEDIA_TYPE,
            mt::IMAGE_LAYER_MEDIA_TYPE,
            mt::IMAGE_CONFIG_MEDIA_TYPE,
            mt::IMAGE_DOCKER_CONFIG_MEDIA_TYPE,
        ];
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
            let sub = entries
                .iter()
                .find(|e| {
                    e["platform"]["os"].as_str() == Some("linux")
                        && e["platform"]["architecture"].as_str() == Some("amd64")
                })
                .or_else(|| entries.iter().find(|e| e["platform"]["architecture"].is_string()))
                .ok_or_else(|| anyhow::anyhow!("manifest list '{reference}' has no linux/amd64 entry"))?;
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
    pub async fn push(&self, reference: &str) -> crate::Result<()> {
        use oci_client::manifest::{OciImageManifest, OciManifest};
        use oci_client::{Reference, RegistryOperation};

        let manifest_blob = self.manifest_blob(reference)?;
        let im: OciImageManifest = serde_json::from_slice(&manifest_blob)?;
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
        let entry = idx["manifests"]
            .as_array()
            .and_then(|a| a.iter().find(|m| m["annotations"][ANN_REF].as_str() == Some(reference)))
            .ok_or_else(|| anyhow::anyhow!("image '{reference}' not in local store"))?;
        let mdig_full = entry["digest"].as_str().unwrap_or("").to_string();
        let msize = entry["size"].as_u64().unwrap_or(0);
        let mdig = strip(&mdig_full);
        let manifest: serde_json::Value = serde_json::from_slice(&std::fs::read(self.blob_path(mdig))?)?;

        let tmp = std::env::temp_dir().join(format!("crater-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let blobs = tmp.join("blobs").join("sha256");
        std::fs::create_dir_all(&blobs)?;
        std::fs::write(tmp.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#)?;
        let copy = |d: &str| -> crate::Result<()> {
            std::fs::copy(self.blob_path(strip(d)), blobs.join(strip(d)))?;
            Ok(())
        };
        copy(mdig)?;
        if let Some(c) = manifest["config"]["digest"].as_str() {
            copy(c)?;
        }
        if let Some(ls) = manifest["layers"].as_array() {
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
            "annotations": { ANN_REF: reference }
        });
        if let Some(at) = manifest["artifactType"].as_str() {
            m_entry["artifactType"] = json!(at);
        }
        let index = json!({
            "schemaVersion": 2,
            "mediaType": "application/vnd.oci.image.index.v1+json",
            "manifests": [m_entry]
        });
        std::fs::write(tmp.join("index.json"), serde_json::to_vec_pretty(&index)?)?;
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

/// An oci-client honoring `$CRATER_INSECURE_REGISTRIES` (comma-separated hosts
/// served over plain HTTP, e.g. a temp zot at `192.168.73.5:5000`).
fn registry_client() -> oci_client::Client {
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
fn registry_of(reference: &str) -> String {
    let first = reference.split('/').next().unwrap_or("");
    if first.contains('.') || first.contains(':') || first == "localhost" {
        first.to_string()
    } else {
        "docker.io".to_string()
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

/// Resolve registry auth for a reference: stored creds → Basic, else Anonymous.
fn auth_for(reference: &str) -> oci_client::secrets::RegistryAuth {
    use oci_client::secrets::RegistryAuth;
    let reg = registry_of(reference);
    let f: Option<AuthFile> = std::fs::read(auth_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok());
    if let Some(c) = f.and_then(|f| f.registries.get(&reg).cloned()) {
        RegistryAuth::Basic(c.username, c.password)
    } else {
        RegistryAuth::Anonymous
    }
}
