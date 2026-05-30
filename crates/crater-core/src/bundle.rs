//! Offline bundle format (M2).
//!
//! A bundle is a single `*.bundle` file (tar + gzip, pure-Rust backend) that
//! carries everything needed to deploy a spec with **zero network access** on
//! the target side:
//!
//! ```text
//! manifest.yaml                      bundle metadata + blob index (sha256)
//! components/<name>/component.yaml   component descriptors
//! components/<name>/templates/*      templates
//! blobs/<sha256>                     downloaded artifacts, content-addressed
//! ```
//!
//! Build (online control machine): resolve a spec's components, fetch every
//! `download` action's URL, hash it, and pack. Deploy (offline): unpack, then
//! run the normal plan but feed pre-fetched blobs to the target instead of
//! having it `curl`.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const BUNDLE_FORMAT_VERSION: u32 = 1;

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
        fs::create_dir_all(root.join("blobs"))?;
        Ok(Self { root })
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.yaml")
    }
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join("blobs")
    }
    pub fn components_dir(&self) -> PathBuf {
        self.root.join("components")
    }
    pub fn blob_path(&self, sha256: &str) -> PathBuf {
        self.blobs_dir().join(sha256)
    }

    pub fn write_manifest(&self, m: &Manifest) -> crate::Result<()> {
        let yaml = serde_yaml::to_string(m)?;
        fs::write(self.manifest_path(), yaml)?;
        Ok(())
    }

    pub fn read_manifest(&self) -> crate::Result<Manifest> {
        let text = fs::read_to_string(self.manifest_path())?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// Store a blob by content hash; returns its [`BlobEntry`].
    pub fn store_blob(&self, source_url: &str, data: &[u8]) -> crate::Result<BlobEntry> {
        let sha = sha256_hex(data);
        fs::write(self.blob_path(&sha), data)?;
        Ok(BlobEntry {
            source_url: source_url.to_string(),
            sha256: sha,
            size: data.len() as u64,
        })
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

/// Pack a staged directory into a single `.bundle` (tar.gz) file.
pub fn pack(stage_root: &Path, out_file: &Path) -> crate::Result<()> {
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let f = fs::File::create(out_file)?;
    let enc = GzEncoder::new(f, Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all(".", stage_root)?;
    let enc = tar.into_inner()?;
    enc.finish()?;
    Ok(())
}

/// Unpack a `.bundle` (tar.gz) into a directory, returning a [`BundleStage`].
pub fn unpack(bundle_file: &Path, dest_root: &Path) -> crate::Result<BundleStage> {
    use flate2::read::GzDecoder;

    fs::create_dir_all(dest_root)?;
    let f = fs::File::open(bundle_file)?;
    let dec = GzDecoder::new(f);
    let mut ar = tar::Archive::new(dec);
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
