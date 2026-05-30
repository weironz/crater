//! `crater` CLI.
//!
//! Forms:
//!   crater <component> [--version X] [--os debian|rhel] [--apply]   shortcut deploy
//!   crater apply -f crater.yaml                                     declarative spec
//!   crater agent --task ...                                         internal (on node)

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

use crater_core::component::ComponentDescriptor;
use crater_core::engine::{build_plan, PlanContext, Step};
use crater_core::executor::{Executor, LocalExecutor};
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
    },
    /// Internal: self-bootstrap agent mode running on the target node.
    Agent {
        #[arg(long)]
        task: Option<String>,
    },
    /// Shortcut: `crater <component> [--version X] [--os debian|rhel] [--apply]`.
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
        Cmd::Apply { file } => apply_spec(&file).await,
        Cmd::Agent { task } => {
            println!("[agent] self-bootstrap mode (TODO M1). task={task:?}");
            Ok(())
        }
        Cmd::Component(args) => deploy_shortcut(args).await,
    }
}

async fn deploy_shortcut(args: Vec<String>) -> Result<()> {
    let mut it = args.into_iter();
    let name = it.next().ok_or_else(|| anyhow!("missing component name"))?;

    let mut version: Option<String> = None;
    let mut os_override: Option<String> = None;
    let mut components_dir = PathBuf::from("components");
    let mut do_apply = false;

    let rest: Vec<String> = it.collect();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--version" => {
                version = rest.get(i + 1).cloned();
                i += 2;
            }
            "--os" => {
                os_override = rest.get(i + 1).cloned();
                i += 2;
            }
            "--components-dir" => {
                if let Some(v) = rest.get(i + 1) {
                    components_dir = PathBuf::from(v);
                }
                i += 2;
            }
            "--apply" => {
                do_apply = true;
                i += 1;
            }
            other => return Err(anyhow!("unknown flag: {other}")),
        }
    }

    let path = components_dir.join(&name).join("component.yaml");
    let desc = ComponentDescriptor::from_yaml_file(&path)
        .map_err(|e| anyhow!("failed to load component '{name}' from {}: {e}", path.display()))?;

    let osf = match os_override {
        Some(s) => OsFamily::from_name(&s),
        None => os::detect_local(),
    };
    let ver = version
        .or_else(|| desc.version_default.clone())
        .unwrap_or_else(|| "latest".into());

    let ctx = PlanContext {
        os: osf,
        version: ver.clone(),
    };
    let plan = build_plan(&desc, &ctx);

    println!("Component : {}", desc.name);
    println!("Version   : {ver}");
    println!("OS family : {}", osf.as_str());
    println!(
        "Mode      : {}",
        if do_apply { "APPLY (local)" } else { "DRY-RUN" }
    );
    println!("Steps     : {}", plan.len());
    println!("------------------------------------------");
    print_plan(&plan);
    println!("------------------------------------------");

    if do_apply {
        run_plan_local(&plan).await
    } else {
        println!("Dry-run only. Re-run with --apply to execute locally.");
        Ok(())
    }
}

async fn apply_spec(file: &Path) -> Result<()> {
    let spec = CraterSpec::from_yaml_file(file)?;
    println!(
        "Loaded spec: {} host(s), {} component(s)",
        spec.inventory.hosts.len(),
        spec.components.len()
    );

    let components_dir = PathBuf::from("components");
    for cref in &spec.components {
        let path = components_dir.join(&cref.name).join("component.yaml");
        let desc = ComponentDescriptor::from_yaml_file(&path)?;
        let ver = cref
            .version
            .clone()
            .or_else(|| desc.version_default.clone())
            .unwrap_or_else(|| "latest".into());
        let ctx = PlanContext {
            os: OsFamily::Unknown,
            version: ver.clone(),
        };
        let plan = build_plan(&desc, &ctx);
        println!("\n=== component: {} (v{ver}) — {} steps ===", cref.name, plan.len());
        print_plan(&plan);
    }
    println!("\n(apply over SSH/inventory is a later M1 step; dry-run shown above.)");
    Ok(())
}

fn print_plan(plan: &[Step]) {
    for (i, s) in plan.iter().enumerate() {
        println!("{:>2}. [{:?}] {}", i + 1, s.phase, s.describe);
        if let Some(cmd) = &s.command {
            println!("      $ {cmd}");
        }
    }
}

async fn run_plan_local(plan: &[Step]) -> Result<()> {
    let exec = LocalExecutor;
    for (i, s) in plan.iter().enumerate() {
        let Some(cmd) = &s.command else {
            println!("{:>2}. SKIP (no command yet): {}", i + 1, s.describe);
            continue;
        };
        println!("{:>2}. RUN: {}", i + 1, s.describe);
        let out = exec.run(cmd).await?;
        if !out.stdout.trim().is_empty() {
            println!("     stdout: {}", out.stdout.trim());
        }
        if !out.ok() {
            if !out.stderr.trim().is_empty() {
                println!("     stderr: {}", out.stderr.trim());
            }
            return Err(anyhow!("step {} failed with exit code {}", i + 1, out.code));
        }
    }
    println!("All steps completed.");
    Ok(())
}
