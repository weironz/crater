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
//!   └─ material blob(s)                fetched files, annotated with their material name
//! components/<name>/...               crater convenience copy (deploy reads here; OCI tools ignore it)
//! ```
//!
//! Content-addressing means digests ARE the integrity check (no hand-written
//! sha list). Container images (nested OCI blobs) + temp registry are the next
//! increment; today's layout carries file materials. Build (online): fetch every
//! declared `material` URL, hash, assemble the layout. Deploy (offline): unpack,
//! verify by digest, run the plan feeding pre-fetched blobs instead of `curl`.

use std::fs;
use std::io::{Read, Write};
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

// ---- B 类 OCI artifact (D-032): a crater component, not a runnable image ----
/// artifactType marking a manifest as a crater component (image-spec 1.1 form:
/// an image-manifest carrying `artifactType` — max registry/oci-client compat).
pub const AT_COMPONENT: &str = "application/vnd.crater.component.v1";
/// A project artifact (D-098): recipe = the LOCKED project yaml (each play's
/// `source` rewritten to the built task artifact's ref). No material layers —
/// the closure lives in the referenced task artifacts, bundled alongside.
pub const AT_PROJECT: &str = "application/vnd.crater.project.v1";
const MT_COMPONENT_CONFIG: &str = "application/vnd.crater.component.config.v1+json";
/// Layer carrying the component recipe (component.yaml).
const MT_RECIPE: &str = "application/vnd.crater.recipe.v1+yaml";
/// Layer carrying a fetched material (binary/tarball), annotated with its
/// logical material name (D-034) so `place` resolves it offline by name.
const MT_MATERIAL: &str = "application/vnd.crater.material.v1";
const ANN_MATERIAL_NAME: &str = "org.crater.material.name";
/// Layer fetch class (D-087): `embedded` = self-authored file with no online
/// source → always pulled (even a thin pull); `dependency` = has url/ref/pkgs →
/// skipped on a thin pull, fetched online at apply. Absent ⇒ treated as embedded
/// (old artifacts pull in full — safe, just no thin savings).
const ANN_MATERIAL_FETCH: &str = "org.crater.material.fetch";
const ANN_COMPONENT_NAME: &str = "org.crater.component.name";
const ANN_COMPONENT_VERSION: &str = "org.crater.component.version";
const ANN_RUN_MODE: &str = "org.crater.run-mode";

/// A crater component artifact materialized for install: the recipe is written
/// under `components/<name>/component.yaml`, and `blobmap` maps each material's
/// logical name to its local blob (so the recipe's `place` actions resolve
/// offline). Drives the task engine in offline mode — no fake rootfs extraction.
#[derive(Debug, Clone)]
pub struct MaterializedComponent {
    pub name: String,
    pub version: String,
    pub blobmap: std::collections::BTreeMap<String, PathBuf>,
    /// The artifact's ref (`ref.name` index annotation) — how a project's
    /// locked play `source` finds its task in a bundle (D-098). Empty when
    /// materialized outside a bundle index (e.g. straight from a manifest).
    pub reference: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    pub format_version: u32,
    /// Bundle display name (usually derived from the spec / first component).
    pub name: String,
    pub components: Vec<ManifestComponent>,
    /// Content-addressed blob index, keyed by the (rendered) source URL.
    pub blobs: Vec<BlobEntry>,
    /// Container images PULLED from a registry into the bundle (D-018 ②).
    #[serde(default)]
    pub images: Vec<ImageRef>,
    /// App rootfs images BUILT by crater (yq etc.): a single fs layer crater
    /// extracts to `/` on the target to install — no container runtime needed.
    #[serde(default)]
    pub rootfs: Vec<ImageRef>,
}

/// A container image packed into the bundle: its OCI blobs are stored under
/// `blobs/sha256/`, referenced by a (synthesized) image manifest. (D-018 ②)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImageRef {
    pub reference: String,
    /// sha256 hex of the image manifest blob (no `sha256:` prefix).
    pub manifest_digest: String,
    pub manifest_size: u64,
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

    /// Pull a container image and store its OCI blobs (config + layers +
    /// synthesized manifest) under `blobs/sha256/` (D-018 ②). Returns an
    /// [`ImageRef`]; the image becomes a first-class manifest in `index.json`,
    /// so the whole archive is `ctr image import`-able. Pure Rust via oci-client
    /// (rustls), so no Docker/skopeo on the control machine.
    pub async fn pull_image(&self, reference: &str) -> crate::Result<ImageRef> {
        use oci_client::manifest as mt;
        use oci_client::Reference;

        let r: Reference = reference
            .parse()
            .map_err(|e| anyhow::anyhow!("bad image ref '{reference}': {e}"))?;
        // 与 `crater pull` 走同一套客户端与凭据:私有 registry 的镜像、
        // 本地 HTTP registry(`$CRATER_INSECURE_REGISTRIES`)都要能烤进闭包。
        // 早先这里写死匿名 + 默认配置,等于宣布"闭包只支持公开镜像"。
        let client = crate::store::registry_client();
        let auth = crate::store::auth_for(reference);
        let accepted = vec![
            mt::IMAGE_MANIFEST_MEDIA_TYPE,
            mt::IMAGE_MANIFEST_LIST_MEDIA_TYPE,
            mt::IMAGE_DOCKER_LAYER_GZIP_MEDIA_TYPE,
            mt::IMAGE_LAYER_GZIP_MEDIA_TYPE,
            mt::IMAGE_LAYER_MEDIA_TYPE,
            mt::IMAGE_CONFIG_MEDIA_TYPE,
            mt::IMAGE_DOCKER_CONFIG_MEDIA_TYPE,
        ];
        let img = client
            .pull(&r, &auth, accepted)
            .await
            .map_err(|e| anyhow::anyhow!("pull image '{reference}': {e}"))?;

        // Store config + each layer as content-addressed blobs.
        let (cfg_digest, cfg_size) = self.store_raw(&img.config.data)?;
        let mut layers_json = Vec::new();
        for layer in &img.layers {
            let (d, sz) = self.store_raw(&layer.data)?;
            layers_json.push(json!({
                "mediaType": layer.media_type, "digest": format!("sha256:{d}"), "size": sz
            }));
        }
        // Synthesize a self-consistent OCI image manifest over our stored blobs.
        let manifest = json!({
            "schemaVersion": 2,
            "mediaType": MT_MANIFEST,
            "config": {"mediaType": img.config.media_type, "digest": format!("sha256:{cfg_digest}"), "size": cfg_size},
            "layers": layers_json
        });
        let mbytes = serde_json::to_vec(&manifest)?;
        let (mdig, msize) = self.store_raw(&mbytes)?;
        Ok(ImageRef {
            reference: reference.to_string(),
            manifest_digest: mdig,
            manifest_size: msize,
        })
    }

    /// Build an OCI image whose single layer is a rootfs containing `files`
    /// (each `(path, bytes, mode)`, path relative to `/`). crater's own `build`:
    /// wrap an artifact (e.g. the yq binary) into an OCI image, no Docker. The
    /// target installs it by extracting the layer to `/` (crater-native load).
    pub fn store_rootfs_layer(
        &self,
        reference: &str,
        files: &[(String, Vec<u8>, u32)],
    ) -> crate::Result<ImageRef> {
        // Build the rootfs layer tar (paths + exec/permission bits).
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data, mode) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(*mode);
            header.set_cksum();
            let rel = path.trim_start_matches('/');
            builder.append_data(&mut header, rel, data.as_slice())?;
        }
        let layer = builder.into_inner()?;
        self.image_from_layer(reference, &layer)
    }

    /// Like [`store_rootfs_layer`] but the layer is a tar of a real directory
    /// tree (preserving file modes) — used when `crater build --image` has
    /// materialized a component's file actions into a staging rootfs.
    pub fn store_rootfs_layer_dir(&self, reference: &str, dir: &Path) -> crate::Result<ImageRef> {
        let layer = tar_dir_to_vec(dir)?;
        self.image_from_layer(reference, &layer)
    }

    /// Build a **B 类 OCI artifact** for a crater task (D-032): a recipe
    /// layer + one material layer per material (annotated by material name)
    /// + a small config, under an `artifactType` manifest. NOT a runnable
    /// image. Loaded by recipe-replay (materials feed the recipe's `place`
    /// actions offline), not by extracting a rootfs.
    pub fn store_component_artifact(
        &self,
        reference: &str,
        name: &str,
        version: &str,
        run_mode: &str,
        recipe_yaml: &[u8],
        materials: &[(String, bool, Vec<u8>)], // (name, embedded?, bytes) — D-087
    ) -> crate::Result<ImageRef> {
        let (recipe_d, recipe_s) = self.store_raw(recipe_yaml)?;
        let mut layers = vec![json!({
            "mediaType": MT_RECIPE, "digest": format!("sha256:{recipe_d}"), "size": recipe_s,
            "annotations": { ANN_COMPONENT_NAME: name }
        })];
        // Each material layer is annotated with its logical material name (D-034)
        // — the key `place` resolves against during offline recipe-replay.
        for (mat_name, embedded, data) in materials {
            let (d, s) = self.store_raw(data)?;
            let fetch = if *embedded { "embedded" } else { "dependency" };
            layers.push(json!({
                "mediaType": MT_MATERIAL, "digest": format!("sha256:{d}"), "size": s,
                "annotations": { ANN_MATERIAL_NAME: mat_name, ANN_MATERIAL_FETCH: fetch }
            }));
        }
        let cfg = json!({"name": name, "version": version, "runMode": run_mode});
        let (cfg_d, cfg_s) = self.store_raw(&serde_json::to_vec(&cfg)?)?;
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MT_MANIFEST, "artifactType": AT_COMPONENT,
            "config": {"mediaType": MT_COMPONENT_CONFIG, "digest": format!("sha256:{cfg_d}"), "size": cfg_s},
            "layers": layers,
            "annotations": {
                ANN_COMPONENT_NAME: name, ANN_COMPONENT_VERSION: version, ANN_RUN_MODE: run_mode
            }
        });
        let (md, ms) = self.store_raw(&serde_json::to_vec(&manifest)?)?;
        Ok(ImageRef {
            reference: reference.to_string(),
            manifest_digest: md,
            manifest_size: ms,
        })
    }

    /// Store a project artifact (D-098): artifactType [`AT_PROJECT`], single
    /// recipe layer = the locked project yaml. Task closures are NOT inside —
    /// they're separate artifacts in the same bundle/store, referenced by the
    /// locked `source` refs (content-addressed blobs dedup across them).
    pub fn store_project_artifact(
        &self,
        reference: &str,
        name: &str,
        recipe_yaml: &[u8],
    ) -> crate::Result<ImageRef> {
        let (recipe_d, recipe_s) = self.store_raw(recipe_yaml)?;
        let cfg = json!({ "name": name, "runMode": "project" });
        let (cfg_d, cfg_s) = self.store_raw(&serde_json::to_vec(&cfg)?)?;
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MT_MANIFEST, "artifactType": AT_PROJECT,
            "config": {"mediaType": MT_COMPONENT_CONFIG, "digest": format!("sha256:{cfg_d}"), "size": cfg_s},
            "layers": [{
                "mediaType": MT_RECIPE, "digest": format!("sha256:{recipe_d}"), "size": recipe_s,
                "annotations": { ANN_COMPONENT_NAME: name }
            }],
            "annotations": { ANN_COMPONENT_NAME: name, ANN_RUN_MODE: "project" }
        });
        let (md, ms) = self.store_raw(&serde_json::to_vec(&manifest)?)?;
        Ok(ImageRef {
            reference: reference.to_string(),
            manifest_digest: md,
            manifest_size: ms,
        })
    }

    /// Store a fs-layer tar + synthesize a minimal OCI image (config+manifest)
    /// over it. Returns an [`ImageRef`]; the image is a real, pushable OCI image.
    fn image_from_layer(&self, reference: &str, layer: &[u8]) -> crate::Result<ImageRef> {
        let (layer_digest, layer_size) = self.store_raw(layer)?;
        let config = json!({
            "architecture": "amd64", "os": "linux",
            "rootfs": {"type": "layers", "diff_ids": [format!("sha256:{layer_digest}")]}
        });
        let (cfg_digest, cfg_size) = self.store_raw(&serde_json::to_vec(&config)?)?;
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MT_MANIFEST,
            "config": {"mediaType": MT_CONFIG, "digest": format!("sha256:{cfg_digest}"), "size": cfg_size},
            "layers": [{"mediaType": MT_LAYER, "digest": format!("sha256:{layer_digest}"), "size": layer_size}]
        });
        let (mdig, msize) = self.store_raw(&serde_json::to_vec(&manifest)?)?;
        Ok(ImageRef {
            reference: reference.to_string(),
            manifest_digest: mdig,
            manifest_size: msize,
        })
    }

    /// The fs-layer blob digest of a stored image manifest (for `load`/extract).
    pub fn layer_of(&self, manifest_digest: &str) -> crate::Result<String> {
        let m: serde_json::Value = serde_json::from_slice(&fs::read(self.blob_path(manifest_digest))?)?;
        let d = m["layers"][0]["digest"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("image manifest {manifest_digest} has no layer"))?;
        Ok(d.strip_prefix("sha256:").unwrap_or(d).to_string())
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

        // index.json: the crater image manifest (carrying the crater-manifest
        // annotation) + one entry per packed container image (named via the
        // standard ref.name annotation so `ctr image import` names it).
        let mut manifests = vec![json!({
            "mediaType": MT_MANIFEST,
            "digest": format!("sha256:{man_digest}"), "size": man_size,
            "annotations": { ANN_CRATER_MANIFEST: format!("sha256:{cm_digest}") }
        })];
        for img in m.images.iter().chain(m.rootfs.iter()) {
            manifests.push(json!({
                "mediaType": MT_MANIFEST,
                "digest": format!("sha256:{}", img.manifest_digest),
                "size": img.manifest_size,
                "annotations": { "org.opencontainers.image.ref.name": img.reference }
            }));
        }
        let index = json!({
            "schemaVersion": 2, "mediaType": MT_INDEX, "manifests": manifests
        });
        fs::write(self.root.join("index.json"), serde_json::to_vec_pretty(&index)?)?;
        fs::write(self.root.join("oci-layout"), br#"{"imageLayoutVersion":"1.0.0"}"#)?;
        Ok(())
    }

    /// Write an OCI layout whose `index.json` lists crater **component artifacts**
    /// (D-032) — no crater-manifest, no rootfs. Used by `crater build --image`.
    pub fn write_artifact_index(&self, artifacts: &[ImageRef]) -> crate::Result<()> {
        let manifests: Vec<_> = artifacts
            .iter()
            .map(|ir| {
                // Index entries mirror each manifest's actual artifactType
                // (component vs project, D-098) so readers can route by entry.
                let at = fs::read(self.blob_path(&ir.manifest_digest))
                    .ok()
                    .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
                    .and_then(|m| m["artifactType"].as_str().map(String::from))
                    .unwrap_or_else(|| AT_COMPONENT.to_string());
                json!({
                    "mediaType": MT_MANIFEST,
                    "artifactType": at,
                    "digest": format!("sha256:{}", ir.manifest_digest),
                    "size": ir.manifest_size,
                    "annotations": { "org.opencontainers.image.ref.name": ir.reference }
                })
            })
            .collect();
        let index = json!({"schemaVersion": 2, "mediaType": MT_INDEX, "manifests": manifests});
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

/// Extract a (optionally gzip) tar archive into `dest_dir`, dropping `strip`
/// leading path components — the pure-Rust equivalent of the target's
/// 把一组 (相对路径, 字节, mode) 打成 tar.gz —— 蓝图包那一层就是它。
///
/// 输入是**列表而不是目录**:哪些文件该进包是调用方的策略(凭据文件、
/// 闭包、备份都不该进),让这里去猜等于把策略藏在最底层。
pub fn tar_gz_files(files: &[(String, Vec<u8>, u32)]) -> crate::Result<Vec<u8>> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(*mode);
        header.set_mtime(0); // 可复现:同样的内容打出同样的字节
        header.set_cksum();
        builder.append_data(&mut header, path.trim_start_matches('/'), data.as_slice())?;
    }
    let tar = builder.into_inner()?;
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(&tar)?;
    Ok(gz.finish()?)
}

/// `tar -xf … --strip-components`, used to bake an `extract` action into a
/// rootfs layer at build time (`crater build --image`).
pub fn untar_gz_into(dest_dir: &Path, data: &[u8], strip: u32) -> crate::Result<()> {
    use flate2::read::GzDecoder;
    fs::create_dir_all(dest_dir)?;
    // gzip-sniff (magic 1f 8b); otherwise treat as plain tar.
    let is_gz = data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b;
    let reader: Box<dyn Read> = if is_gz {
        Box::new(GzDecoder::new(data))
    } else {
        Box::new(data)
    };
    let mut ar = tar::Archive::new(reader);
    ar.set_preserve_permissions(true);
    for entry in ar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();
        let stripped: PathBuf = path.components().skip(strip as usize).collect();
        if stripped.as_os_str().is_empty() {
            continue;
        }
        // 一个**只有文件条目、没有目录条目**的 tar 是合法的(`tar_gz_files`
        // 打出来的就是),而 `unpack` 不会替你建父目录 —— 少了这一句,
        // `templates/x.j2` 会以 ENOENT 失败,报错还只说"文件不存在"。
        let out = dest_dir.join(stripped);
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        entry.unpack(out)?;
    }
    Ok(())
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

/// If `manifest` is a crater component artifact (D-032), materialize it: write
/// its recipe to `out_components_dir/<name>/component.yaml` and map each
/// material's source-url to its blob (for offline recipe-replay). Returns
/// `None` for a plain (non-crater) image, so callers fall back to rootfs/import.
pub fn materialize_component(
    manifest: &serde_json::Value,
    blobs_dir: &Path,
    out_components_dir: &Path,
) -> crate::Result<Option<MaterializedComponent>> {
    if manifest["artifactType"].as_str() != Some(AT_COMPONENT) {
        return Ok(None);
    }
    let name = manifest["annotations"][ANN_COMPONENT_NAME]
        .as_str()
        .unwrap_or("component")
        .to_string();
    let version = manifest["annotations"][ANN_COMPONENT_VERSION]
        .as_str()
        .unwrap_or("latest")
        .to_string();
    let cdir = out_components_dir.join(&name);
    fs::create_dir_all(&cdir)?;
    let mut blobmap = std::collections::BTreeMap::new();
    if let Some(layers) = manifest["layers"].as_array() {
        for l in layers {
            let mt = l["mediaType"].as_str().unwrap_or("");
            let digest = l["digest"].as_str().unwrap_or("");
            let blob = blobs_dir.join(digest.strip_prefix("sha256:").unwrap_or(digest));
            if mt == MT_RECIPE {
                fs::write(cdir.join("component.yaml"), fs::read(&blob)?)?;
            } else if mt == MT_MATERIAL {
                // Key by material name (D-034); fall back to legacy source-url.
                // Only map blobs actually present locally (D-087): a thin pull
                // leaves `dependency` layers in the registry, so their blobs are
                // absent — those materials are fetched online at apply instead.
                if blob.exists() {
                    if let Some(key) = l["annotations"][ANN_MATERIAL_NAME]
                        .as_str()
                        .or_else(|| l["annotations"][ANN_SOURCE_URL].as_str())
                    {
                        blobmap.insert(key.to_string(), blob);
                    }
                }
            }
        }
    }
    Ok(Some(MaterializedComponent { name, version, blobmap, reference: String::new() }))
}

/// Read the project artifact out of an unpacked bundle, if present (D-098):
/// scan the index for [`AT_PROJECT`], read its recipe layer, parse the LOCKED
/// project (play `source`s are task artifact refs in the same bundle).
pub fn read_artifact_project(bundle_root: &Path) -> crate::Result<Option<crate::project::Project>> {
    let blobs_dir = bundle_root.join("blobs").join("sha256");
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle_root.join("index.json"))?)?;
    let Some(ms) = index["manifests"].as_array() else { return Ok(None) };
    for m in ms {
        if m["artifactType"].as_str() != Some(AT_PROJECT) {
            continue;
        }
        let d = m["digest"].as_str().unwrap_or("");
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(blobs_dir.join(d.strip_prefix("sha256:").unwrap_or(d)))?)?;
        let recipe = manifest["layers"]
            .as_array()
            .and_then(|ls| ls.iter().find(|l| l["mediaType"].as_str() == Some(MT_RECIPE)))
            .and_then(|l| l["digest"].as_str())
            .ok_or_else(|| anyhow::anyhow!("project artifact has no recipe layer"))?;
        let bytes = fs::read(blobs_dir.join(recipe.strip_prefix("sha256:").unwrap_or(recipe)))?;
        return Ok(Some(serde_yaml::from_slice(&bytes)?));
    }
    Ok(None)
}

/// Read all crater component artifacts from an unpacked bundle dir: for each
/// `index.json` manifest with `artifactType` crater.component, materialize it
/// (recipe → `out_components_dir`, materials → blobmap). Empty ⇒ not a B 类
/// artifact bundle (caller falls back to the legacy crater-manifest path).
pub fn read_artifact_components(
    bundle_root: &Path,
    out_components_dir: &Path,
) -> crate::Result<Vec<MaterializedComponent>> {
    let blobs_dir = bundle_root.join("blobs").join("sha256");
    let index: serde_json::Value =
        serde_json::from_slice(&fs::read(bundle_root.join("index.json"))?)?;
    let mut out = Vec::new();
    if let Some(ms) = index["manifests"].as_array() {
        for m in ms {
            if m["artifactType"].as_str() != Some(AT_COMPONENT) {
                continue;
            }
            let d = m["digest"].as_str().unwrap_or("");
            let mblob = fs::read(blobs_dir.join(d.strip_prefix("sha256:").unwrap_or(d)))?;
            let manifest: serde_json::Value = serde_json::from_slice(&mblob)?;
            if let Some(mut mc) = materialize_component(&manifest, &blobs_dir, out_components_dir)? {
                // The bundle index knows the ref — projects look tasks up by it.
                mc.reference = m["annotations"]["org.opencontainers.image.ref.name"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                out.push(mc);
            }
        }
    }
    Ok(out)
}

/// Detect whether a file is an OCI archive (a tar containing `oci-layout`) —
/// lets `crater apply <source>` route offline bundles vs online specs.
pub fn is_oci_archive(path: &Path) -> bool {
    let Ok(f) = fs::File::open(path) else {
        return false;
    };
    let mut ar = tar::Archive::new(f);
    let Ok(entries) = ar.entries() else {
        return false;
    };
    for e in entries.flatten() {
        if let Ok(p) = e.path() {
            let s = p.to_string_lossy();
            if s == "oci-layout" || s == "./oci-layout" {
                return true;
            }
        }
    }
    false
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
            images: vec![],
            rootfs: vec![],
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

    #[test]
    fn rootfs_layer_round_trips() {
        let tmp = std::env::temp_dir().join(format!("crater-rootfs-test-{}", std::process::id()));
        let stage = BundleStage::new(tmp.join("stage")).unwrap();
        let files = vec![("/usr/local/bin/yq".to_string(), b"BINARY".to_vec(), 0o755)];
        let ir = stage.store_rootfs_layer("yq:rootfs", &files).unwrap();
        // The image manifest resolves to a layer blob that is a tar of the rootfs.
        let layer_digest = stage.layer_of(&ir.manifest_digest).unwrap();
        let layer = read_file(&stage.blob_path(&layer_digest)).unwrap();
        let mut ar = tar::Archive::new(layer.as_slice());
        let entry = ar.entries().unwrap().next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap().to_str().unwrap(), "usr/local/bin/yq");
        assert_eq!(entry.header().mode().unwrap() & 0o777, 0o755);
        let _ = fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod project_tests {
    use super::*;

    /// D-098 round-trip: a bundle holding a project artifact + its task
    /// artifacts. read_artifact_project returns the LOCKED project;
    /// read_artifact_components returns the tasks WITH their refs (the lookup
    /// key plays resolve against) and skips the project entry.
    #[test]
    fn project_bundle_roundtrip() {
        let root = std::env::temp_dir().join(format!("crater-proj-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let stage = BundleStage::new(root.clone()).unwrap();

        let task_recipe = b"name: yq\nactions:\n  - action: shell\n    cmd: \"true\"\n";
        let t = stage
            .store_component_artifact("crater/yq:1.0", "yq", "1.0", "task", task_recipe, &[
                ("bin".into(), false, b"BIN".to_vec()),
            ])
            .unwrap();
        let locked = "name: demo\nplays:\n  - source: crater/yq:1.0\n    hosts: all\n";
        let p = stage.store_project_artifact("crater/demo:latest", "demo", locked.as_bytes()).unwrap();
        stage.write_artifact_index(&[p, t]).unwrap();

        // Project comes back parsed and locked.
        let project = read_artifact_project(&root).unwrap().expect("project artifact");
        assert_eq!(project.name, "demo");
        assert_eq!(project.plays[0].source, "crater/yq:1.0");

        // Tasks come back with reference + blobmap; the project entry is skipped.
        let out = root.join("components");
        let mats = read_artifact_components(&root, &out).unwrap();
        assert_eq!(mats.len(), 1);
        assert_eq!(mats[0].reference, "crater/yq:1.0");
        assert_eq!(mats[0].name, "yq");
        assert!(mats[0].blobmap.contains_key("bin"));
        assert!(out.join("yq").join("component.yaml").is_file());

        // A task-only bundle has no project.
        stage.write_artifact_index(&[stage
            .store_component_artifact("crater/yq:1.0", "yq", "1.0", "task", task_recipe, &[])
            .unwrap()])
        .unwrap();
        assert!(read_artifact_project(&root).unwrap().is_none());

        let _ = std::fs::remove_dir_all(&root);
    }
}
