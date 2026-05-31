//! Declarative component descriptor (`components/<name>/component.yaml`).
//!
//! A component is a list of action primitives across three phases:
//! `preflight`, `install`, `verify`. Built-in and third-party components
//! share this exact schema — drop a directory under `components/` to add one.
//!
//! Both [`Check`] and [`Action`] are **internally tagged** (`check:` / `action:`).
//! serde_yaml 0.9 represents *externally* tagged enums with `!Tag` YAML tags,
//! which we avoid so the YAML stays plain `key: value`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentDescriptor {
    pub name: String,
    /// User-friendly aliases that resolve to this component (e.g. `k8s` -> `k3s`).
    /// Lives in data, never in engine code: adding an alias means editing this
    /// descriptor, not recompiling crater.
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub version_default: Option<String>,
    #[serde(default)]
    pub supported_os: Vec<String>,
    /// Other components that must be deployed before this one (M3 DAG).
    #[serde(default)]
    pub requires: Vec<String>,
    /// **Material closure** (D-034): every external thing this component needs
    /// packed for offline — binaries, container images, OS packages — declared
    /// explicitly so `crater build` reads it directly instead of scraping
    /// `install` actions (which would miss anything hidden in `run_cmd`). The
    /// install phase references these by logical name via `action: place`; the
    /// engine, not the spec, decides online-fetch vs offline-push. (Named
    /// `materials` — `artifacts` collides with OCI artifacts, `requires` with the
    /// component DAG above.)
    #[serde(default)]
    pub materials: Vec<Material>,
    /// Container images to pack into the offline bundle (D-018 ②). Pulled at
    /// build time into the OCI layout; loaded on the target at deploy. Data, not
    /// code — the engine names no image.
    #[serde(default)]
    pub images: Vec<String>,
    #[serde(default)]
    pub preflight: Vec<Check>,
    #[serde(default)]
    pub install: Vec<Action>,
    #[serde(default)]
    pub verify: Vec<Action>,
    /// Facts to capture on the host after install and expose to other hosts as
    /// `hostvars.<host>.<name>` (D-030) — the basis for cluster join tokens etc.
    #[serde(default)]
    pub register: Vec<RegisterSpec>,
    /// Reserved for offline artifact manifest (M2). Opaque for now.
    #[serde(default)]
    pub offline: Option<serde_yaml::Value>,
}

/// A fact to capture after a component installs on a host: run `cmd`, store its
/// stdout as `hostvars.<host>.<name>` for other hosts to reference (D-030).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RegisterSpec {
    pub name: String,
    pub cmd: String,
}

/// One declared material in a component's offline closure (D-034). The build
/// side reads these to know exactly what to fetch and pack; the install side
/// references them by `name` via `action: place`. `sha256` is optional in
/// source and verified content-addressed at deploy (free from the OCI digest).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Material {
    pub name: String,
    pub kind: MaterialKind,
    /// `binary`: URL template fetched online and at build (`{{version}}` etc.).
    #[serde(default)]
    pub url_tmpl: Option<String>,
    /// `image`: container image reference template (pulled into the OCI bundle).
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    /// `os_package`: OS-family-keyed package name lists (deb vs rpm fork).
    #[serde(default)]
    pub packages: BTreeMap<String, Vec<String>>,
    /// Optional content digest (sha256 hex, no prefix). Verified if present.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// The three kinds of material a component can declare (D-034). Only `binary`
/// is wired end-to-end today (yq closed loop); `image`/`os_package` are the
/// designed next stage for container/OS-dependent components (mysql/docker).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialKind {
    Binary,
    Image,
    OsPackage,
}

impl ComponentDescriptor {
    pub fn from_yaml_file(path: &std::path::Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// Serialize back to YAML (used to stage an inline recipe into a bundle).
    pub fn to_yaml(&self) -> crate::Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }

    /// systemd unit names this component manages (from `SystemdUnit` actions).
    /// Used to derive what to probe in `doctor` — the unit names live in the
    /// component data, not in engine code.
    pub fn systemd_units(&self) -> Vec<String> {
        self.install
            .iter()
            .chain(self.verify.iter())
            .filter_map(|a| match a {
                Action::SystemdUnit { name, .. } => Some(name.clone()),
                _ => None,
            })
            .collect()
    }
}

/// Preflight checks — internally tagged by the `check:` key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum Check {
    PortFree { port: u16 },
    KernelMin { version: String },
    DiskFree { path: String, min_gb: u64 },
}

/// Action primitives — internally tagged by the `action:` key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// OS-family-keyed package lists under `packages:`.
    PkgInstall {
        packages: BTreeMap<String, Vec<String>>,
    },
    Download {
        /// `{{version}}` is substituted at plan time.
        url_tmpl: String,
        #[serde(default)]
        sha256: Option<String>,
        #[serde(default)]
        dest: Option<PathBuf>,
    },
    Extract {
        to: PathBuf,
        #[serde(default)]
        from: Option<PathBuf>,
        /// tar --strip-components value.
        #[serde(default)]
        strip: u32,
    },
    RenderTemplate {
        src: String,
        dst: PathBuf,
    },
    WriteFile {
        dst: PathBuf,
        content: String,
    },
    SystemdUnit {
        name: String,
        #[serde(default)]
        enable: bool,
        #[serde(default)]
        start: bool,
    },
    RunCmd {
        cmd: String,
        /// Optional idempotency probe (ansible `creates:`-style): if this shell
        /// command exits 0, the target is already in the desired state and `cmd`
        /// is skipped (reported `ok` instead of `changed`). Data, not code.
        #[serde(default)]
        check: Option<String>,
    },
    /// Invoke a module (D-029). `uses` resolves to a data-defined module
    /// `modules/<uses>.yaml`; `with` supplies its params. Lowers to a checked
    /// shell op, so it inherits the idempotency contract.
    Module {
        uses: String,
        #[serde(default)]
        with: BTreeMap<String, serde_yaml::Value>,
    },
    /// Place a declared material (D-034) at `dest`, optionally `chmod mode`.
    /// References a `materials:` entry by logical name — NOT a physical URL. The
    /// engine resolves it per mode: online, the target fetches the material's
    /// `url_tmpl`; offline, the control side pushes the packed blob (content-
    /// verified). This is the online/offline-unifying primitive: one spec line,
    /// the source backend decides where the bytes come from.
    Place {
        material: String,
        dest: PathBuf,
        /// chmod mode (e.g. "0755"); folded into the place so a binary lands
        /// executable in one idempotent step (no separate `chmod` run_cmd).
        #[serde(default)]
        mode: Option<String>,
    },
    /// Load/pull a container image. The runtime is NOT assumed: when `runtime`
    /// is set we use it verbatim; otherwise we probe for whatever generic OCI
    /// tool is on the box (nerdctl/docker/podman/ctr). Reserved for offline
    /// image loading (M2+).
    LoadImage {
        reference: String,
        #[serde(default)]
        runtime: Option<String>,
    },
}

impl Action {
    /// Whether this action's effect is **placing files on disk** (so it can be
    /// baked into an OCI rootfs layer at build time, D-018 ②). The rest are
    /// imperative (run a command / touch system state) and can only be replayed
    /// on the target — `crater build --image` carries them as a residual recipe,
    /// never silently dropping them.
    pub fn produces_files(&self) -> bool {
        matches!(
            self,
            Action::Download { .. }
                | Action::Extract { .. }
                | Action::WriteFile { .. }
                | Action::RenderTemplate { .. }
        )
    }

    /// Short human label for build/deploy summaries.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::PkgInstall { .. } => "pkg_install",
            Action::Download { .. } => "download",
            Action::Extract { .. } => "extract",
            Action::RenderTemplate { .. } => "render_template",
            Action::WriteFile { .. } => "write_file",
            Action::SystemdUnit { .. } => "systemd_unit",
            Action::RunCmd { .. } => "run_cmd",
            Action::Place { .. } => "place",
            Action::Module { .. } => "module",
            Action::LoadImage { .. } => "load_image",
        }
    }
}
