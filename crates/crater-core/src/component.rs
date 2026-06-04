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
    /// Run this fact-gathering only on hosts holding one of these roles (D-071;
    /// empty = all). E.g. register the kubeadm join command only on the
    /// control-plane so workers don't run `kubeadm token create` (which fails).
    #[serde(default)]
    pub when_role: Vec<String>,
    /// Gather this fact only on the FIRST host matching `when_role` (D-077): the
    /// implicit init node. `kubeadm token create` / `upload-certs` run once there.
    #[serde(default)]
    pub run_once: bool,
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
    /// `file`: URL template fetched online and at build (`{{version}}` etc.).
    #[serde(default)]
    pub url_tmpl: Option<String>,
    /// `file`: task-relative path to a hand-authored local file (e.g.
    /// `files/containerd.service`) — used for content official sources don't
    /// provide (systemd units, drop-ins, configs). `build` reads and packs it as
    /// a blob; `place` pushes it verbatim (copy semantics, no `{{}}` render).
    /// Mutually exclusive with `url_tmpl` (D-066).
    #[serde(default)]
    pub src: Option<std::path::PathBuf>,
    /// `image`: container image reference template (pulled into the OCI bundle).
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    /// `os_package`: OS-family-keyed package name lists (deb vs rpm fork).
    #[serde(default)]
    pub packages: BTreeMap<String, Vec<String>>,
    /// `os_package`: base OS image (e.g. "ubuntu:24.04") that `crater build`
    /// resolves the .deb/.rpm dependency closure in, via buildah (D-062). Pins
    /// this material's offline support to that OS×version (× `arch`).
    #[serde(default)]
    pub base: Option<String>,
    /// Optional content digest (sha256 hex, no prefix). Verified if present.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// The three kinds of material a component can declare (D-034), distinguished by
/// **how the content is acquired** at build time. All three are wired end-to-end:
/// `file` (download an arbitrary file by `url_tmpl` — binaries, tarballs, YAML
/// manifests, configs; arch-variant aware), `image` (pull a container image into
/// a self-contained oci-archive), `os_package` (resolve a .deb/.rpm closure via
/// buildah).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialKind {
    /// Download an arbitrary file via `url_tmpl` (or read a local `src`).
    File,
    Image,
    OsPackage,
}



/// Action primitives — internally tagged by the `action:` key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// OS-family-keyed package lists under `packages:`. Online installs from the
    /// system repo. For OFFLINE, set `material` to a `kind: os_package` material
    /// whose .deb/.rpm closure was packed at build (D-062): offline installs
    /// from the packed closure (`apt-get install ./*.deb` / `dnf install ./*.rpm`).
    #[serde(rename = "package")]
    PkgInstall {
        #[serde(default)]
        packages: BTreeMap<String, Vec<String>>,
        #[serde(default)]
        material: Option<String>,
    },
    #[serde(rename = "unarchive")]
    Extract {
        to: PathBuf,
        /// A path already on the target (e.g. a prior `place`d tmp).
        #[serde(default)]
        from: Option<PathBuf>,
        /// Take a declared archive material directly (D-073): the engine fetches
        /// it (online: download; offline: stream the packed blob to a temp) AND
        /// extracts to `to` in ONE step — no manual `place`-to-/tmp + temp file.
        /// Mutually exclusive with `from`.
        #[serde(default)]
        material: Option<String>,
        /// tar --strip-components value.
        #[serde(default)]
        strip: u32,
        /// Idempotency probe (ansible `unarchive: creates:` style): if this path
        /// already exists, the archive is considered already extracted and the
        /// step is skipped (reported `ok`). Data, not code — the author declares
        /// what the extract produces (e.g. a key binary).
        #[serde(default)]
        creates: Option<PathBuf>,
    },
    /// Render a `kind: file` template material with minijinja (D-075) and write
    /// it to `dst`. The template's bytes travel in the OCI (packed material), so
    /// it's offline-safe; rendering happens on control with the inventory context
    /// (`groups.<role>` etc.), supporting `{% for %}`/`{{ }}`.
    #[serde(rename = "template")]
    RenderTemplate {
        /// A `kind: file` material whose content is the template (e.g. a `.j2`).
        material: String,
        dst: PathBuf,
    },
    /// Run a command through the target's shell (ansible `shell` — pipes, `&&`,
    /// redirects, env prefixes all work).
    #[serde(rename = "shell")]
    RunCmd {
        cmd: String,
        /// Optional idempotency probe (ansible `creates:`-style): if this shell
        /// command exits 0, the target is already in the desired state and `cmd`
        /// is skipped (reported `ok` instead of `changed`). Data, not code.
        #[serde(default)]
        check: Option<String>,
    },
    /// Invoke a reusable role (D-029) — ansible's `role`. `uses` resolves to a
    /// data-defined `roles/<uses>.yaml`; `with` supplies its params. Lowers to a
    /// checked shell op, so it inherits the idempotency contract.
    #[serde(rename = "role")]
    Module {
        uses: String,
        #[serde(default)]
        with: BTreeMap<String, serde_yaml::Value>,
    },
    /// Load one or more `kind: image` materials (D-061). Use `material` for one
    /// or `materials: [..]` for a batch (D-074) — they share one runtime probe
    /// and `namespace`. Online: the runtime pulls each `ref`. Offline: the
    /// control side pushes each packed oci-archive blob and the runtime imports
    /// it. Runtime is NOT assumed — `runtime` if set, else probe nerdctl/ctr/
    /// docker/podman. `namespace` (e.g. `k8s.io`) is passed to ctr/nerdctl.
    LoadImage {
        #[serde(default)]
        material: Option<String>,
        #[serde(default)]
        materials: Vec<String>,
        #[serde(default)]
        namespace: Option<String>,
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
    /// Put a file at `dest` — ansible `copy` (D-037-b / D-068 / D-090). The
    /// source is EXACTLY ONE of:
    ///   `content`  — inline text, `{{var}}`-rendered;
    ///   `src`      — a control-side file under the task dir, inlined into the
    ///                plan so it works under the agent too (text only);
    ///   `material` — a `materials:` entry by logical name (binary-safe,
    ///                arch-variants, D-090 absorbed the former `place`). The
    ///                engine resolves it per mode: online, the target fetches
    ///                the material's `url_tmpl` (or control pushes its `src`);
    ///                offline, the control side pushes the packed blob
    ///                (content-verified).
    /// Written idempotently (sha256) + optional chmod.
    Copy {
        dest: PathBuf,
        #[serde(default)]
        src: Option<String>,
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        material: Option<String>,
        #[serde(default)]
        mode: Option<String>,
    },
    /// Manage a systemd service — ansible `service`/`systemd` (D-037-b / D-069).
    /// Does daemon-reload, then enable/disable + start/stop/restart. Idempotent
    /// for started/stopped/enabled (is-active / is-enabled probes).
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
    /// Manage a container on the target's runtime — a deliberately SLIM
    /// community.docker.docker_container (D-092). Convergence is
    /// fingerprint-based: the engine hashes the rendered spec (image / ports /
    /// volumes / env / command / args / restart_policy) into a `crater.spec`
    /// label; a RUNNING container whose label matches is `ok`, anything else
    /// (absent / crash-looping / any param changed) is replaced (`rm -f` +
    /// `run`). Containers are immutable — no in-place update, and deliberately
    /// no Ansible `comparisons:` per-option diff policy.
    DockerContainer {
        name: String,
        /// Image reference; required for `state: started`.
        #[serde(default)]
        image: Option<String>,
        #[serde(default)]
        state: ContainerState,
        /// docker `--restart`: no | on-failure | always | unless-stopped.
        #[serde(default)]
        restart_policy: Option<String>,
        /// Port mappings, docker `-p` syntax: "HOST:CONTAINER".
        #[serde(default)]
        ports: Vec<String>,
        /// Bind mounts, docker `-v` syntax: "HOST:CONTAINER".
        #[serde(default)]
        volumes: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        /// Command appended after the image (raw).
        #[serde(default)]
        command: Option<String>,
        /// Extra raw `run` flags (escape hatch — data, not logic; counted
        /// into the fingerprint like everything else).
        #[serde(default)]
        args: Vec<String>,
        /// CLI to drive (docker-compatible, e.g. podman); default `docker`.
        #[serde(default)]
        runtime: Option<String>,
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

/// Desired state for `docker_container` (default started).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ContainerState {
    #[default]
    Started,
    Stopped,
    Absent,
}

impl Action {
    /// Short human label for build/deploy summaries.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::PkgInstall { .. } => "package",
            Action::Extract { .. } => "unarchive",
            Action::RenderTemplate { .. } => "template",
            Action::RunCmd { .. } => "shell",
            Action::Module { .. } => "role",
            Action::LoadImage { .. } => "load_image",
            Action::File { .. } => "file",
            Action::Copy { .. } => "copy",
            Action::Service { .. } => "service",
            Action::Lineinfile { .. } => "lineinfile",
            Action::User { .. } => "user",
            Action::Group { .. } => "group",
            Action::DockerContainer { .. } => "docker_container",
        }
    }
}
