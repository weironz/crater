//! Offline bundle format — **OCI Image Layout** (D-018).
//!
//! A bundle is a single file (an `oci-archive`: a plain tar of a spec-conformant
//! OCI Image Layout) that carries everything to deploy a spec with **zero
//! network on the target**:
//!
//! ```text
//! oci-layout                         {"imageLayoutVersion":"1.0.0"}
//! index.json                         OCI image index → image manifest
//!                                       (annotation org.crater.manifest → crater-manifest blob)
//! blobs/sha256/<digest>              content-addressed:
//!   ├─ crater-manifest (JSON)          spec metadata + blob index (source-url → sha256)
//!   ├─ OCI config                      minimal config (rootfs.diff_ids)
//!   ├─ OCI image manifest              config + layers (components + each artifact)
//!   ├─ components layer (tar)          component.yaml + templates
//!   └─ artifact blob(s)                downloaded files, annotated with their source URL
//! components/<name>/...               crater convenience copy (deploy reads here; OCI tools ignore it)
//! ```
//!
//! Content-addressing means digests ARE the integrity check (no hand-written
//! sha list). Container images (nested OCI blobs) + temp registry are the next
//! increment; today's layout carries file artifacts. Build (online): fetch every
//! `download` URL, hash, assemble the layout. Deploy (offline): unpack, verify
//! by digest, run the plan feeding pre-fetched blobs instead of `curl`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

pub const BUNDLE_FORMAT_VERSION: u32 = 2;

const MT_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MT_CONFIG: &str = "application/vnd.oci.image.config.v1+json";
const MT_LAYER: &str = "application/vnd.oci.image.layer.v1.tar";
const MT_ARTIFACT: &str = "application/vnd.crater.artifact.v1";
const ANN_CRATER_MANIFEST: &str = "org.crater.manifest";
const ANN_SOURCE_URL: &str = "org.crater.source-url";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub format_version: u32,
    /// Bundle display name (usually derived from the spec / first component).
    pub name: String,
    pub components: Vec<ManifestComponent>,
    /// Content-addressed blob index, keyed by the (rendered) source URL.
    pub blobs: Vec<BlobEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ManifestComponent {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BlobEntry {
    /// The rendered URL this blob was fetched from (lookup key during deploy).
    pub source_url: String,
    pub sha256: String,
    pub size: u64,
}

impl Manifest {
    pub fn blob_for_url(&self, url: &str) -> Option<&BlobEntry> {
        self.blobs.iter().find(|b| b.source_url == url)
    }
}

/// Compute the hex sha256 of a byte slice.
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let out = h.finalize();
    let mut s = String::with_capacity(out.len() * 2);
    for b in out {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A staging directory layout for a bundle, before/after (un)packing.
pub struct BundleStage {
    pub root: PathBuf,
}

impl BundleStage {
    pub fn new(root: PathBuf) -> crate::Result<Self> {
        fs::create_dir_all(root.join("components"))?;
        fs::create_dir_all(root.join("blobs").join("sha256"))?;
        Ok(Self { root })
    }

    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs").join("sha256")
    }
    pub fn components_dir(&self) -> PathBuf {
        self.root.join("components")
    }
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.blobs_dir().join(sha256)
    }

    /// Store raw bytes content-addressed; returns (sha256_hex, size).
    fn store_raw(&self, data: &[u8]) -> crate::Result<(String, u64)> {
        let sha = sha256_hex(data);
        fs::write(self.blob_path(&sha), data)?;
        Ok((sha, data.len() as u64))
    }

    /// Store an artifact blob (keyed by its source URL); returns its [`BlobEntry`].
    pub fn store_blob(&self, source_url: &str, data: &[u8]) -> crate::Result<BlobEntry> {
        let (sha, size) = self.store_raw(data)?;
        Ok(BlobEntry {
            source_url: source_url.to_string(),
            sha256: sha,
            size,
        })
    }

    /// Assemble the OCI Image Layout: store the crater-manifest, components
    /// layer, OCI config + image manifest as blobs, and write `index.json` +
    /// `oci-layout`. (Named `write_manifest` for call-site stability.)
    pub fn write_manifest(&self, m: &Manifest) -> crate::Result<()> {
        // crater-manifest blob (our deploy metadata).
        let cm = serde_json::to_vec_pretty(m)?;
        let (cm_digest, _) = self.store_raw(&cm)?;

        // components/ → a single tar layer (OCI conformance; deploy reads the
        // real dir, this layer is for OCI tooling).
        let layer = tar_dir_to_vec(&self.components_dir())?;
        let (layer_digest, layer_size) = self.store_raw(&layer)?;

        // OCI config.
        let config = json!({
            "architecture": "amd64", "os": "linux",
            "rootfs": {"type": "layers", "diff_ids": [format!("sha256:{layer_digest}")]}
        });
        let (cfg_digest, cfg_size) = self.store_raw(&serde_json::to_vec(&config)?)?;

        // OCI image manifest: config + components layer + one layer per artifact.
        let mut layers = vec![json!({
            "mediaType": MT_LAYER, "digest": format!("sha256:{layer_digest}"), "size": layer_size
        })];
        for b in &m.blobs {
            layers.push(json!({
                "mediaType": MT_ARTIFACT,
                "digest": format!("sha256:{}", b.sha256),
                "size": b.size,
                "annotations": { ANN_SOURCE_URL: b.source_url }
            }));
        }
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MT_MANIFEST,
            "config": {"mediaType": MT_CONFIG, "digest": format!("sha256:{cfg_digest}"), "size": cfg_size},
            "layers": layers
        });
        let (man_digest, man_size) = self.store_raw(&serde_json::to_vec(&manifest)?)?;

        // index.json points at the manifest; the crater-manifest is found via annotation.
        let index = json!({
            "schemaVersion": 2, "mediaType": MT_INDEX,
            "manifests": [{
                "mediaType": MT_MANIFEST,
                "digest": format!("sha256:{man_digest}"), "size": man_size,
                "annotations": { ANN_CRATER_MANIFEST: format!("sha256:{cm_digest}") }
            }]
        });
        fs::write(self.root.join("index.json"), serde_json::to_vec_pretty(&index)?)?;
        fs::write(self.root.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#)?;
        Ok(())
    }

    /// Read the crater-manifest back out of the OCI layout (index → annotation → blob).
    pub fn read_manifest(&self) -> crate::Result<Manifest> {
        let index: serde_json::Value =
            serde_json::from_slice(&fs::read(self.root.join("index.json"))?)?;
        let ann = index["manifests"][0]["annotations"][ANN_CRATER_MANIFEST]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("index.json missing {ANN_CRATER_MANIFEST} annotation"))?;
        let digest = ann.strip_prefix("sha256:").unwrap_or(ann);
        let bytes = fs::read(self.blob_path(digest))
            .map_err(|e| anyhow::anyhow!("read crater-manifest blob {digest}: {e}"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    /// Verify every blob on disk matches its manifest hash.
    pub fn verify(&self, m: &Manifest) -> crate::Result<()> {
        for b in &m.blobs {
            let p = self.blob_path(&b.sha256);
            let data =
                fs::read(&p).map_err(|e| anyhow::anyhow!("missing blob {}: {e}", b.sha256))?;
            let got = sha256_hex(&data);
            if got != b.sha256 {
                anyhow::bail!(
                    "blob checksum mismatch for {}: manifest {} != actual {}",
                    b.source_url,
                    b.sha256,
                    got
                );
            }
        }
        Ok(())
    }
}

/// Tar a directory into an in-memory plain tar (used for the components layer).
fn tar_dir_to_vec(dir: &Path) -> crate::Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::new());
    tar.append_dir_all(".", dir)?;
    Ok(tar.into_inner()?)
}

/// Pack a staged OCI layout into a single file (an `oci-archive`: plain tar —
/// blobs are already-compressed artifacts, so no outer gzip).
pub fn pack(stage_root: &Path, out_file: &Path) -> crate::Result<()> {
    let f = fs::File::create(out_file)?;
    let mut tar = tar::Builder::new(f);
    tar.append_dir_all(".", stage_root)?;
    tar.into_inner()?;
    Ok(())
}

/// Unpack an `oci-archive` (plain tar) into a directory, returning a [`BundleStage`].
pub fn unpack(bundle_file: &Path, dest_root: &Path) -> crate::Result<BundleStage> {
    fs::create_dir_all(dest_root)?;
    let f = fs::File::open(bundle_file)?;
    let mut ar = tar::Archive::new(f);
    ar.unpack(dest_root)?;
    Ok(BundleStage {
        root: dest_root.to_path_buf(),
    })
}

/// Read a whole file into memory (small helper used by build).
pub fn read_file(path: &Path) -> crate::Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    let mut v = Vec::new();
    f.read_to_end(&mut v)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn pack_unpack_roundtrip() {
        let tmp =
            std::env::temp_dir().join(format!("crater-bundle-test-{}", std::process::id()));
        let stage_root = tmp.join("stage");
        let stage = BundleStage::new(stage_root.clone()).unwrap();
        let blob = stage
            .store_blob("https://example/x.tgz", b"hello-artifact")
            .unwrap();
        let m = Manifest {
            format_version: BUNDLE_FORMAT_VERSION,
            name: "test".into(),
            components: vec![ManifestComponent {
                name: "docker".into(),
                version: "24.0".into(),
            }],
            blobs: vec![blob.clone()],
        };
        stage.write_manifest(&m).unwrap();

        let bundle = tmp.join("out.bundle");
        pack(&stage_root, &bundle).unwrap();
        assert!(bundle.exists());

        let dest = tmp.join("unpacked");
        let stage2 = unpack(&bundle, &dest).unwrap();
        // Conformant OCI Image Layout: oci-layout + index.json + blobs/sha256/.
        assert!(dest.join("oci-layout").is_file());
        assert!(dest.join("index.json").is_file());
        assert!(dest.join("blobs").join("sha256").is_dir());

        let m2 = stage2.read_manifest().unwrap();
        assert_eq!(m2.name, "test");
        assert_eq!(m2.blobs.len(), 1);
        assert_eq!(m2.blobs[0].sha256, blob.sha256);
        stage2.verify(&m2).unwrap();

        let restored = read_file(&stage2.blob_path(&blob.sha256)).unwrap();
        assert_eq!(restored, b"hello-artifact");

        let _ = fs::remove_dir_all(&tmp);
    }
}
