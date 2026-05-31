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
    pub size: u64,
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
                out.push(StoredImage {
                    reference: m["annotations"][ANN_REF].as_str().unwrap_or("<untagged>").to_string(),
                    digest: m["digest"].as_str().unwrap_or("").to_string(),
                    size: m["size"].as_u64().unwrap_or(0),
                });
            }
        }
        Ok(out)
    }

    pub fn has(&self, reference: &str) -> bool {
        self.list()
            .map(|l| l.iter().any(|s| s.reference == reference))
            .unwrap_or(false)
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

        // Fetch config + each layer blob by digest. We use `pull_blob` (not the
        // high-level `pull`, which rejects non-image layer mediaTypes like our
        // crater.recipe/material) so artifacts pull cleanly.
        let m: serde_json::Value = serde_json::from_slice(&raw)?;
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
            let mut buf: Vec<u8> = Vec::new();
            client
                .pull_blob(&r, d.as_str(), &mut buf)
                .await
                .map_err(|e| anyhow::anyhow!("pull blob {d} of '{reference}': {e}"))?;
            self.store_raw(&buf)?;
        }
        let (md, ms) = self.store_raw(&raw)?;
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

    /// Import an oci-archive file (e.g. `crater build --image` output) into the
    /// store under `as_ref`: copy its image's manifest/config/layer blobs in and
    /// tag them. Picks the archive manifest carrying an `image.ref.name` (the
    /// rootfs image), not crater's own bundle manifest.
    pub fn import_oci_archive(&self, archive: &std::path::Path, as_ref: &str) -> crate::Result<()> {
        let tmp = std::env::temp_dir().join(format!("crater-import-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        crate::bundle::unpack(archive, &tmp)?;
        let index: serde_json::Value = serde_json::from_slice(&std::fs::read(tmp.join("index.json"))?)?;
        let entry = index["manifests"]
            .as_array()
            .and_then(|a| a.iter().find(|m| m["annotations"][ANN_REF].as_str().is_some()))
            .ok_or_else(|| anyhow::anyhow!("{}: no image manifest (ref.name) in archive", archive.display()))?;
        let mdig = entry["digest"].as_str().unwrap_or("").to_string();
        let mdig = strip(&mdig);
        let src_blob = |d: &str| tmp.join("blobs").join("sha256").join(strip(d));
        let copy_in = |d: &str| -> crate::Result<()> {
            let data = std::fs::read(src_blob(d))?;
            self.store_raw(&data)?;
            Ok(())
        };
        // manifest + config + layers.
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
        self.tag(as_ref, mdig, msize)?;
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
