//! `crater` CLI.
//!
//! Forms:
//!   crater <component> [--host H --user U --password P --port N] [--version X] [--os debian|rhel] [--apply]
//!   crater apply -f crater.yaml [--apply]
//!   crater agent --task ...            (internal, on the node)
//!
//! Without --host the target is the local machine. With --host the control
//! plane drives the remote node over SSH (agentless).

use std::path::PathBuf;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

use crater_core::component::ComponentDescriptor;
use crater_core::engine::{self, build_plan, Op, PlanContext};
use crater_core::executor::{Executor, LocalExecutor, SshExecutor};
use crater_core::os::{self, OsFamily};
use crater_core::spec::CraterSpec;

#[derive(Parser)]
#[command(
    name = "crater",
    version,
    about = "Deploy anything — online & offline component/cluster installer"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Apply a declarative spec file (crater.yaml).
    Apply {
        #[arg(short, long)]
        file: PathBuf,
        /// Actually execute (default is dry-run).
        #[arg(long)]
        apply: bool,
    },
    /// Internal: self-bootstrap agent mode running on the target node.
    Agent {
        #[arg(long)]
        task: Option<String>,
    },
    /// Shortcut: `crater <component> [flags]`.
    #[command(external_subcommand)]
    Component(Vec<String>),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .init();

    match Cli::parse().cmd {
        Cmd::Apply { file, apply } => apply_spec(&file, apply).await,
        Cmd::Agent { task } => {
            println!("[agent] self-bootstrap mode (TODO M3+). task={task:?}");
            Ok(())
        }
        Cmd::Component(args) => deploy_shortcut(args).await,
    }
}

#[derive(Default)]
struct ShortcutFlags {
    version: Option<String>,
    os_override: Option<String>,
    host: Option<String>,
    user: String,
    port: u16,
    password: Option<String>,
    components_dir: PathBuf,
    do_apply: bool,
}

fn parse_flags(rest: &[String]) -> Result<ShortcutFlags> {
    let mut f = ShortcutFlags {
        user: "root".into(),
        port: 22,
        components_dir: PathBuf::from("components"),
        password: std::env::var("CRATER_SSH_PASSWORD").ok(),
        ..Default::default()
    };
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--version" => {
                f.version = rest.get(i + 1).cloned();
                i += 2;
            }
            "--os" => {
                f.os_override = rest.get(i + 1).cloned();
                i += 2;
            }
            "--host" => {
                f.host = rest.get(i + 1).cloned();
                i += 2;
            }
            "--user" => {
                if let Some(v) = rest.get(i + 1) {
                    f.user = v.clone();
                }
                i += 2;
            }
            "--password" => {
                f.password = rest.get(i + 1).cloned();
                i += 2;
            }
            "--port" => {
                if let Some(v) = rest.get(i + 1) {
                    f.port = v.parse().map_err(|_| anyhow!("invalid --port: {v}"))?;
                }
                i += 2;
            }
            "--components-dir" => {
                if let Some(v) = rest.get(i + 1) {
                    f.components_dir = PathBuf::from(v);
                }
                i += 2;
            }
            "--apply" => {
                f.do_apply = true;
                i += 1;
            }
            other => return Err(anyhow!("unknown flag: {other}")),
        }
    }
    Ok(f)
}

async fn deploy_shortcut(args: Vec<String>) -> Result<()> {
    let mut it = args.into_iter();
    let name = it.next().ok_or_else(|| anyhow!("missing component name"))?;
    let rest: Vec<String> = it.collect();
    let f = parse_flags(&rest)?;

    let component_dir = f.components_dir.join(&name);
    let desc = ComponentDescriptor::from_yaml_file(&component_dir.join("component.yaml"))
        .map_err(|e| anyhow!("failed to load component '{name}': {e}"))?;

    // Build the target executor.
    let exec: Box<dyn Executor> = match &f.host {
        Some(host) => {
            let pw = f
                .password
                .as_deref()
                .ok_or_else(|| anyhow!("--password (or CRATER_SSH_PASSWORD) required for --host"))?;
            println!("Connecting to {}@{host}:{} ...", f.user, f.port);
            Box::new(SshExecutor::connect(host, f.port, &f.user, pw).await?)
        }
        None => Box::new(LocalExecutor),
    };

    // Resolve OS family.
    let osf = match &f.os_override {
        Some(s) => OsFamily::from_name(s),
        None => {
            if f.host.is_some() {
                os::detect_via(exec.as_ref()).await
            } else {
                os::detect_local()
            }
        }
    };

    let ver = f
        .version
        .clone()
        .or_else(|| desc.version_default.clone())
        .unwrap_or_else(|| "latest".into());

    let ctx = PlanContext::new(osf, ver.clone(), component_dir);
    let plan = build_plan(&desc, &ctx)?;

    println!("Component : {}", desc.name);
    println!("Version   : {ver}");
    println!("Target    : {}", exec.label());
    println!("OS family : {}", osf.as_str());
    println!(
        "Mode      : {}",
        if f.do_apply { "APPLY" } else { "DRY-RUN" }
    );
    println!("Steps     : {}", plan.len());
    println!("------------------------------------------");
    print_plan(&plan);
    println!("------------------------------------------");

    if f.do_apply {
        engine::execute(&plan, exec.as_ref()).await
    } else {
        println!("Dry-run only. Re-run with --apply to execute.");
        Ok(())
    }
}

async fn apply_spec(file: &std::path::Path, do_apply: bool) -> Result<()> {
    let spec = CraterSpec::from_yaml_file(file)?;
    let components_dir = PathBuf::from("components");
    println!(
        "Spec: {} host(s), {} component(s), mode={}",
        spec.inventory.hosts.len(),
        spec.components.len(),
        if do_apply { "APPLY" } else { "DRY-RUN" }
    );

    if spec.inventory.hosts.is_empty() {
        // No hosts: just print plans locally (dry-run aid).
        for cref in &spec.components {
            let component_dir = components_dir.join(&cref.name);
            let desc =
                ComponentDescriptor::from_yaml_file(&component_dir.join("component.yaml"))?;
            let ver = cref
                .version
                .clone()
                .or_else(|| desc.version_default.clone())
                .unwrap_or_else(|| "latest".into());
            let ctx = PlanContext::new(OsFamily::Unknown, ver.clone(), component_dir);
            let plan = build_plan(&desc, &ctx)?;
            println!("\n=== {} (v{ver}) — {} steps ===", cref.name, plan.len());
            print_plan(&plan);
        }
        return Ok(());
    }

    for host in &spec.inventory.hosts {
        println!("\n########## host: {} ({}) ##########", host.name, host.address);
        let exec: Box<dyn Executor> = if do_apply {
            let pw = host
                .password
                .as_deref()
                .or_else(|| Some(""))
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("host {} has no password (M1 requires one)", host.name))?;
            Box::new(SshExecutor::connect(&host.address, host.port, &host.user, pw).await?)
        } else {
            Box::new(LocalExecutor)
        };

        let osf = if do_apply {
            os::detect_via(exec.as_ref()).await
        } else {
            OsFamily::Unknown
        };

        for cref in &spec.components {
            // role filter: if host declares roles, only run matching components
            if !host.roles.is_empty() && !host.roles.contains(&cref.name) {
                continue;
            }
            let component_dir = components_dir.join(&cref.name);
            let desc =
                ComponentDescriptor::from_yaml_file(&component_dir.join("component.yaml"))?;
            let ver = cref
                .version
                .clone()
                .or_else(|| desc.version_default.clone())
                .unwrap_or_else(|| "latest".into());
            let ctx = PlanContext::new(osf, ver.clone(), component_dir);
            let plan = build_plan(&desc, &ctx)?;
            println!("\n--- component {} (v{ver}) — {} steps ---", cref.name, plan.len());
            print_plan(&plan);
            if do_apply {
                engine::execute(&plan, exec.as_ref()).await?;
            }
        }
    }
    if !do_apply {
        println!("\n(dry-run; re-run with --apply to execute over SSH.)");
    }
    Ok(())
}

fn print_plan(plan: &[Op]) {
    for (i, op) in plan.iter().enumerate() {
        println!("{:>2}. [{:?}] {}", i + 1, op.phase(), op.describe());
        if let Some(p) = op.preview() {
            let oneline: String = p.lines().next().unwrap_or("").chars().take(120).collect();
            println!("      $ {oneline}");
        }
    }
}
