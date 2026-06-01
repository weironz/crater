//! Action primitives + supporting types for tasks (D-037/D-046).
//!
//! [`Action`] (internally tagged by `action:`) is the primitive set a task's
//! `actions:` use; [`Material`] (D-034) and [`RegisterSpec`] (D-030) round it
//! out. We use internal serde tags (not `!Tag`) so the YAML stays plain
//! `key: value`. (The old component descriptor / `components/` model was folded
//! into tasks in D-046.)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;


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
    /// CPU arch this variant targets (D-048). `None` = arch-neutral (scripts,
    /// configs, jars; or a single-arch shortcut). Declare several same-named
    /// materials each with a distinct `arch` for a multi-arch binary; `place`
    /// picks the one matching the target host (`uname -m`). A single-arch
    /// binary SHOULD still set `arch` so a mismatched target fails loudly
    /// rather than receiving a wrong-arch binary.
    #[serde(default)]
    pub arch: Option<crate::arch::Arch>,
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



/// Action primitives — internally tagged by the `action:` key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// OS-family-keyed package lists under `packages:`.
    PkgInstall {
        packages: BTreeMap<String, Vec<String>>,
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
    /// Manage a path's state (D-037-b): create a directory, remove a path, or
    /// touch a file — with optional mode/owner/group. Idempotent in the engine.
    File {
        path: PathBuf,
        state: FileState,
        #[serde(default)]
        mode: Option<String>,
        #[serde(default)]
        owner: Option<String>,
        #[serde(default)]
        group: Option<String>,
    },
    /// Copy a control-side file to the target (D-037-b). `src` is resolved
    /// relative to the component/task dir; the content is inlined into the plan
    /// (so it works under the agent too), written idempotently (sha256), chmod.
    Copy {
        src: String,
        dest: PathBuf,
        #[serde(default)]
        mode: Option<String>,
    },
    /// Manage a systemd service (D-037-b): a generalization of `systemd_unit`
    /// adding stop/restart. Idempotent for started/stopped (probe is-active).
    Service {
        name: String,
        #[serde(default)]
        state: Option<ServiceState>,
        #[serde(default)]
        enabled: Option<bool>,
    },
    /// Ensure a single line is present/absent in a file (D-037-b). Idempotent in
    /// the engine via a grep probe. `regexp` (if set) matches the line to
    /// replace; `create` makes the file if missing.
    Lineinfile {
        path: PathBuf,
        line: String,
        #[serde(default)]
        regexp: Option<String>,
        #[serde(default)]
        state: Presence,
        #[serde(default)]
        create: bool,
    },
    /// Ensure a system user exists/absent (D-037-b). Idempotent (`id` probe).
    User {
        name: String,
        #[serde(default)]
        state: Presence,
        #[serde(default)]
        system: bool,
        #[serde(default)]
        shell: Option<String>,
        #[serde(default)]
        home: Option<String>,
        #[serde(default)]
        groups: Vec<String>,
    },
    /// Ensure a system group exists/absent (D-037-b). Idempotent (`getent`).
    Group {
        name: String,
        #[serde(default)]
        state: Presence,
        #[serde(default)]
        system: bool,
    },
}

/// Desired state for the `file` primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileState {
    /// `mkdir -p` the path (idempotent: skip if already a directory).
    Directory,
    /// `rm -rf` the path (idempotent: skip if it doesn't exist).
    Absent,
    /// Ensure the file exists (`touch`; idempotent: skip if present).
    Touch,
}

/// Desired runtime state for the `service` primitive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    Started,
    Stopped,
    Restarted,
}

/// Present/absent state, shared by `lineinfile`/`user`/`group` (default present).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    #[default]
    Present,
    Absent,
}

impl Action {
    /// Short human label for build/deploy summaries.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::PkgInstall { .. } => "pkg_install",
            Action::Extract { .. } => "extract",
            Action::RenderTemplate { .. } => "render_template",
            Action::WriteFile { .. } => "write_file",
            Action::SystemdUnit { .. } => "systemd_unit",
            Action::RunCmd { .. } => "run_cmd",
            Action::Place { .. } => "place",
            Action::Module { .. } => "module",
            Action::LoadImage { .. } => "load_image",
            Action::File { .. } => "file",
            Action::Copy { .. } => "copy",
            Action::Service { .. } => "service",
            Action::Lineinfile { .. } => "lineinfile",
            Action::User { .. } => "user",
            Action::Group { .. } => "group",
        }
    }
}
