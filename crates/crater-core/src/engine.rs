//! Plan builder: turns a [`ComponentDescriptor`] into an ordered list of
//! [`Step`]s for a given OS family and version. The plan is what gets shown
//! in dry-run and what the executor runs in apply mode.

use std::collections::BTreeMap;

use crate::component::{Action, Check, ComponentDescriptor};
use crate::os::OsFamily;

#[derive(Debug, Clone, Copy)]
pub enum Phase {
    Preflight,
    Install,
    Verify,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub phase: Phase,
    pub describe: String,
    /// Shell command to run, if this primitive maps cleanly to one yet.
    /// `None` means "not lowered to a command yet" (e.g. render_template).
    pub command: Option<String>,
}

pub struct PlanContext {
    pub os: OsFamily,
    pub version: String,
}

pub fn build_plan(desc: &ComponentDescriptor, ctx: &PlanContext) -> Vec<Step> {
    let mut steps = Vec::new();
    for c in &desc.preflight {
        steps.push(check_step(c));
    }
    for a in &desc.install {
        steps.push(action_step(Phase::Install, a, ctx));
    }
    for a in &desc.verify {
        steps.push(action_step(Phase::Verify, a, ctx));
    }
    steps
}

fn check_step(c: &Check) -> Step {
    let (describe, command) = match c {
        Check::PortFree { port } => (
            format!("port {port} is free"),
            Some(format!("! ss -ltn | grep -q ':{port} '")),
        ),
        Check::KernelMin { version } => {
            (format!("kernel >= {version}"), Some("uname -r".to_string()))
        }
        Check::DiskFree { path, min_gb } => (
            format!("{path} has >= {min_gb}GB free"),
            Some(format!("df -BG {path}")),
        ),
    };
    Step {
        phase: Phase::Preflight,
        describe,
        command,
    }
}

fn action_step(phase: Phase, a: &Action, ctx: &PlanContext) -> Step {
    match a {
        Action::PkgInstall { packages } => {
            let pkgs = pick_packages(packages, ctx.os);
            Step {
                phase,
                describe: format!("install packages: [{}]", pkgs.join(", ")),
                command: Some(ctx.os.install_cmd(&pkgs)),
            }
        }
        Action::Download { url_tmpl, .. } => {
            let url = url_tmpl.replace("{{version}}", &ctx.version);
            Step {
                phase,
                describe: format!("download {url}"),
                command: Some(format!("curl -fL -o /tmp/crater-dl '{url}'")),
            }
        }
        Action::Extract { to, .. } => Step {
            phase,
            describe: format!("extract -> {}", to.display()),
            command: Some(format!("tar -xf /tmp/crater-dl -C {}", to.display())),
        },
        Action::RenderTemplate { src, dst } => Step {
            phase,
            describe: format!("render {} -> {}", src, dst.display()),
            command: None,
        },
        Action::WriteFile { dst, .. } => Step {
            phase,
            describe: format!("write file {}", dst.display()),
            command: None,
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
                cmds.push(format!("systemctl start {name}"));
            }
            Step {
                phase,
                describe: format!("systemd unit {name}"),
                command: Some(cmds.join(" && ")),
            }
        }
        Action::RunCmd { cmd } => Step {
            phase,
            describe: format!("run: {cmd}"),
            command: Some(cmd.clone()),
        },
        Action::LoadImage { reference } => Step {
            phase,
            describe: format!("load image {reference}"),
            command: None,
        },
    }
}

fn pick_packages(by_os: &BTreeMap<String, Vec<String>>, os: OsFamily) -> Vec<String> {
    for key in os.match_keys() {
        if let Some(v) = by_os.get(*key) {
            return v.clone();
        }
    }
    Vec::new()
}
