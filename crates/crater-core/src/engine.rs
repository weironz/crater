//! Plan builder + executor.
//!
//! `plan_from_task` lowers a task's `actions` into an ordered list of [`Op`]s
//! (control flow — when-filter, needs-ordering — all in Rust, D-036). `execute`
//! / `execute_task` run them against any [`Executor`] (local or SSH); the agent
//! runs the same ops on the target.
//!
//! Offline mode: when [`PlanContext::offline_blobs`] is set, `place` actions
//! become [`Op::PushFile`] (push a pre-fetched blob) instead of `curl`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::component::Action;
use crate::executor::Executor;
use crate::os::OsFamily;
use crate::source::OnlineSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Phase {
    // Serialize as PascalCase (keeps the lowered-plan wire format stable for the
    // self-bootstrap agent); `alias` lets task YAML use lowercase `phase: verify`.
    #[serde(alias = "preflight")]
    Preflight,
    #[default]
    #[serde(alias = "install")]
    Install,
    #[serde(alias = "verify")]
    Verify,
}

/// Per-step outcome, reported ansible-style so re-runs are legible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// Already in the desired state (or a passing read-only check) — no change.
    Ok,
    /// The step mutated the target.
    Changed,
    /// A soft (non-fatal) failure — surfaced but execution continues.
    Warn,
}

impl StepStatus {
    pub fn tag(&self) -> &'static str {
        match self {
            StepStatus::Ok => "ok",
            StepStatus::Changed => "changed",
            StepStatus::Warn => "warn",
        }
    }
}

/// One image in an [`Op::ImageImport`] batch (D-074): a reference plus, offline,
/// the control-side oci-archive blob to push+import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageItem {
    pub reference: String,
    #[serde(default)]
    pub local_archive: Option<PathBuf>,
}

/// A concrete, executor-dispatchable operation.
///
/// Serializable so a lowered plan can be shipped to a target and run there by
/// the self-bootstrap agent (`crater agent --plan`, D-019).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Op {
    /// Run a shell command on the target.
    Shell {
        phase: Phase,
        describe: String,
        cmd: String,
        /// If true, a non-zero exit is a warning, not a hard failure
        /// (used by preflight checks so we surface issues without aborting).
        soft_fail: bool,
        /// Idempotency probe for `Install` steps: if set and it exits 0, the
        /// target is already in the desired state, so `cmd` is skipped and the
        /// step reports `ok` instead of `changed`. Ignored for other phases.
        check: Option<String>,
    },
    /// Write a file (rendered template or inline content) on the target.
    WriteFile {
        phase: Phase,
        describe: String,
        path: String,
        content: String,
        /// Optional chmod mode applied after writing (e.g. "0644", D-037-b copy).
        #[serde(default)]
        mode: Option<String>,
    },
    /// Push a pre-fetched local blob (offline bundle) to the target.
    PushFile {
        phase: Phase,
        describe: String,
        local_path: PathBuf,
        dest: String,
        /// Optional chmod mode applied after the push (e.g. "0755"), so a
        /// `place`d binary lands executable in one step (D-034).
        #[serde(default)]
        mode: Option<String>,
    },
    /// Load one or more container images into the target's runtime (D-061/074).
    /// Each [`ImageItem`]: offline → `local_archive` (pushed + `import`ed),
    /// online → `reference` pulled. One runtime probe + `namespace` for the
    /// whole batch. Runtime is probed unless `runtime` is set.
    ImageImport {
        phase: Phase,
        describe: String,
        images: Vec<ImageItem>,
        #[serde(default)]
        namespace: Option<String>,
        #[serde(default)]
        runtime: Option<String>,
    },
    /// Install OS packages (D-062). Online: install `packages` from the system
    /// repo. Offline: `local_archive` is a control-side tar of the .deb/.rpm
    /// dependency closure (built via buildah) — pushed, extracted, and installed
    /// locally (`apt-get install ./*.deb` / `dnf install ./*.rpm`).
    PackageInstall {
        phase: Phase,
        describe: String,
        packages: Vec<String>,
        /// "debian" | "rhel" — picks apt vs dnf.
        family: String,
        #[serde(default)]
        local_archive: Option<PathBuf>,
        #[serde(default)]
        check: Option<String>,
    },
    /// Fetch an archive material and extract it to `to` in ONE step (D-073).
    /// Offline: `local_archive` is the control-side blob (streamed to a temp,
    /// extracted, removed). Online: `url` is downloaded on the target. Idempotent
    /// when `creates` is set (skip — and skip the fetch — if it already exists).
    UnarchiveMaterial {
        phase: Phase,
        describe: String,
        to: String,
        strip: u32,
        #[serde(default)]
        creates: Option<String>,
        #[serde(default)]
        local_archive: Option<PathBuf>,
        #[serde(default)]
        url: Option<String>,
    },
}

impl Op {
    pub fn phase(&self) -> Phase {
        match self {
            Op::Shell { phase, .. }
            | Op::WriteFile { phase, .. }
            | Op::PushFile { phase, .. }
            | Op::ImageImport { phase, .. }
            | Op::UnarchiveMaterial { phase, .. }
            | Op::PackageInstall { phase, .. } => *phase,
        }
    }
    pub fn describe(&self) -> &str {
        match self {
            Op::Shell { describe, .. }
            | Op::WriteFile { describe, .. }
            | Op::PushFile { describe, .. }
            | Op::ImageImport { describe, .. }
            | Op::UnarchiveMaterial { describe, .. }
            | Op::PackageInstall { describe, .. } => describe,
        }
    }
    /// Shell command / path preview for dry-run, if any.
    pub fn preview(&self) -> Option<&str> {
        match self {
            Op::Shell { cmd, .. } => Some(cmd),
            Op::WriteFile { path, .. } => Some(path),
            Op::PushFile { dest, .. } => Some(dest),
            Op::ImageImport { describe, .. } => Some(describe),
            Op::UnarchiveMaterial { to, .. } => Some(to),
            Op::PackageInstall { describe, .. } => Some(describe),
        }
    }
}

#[derive(Clone)]
pub struct PlanContext {
    pub os: OsFamily,
    /// Target CPU arch (D-048), detected per host via `uname -m`. Drives
    /// per-arch material selection in `place`.
    pub target_arch: crate::arch::Arch,
    pub version: String,
    /// Component directory (for resolving template sources).
    pub component_dir: PathBuf,
    /// Template variables (`version` plus user params).
    pub vars: BTreeMap<String, String>,
    /// Mirror rewriter for material URLs (online mode, China-friendly).
    pub source: OnlineSource,
    /// Offline mode: maps a material key -> local pre-fetched blob, so `place`
    /// resolves the packed blob (D-034). Key is the material name, or
    /// `name@arch` for an arch-specific variant (D-048).
    pub offline_blobs: Option<BTreeMap<String, PathBuf>>,
    /// Declared materials by name (D-034), each entry holding the per-arch
    /// variants (D-048); `place` picks the one matching `target_arch`.
    pub materials: BTreeMap<String, Vec<crate::component::Material>>,
    /// Directory holding data-defined roles (`roles/<name>.yaml`, D-029).
    pub roles_dir: PathBuf,
    /// The target host's inventory roles (D-071), used by `when_role` step
    /// filtering. Empty = host has no roles (matches every `when_role`).
    pub host_roles: Vec<String>,
    /// Role → its member hosts as (name, address) (D-075), exposed to the
    /// `template` action's minijinja context as `groups.<role>` = list of
    /// `{name, ip}`. Drives declarative iteration (e.g. haproxy server list).
    pub groups: BTreeMap<String, Vec<(String, String)>>,
}

impl PlanContext {
    pub fn new(os: OsFamily, version: String, component_dir: PathBuf) -> Self {
        let mut vars = BTreeMap::new();
        vars.insert("version".to_string(), version.clone());
        Self {
            os,
            target_arch: crate::arch::Arch::Unknown,
            version,
            component_dir,
            vars,
            source: OnlineSource::with_default_mirrors(),
            offline_blobs: None,
            materials: BTreeMap::new(),
            roles_dir: PathBuf::from("roles"),
            host_roles: Vec::new(),
            groups: BTreeMap::new(),
        }
    }

    /// Switch this context into offline mode with a blob map.
    pub fn with_offline(mut self, blobs: BTreeMap<String, PathBuf>) -> Self {
        self.offline_blobs = Some(blobs);
        self
    }

    /// Register a declared material under its name (D-048: same-named variants
    /// accumulate so `place` can pick by arch).
    pub fn add_material(&mut self, m: crate::component::Material) {
        self.materials.entry(m.name.clone()).or_default().push(m);
    }

    /// Resolve a `place` material reference to the variant matching the target
    /// arch (D-048). Rule: a variant whose `arch` equals the target wins; else
    /// an arch-neutral (`arch: None`) variant; else it's an error — packaged for
    /// the wrong arch, which must fail loudly (esp. offline/air-gap).
    pub fn resolve_material(&self, name: &str) -> crate::Result<&crate::component::Material> {
        use crate::arch::Arch;
        let variants = self.materials.get(name).filter(|v| !v.is_empty()).ok_or_else(|| {
            anyhow::anyhow!("place: unknown material '{name}' (declare it under `materials:`)")
        })?;
        // Exact arch match first.
        let exact: Vec<&crate::component::Material> = variants
            .iter()
            .filter(|m| m.arch == Some(self.target_arch))
            .collect();
        if exact.len() == 1 {
            return Ok(exact[0]);
        }
        if exact.len() > 1 {
            anyhow::bail!("place: material '{name}' has duplicate variants for arch {}", self.target_arch.as_str());
        }
        // Fall back to an arch-neutral variant.
        let neutral: Vec<&crate::component::Material> =
            variants.iter().filter(|m| m.arch.is_none()).collect();
        match neutral.len() {
            1 => Ok(neutral[0]),
            0 => {
                // Distinguish "wrong arch" from "no arch info" for a clear message.
                if self.target_arch == Arch::Unknown {
                    anyhow::bail!(
                        "place: material '{name}' is arch-specific but the target arch is unknown (uname -m failed)"
                    )
                }
                anyhow::bail!(
                    "place: material '{name}' is not packaged for arch {} (declared: {})",
                    self.target_arch.as_str(),
                    variants
                        .iter()
                        .map(|m| m.arch.map(|a| a.as_str()).unwrap_or("neutral"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
            _ => anyhow::bail!("place: material '{name}' has multiple arch-neutral variants (ambiguous)"),
        }
    }

    /// The offline blob key for a resolved material (D-048): plain name for an
    /// arch-neutral material, `name@arch` for an arch-specific variant. Build
    /// annotates layers by the same key.
    pub fn material_blob_key(m: &crate::component::Material) -> String {
        match m.arch {
            Some(a) => format!("{}@{}", m.name, a.as_str()),
            None => m.name.clone(),
        }
    }

    /// The rendered URL for a material's `url_tmpl` (online fetch / build).
    /// Offline mode skips mirror rewrite (the raw URL is the manifest key).
    pub fn rendered_url(&self, url_tmpl: &str) -> crate::Result<String> {
        let raw = render(url_tmpl, &self.vars)?;
        Ok(if self.offline_blobs.is_some() {
            raw
        } else {
            self.source.rewrite(&raw)
        })
    }
}



/// A lowered task step: the Op plus its run policy (D-037-b). Serializable so the
/// whole task plan can be shipped to the self-bootstrap agent (D-044).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskStep {
    pub op: Op,
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub ignore_errors: bool,
    #[serde(default)]
    pub notify: Vec<String>,
}

/// A serializable task plan (steps + handlers) for the agent (D-044): the
/// control plane renders+lowers it, the target's `crater agent --task-plan`
/// runs `execute_task` locally — same agent model as the component plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskPlan {
    pub steps: Vec<TaskStep>,
    #[serde(default)]
    pub handlers: BTreeMap<String, Op>,
}

pub fn task_plan_to_yaml(
    steps: &[TaskStep],
    handlers: &BTreeMap<String, Op>,
) -> crate::Result<String> {
    let plan = TaskPlan {
        steps: steps.to_vec(),
        handlers: handlers.clone(),
    };
    Ok(serde_yaml::to_string(&plan)?)
}

pub fn task_plan_from_yaml(text: &str) -> crate::Result<TaskPlan> {
    Ok(serde_yaml::from_str(text)?)
}

/// Build an executable plan from a task's `actions` (D-037). **All control flow
/// lives here in Rust** (D-036): `when_os`/`when_offline` filter steps, `needs`
/// topologically orders them; the YAML only declared primitives + data.
pub fn plan_from_task(
    actions: &[crate::task::ActionStep],
    ctx: &PlanContext,
) -> crate::Result<Vec<TaskStep>> {
    let offline = ctx.offline_blobs.is_some();
    let os = ctx.os;

    // 1) Filter by the closed-enum conditions (NOT free expressions, D-036).
    //    A step's stable id is its declared `id`, else `action<index>`.
    struct Item<'a> {
        id: String,
        step: &'a crate::task::ActionStep,
    }
    let mut items: Vec<Item> = Vec::new();
    let mut filtered: BTreeSet<String> = BTreeSet::new();
    for (i, s) in actions.iter().enumerate() {
        let id = s.id.clone().unwrap_or_else(|| format!("action{i}"));
        let os_ok = s.when_os.is_empty()
            || os.match_keys().iter().any(|k| s.when_os.iter().any(|w| w == k));
        let off_ok = s.when_offline.map_or(true, |w| w == offline);
        // when_role (D-071): run only on hosts holding one of these roles.
        let role_ok = s.when_role.is_empty()
            || s.when_role.iter().any(|r| ctx.host_roles.iter().any(|h| h == r));
        if os_ok && off_ok && role_ok {
            items.push(Item { id, step: s });
        } else {
            filtered.insert(id);
        }
    }

    // 2) Topologically order by `needs`. A need pointing at a filtered-out step
    //    is treated as satisfied (its precondition doesn't apply); a need on a
    //    truly-undefined id is a hard error (catch typos loud).
    let active: BTreeSet<String> = items.iter().map(|it| it.id.clone()).collect();
    for it in &items {
        for n in &it.step.needs {
            if !active.contains(n) && !filtered.contains(n) {
                anyhow::bail!("action '{}' needs '{n}', which is not defined", it.id);
            }
        }
    }
    let nodes: Vec<crate::dag::DepNode> = items
        .iter()
        .map(|it| crate::dag::DepNode {
            name: it.id.clone(),
            requires: it
                .step
                .needs
                .iter()
                .filter(|n| active.contains(*n))
                .cloned()
                .collect(),
        })
        .collect();
    let order = crate::dag::topo_sort(&nodes)?;

    // 3) Lower each step to an Op (+ its run policy) in dependency order.
    let by_id: BTreeMap<&str, &Item> = items.iter().map(|it| (it.id.as_str(), it)).collect();
    let mut steps = Vec::new();
    for id in order {
        let it = by_id[id.as_str()];
        steps.push(TaskStep {
            op: action_op(it.step.phase, &it.step.action, ctx)?,
            retries: it.step.retries,
            ignore_errors: it.step.ignore_errors,
            notify: it.step.notify.clone(),
        });
    }
    Ok(steps)
}

/// Lower a task's handlers (D-037-b) to `id -> Op`. Each handler needs an `id`
/// (the name a step `notify`s).
pub fn plan_handlers(
    handlers: &[crate::task::ActionStep],
    ctx: &PlanContext,
) -> crate::Result<BTreeMap<String, Op>> {
    let mut out = BTreeMap::new();
    for h in handlers {
        let id = h
            .id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("handler needs an `id` (a step notifies it by id)"))?;
        out.insert(id, action_op(h.phase, &h.action, ctx)?);
    }
    Ok(out)
}

/// Run one Op with a retry/ignore-errors policy (D-037-b). `total == 0` marks a
/// handler (logged differently). Returns the final status.
async fn run_one(
    op: &Op,
    exec: &dyn Executor,
    n: usize,
    total: usize,
    retries: u32,
    ignore_errors: bool,
) -> crate::Result<StepStatus> {
    let mut attempt = 0u32;
    loop {
        match exec_one(op, exec, n).await {
            Ok((status, surfaced)) => {
                let line = if total == 0 {
                    format!("  ⤷ handler {} → {}", op.describe(), paint_status(status))
                } else {
                    format!("[{n}/{total}] {} → {}", op.describe(), paint_status(status))
                };
                match status {
                    StepStatus::Warn => tracing::warn!("{line}"),
                    _ => tracing::info!("{line}"),
                }
                for l in surfaced {
                    tracing::info!("      {l}");
                }
                return Ok(status);
            }
            Err(e) => {
                if attempt < retries {
                    attempt += 1;
                    tracing::warn!("[{n}/{total}] {} failed, retry {attempt}/{retries}: {e}", op.describe());
                    continue;
                }
                if ignore_errors {
                    tracing::warn!("[{n}/{total}] {} failed (ignore_errors): {e}", op.describe());
                    return Ok(StepStatus::Warn);
                }
                return Err(e);
            }
        }
    }
}

/// Execute a task plan from the control plane (D-037-b): each step with its
/// retry/ignore-errors policy; `changed` steps queue their `notify` handlers,
/// which run once (deduped, in order) at the end.
pub async fn execute_task(
    steps: &[TaskStep],
    handlers: &BTreeMap<String, Op>,
    exec: &dyn Executor,
) -> crate::Result<()> {
    let total = steps.len();
    let (mut n_ok, mut n_changed, mut n_warn) = (0u32, 0u32, 0u32);
    let mut notified: Vec<String> = Vec::new();
    for (i, st) in steps.iter().enumerate() {
        let status = run_one(&st.op, exec, i + 1, total, st.retries, st.ignore_errors).await?;
        match status {
            StepStatus::Ok => n_ok += 1,
            StepStatus::Changed => {
                n_changed += 1;
                for h in &st.notify {
                    if !notified.contains(h) {
                        notified.push(h.clone());
                    }
                }
            }
            StepStatus::Warn => n_warn += 1,
        }
    }
    // Run notified handlers once, in notify order (ansible semantics).
    for hid in &notified {
        match handlers.get(hid) {
            Some(op) => {
                let status = run_one(op, exec, 0, 0, 0, false).await?;
                match status {
                    StepStatus::Ok => n_ok += 1,
                    StepStatus::Changed => n_changed += 1,
                    StepStatus::Warn => n_warn += 1,
                }
            }
            None => tracing::warn!("notify: no handler with id '{hid}'"),
        }
    }
    tracing::info!(
        "done on {}: changed={n_changed} ok={n_ok} warn={n_warn} ({total} step(s){})",
        exec.label(),
        if notified.is_empty() {
            String::new()
        } else {
            format!(", {} handler(s)", notified.len())
        }
    );
    Ok(())
}



fn action_op(phase: Phase, a: &Action, ctx: &PlanContext) -> crate::Result<Op> {
    let op = match a {
        Action::PkgInstall { packages, material } => {
            // Resolve package list: the referenced os_package material's, else inline.
            let pkgs = if let Some(name) = material {
                let m = ctx.resolve_material(name)?;
                pick_packages(&m.packages, ctx.os)
            } else {
                pick_packages(packages, ctx.os)
            };
            let family = match ctx.os {
                OsFamily::Debian => "debian",
                OsFamily::Rhel => "rhel",
                OsFamily::Unknown => "unknown",
            };
            let check = match ctx.os {
                OsFamily::Debian => Some(format!("dpkg -s {} >/dev/null 2>&1", pkgs.join(" "))),
                OsFamily::Rhel => Some(format!("rpm -q {} >/dev/null 2>&1", pkgs.join(" "))),
                OsFamily::Unknown => None,
            };
            // Offline + an os_package material → install from the packed closure.
            let local_archive = match (material, &ctx.offline_blobs) {
                (Some(name), Some(blobs)) => {
                    let m = ctx.resolve_material(name)?;
                    let key = PlanContext::material_blob_key(m);
                    Some(
                        blobs
                            .get(&key)
                            .or_else(|| blobs.get(name))
                            .ok_or_else(|| anyhow::anyhow!("offline bundle missing os_package '{key}'"))?
                            .clone(),
                    )
                }
                _ => None,
            };
            Op::PackageInstall {
                phase,
                describe: format!("install packages: [{}]", pkgs.join(", ")),
                packages: pkgs,
                family: family.to_string(),
                local_archive,
                check,
            }
        }
        Action::Place { material, dest, mode } => {
            let dest = dest.display().to_string();
            // Pick the variant matching the target arch (D-048), online or off.
            let m = ctx.resolve_material(material)?;
            if let Some(blobs) = &ctx.offline_blobs {
                // Offline: push the packed blob, keyed by name (or name@arch).
                let key = PlanContext::material_blob_key(m);
                let local = blobs
                    .get(&key)
                    .or_else(|| blobs.get(material)) // legacy single-arch packs
                    .ok_or_else(|| anyhow::anyhow!("offline bundle missing material '{key}'"))?;
                Op::PushFile {
                    phase,
                    describe: format!("place (offline) {key} -> {dest}"),
                    local_path: local.clone(),
                    dest,
                    mode: mode.clone(),
                }
            } else if let Some(src) = &m.src {
                // Online + hand-authored local file (D-066): push it from the
                // control machine's task dir (no URL to curl). Copy semantics.
                let local = ctx.component_dir.join(src);
                Op::PushFile {
                    phase,
                    describe: format!("place {material} <- {}", local.display()),
                    local_path: local,
                    dest,
                    mode: mode.clone(),
                }
            } else {
                // Online: the target fetches the variant's declared URL itself.
                let tmpl = m.url_tmpl.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("place: material '{material}' has no url_tmpl or src for online fetch")
                })?;
                // {{arch}} = the resolved material's arch (single source, D-064).
                let raw = if let Some(a) = m.arch {
                    let mut vars = ctx.vars.clone();
                    vars.insert("arch".to_string(), a.as_str().to_string());
                    render(tmpl, &vars)?
                } else {
                    render(tmpl, &ctx.vars)?
                };
                let url = if ctx.offline_blobs.is_some() { raw } else { ctx.source.rewrite(&raw) };
                let mut cmd = format!("curl -fL --retry 3 -o '{dest}' '{url}'");
                if let Some(mode) = mode {
                    cmd.push_str(&format!(" && chmod {mode} '{dest}'"));
                }
                Op::Shell {
                    phase,
                    describe: format!("place {material} <- {url}"),
                    cmd,
                    soft_fail: false,
                    check: Some(format!("test -s '{dest}'")),
                }
            }
        }
        Action::Extract { to, from, material, strip, creates } => {
            let to_s = to.display().to_string();
            let creates_s = creates.as_ref().map(|p| p.display().to_string());
            if let Some(name) = material {
                // D-073: fetch the material AND extract in one step. Offline → the
                // packed blob; online → its url_tmpl (arch-injected like `place`).
                let m = ctx.resolve_material(name)?;
                let (local_archive, url) = if let Some(blobs) = &ctx.offline_blobs {
                    let key = PlanContext::material_blob_key(m);
                    let local = blobs
                        .get(&key)
                        .or_else(|| blobs.get(name))
                        .ok_or_else(|| anyhow::anyhow!("offline bundle missing material '{key}'"))?
                        .clone();
                    (Some(local), None)
                } else {
                    let tmpl = m.url_tmpl.as_deref().ok_or_else(|| {
                        anyhow::anyhow!("unarchive: material '{name}' has no url_tmpl for online fetch")
                    })?;
                    let raw = if let Some(a) = m.arch {
                        let mut vars = ctx.vars.clone();
                        vars.insert("arch".to_string(), a.as_str().to_string());
                        render(tmpl, &vars)?
                    } else {
                        render(tmpl, &ctx.vars)?
                    };
                    (None, Some(ctx.source.rewrite(&raw)))
                };
                Op::UnarchiveMaterial {
                    phase,
                    describe: format!("unarchive {name} -> {to_s}"),
                    to: to_s,
                    strip: *strip,
                    creates: creates_s,
                    local_archive,
                    url,
                }
            } else {
                let src = from
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "/tmp/crater-dl".to_string());
                Op::Shell {
                    phase,
                    describe: format!("extract {src} -> {to_s}"),
                    cmd: format!(
                        "mkdir -p '{to_s}' && tar -xf '{src}' --strip-components={strip} -C '{to_s}'"
                    ),
                    soft_fail: false,
                    // Idempotent when the author declares `creates:` (the extract's
                    // expected product); else re-extract every run (overwrite-safe).
                    check: creates_s.map(|c| format!("test -e '{c}'")),
                }
            }
        }
        Action::RenderTemplate { material, dst } => {
            // D-075: the template is a packed `kind: file` material. Read its bytes
            // on control (offline → the packed blob; online → its `src` on disk),
            // render with minijinja + inventory context, lower to a plain WriteFile
            // (so it ships in the recipe and works offline).
            let m = ctx.resolve_material(material)?;
            let raw = if let Some(blobs) = &ctx.offline_blobs {
                let key = PlanContext::material_blob_key(m);
                let path = blobs
                    .get(&key)
                    .or_else(|| blobs.get(material))
                    .ok_or_else(|| anyhow::anyhow!("offline bundle missing template material '{key}'"))?;
                std::fs::read_to_string(path)
                    .map_err(|e| anyhow::anyhow!("read template blob {}: {e}", path.display()))?
            } else {
                let src = m.src.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("template: material '{material}' has no `src` (a local .j2)")
                })?;
                let path = ctx.component_dir.join(src);
                std::fs::read_to_string(&path)
                    .map_err(|e| anyhow::anyhow!("read template {}: {e}", path.display()))?
            };
            Op::WriteFile {
                phase,
                describe: format!("render template {material} -> {}", dst.display()),
                path: dst.display().to_string(),
                content: render_template_str(&raw, ctx)?,
                mode: None,
            }
        }
        Action::RunCmd { cmd, check } => Op::Shell {
            phase,
            describe: format!("run: {cmd}"),
            cmd: render(cmd, &ctx.vars)?,
            soft_fail: false,
            // Author-supplied idempotency probe (data), rendered like the cmd.
            check: check.as_ref().map(|c| render(c, &ctx.vars)).transpose()?,
        },
        Action::Module { uses, with } => {
            // Data-defined role (D-029): load roles/<uses>.yaml, render its
            // check/act with `with` (+ ctx vars), lower to a checked shell op.
            let path = ctx.roles_dir.join(format!("{uses}.yaml"));
            let desc = crate::module::ModuleDescriptor::from_yaml_file(&path)?;
            let with_strings: BTreeMap<String, String> = with
                .iter()
                .map(|(k, v)| (k.clone(), yaml_value_to_string(v)))
                .collect();
            desc.check_params(uses, &with_strings)?;
            let mut vars = ctx.vars.clone();
            vars.extend(with_strings);
            Op::Shell {
                phase,
                describe: format!("module {uses}"),
                cmd: render(&desc.act, &vars)?,
                soft_fail: false,
                check: desc.check.as_ref().map(|c| render(c, &vars)).transpose()?,
            }
        }
        Action::LoadImage { material, materials, namespace, runtime } => {
            // Resolve one or more kind:image materials (D-061/074): online →
            // runtime pulls each `ref`; offline → import each packed oci-archive.
            let names: Vec<&String> = material.iter().chain(materials.iter()).collect();
            if names.is_empty() {
                anyhow::bail!("load_image: needs `material` or `materials`");
            }
            let mut images = Vec::with_capacity(names.len());
            for name in &names {
                let m = ctx.resolve_material(name)?;
                let reference = m.reference.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("load_image: material '{name}' has no `ref` (kind: image)")
                })?;
                let reference = render(reference, &ctx.vars)?;
                let local_archive = if let Some(blobs) = &ctx.offline_blobs {
                    let key = PlanContext::material_blob_key(m);
                    Some(
                        blobs
                            .get(&key)
                            .or_else(|| blobs.get(name.as_str()))
                            .ok_or_else(|| anyhow::anyhow!("offline bundle missing image material '{key}'"))?
                            .clone(),
                    )
                } else {
                    None
                };
                images.push(ImageItem { reference, local_archive });
            }
            let describe = if images.len() == 1 {
                format!("load image {}", images[0].reference)
            } else {
                format!("load {} images", images.len())
            };
            Op::ImageImport {
                phase,
                describe,
                images,
                namespace: namespace.clone(),
                runtime: runtime.clone(),
            }
        }
        Action::File {
            path,
            state,
            mode,
            owner,
            group,
        } => {
            use crate::component::FileState;
            let p = path.display().to_string();
            let chmod = mode
                .as_ref()
                .map(|m| format!(" && chmod {m} '{p}'"))
                .unwrap_or_default();
            let chown = match (owner, group) {
                (Some(o), Some(g)) => format!(" && chown {o}:{g} '{p}'"),
                (Some(o), None) => format!(" && chown {o} '{p}'"),
                (None, Some(g)) => format!(" && chgrp {g} '{p}'"),
                (None, None) => String::new(),
            };
            let (describe, cmd, check) = match state {
                FileState::Directory => (
                    format!("dir {p}"),
                    format!("mkdir -p '{p}'{chmod}{chown}"),
                    Some(format!("test -d '{p}'")),
                ),
                FileState::Absent => (
                    format!("remove {p}"),
                    format!("rm -rf '{p}'"),
                    Some(format!("test ! -e '{p}'")),
                ),
                FileState::Touch => (
                    format!("touch {p}"),
                    format!("touch '{p}'{chmod}{chown}"),
                    Some(format!("test -e '{p}'")),
                ),
            };
            Op::Shell {
                phase,
                describe,
                cmd,
                soft_fail: false,
                check,
            }
        }
        Action::Copy { dest, src, content, mode } => {
            // ansible `copy`: inline `content` OR a control-side `src` file
            // (read + inlined so it works under the agent, which can't reach
            // control-side paths — text only; binaries go through `place`).
            let content = match (content.as_deref(), src.as_deref()) {
                (Some(c), None) => render(c, &ctx.vars)?,
                (None, Some(s)) => {
                    let src_path = ctx.component_dir.join(s);
                    let bytes = std::fs::read(&src_path)
                        .map_err(|e| anyhow::anyhow!("copy: read {}: {e}", src_path.display()))?;
                    String::from_utf8(bytes).map_err(|_| {
                        anyhow::anyhow!(
                            "copy: {} is not UTF-8 (use `place` for binaries)",
                            src_path.display()
                        )
                    })?
                }
                (Some(_), Some(_)) => {
                    return Err(anyhow::anyhow!("copy: set either `content` or `src`, not both"))
                }
                (None, None) => return Err(anyhow::anyhow!("copy: needs `content` or `src`")),
            };
            let describe = match src.as_deref() {
                Some(s) => format!("copy {s} -> {}", dest.display()),
                None => format!("write file {}", dest.display()),
            };
            Op::WriteFile {
                phase,
                describe,
                path: dest.display().to_string(),
                content,
                mode: mode.clone(),
            }
        }
        Action::Service {
            name,
            state,
            enabled,
        } => {
            use crate::component::ServiceState;
            let mut cmds = vec!["systemctl daemon-reload".to_string()];
            match enabled {
                Some(true) => cmds.push(format!("systemctl enable {name}")),
                Some(false) => cmds.push(format!("systemctl disable {name}")),
                None => {}
            }
            let mut probes = Vec::new();
            match state {
                Some(ServiceState::Started) => {
                    cmds.push(format!("systemctl start {name}"));
                    probes.push(format!("systemctl is-active --quiet {name}"));
                }
                Some(ServiceState::Stopped) => {
                    cmds.push(format!("systemctl stop {name}"));
                    probes.push(format!("! systemctl is-active --quiet {name}"));
                }
                // restart is never "already done" — always runs (reports changed).
                Some(ServiceState::Restarted) => cmds.push(format!("systemctl restart {name}")),
                None => {}
            }
            // The skip-check gates on the desired STATE only (is-active), NOT on
            // is-enabled: a stopped-but-enabled service must still get started, and
            // a `restarted` must always run. (Bug D-075b: an is-enabled probe here
            // skipped the whole step — incl. start/restart — when already enabled.)
            let check = if probes.is_empty() {
                None
            } else {
                Some(probes.join(" && "))
            };
            Op::Shell {
                phase,
                describe: format!("service {name}"),
                cmd: cmds.join(" && "),
                soft_fail: false,
                check,
            }
        }
        Action::Lineinfile {
            path,
            line,
            regexp,
            state,
            create,
        } => {
            use crate::component::Presence;
            let p = path.display().to_string();
            // line/regexp enter the shell single-quoted (assumed no `'`).
            match state {
                Presence::Present => {
                    let pre = if *create {
                        format!("mkdir -p \"$(dirname '{p}')\" && touch '{p}'; ")
                    } else {
                        String::new()
                    };
                    // On a check miss: drop any regexp-matching line, then append.
                    let cmd = match regexp {
                        Some(re) => format!(
                            "{pre}sed -i -E '\\|{re}|d' '{p}' 2>/dev/null; printf '%s\\n' '{line}' >> '{p}'"
                        ),
                        None => format!("{pre}printf '%s\\n' '{line}' >> '{p}'"),
                    };
                    Op::Shell {
                        phase,
                        describe: format!("lineinfile {p}"),
                        cmd,
                        soft_fail: false,
                        check: Some(format!("grep -qxF '{line}' '{p}' 2>/dev/null")),
                    }
                }
                Presence::Absent => {
                    let (cmd, check) = match regexp {
                        Some(re) => (
                            format!("sed -i -E '\\|{re}|d' '{p}' 2>/dev/null || true"),
                            format!("! grep -qE '{re}' '{p}' 2>/dev/null"),
                        ),
                        None => (
                            format!("grep -vxF '{line}' '{p}' > '{p}.crater.tmp' 2>/dev/null && mv '{p}.crater.tmp' '{p}' || true"),
                            format!("! grep -qxF '{line}' '{p}' 2>/dev/null"),
                        ),
                    };
                    Op::Shell {
                        phase,
                        describe: format!("lineinfile -{p}"),
                        cmd,
                        soft_fail: false,
                        check: Some(check),
                    }
                }
            }
        }
        Action::User {
            name,
            state,
            system,
            shell,
            home,
            groups,
        } => {
            use crate::component::Presence;
            match state {
                Presence::Present => {
                    let mut opts = String::new();
                    if *system {
                        opts.push_str(" -r");
                    }
                    if let Some(s) = shell {
                        opts.push_str(&format!(" -s '{s}'"));
                    }
                    if let Some(h) = home {
                        opts.push_str(&format!(" -d '{h}' -m"));
                    }
                    if !groups.is_empty() {
                        opts.push_str(&format!(" -G '{}'", groups.join(",")));
                    }
                    Op::Shell {
                        phase,
                        describe: format!("user {name}"),
                        cmd: format!("useradd{opts} '{name}'"),
                        soft_fail: false,
                        check: Some(format!("id '{name}' >/dev/null 2>&1")),
                    }
                }
                Presence::Absent => Op::Shell {
                    phase,
                    describe: format!("user -{name}"),
                    cmd: format!("userdel -r '{name}' 2>/dev/null || userdel '{name}'"),
                    soft_fail: false,
                    check: Some(format!("! id '{name}' >/dev/null 2>&1")),
                },
            }
        }
        Action::Group {
            name,
            state,
            system,
        } => {
            use crate::component::Presence;
            match state {
                Presence::Present => Op::Shell {
                    phase,
                    describe: format!("group {name}"),
                    cmd: format!("groupadd{} '{name}'", if *system { " -r" } else { "" }),
                    soft_fail: false,
                    check: Some(format!("getent group '{name}' >/dev/null 2>&1")),
                },
                Presence::Absent => Op::Shell {
                    phase,
                    describe: format!("group -{name}"),
                    cmd: format!("groupdel '{name}'"),
                    soft_fail: false,
                    check: Some(format!("! getent group '{name}' >/dev/null 2>&1")),
                },
            }
        }
    };
    Ok(op)
}

/// Render `{{ path }}` substitutions — and NOTHING else. This renderer is
/// **deliberately crippled** (D-036): pure value substitution, never evaluation.
/// Any `{{ ... }}` whose body is not a bare dotted path (it contains an operator,
/// a filter `|`, a quote, a parenthesis, or space-separated tokens) is logic/an
/// expression and is **rejected with an error** — logic belongs in the Rust
/// engine, not in YAML. Do NOT "upgrade" this to Tera/minijinja: that reopens the
/// Ansible trap (YAML-as-untyped-unanalyzable-language) this principle exists to
/// prevent, and breaks dry-run / preflight / AI-review (all need static YAML).
///
/// A bare path with no value — `hostvars.*` before its producer registers, a
/// dry-run, or a downstream tool's own `{{.Field}}` template — is left verbatim
/// (unresolved, not an error): absence of a value is a timing/passthrough case,
/// not a logic violation.
pub fn render(tpl: &str, vars: &BTreeMap<String, String>) -> crate::Result<String> {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let close = after
            .find("}}")
            .ok_or_else(|| anyhow::anyhow!("模板有未闭合的 `{{{{`(D-036):{tpl}"))?;
        let raw = &rest[open..open + 2 + close + 2]; // 完整 `{{...}}`
        let key = after[..close].trim();
        if !is_bare_path(key) {
            anyhow::bail!(
                "模板里不允许逻辑/表达式(D-036):`{raw}`。条件/循环/计算放进 Rust 引擎,YAML 模板只做取值 `{{{{ path }}}}`"
            );
        }
        match vars.get(key) {
            Some(v) => out.push_str(v),
            None => out.push_str(raw), // 未定义:原样保留(时序 / dry-run / 下游模板)
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Render a `template` action's source with minijinja (D-075 — the **template
/// layer** allows declarative iteration `{% for %}`/`{{ }}`; business logic still
/// lives in Rust per D-036). Context: every scalar task var, plus structured
/// `groups.<role>` = list of `{name, ip}` for iterating inventory members.
pub fn render_template_str(tpl: &str, ctx: &PlanContext) -> crate::Result<String> {
    use serde_json::{Map, Value};
    let mut data = Map::new();
    // Scalar vars (skip the dotted keys — `groups.x`/`hostvars.x.y` are flattened
    // for the simple `render()` path; here `groups` is exposed structured below).
    for (k, v) in &ctx.vars {
        if !k.contains('.') {
            data.insert(k.clone(), Value::String(v.clone()));
        }
    }
    // groups.<role> = [{name, ip}, ...]
    let groups: Map<String, Value> = ctx
        .groups
        .iter()
        .map(|(role, members)| {
            let arr = members
                .iter()
                .map(|(name, ip)| {
                    let mut m = Map::new();
                    m.insert("name".into(), Value::String(name.clone()));
                    m.insert("ip".into(), Value::String(ip.clone()));
                    Value::Object(m)
                })
                .collect();
            (role.clone(), Value::Array(arr))
        })
        .collect();
    data.insert("groups".into(), Value::Object(groups));

    let mut env = minijinja::Environment::new();
    // Line-oriented config files: don't leave blank lines where `{% %}` blocks were.
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.render_str(tpl, Value::Object(data))
        .map_err(|e| anyhow::anyhow!("render template (minijinja): {e}"))
}

/// A bare dotted variable path (`version`, `hostvars.server.token`, `.Field`):
/// only alphanumerics, `_`, `.`, `-`. Anything else (spaces, operators, `|`,
/// quotes, parens) makes it an expression — forbidden by D-036.
fn is_bare_path(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

fn pick_packages(by_os: &BTreeMap<String, Vec<String>>, os: OsFamily) -> Vec<String> {
    for key in os.match_keys() {
        if let Some(v) = by_os.get(*key) {
            return v.clone();
        }
    }
    Vec::new()
}

/// Stringify a module param value for `{{var}}` substitution.
fn yaml_value_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => b.to_string(),
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => String::new(),
        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

/// Serialize a lowered plan for shipping to the self-bootstrap agent (D-019).
/// The wire format is internal (control writes, agent reads, same version), so
/// the default serde representation is fine.
pub fn plan_to_yaml(ops: &[Op]) -> crate::Result<String> {
    Ok(serde_yaml::to_string(ops)?)
}

/// Parse a plan shipped to `crater agent --plan`.
pub fn plan_from_yaml(text: &str) -> crate::Result<Vec<Op>> {
    Ok(serde_yaml::from_str(text)?)
}

/// Color a status word for terminals (ansible-style: ok=green, changed=yellow,
/// warn=yellow). On a non-TTY (piped, or agent over SSH) it stays plain so no
/// escape codes leak into captured/forwarded output.
fn paint_status(s: StepStatus) -> String {
    use std::io::IsTerminal;
    let word = s.tag();
    if !std::io::stdout().is_terminal() {
        return word.to_string();
    }
    let code = match s {
        StepStatus::Ok => "32",      // green
        StepStatus::Changed => "33", // yellow
        StepStatus::Warn => "33",
    };
    format!("\x1b[{code}m{word}\x1b[0m")
}

/// Execute a plan against a target. Stops on the first hard failure.
///
/// Each step reports `ok` / `changed` / `warn` ansible-style: read-only phases
/// (preflight/verify) report `ok` or `warn`; install steps run their
/// idempotency `check` first and report `ok` (already satisfied, skipped) or
/// `changed` (acted). A re-run of an already-converged plan is all `ok`.
/// Command output is logged at DEBUG (set `CRATER_LOG=debug`), except verify
/// output, which is surfaced at INFO since it's the result the user wants.
pub async fn execute(ops: &[Op], exec: &dyn Executor) -> crate::Result<()> {
    let total = ops.len();
    let (mut n_ok, mut n_changed, mut n_warn) = (0u32, 0u32, 0u32);
    for (i, op) in ops.iter().enumerate() {
        let n = i + 1;
        let (status, surfaced) = exec_one(op, exec, n).await?;
        let line = format!("[{n}/{total}] {} → {}", op.describe(), paint_status(status));
        match status {
            StepStatus::Warn => tracing::warn!("{line}"),
            _ => tracing::info!("{line}"),
        }
        for l in surfaced {
            tracing::info!("      {l}");
        }
        match status {
            StepStatus::Ok => n_ok += 1,
            StepStatus::Changed => n_changed += 1,
            StepStatus::Warn => n_warn += 1,
        }
    }
    tracing::info!(
        "done on {}: changed={n_changed} ok={n_ok} warn={n_warn} ({total} step(s))",
        exec.label()
    );
    Ok(())
}

/// Run one op; return its status plus any lines to surface at INFO (verify
/// output). Command stdout/stderr otherwise goes to DEBUG.
async fn exec_one(
    op: &Op,
    exec: &dyn Executor,
    n: usize,
) -> crate::Result<(StepStatus, Vec<String>)> {
    match op {
        Op::Shell {
            phase,
            cmd,
            soft_fail,
            check,
            ..
        } => {
            // Install steps converge: if the idempotency probe passes, the
            // target is already in the desired state — skip the command.
            if *phase == Phase::Install {
                if let Some(probe) = check {
                    if exec.run(probe).await?.ok() {
                        return Ok((StepStatus::Ok, Vec::new()));
                    }
                }
            }
            let out = exec.run(cmd).await?;
            let so = out.stdout.trim();
            // Verify output is the result the user wants → INFO; the rest → DEBUG.
            let surfaced = if !so.is_empty() && *phase == Phase::Verify {
                so.lines().map(str::to_string).collect()
            } else {
                for l in so.lines() {
                    tracing::debug!("      {l}");
                }
                Vec::new()
            };
            if out.ok() {
                let st = match phase {
                    Phase::Install => StepStatus::Changed,
                    Phase::Preflight | Phase::Verify => StepStatus::Ok,
                };
                Ok((st, surfaced))
            } else {
                for l in out.stderr.trim().lines() {
                    tracing::debug!("      ! {l}");
                }
                if *soft_fail {
                    Ok((StepStatus::Warn, surfaced))
                } else {
                    tracing::error!("[{n}] {} failed (exit {})", op.describe(), out.code);
                    anyhow::bail!("step {n} failed (exit {}): {}", out.code, out.stderr.trim());
                }
            }
        }
        Op::WriteFile {
            path, content, mode, ..
        } => {
            // Idempotency: skip the write if the remote file already matches.
            let want = crate::bundle::sha256_hex(content.as_bytes());
            let probe = format!("sha256sum '{path}' 2>/dev/null | cut -d' ' -f1");
            let cur = exec.run(&probe).await?;
            if cur.ok() && cur.stdout.trim() == want {
                // Content already in place; still ensure mode if requested.
                if let Some(m) = mode {
                    exec.run(&format!("chmod {m} '{path}'")).await?;
                }
                return Ok((StepStatus::Ok, Vec::new()));
            }
            exec.write_file(path, content.as_bytes()).await?;
            if let Some(m) = mode {
                exec.run(&format!("chmod {m} '{path}'")).await?;
            }
            tracing::debug!("      wrote {} bytes -> {path}", content.len());
            Ok((StepStatus::Changed, Vec::new()))
        }
        Op::PushFile {
            local_path,
            dest,
            mode,
            ..
        } => {
            let data = std::fs::read(local_path)
                .map_err(|e| anyhow::anyhow!("read local blob {}: {e}", local_path.display()))?;
            // Idempotency: skip the push if the remote blob already matches.
            let want = crate::bundle::sha256_hex(&data);
            let probe = format!("sha256sum '{dest}' 2>/dev/null | cut -d' ' -f1");
            let cur = exec.run(&probe).await?;
            if cur.ok() && cur.stdout.trim() == want {
                return Ok((StepStatus::Ok, Vec::new()));
            }
            exec.write_file(dest, &data).await?;
            if let Some(mode) = mode {
                exec.run(&format!("chmod {mode} '{dest}'")).await?;
            }
            tracing::debug!("      pushed {} bytes -> {dest}", data.len());
            Ok((StepStatus::Changed, Vec::new()))
        }
        Op::UnarchiveMaterial {
            to,
            strip,
            creates,
            local_archive,
            url,
            ..
        } => {
            // Idempotency: declared product present → skip (AND skip the fetch).
            if let Some(c) = creates {
                if exec.run(&format!("test -e '{c}'")).await?.ok() {
                    return Ok((StepStatus::Ok, Vec::new()));
                }
            }
            let tmp = format!("/tmp/crater-arc-{}.tar", sanitize(to));
            if let Some(archive) = local_archive {
                // Offline: stream the packed blob to a temp on the target.
                let data = std::fs::read(archive)
                    .map_err(|e| anyhow::anyhow!("read archive {}: {e}", archive.display()))?;
                exec.write_file(&tmp, &data).await?;
                tracing::debug!("      pushed {} bytes archive -> {tmp}", data.len());
            } else if let Some(u) = url {
                // Online: the target downloads it.
                let out = exec.run(&format!("curl -fL --retry 3 -o '{tmp}' '{u}'")).await?;
                if !out.ok() {
                    anyhow::bail!("step {n} fetch {u} failed (exit {}): {}", out.code, out.stderr.trim());
                }
            } else {
                anyhow::bail!("step {n} unarchive: no material blob or url");
            }
            let cmd = format!(
                "mkdir -p '{to}' && tar -xf '{tmp}' --strip-components={strip} -C '{to}'; rc=$?; rm -f '{tmp}'; exit $rc"
            );
            let out = exec.run(&cmd).await?;
            if out.ok() {
                Ok((StepStatus::Changed, Vec::new()))
            } else {
                anyhow::bail!("step {n} unarchive failed (exit {}): {}", out.code, out.stderr.trim());
            }
        }
        Op::ImageImport {
            images,
            namespace,
            runtime,
            ..
        } => {
            // Pick a container runtime ONCE for the whole batch: explicit, else
            // probe (D-061/074).
            let rt = match runtime {
                Some(r) => r.clone(),
                None => {
                    let probe = "for r in nerdctl ctr docker podman; do command -v $r >/dev/null 2>&1 && { echo $r; break; }; done";
                    let out = exec.run(probe).await?;
                    let r = out.stdout.trim().to_string();
                    if r.is_empty() {
                        anyhow::bail!("step {n}: no container runtime (nerdctl/ctr/docker/podman) on target");
                    }
                    r
                }
            };
            // namespace only applies to ctr/nerdctl (`-n`); ignored for docker/podman.
            let ns = match namespace {
                Some(n) if rt == "ctr" || rt == "nerdctl" => format!(" -n {n}"),
                _ => String::new(),
            };
            for img in images {
                let reference = &img.reference;
                let cmd = if let Some(archive) = &img.local_archive {
                    // Offline: push the oci-archive, then import it.
                    let data = std::fs::read(archive)
                        .map_err(|e| anyhow::anyhow!("read image archive {}: {e}", archive.display()))?;
                    let remote = format!("/tmp/crater-img-{}.tar", sanitize(reference));
                    exec.write_file(&remote, &data).await?;
                    tracing::debug!("      pushed {} bytes oci-archive -> {remote}", data.len());
                    match rt.as_str() {
                        "ctr" => format!("ctr{ns} images import {remote} && rm -f {remote}"),
                        "nerdctl" => format!("nerdctl{ns} load -i {remote} && rm -f {remote}"),
                        _ => format!("{rt} load -i {remote} && rm -f {remote}"),
                    }
                } else {
                    // Online: runtime pulls the reference.
                    match rt.as_str() {
                        "ctr" => format!("ctr{ns} images pull {reference}"),
                        _ => format!("{rt} pull {reference}"),
                    }
                };
                let out = exec.run(&cmd).await?;
                if !out.ok() {
                    for l in out.stderr.trim().lines() {
                        tracing::debug!("      ! {l}");
                    }
                    anyhow::bail!("step {n} image import '{reference}' failed (exit {}): {}", out.code, out.stderr.trim());
                }
                tracing::debug!("      imported {reference}");
            }
            Ok((StepStatus::Changed, Vec::new()))
        }
        Op::PackageInstall {
            packages,
            family,
            local_archive,
            check,
            ..
        } => {
            // Idempotency: all packages already present → skip.
            if let Some(probe) = check {
                if exec.run(probe).await?.ok() {
                    return Ok((StepStatus::Ok, Vec::new()));
                }
            }
            let cmd = if let Some(archive) = local_archive {
                // Offline: push the closure tar, extract, install local packages.
                let data = std::fs::read(archive)
                    .map_err(|e| anyhow::anyhow!("read package archive {}: {e}", archive.display()))?;
                let dir = format!("/tmp/crater-pkgs-{}", sanitize(&packages.join("-")));
                let tar = format!("{dir}.tar");
                exec.write_file(&tar, &data).await?;
                tracing::debug!("      pushed {} bytes pkg closure -> {tar}", data.len());
                match family.as_str() {
                    "rhel" => format!(
                        "mkdir -p {dir} && tar -xf {tar} -C {dir} && dnf install -y {dir}/*.rpm || yum install -y {dir}/*.rpm; rm -rf {dir} {tar}"
                    ),
                    _ => format!(
                        "mkdir -p {dir} && tar -xf {tar} -C {dir} && DEBIAN_FRONTEND=noninteractive apt-get install -y {dir}/*.deb; rc=$?; rm -rf {dir} {tar}; exit $rc"
                    ),
                }
            } else {
                // Online: install from the system repo.
                match family.as_str() {
                    "rhel" => format!("dnf install -y {p} || yum install -y {p}", p = packages.join(" ")),
                    "debian" => format!(
                        "apt-get update -y && DEBIAN_FRONTEND=noninteractive apt-get install -y {}",
                        packages.join(" ")
                    ),
                    _ => format!("echo 'unknown OS family: cannot install {}'; exit 1", packages.join(" ")),
                }
            };
            let out = exec.run(&cmd).await?;
            if out.ok() {
                Ok((StepStatus::Changed, Vec::new()))
            } else {
                for l in out.stderr.trim().lines() {
                    tracing::debug!("      ! {l}");
                }
                anyhow::bail!("step {n} package install failed (exit {}): {}", out.code, out.stderr.trim());
            }
        }
    }
}

/// Make an image reference safe for a temp filename.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Action;

    #[test]
    fn unarchive_takes_material_directly() {
        // D-073: `unarchive` with `material` lowers to one UnarchiveMaterial op
        // (fetch+extract), no separate place step. Offline → carries the blob.
        let mut ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        ctx.add_material(test_material("app", "https://x/app.tgz"));
        let mut blobs = BTreeMap::new();
        blobs.insert("app".to_string(), PathBuf::from("/blob/app.tar"));
        ctx.offline_blobs = Some(blobs);
        let act = Action::Extract {
            to: PathBuf::from("/usr/local"),
            from: None,
            material: Some("app".into()),
            strip: 0,
            creates: Some(PathBuf::from("/usr/local/bin/app")),
        };
        match action_op(Phase::Install, &act, &ctx).unwrap() {
            Op::UnarchiveMaterial { to, local_archive, creates, .. } => {
                assert_eq!(to, "/usr/local");
                assert_eq!(local_archive, Some(PathBuf::from("/blob/app.tar")));
                assert_eq!(creates.as_deref(), Some("/usr/local/bin/app"));
            }
            other => panic!("expected UnarchiveMaterial, got {other:?}"),
        }
    }

    #[test]
    fn service_check_gates_on_state_not_enabled() {
        // D-075b: restarted+enabled must ALWAYS run (no check); started gates on
        // is-active only (is-enabled must not skip a stopped-but-enabled service).
        use crate::component::ServiceState;
        let ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        let restarted = Action::Service {
            name: "haproxy".into(),
            state: Some(ServiceState::Restarted),
            enabled: Some(true),
        };
        match action_op(Phase::Install, &restarted, &ctx).unwrap() {
            Op::Shell { check, cmd, .. } => {
                assert!(check.is_none(), "restarted must always run, got check={check:?}");
                assert!(cmd.contains("systemctl restart haproxy"));
            }
            _ => panic!("service → shell"),
        }
        let started = Action::Service {
            name: "kubelet".into(),
            state: Some(ServiceState::Started),
            enabled: Some(true),
        };
        match action_op(Phase::Install, &started, &ctx).unwrap() {
            Op::Shell { check, .. } => {
                let c = check.unwrap();
                assert!(c.contains("is-active"), "started gates on is-active: {c}");
                assert!(!c.contains("is-enabled"), "must NOT gate on is-enabled: {c}");
            }
            _ => panic!("service → shell"),
        }
    }

    #[test]
    fn template_renders_minijinja_loop_over_groups() {
        // D-075: the template layer supports {% for %} over structured groups.
        let mut ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        ctx.vars.insert("apiserver_port".into(), "6443".into());
        ctx.groups.insert(
            "controlplane".into(),
            vec![("n11".into(), "10.0.0.11".into()), ("n12".into(), "10.0.0.12".into())],
        );
        let tpl = "backend kube-apiserver\n{% for node in groups.controlplane %}  server {{ node.name }} {{ node.ip }}:{{ apiserver_port }} check\n{% endfor %}";
        let out = render_template_str(tpl, &ctx).unwrap();
        assert!(out.contains("server n11 10.0.0.11:6443 check"), "got:\n{out}");
        assert!(out.contains("server n12 10.0.0.12:6443 check"), "got:\n{out}");
    }

    #[test]
    fn load_image_takes_a_materials_list() {
        // D-074: one load_image with `materials: [..]` → one ImageImport op
        // carrying all of them (single runtime probe).
        let mut ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        for n in ["a", "b", "c"] {
            let mut m = test_material(n, "unused");
            m.kind = crate::component::MaterialKind::Image;
            m.reference = Some(format!("repo/{n}:1"));
            m.url_tmpl = None;
            ctx.add_material(m);
        }
        let act = Action::LoadImage {
            material: None,
            materials: vec!["a".into(), "b".into(), "c".into()],
            namespace: Some("k8s.io".into()),
            runtime: None,
        };
        match action_op(Phase::Install, &act, &ctx).unwrap() {
            Op::ImageImport { images, namespace, .. } => {
                assert_eq!(images.len(), 3);
                assert_eq!(images[0].reference, "repo/a:1");
                assert_eq!(namespace.as_deref(), Some("k8s.io"));
            }
            other => panic!("expected ImageImport, got {other:?}"),
        }
    }

    #[test]
    fn when_role_filters_steps_by_host_roles() {
        // D-071: a step with when_role runs only on hosts holding the role.
        let step = |id: &str, role: &str| crate::task::ActionStep {
            id: Some(id.into()),
            needs: vec![],
            phase: Phase::Install,
            when_os: vec![],
            when_role: if role.is_empty() { vec![] } else { vec![role.into()] },
            when_offline: None,
            retries: 0,
            ignore_errors: false,
            notify: vec![],
            action: Action::RunCmd { cmd: "true".into(), check: None },
        };
        let actions = vec![step("common", ""), step("init", "bootstrap"), step("join", "worker")];

        let mut ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        ctx.host_roles = vec!["bootstrap".into()];
        // bootstrap host: common + init run, join filtered out.
        assert_eq!(plan_from_task(&actions, &ctx).unwrap().len(), 2);

        ctx.host_roles = vec!["worker".into()];
        assert_eq!(plan_from_task(&actions, &ctx).unwrap().len(), 2); // common+join

        ctx.host_roles = vec![]; // no roles → only the unconditional step
        assert_eq!(plan_from_task(&actions, &ctx).unwrap().len(), 1);
    }

    fn test_material(name: &str, url: &str) -> crate::component::Material {
        crate::component::Material {
            name: name.into(),
            kind: crate::component::MaterialKind::File,
            arch: None,
            url_tmpl: Some(url.into()),
            src: None,
            reference: None,
            packages: Default::default(),
            base: None,
            sha256: None,
        }
    }

    fn arch_material(name: &str, a: crate::arch::Arch) -> crate::component::Material {
        let mut m = test_material(name, "https://example.com/x");
        m.arch = Some(a);
        m
    }

    #[test]
    fn resolve_material_picks_by_arch() {
        use crate::arch::Arch;
        let mut ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        ctx.add_material(arch_material("docker", Arch::Amd64));
        ctx.add_material(arch_material("docker", Arch::Arm64));

        // Exact arch match wins.
        ctx.target_arch = Arch::Arm64;
        assert_eq!(ctx.resolve_material("docker").unwrap().arch, Some(Arch::Arm64));
        assert_eq!(
            PlanContext::material_blob_key(ctx.resolve_material("docker").unwrap()),
            "docker@arm64"
        );

        // No variant for the target arch → loud error (not silent wrong-arch).
        ctx.target_arch = Arch::Unknown;
        assert!(ctx.resolve_material("docker").is_err());
    }

    #[test]
    fn resolve_material_falls_back_to_neutral() {
        use crate::arch::Arch;
        let mut ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        ctx.add_material(test_material("script", "https://example.com/s.sh")); // arch: None
        ctx.target_arch = Arch::Arm64;
        let m = ctx.resolve_material("script").unwrap();
        assert_eq!(m.arch, None);
        assert_eq!(PlanContext::material_blob_key(m), "script"); // plain name, no @arch
    }

    fn shell_check(op: &Op) -> Option<&str> {
        match op {
            Op::Shell { check, .. } => check.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn install_actions_get_idempotency_checks() {
        let mut ctx = PlanContext::new(OsFamily::Debian, "1.0".into(), PathBuf::from("."));
        ctx.add_material(test_material("x", "https://example.com/x"));

        // place (online) -> check that the artifact already exists
        let dl = action_op(
            Phase::Install,
            &Action::Place {
                material: "x".into(),
                dest: PathBuf::from("/usr/local/bin/x"),
                mode: None,
            },
            &ctx,
        )
        .unwrap();
        assert_eq!(shell_check(&dl), Some("test -s '/usr/local/bin/x'"));

        // run_cmd carries the author-supplied probe from data
        let rc = action_op(
            Phase::Install,
            &Action::RunCmd {
                cmd: "chmod +x /usr/local/bin/x".into(),
                check: Some("test -x /usr/local/bin/x".into()),
            },
            &ctx,
        )
        .unwrap();
        assert_eq!(shell_check(&rc), Some("test -x /usr/local/bin/x"));
    }

    #[test]
    fn verify_step_has_no_check_and_reports_read_only() {
        let ctx = PlanContext::new(OsFamily::Debian, "1.0".into(), PathBuf::from("."));
        let v = action_op(
            Phase::Verify,
            &Action::RunCmd {
                cmd: "x --version".into(),
                check: None,
            },
            &ctx,
        )
        .unwrap();
        assert_eq!(shell_check(&v), None);
        assert_eq!(v.phase(), Phase::Verify);
    }

    #[test]
    fn render_supports_dotted_and_spaced_keys() {
        let mut vars = BTreeMap::new();
        vars.insert("hostvars.leader.token".to_string(), "T".to_string());
        assert_eq!(render("x={{hostvars.leader.token}}", &vars).unwrap(), "x=T");
        assert_eq!(render("x={{ hostvars.leader.token }}", &vars).unwrap(), "x=T");
    }

    #[test]
    fn render_leaves_unresolved_bare_paths_verbatim() {
        let vars = BTreeMap::new();
        // hostvars not yet registered / dry-run → kept as-is, not an error.
        assert_eq!(
            render("t={{ hostvars.s.tok }}", &vars).unwrap(),
            "t={{ hostvars.s.tok }}"
        );
        // a downstream tool's own template (docker --format) is a bare path too.
        assert_eq!(
            render("--format '{{.Server.Version}}'", &vars).unwrap(),
            "--format '{{.Server.Version}}'"
        );
    }

    #[test]
    fn render_rejects_logic_in_templates() {
        // D-036: conditionals, computation, filters, loops — all rejected, never
        // silently passed through. Logic must live in Rust, not YAML.
        let vars = BTreeMap::new();
        for bad in [
            "{{ env == 'prod' }}",
            "{{ mem * 0.5 }}",
            "heap: {{ (mem_total * 0.5) | int }}M",
            "{{ range .Nodes }}",
            "{{ a or b }}",
            "{{ }}",
        ] {
            assert!(
                render(bad, &vars).is_err(),
                "expected D-036 rejection for template: {bad}"
            );
        }
    }

    #[test]
    fn module_action_lowers_to_checked_shell() {
        let dir = std::env::temp_dir().join("crater-module-test");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lineinfile.yaml"),
            "params: [path, line]\ncheck: 'grep -qF \"{{line}}\" \"{{path}}\"'\nact: 'echo \"{{line}}\" >> \"{{path}}\"'\n",
        )
        .unwrap();
        let mut ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        ctx.roles_dir = dir;

        let mut with = BTreeMap::new();
        with.insert("path".into(), serde_yaml::Value::String("/etc/hosts".into()));
        with.insert("line".into(), serde_yaml::Value::String("1.1.1.1 x".into()));
        let op = action_op(
            Phase::Install,
            &Action::Module {
                uses: "lineinfile".into(),
                with,
            },
            &ctx,
        )
        .unwrap();
        match op {
            Op::Shell { cmd, check, .. } => {
                assert_eq!(cmd, "echo \"1.1.1.1 x\" >> \"/etc/hosts\"");
                assert_eq!(check.as_deref(), Some("grep -qF \"1.1.1.1 x\" \"/etc/hosts\""));
            }
            _ => panic!("module should lower to a shell op"),
        }
    }

    #[test]
    fn module_missing_param_errors() {
        let dir = std::env::temp_dir().join("crater-module-test2");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("needs.yaml"), "params: [a, b]\nact: 'echo {{a}} {{b}}'\n").unwrap();
        let mut ctx = PlanContext::new(OsFamily::Debian, "1".into(), PathBuf::from("."));
        ctx.roles_dir = dir;
        let mut with = BTreeMap::new();
        with.insert("a".into(), serde_yaml::Value::String("x".into()));
        let err = action_op(
            Phase::Install,
            &Action::Module {
                uses: "needs".into(),
                with,
            },
            &ctx,
        )
        .unwrap_err();
        assert!(err.to_string().contains("missing required param 'b'"));
    }

    #[test]
    fn plan_round_trips_through_yaml() {
        // The agent wire format must survive serialize -> deserialize intact,
        // including the idempotency `check` (D-019).
        let mut ctx = PlanContext::new(OsFamily::Debian, "4.53.2".into(), PathBuf::from("."));
        ctx.add_material(test_material("x", "https://example.com/v{{version}}/x"));
        let plan = vec![
            action_op(
                Phase::Install,
                &Action::Place {
                    material: "x".into(),
                    dest: PathBuf::from("/usr/local/bin/x"),
                    mode: None,
                },
                &ctx,
            )
            .unwrap(),
            action_op(
                Phase::Verify,
                &Action::RunCmd {
                    cmd: "x --version".into(),
                    check: None,
                },
                &ctx,
            )
            .unwrap(),
        ];
        let yaml = plan_to_yaml(&plan).unwrap();
        let back = plan_from_yaml(&yaml).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(shell_check(&back[0]), Some("test -s '/usr/local/bin/x'"));
        assert_eq!(back[1].phase(), Phase::Verify);
    }
}
