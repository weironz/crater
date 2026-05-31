//! Plan builder + executor.
//!
//! `build_plan` lowers a [`ComponentDescriptor`] into an ordered list of [`Op`]s
//! for a given OS family / version / params. `execute` runs them against any
//! [`Executor`] (local or SSH). The same ops drive dry-run printing.
//!
//! Offline mode: when [`PlanContext::offline_blobs`] is set, `download` actions
//! become [`Op::PushFile`] (push a pre-fetched blob) instead of `curl`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::component::{Action, Check, ComponentDescriptor};
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
}

impl Op {
    pub fn phase(&self) -> Phase {
        match self {
            Op::Shell { phase, .. } | Op::WriteFile { phase, .. } | Op::PushFile { phase, .. } => {
                *phase
            }
        }
    }
    pub fn describe(&self) -> &str {
        match self {
            Op::Shell { describe, .. }
            | Op::WriteFile { describe, .. }
            | Op::PushFile { describe, .. } => describe,
        }
    }
    /// Shell command / path preview for dry-run, if any.
    pub fn preview(&self) -> Option<&str> {
        match self {
            Op::Shell { cmd, .. } => Some(cmd),
            Op::WriteFile { path, .. } => Some(path),
            Op::PushFile { dest, .. } => Some(dest),
        }
    }
}

#[derive(Clone)]
pub struct PlanContext {
    pub os: OsFamily,
    pub version: String,
    /// Component directory (for resolving template sources).
    pub component_dir: PathBuf,
    /// Template variables (`version` plus user params).
    pub vars: BTreeMap<String, String>,
    /// Mirror rewriter for download URLs (online mode, China-friendly).
    pub source: OnlineSource,
    /// Offline mode: maps a key -> local pre-fetched blob. Download actions key
    /// by rendered URL; `place` actions key by material name (D-034).
    pub offline_blobs: Option<BTreeMap<String, PathBuf>>,
    /// Declared materials by name (D-034), populated from the descriptor in
    /// `build_plan` so `place` can resolve a material's URL for online fetch.
    pub materials: BTreeMap<String, crate::component::Material>,
    /// Directory holding data-defined modules (`modules/<name>.yaml`, D-029).
    pub modules_dir: PathBuf,
}

impl PlanContext {
    pub fn new(os: OsFamily, version: String, component_dir: PathBuf) -> Self {
        let mut vars = BTreeMap::new();
        vars.insert("version".to_string(), version.clone());
        Self {
            os,
            version,
            component_dir,
            vars,
            source: OnlineSource::with_default_mirrors(),
            offline_blobs: None,
            materials: BTreeMap::new(),
            modules_dir: PathBuf::from("modules"),
        }
    }

    /// Switch this context into offline mode with a blob map.
    pub fn with_offline(mut self, blobs: BTreeMap<String, PathBuf>) -> Self {
        self.offline_blobs = Some(blobs);
        self
    }

    /// The rendered download URL for an action.
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

/// Collect every (rendered_url, dest_path) a component would download.
/// Used by `crater build` to know what to fetch into the bundle.
pub fn collect_downloads(
    desc: &ComponentDescriptor,
    ctx: &PlanContext,
) -> crate::Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for a in desc.install.iter().chain(desc.verify.iter()) {
        if let Action::Download { url_tmpl, dest, .. } = a {
            let url = ctx.rendered_url(url_tmpl)?;
            let dest = dest
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "/tmp/crater-dl".to_string());
            out.push((url, dest));
        }
    }
    Ok(out)
}

pub fn build_plan(desc: &ComponentDescriptor, ctx: &PlanContext) -> crate::Result<Vec<Op>> {
    // Make the declared materials (D-034) resolvable by `place` actions.
    let mut ctx = ctx.clone();
    for m in &desc.materials {
        ctx.materials.insert(m.name.clone(), m.clone());
    }
    let ctx = &ctx;

    let mut ops = Vec::new();
    for c in &desc.preflight {
        ops.push(check_op(c));
    }
    for a in &desc.install {
        ops.push(action_op(Phase::Install, a, ctx)?);
    }
    for a in &desc.verify {
        ops.push(action_op(Phase::Verify, a, ctx)?);
    }
    Ok(ops)
}

/// Build an executable plan from a task's `actions` (D-037). **All control flow
/// lives here in Rust** (D-036): `when_os`/`when_offline` filter steps, `needs`
/// topologically orders them; the YAML only declared primitives + data. The
/// caller must have populated `ctx.materials` (for `place`) and `ctx.os`.
pub fn plan_from_task(
    actions: &[crate::task::ActionStep],
    ctx: &PlanContext,
) -> crate::Result<Vec<Op>> {
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
        if os_ok && off_ok {
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

    // 3) Lower each step to an Op in dependency order.
    let by_id: BTreeMap<&str, &Item> = items.iter().map(|it| (it.id.as_str(), it)).collect();
    let mut ops = Vec::new();
    for id in order {
        let it = by_id[id.as_str()];
        ops.push(action_op(it.step.phase, &it.step.action, ctx)?);
    }
    Ok(ops)
}

/// Collect every binary material a component declares (D-034), as
/// `(material_name, rendered_url)` — what `crate build` fetches and packs,
/// keyed by material name (the same key `place` resolves against offline).
pub fn collect_materials(
    desc: &ComponentDescriptor,
    ctx: &PlanContext,
) -> crate::Result<Vec<(String, String)>> {
    use crate::component::MaterialKind;
    let mut out = Vec::new();
    for m in &desc.materials {
        if m.kind == MaterialKind::Binary {
            if let Some(tmpl) = &m.url_tmpl {
                out.push((m.name.clone(), ctx.rendered_url(tmpl)?));
            }
        }
    }
    Ok(out)
}

fn check_op(c: &Check) -> Op {
    let (describe, cmd) = match c {
        Check::PortFree { port } => (
            format!("port {port} is free"),
            format!("if ss -ltn 2>/dev/null | grep -q ':{port} '; then echo 'PORT {port} IN USE'; exit 1; fi"),
        ),
        Check::KernelMin { version } => {
            (format!("kernel >= {version}"), "uname -r".to_string())
        }
        Check::DiskFree { path, min_gb } => (
            format!("{path} has >= {min_gb}GB free"),
            format!("df -BG {path} 2>/dev/null | tail -1 || df -BG / | tail -1"),
        ),
    };
    Op::Shell {
        phase: Phase::Preflight,
        describe,
        cmd,
        soft_fail: true,
        check: None,
    }
}

fn action_op(phase: Phase, a: &Action, ctx: &PlanContext) -> crate::Result<Op> {
    let op = match a {
        Action::PkgInstall { packages } => {
            let pkgs = pick_packages(packages, ctx.os);
            // Idempotency: skip the install if every package is already present.
            let check = match ctx.os {
                OsFamily::Debian => {
                    Some(format!("dpkg -s {} >/dev/null 2>&1", pkgs.join(" ")))
                }
                OsFamily::Rhel => Some(format!("rpm -q {} >/dev/null 2>&1", pkgs.join(" "))),
                OsFamily::Unknown => None,
            };
            Op::Shell {
                phase,
                describe: format!("install packages: [{}]", pkgs.join(", ")),
                cmd: ctx.os.install_cmd(&pkgs),
                soft_fail: false,
                check,
            }
        }
        Action::Download { url_tmpl, dest, .. } => {
            let url = ctx.rendered_url(url_tmpl)?;
            let dest = dest
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "/tmp/crater-dl".to_string());
            if let Some(blobs) = &ctx.offline_blobs {
                let local = blobs
                    .get(&url)
                    .ok_or_else(|| anyhow::anyhow!("offline bundle missing blob for {url}"))?;
                Op::PushFile {
                    phase,
                    describe: format!("push (offline) -> {dest}"),
                    local_path: local.clone(),
                    dest,
                    mode: None,
                }
            } else {
                Op::Shell {
                    phase,
                    describe: format!("download {url}"),
                    cmd: format!("curl -fL --retry 3 -o '{dest}' '{url}'"),
                    soft_fail: false,
                    // Skip the download if the artifact is already present.
                    check: Some(format!("test -s '{dest}'")),
                }
            }
        }
        Action::Place { material, dest, mode } => {
            let dest = dest.display().to_string();
            if let Some(blobs) = &ctx.offline_blobs {
                // Offline: push the packed blob, keyed by material NAME (D-034).
                let local = blobs.get(material).ok_or_else(|| {
                    anyhow::anyhow!("offline bundle missing material '{material}'")
                })?;
                Op::PushFile {
                    phase,
                    describe: format!("place (offline) {material} -> {dest}"),
                    local_path: local.clone(),
                    dest,
                    mode: mode.clone(),
                }
            } else {
                // Online: the target fetches the material's declared URL itself.
                let m = ctx.materials.get(material).ok_or_else(|| {
                    anyhow::anyhow!("place: unknown material '{material}' (declare it under `materials:`)")
                })?;
                let tmpl = m.url_tmpl.as_deref().ok_or_else(|| {
                    anyhow::anyhow!("place: material '{material}' has no url_tmpl for online fetch")
                })?;
                let url = ctx.rendered_url(tmpl)?;
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
        Action::Extract { to, from, strip } => {
            let src = from
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "/tmp/crater-dl".to_string());
            Op::Shell {
                phase,
                describe: format!("extract {src} -> {}", to.display()),
                cmd: format!(
                    "mkdir -p '{to}' && tar -xf '{src}' --strip-components={strip} -C '{to}'",
                    to = to.display(),
                    strip = strip
                ),
                soft_fail: false,
                // No reliable generic "already extracted" probe; re-extracting
                // is harmless/overwrites, so we always run it (reports changed).
                check: None,
            }
        }
        Action::RenderTemplate { src, dst } => {
            let tpl_path = ctx.component_dir.join("templates").join(src);
            let raw = std::fs::read_to_string(&tpl_path)
                .map_err(|e| anyhow::anyhow!("read template {}: {e}", tpl_path.display()))?;
            Op::WriteFile {
                phase,
                describe: format!("render {} -> {}", src, dst.display()),
                path: dst.display().to_string(),
                content: render(&raw, &ctx.vars)?,
            }
        }
        Action::WriteFile { dst, content } => Op::WriteFile {
            phase,
            describe: format!("write file {}", dst.display()),
            path: dst.display().to_string(),
            content: render(content, &ctx.vars)?,
        },
        Action::SystemdUnit {
            name,
            enable,
            start,
        } => {
            let mut cmds = vec!["systemctl daemon-reload".to_string()];
            if *enable {
                cmds.push(format!("systemctl enable {name}"));
            }
            if *start {
                cmds.push(format!("systemctl restart {name}"));
            }
            // Idempotency: skip if the unit is already in the wanted state.
            let mut probes = Vec::new();
            if *enable {
                probes.push(format!("systemctl is-enabled --quiet {name}"));
            }
            if *start {
                probes.push(format!("systemctl is-active --quiet {name}"));
            }
            let check = if probes.is_empty() {
                None
            } else {
                Some(probes.join(" && "))
            };
            Op::Shell {
                phase,
                describe: format!("systemd unit {name} (enable={enable}, start={start})"),
                cmd: cmds.join(" && "),
                soft_fail: false,
                check,
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
            // Data-defined module (D-029): load modules/<uses>.yaml, render its
            // check/act with `with` (+ ctx vars), lower to a checked shell op.
            let path = ctx.modules_dir.join(format!("{uses}.yaml"));
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
        Action::LoadImage { reference, runtime } => {
            let cmd = match runtime {
                // Explicit runtime from data: use it verbatim.
                Some(rt) => format!("{rt} pull '{reference}'"),
                // No runtime declared: pick whichever generic OCI tool exists.
                None => format!(
                    "for rt in nerdctl docker podman ctr; do \
                       if command -v $rt >/dev/null 2>&1; then \
                         [ \"$rt\" = ctr ] && rt=\"ctr image\"; \
                         exec $rt pull '{reference}'; \
                       fi; \
                     done; \
                     echo 'no container runtime found' >&2; exit 1"
                ),
            };
            Op::Shell {
                phase,
                describe: format!("load image {reference}"),
                cmd,
                soft_fail: false,
                check: None,
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
        Op::WriteFile { path, content, .. } => {
            // Idempotency: skip the write if the remote file already matches.
            let want = crate::bundle::sha256_hex(content.as_bytes());
            let probe = format!("sha256sum '{path}' 2>/dev/null | cut -d' ' -f1");
            let cur = exec.run(&probe).await?;
            if cur.ok() && cur.stdout.trim() == want {
                return Ok((StepStatus::Ok, Vec::new()));
            }
            exec.write_file(path, content.as_bytes()).await?;
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::Action;

    fn shell_check(op: &Op) -> Option<&str> {
        match op {
            Op::Shell { check, .. } => check.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn install_actions_get_idempotency_checks() {
        let ctx = PlanContext::new(OsFamily::Debian, "1.0".into(), PathBuf::from("."));

        // download -> check that the artifact already exists
        let dl = action_op(
            Phase::Install,
            &Action::Download {
                url_tmpl: "https://example.com/x".into(),
                sha256: None,
                dest: Some(PathBuf::from("/usr/local/bin/x")),
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
        ctx.modules_dir = dir;

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
        ctx.modules_dir = dir;
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
        let ctx = PlanContext::new(OsFamily::Debian, "4.53.2".into(), PathBuf::from("."));
        let plan = vec![
            action_op(
                Phase::Install,
                &Action::Download {
                    url_tmpl: "https://example.com/v{{version}}/x".into(),
                    sha256: None,
                    dest: Some(PathBuf::from("/usr/local/bin/x")),
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
