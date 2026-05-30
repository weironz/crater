//! Plan builder + executor.
//!
//! `build_plan` lowers a [`ComponentDescriptor`] into an ordered list of [`Op`]s
//! for a given OS family / version / params. `execute` runs them against any
//! [`Executor`] (local or SSH). The same ops drive dry-run printing.
//!
//! Offline mode: when [`PlanContext::offline_blobs`] is set, `download` actions
//! become [`Op::PushFile`] (push a pre-fetched blob) instead of `curl`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::component::{Action, Check, ComponentDescriptor};
use crate::executor::Executor;
use crate::os::OsFamily;
use crate::source::OnlineSource;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Preflight,
    Install,
    Verify,
}

/// A concrete, executor-dispatchable operation.
#[derive(Debug, Clone)]
pub enum Op {
    /// Run a shell command on the target.
    Shell {
        phase: Phase,
        describe: String,
        cmd: String,
        /// If true, a non-zero exit is a warning, not a hard failure
        /// (used by preflight checks so we surface issues without aborting).
        soft_fail: bool,
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

pub struct PlanContext {
    pub os: OsFamily,
    pub version: String,
    /// Component directory (for resolving template sources).
    pub component_dir: PathBuf,
    /// Template variables (`version` plus user params).
    pub vars: BTreeMap<String, String>,
    /// Mirror rewriter for download URLs (online mode, China-friendly).
    pub source: OnlineSource,
    /// Offline mode: maps a rendered download URL -> local pre-fetched blob.
    pub offline_blobs: Option<BTreeMap<String, PathBuf>>,
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
        }
    }

    /// Switch this context into offline mode with a blob map.
    pub fn with_offline(mut self, blobs: BTreeMap<String, PathBuf>) -> Self {
        self.offline_blobs = Some(blobs);
        self
    }

    /// The rendered download URL for an action.
    /// Offline mode skips mirror rewrite (the raw URL is the manifest key).
    pub fn rendered_url(&self, url_tmpl: &str) -> String {
        let raw = render(url_tmpl, &self.vars);
        if self.offline_blobs.is_some() {
            raw
        } else {
            self.source.rewrite(&raw)
        }
    }
}

/// Collect every (rendered_url, dest_path) a component would download.
/// Used by `crater build` to know what to fetch into the bundle.
pub fn collect_downloads(desc: &ComponentDescriptor, ctx: &PlanContext) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for a in desc.install.iter().chain(desc.verify.iter()) {
        if let Action::Download { url_tmpl, dest, .. } = a {
            let url = ctx.rendered_url(url_tmpl);
            let dest = dest
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "/tmp/crater-dl".to_string());
            out.push((url, dest));
        }
    }
    out
}

pub fn build_plan(desc: &ComponentDescriptor, ctx: &PlanContext) -> crate::Result<Vec<Op>> {
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
    }
}

fn action_op(phase: Phase, a: &Action, ctx: &PlanContext) -> crate::Result<Op> {
    let op = match a {
        Action::PkgInstall { packages } => {
            let pkgs = pick_packages(packages, ctx.os);
            Op::Shell {
                phase,
                describe: format!("install packages: [{}]", pkgs.join(", ")),
                cmd: ctx.os.install_cmd(&pkgs),
                soft_fail: false,
            }
        }
        Action::Download { url_tmpl, dest, .. } => {
            let url = ctx.rendered_url(url_tmpl);
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
                }
            } else {
                Op::Shell {
                    phase,
                    describe: format!("download {url}"),
                    cmd: format!("curl -fL --retry 3 -o '{dest}' '{url}'"),
                    soft_fail: false,
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
                content: render(&raw, &ctx.vars),
            }
        }
        Action::WriteFile { dst, content } => Op::WriteFile {
            phase,
            describe: format!("write file {}", dst.display()),
            path: dst.display().to_string(),
            content: render(content, &ctx.vars),
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
            Op::Shell {
                phase,
                describe: format!("systemd unit {name} (enable={enable}, start={start})"),
                cmd: cmds.join(" && "),
                soft_fail: false,
            }
        }
        Action::RunCmd { cmd } => Op::Shell {
            phase,
            describe: format!("run: {cmd}"),
            cmd: render(cmd, &ctx.vars),
            soft_fail: false,
        },
        Action::LoadImage { reference } => Op::Shell {
            phase,
            describe: format!("load image {reference}"),
            cmd: format!("docker pull '{reference}' || ctr image pull '{reference}'"),
            soft_fail: false,
        },
    };
    Ok(op)
}

/// Minimal `{{var}}` substitution. Tera/minijinja can replace this later.
fn render(tpl: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = tpl.to_string();
    for (k, v) in vars {
        out = out.replace(&format!("{{{{{k}}}}}"), v);
    }
    out
}

fn pick_packages(by_os: &BTreeMap<String, Vec<String>>, os: OsFamily) -> Vec<String> {
    for key in os.match_keys() {
        if let Some(v) = by_os.get(*key) {
            return v.clone();
        }
    }
    Vec::new()
}

/// Execute a plan against a target. Stops on the first hard failure.
pub async fn execute(ops: &[Op], exec: &dyn Executor) -> crate::Result<()> {
    let total = ops.len();
    for (i, op) in ops.iter().enumerate() {
        let n = i + 1;
        println!("[{n}/{total}] {:?} {}", op.phase(), op.describe());
        match op {
            Op::Shell { cmd, soft_fail, .. } => {
                let out = exec.run(cmd).await?;
                let so = out.stdout.trim();
                if !so.is_empty() {
                    for line in so.lines() {
                        println!("      | {line}");
                    }
                }
                if !out.ok() {
                    let se = out.stderr.trim();
                    if !se.is_empty() {
                        println!("      ! {se}");
                    }
                    if *soft_fail {
                        println!("      (warning: exit {}, continuing)", out.code);
                    } else {
                        anyhow::bail!("step {n} failed (exit {}): {}", out.code, se);
                    }
                }
            }
            Op::WriteFile { path, content, .. } => {
                exec.write_file(path, content.as_bytes()).await?;
                println!("      | wrote {} bytes", content.len());
            }
            Op::PushFile {
                local_path, dest, ..
            } => {
                let data = std::fs::read(local_path)
                    .map_err(|e| anyhow::anyhow!("read local blob {}: {e}", local_path.display()))?;
                exec.write_file(dest, &data).await?;
                println!("      | pushed {} bytes -> {dest}", data.len());
            }
        }
    }
    println!("Done: {total} step(s) completed on {}.", exec.label());
    Ok(())
}
