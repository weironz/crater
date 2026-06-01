//! `crater` CLI — a declarative remote-execution engine (task model).
//!
//! Forms (executes by default; pass --dry-run to only print the plan):
//!   crater apply <task>.yaml [--host a,b | -i inv.yaml] [--key K] [--dry-run|--shell]
//!   crater apply <name>                       # named task → tasks/<name>.yaml
//!   crater <name> [flags]                      # shorthand for `crater apply <name>`
//!   crater apply <image-ref|x.oci> --host H    # deploy an image / offline artifact
//!   crater build -f task.yaml [-t ref]         # → B 类 OCI artifact in the local store
//!   crater save <ref> -o x.oci                 # export a stored artifact to a file
//!   crater ai "<request>" [-o task.yaml]       # NL → validated task
//!   crater doctor --file log.txt | --host H    # offline rule-based diagnosis
//!   crater run --host H --password P -- <cmd>  # ad-hoc (ansible -m shell style)
//!   crater agent --plan|--task-plan <file>     # internal (runs on the target node)

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use tracing::info;

use crater_core::arch;
use crater_core::bundle;
use crater_core::engine::{self, Op, PlanContext};
use crater_core::executor::{Executor, LocalExecutor, SshExecutor};
use crater_core::os::{self, OsFamily};
use crater_core::source::{self, OnlineSource};
use crater_core::spec::CraterSpec;
use crater_core::store::ImageStore;

#[derive(Parser)]
#[command(
    name = "crater",
    version,
    about = "Deploy anything — declarative remote-execution engine (task model, online & offline)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Apply a task — one command for online & offline (D-020). `<source>`
    /// auto-detects: a task `.yaml`, a named task (`tasks/<name>.yaml`), an image
    /// reference, or an `.oci` artifact. `--host`/`-i`/none picks targets. Same
    /// engine & idempotency online or offline (offline replays the artifact).
    Apply {
        /// `<source>`, or a `<name>` label when a second positional `<source>`
        /// follows: `crater apply app01 docker.io/library/app01:v1.0`.
        arg1: Option<String>,
        /// `<source>` (image ref | x.oci | spec.yaml | component) when the first
        /// positional is a name.
        arg2: Option<String>,
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Inventory file (its `inventory:` hosts) — large-fleet form, per-host
        /// creds. A spec source carries its own inventory.
        #[arg(short = 'i', long)]
        inventory: Option<PathBuf>,
        /// Target host(s), comma-separated for a small fleet sharing one credential:
        /// `--host 10.0.0.5,10.0.0.6`. Omit (and no `-i`) → local install.
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        /// SSH private-key file (alternative to --password), shared by all --host.
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Print the plan without executing.
        #[arg(long)]
        dry_run: bool,
        /// Force the agentless shell executor instead of the default agent.
        #[arg(long)]
        shell: bool,
    },
    /// Delete/uninstall a task's deployment by running its authored `teardown:`
    /// (D-049). **Opt-in**: only a task that defines `teardown:` has this — there
    /// is NO auto-inversion of `actions:` (real cleanup, e.g. kubeadm reset or
    /// rm /var/lib/mysql, targets runtime state the install never created).
    Delete {
        /// `<source>`: a named task, task.yaml, x.oci bundle, or image ref.
        source: Option<String>,
        #[arg(short, long)]
        file: Option<PathBuf>,
        #[arg(short = 'i', long)]
        inventory: Option<PathBuf>,
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        key: Option<PathBuf>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Print the teardown plan without executing.
        #[arg(long)]
        dry_run: bool,
        /// Force the agentless shell executor instead of the default agent.
        #[arg(long)]
        shell: bool,
    },
    /// Build a task into a B 类 OCI artifact in the local store (like
    /// `docker build`). Export to a file with `crater save`.
    Build {
        /// Task file to build (its `materials` are fetched and packed).
        #[arg(short, long)]
        file: PathBuf,
        /// Reference (tag) for the artifact, e.g. `192.168.1.5:5000/yq:1.0`.
        /// Defaults to `crater/<name>:<version>`.
        #[arg(short = 't', long)]
        tag: Option<String>,
        /// Restrict packed material arches (D-048), e.g. `--arch amd64` or
        /// `--arch amd64,arm64`. Default: pack every declared arch variant.
        #[arg(long, value_delimiter = ',')]
        arch: Vec<String>,
    },
    /// Export a stored artifact/image to an oci-archive file (like `docker save`).
    Save {
        /// Reference in the local store (`crater images` to list).
        reference: String,
        /// Output file, e.g. yq.oci
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Deploy an offline `.oci` task artifact to a target (= `crater apply
    /// <x.oci>`; zero network on the target).
    Deploy {
        #[arg(long)]
        bundle: PathBuf,
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Print the plan without executing.
        #[arg(long)]
        dry_run: bool,
    },
    /// AI copilot: natural language -> a validated task yaml.
    /// Configure via CRATER_AI_ENDPOINT / CRATER_AI_KEY / CRATER_AI_MODEL.
    Ai {
        #[arg(trailing_var_arg = true, required = true)]
        request: Vec<String>,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Diagnose failures with built-in offline rules (+ optional AI) (M5).
    Doctor {
        /// Analyze this local log/error file (fully offline, no SSH).
        #[arg(long)]
        file: Option<PathBuf>,
        /// Or collect diagnostics from this host over SSH.
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Also ask the configured AI endpoint for deeper analysis, if set.
        #[arg(long)]
        ai: bool,
    },
    /// Run an ad-hoc command on a target over SSH (ansible -m shell style).
    Run {
        #[arg(long)]
        host: String,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// Copy a local file to a target over SSH (chunked base64, no scp needed).
    Cp {
        #[arg(long)]
        host: String,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Local source file.
        #[arg(long)]
        src: PathBuf,
        /// Remote destination path.
        #[arg(long)]
        dst: String,
        /// chmod the remote file (e.g. 755) after upload.
        #[arg(long)]
        chmod: Option<String>,
    },
    /// List images in the local store (~/.crater/store).
    Images,
    /// Pull an image from a registry into the local store.
    Pull {
        /// e.g. docker.io/library/busybox:latest
        reference: String,
    },
    /// Push a stored image to a registry.
    Push {
        reference: String,
    },
    /// Import an oci-archive file (e.g. `crater save` output) into the store.
    Load {
        /// Path to the .oci archive.
        file: PathBuf,
        /// Tag to store it under (default: the archive's embedded ref.name, e.g.
        /// from `build -t`), e.g. 192.168.73.5:5000/yq:4.53.2
        #[arg(long = "as")]
        as_ref: Option<String>,
    },
    /// Add a new reference (alias) to a stored image, like `docker tag`.
    Tag {
        /// Existing reference in the local store.
        source: String,
        /// New reference to point at the same manifest (e.g. a registry address).
        target: String,
    },
    /// Registry credentials.
    Registry {
        #[command(subcommand)]
        cmd: RegistryCmd,
    },
    /// Generate a starter file to edit (e.g. a sample inventory).
    Create {
        #[command(subcommand)]
        what: CreateWhat,
    },
    /// Internal: self-bootstrap agent. Runs ON the target node, executing a
    /// lowered plan locally (pushed here by the control machine). D-019.
    Agent {
        /// Path to a serialized component plan (Vec<Op>) to execute locally.
        #[arg(long)]
        plan: Option<PathBuf>,
        /// Path to a serialized task plan (steps + handlers, D-044) to run locally.
        #[arg(long)]
        task_plan: Option<PathBuf>,
    },
    /// Shortcut: `crater <component> [flags]`.
    #[command(external_subcommand)]
    Component(Vec<String>),
}

#[derive(Subcommand)]
enum RegistryCmd {
    /// Store credentials for a registry (used by pull/push).
    Login {
        /// Registry host, e.g. docker.io or registry.example.com:5000
        registry: String,
        #[arg(short, long)]
        username: String,
        #[arg(short, long)]
        password: String,
    },
}

#[derive(Subcommand)]
enum CreateWhat {
    /// Write a sample inventory.yaml (host list for `-i`) to edit.
    Inventory {
        /// Output path.
        #[arg(default_value = "inventory.yaml")]
        path: PathBuf,
        /// Overwrite if the file already exists.
        #[arg(long)]
        force: bool,
    },
}

/// Compact wall-clock timer (`HH:MM:SS`, UTC) — dependency-free, keeps log
/// lines short vs the default RFC3339 timestamp.
struct ClockTime;
impl tracing_subscriber::fmt::time::FormatTime for ClockTime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        write!(w, "{:02}:{:02}:{:02}", (s / 3600) % 24, (s / 60) % 60, s % 60)
    }
}

fn log_level() -> tracing::Level {
    match std::env::var("CRATER_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("trace") => tracing::Level::TRACE,
        Some("debug") => tracing::Level::DEBUG,
        Some("warn") => tracing::Level::WARN,
        Some("error") => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // ANSI only on a real terminal — keeps redirected/piped output and the
    // agent's SSH-forwarded output free of escape codes.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    tracing_subscriber::fmt()
        .with_max_level(log_level())
        .with_timer(ClockTime)
        .with_target(false)
        .with_ansi(ansi)
        .with_writer(std::io::stdout)
        .init();

    match Cli::parse().cmd {
        Cmd::Apply {
            arg1,
            arg2,
            file,
            inventory,
            host,
            user,
            password,
            key,
            port,
            dry_run,
            shell,
        } => {
            // Two positional forms: `apply <source>` or `apply <name> <source>`.
            let (name, source) = match (arg1, arg2) {
                (Some(a), Some(b)) => (Some(a), Some(b)),
                (Some(a), None) => (None, Some(a)),
                (None, _) => (None, None),
            };
            apply_source(name, source, file, inventory, host, user, password, key, port, dry_run, shell, false)
                .await
        }
        Cmd::Delete {
            source,
            file,
            inventory,
            host,
            user,
            password,
            key,
            port,
            dry_run,
            shell,
        } => {
            apply_source(None, source, file, inventory, host, user, password, key, port, dry_run, shell, true)
                .await
        }
        Cmd::Build { file, tag, arch } => build_to_store(&file, tag, &arch).await,
        Cmd::Save { reference, output } => {
            ImageStore::open()?.export_oci_archive(&reference, &output)?;
            info!("saved {reference} → {}", output.display());
            Ok(())
        }
        Cmd::Deploy {
            bundle,
            host,
            user,
            password,
            port,
            dry_run,
        } => deploy_bundle(&bundle, host, &user, password, port, !dry_run).await,
        Cmd::Cp {
            host,
            user,
            password,
            port,
            src,
            dst,
            chmod,
        } => push_file(&host, &user, password, port, &src, &dst, chmod).await,
        Cmd::Images => list_images().await,
        Cmd::Pull { reference } => pull_image(&reference).await,
        Cmd::Push { reference } => push_image(&reference).await,
        Cmd::Load { file, as_ref } => {
            let r = ImageStore::open()?.import_oci_archive(&file, as_ref.as_deref())?;
            info!("loaded {} → {r}", file.display());
            Ok(())
        }
        Cmd::Tag { source, target } => {
            ImageStore::open()?.retag(&source, &target)?;
            info!("tagged {source} → {target}");
            Ok(())
        }
        Cmd::Registry { cmd } => match cmd {
            RegistryCmd::Login {
                registry,
                username,
                password,
            } => {
                crater_core::store::save_login(&registry, &username, &password)?;
                info!("saved credentials for {registry}");
                Ok(())
            }
        },
        Cmd::Create { what } => match what {
            CreateWhat::Inventory { path, force } => create_inventory(&path, force),
        },
        Cmd::Ai { request, output } => ai_generate(&request.join(" "), output).await,
        Cmd::Doctor {
            file,
            host,
            user,
            password,
            port,
            ai,
        } => doctor(file, host, &user, password, port, ai).await,
        Cmd::Run {
            host,
            user,
            password,
            port,
            cmd,
        } => run_adhoc(&host, &user, password, port, &cmd.join(" ")).await,
        Cmd::Agent { plan, task_plan } => run_agent(plan, task_plan).await,
        Cmd::Component(args) => component_shortcut(args).await,
    }
}

async fn run_adhoc(
    host: &str,
    user: &str,
    password: Option<String>,
    port: u16,
    cmd: &str,
) -> Result<()> {
    let pw = password
        .or_else(|| std::env::var("CRATER_SSH_PASSWORD").ok())
        .ok_or_else(|| anyhow!("--password (or CRATER_SSH_PASSWORD) required"))?;
    let exec = SshExecutor::connect(host, port, user, &pw).await?;
    let out = exec.run(cmd).await?;
    print!("{}", out.stdout);
    if !out.stderr.trim().is_empty() {
        eprintln!("--- stderr ---\n{}", out.stderr);
    }
    println!("--- exit {} ---", out.code);
    std::process::exit(if out.ok() { 0 } else { out.code });
}

#[allow(clippy::too_many_arguments)]
async fn push_file(
    host: &str,
    user: &str,
    password: Option<String>,
    port: u16,
    src: &Path,
    dst: &str,
    chmod: Option<String>,
) -> Result<()> {
    let pw = password
        .or_else(|| std::env::var("CRATER_SSH_PASSWORD").ok())
        .ok_or_else(|| anyhow!("--password (or CRATER_SSH_PASSWORD) required"))?;
    let data = std::fs::read(src).map_err(|e| anyhow!("read {}: {e}", src.display()))?;
    println!(
        "Pushing {} ({} bytes) -> {user}@{host}:{dst} ...",
        src.display(),
        data.len()
    );
    let exec = SshExecutor::connect(host, port, user, &pw).await?;
    exec.write_file(dst, &data).await?;
    if let Some(mode) = chmod {
        let out = exec.run(&format!("chmod {mode} '{dst}'")).await?;
        if !out.ok() {
            anyhow::bail!("chmod {mode} {dst} failed: {}", out.stderr.trim());
        }
    }
    // Confirm via sha256 on the remote side.
    let out = exec.run(&format!("sha256sum '{dst}' | cut -d' ' -f1")).await?;
    println!("remote sha256: {}", out.stdout.trim());
    println!("local  sha256: {}", crater_core::bundle::sha256_hex(&data));
    println!("Done.");
    Ok(())
}

/// `crater <name> [flags]` ≡ `crater apply <name> [flags]` (D-046): the bare
/// name routes to the named task `tasks/<name>.yaml`. The old component-spec
/// shortcut is gone — everything is a task.
async fn component_shortcut(args: Vec<String>) -> Result<()> {
    let mut name: Option<String> = None;
    let mut host: Option<String> = None;
    let mut user = String::from("root");
    let mut password: Option<String> = std::env::var("CRATER_SSH_PASSWORD").ok();
    let mut key: Option<PathBuf> = None;
    let mut port: u16 = 22;
    let mut dry_run = false;
    let mut shell = false;
    let mut inventory: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => {
                i += 1;
                host = args.get(i).cloned();
            }
            "--user" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    user = v.clone();
                }
            }
            "--password" => {
                i += 1;
                password = args.get(i).cloned();
            }
            "--key" => {
                i += 1;
                key = args.get(i).map(PathBuf::from);
            }
            "--port" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    port = v.parse().map_err(|_| anyhow!("invalid --port: {v}"))?;
                }
            }
            "-i" | "--inventory" => {
                i += 1;
                inventory = args.get(i).map(PathBuf::from);
            }
            "--dry-run" => dry_run = true,
            "--shell" => shell = true,
            s if !s.starts_with('-') && name.is_none() => name = Some(s.to_string()),
            other => anyhow::bail!(
                "unknown flag '{other}' for `crater <name>`; use `crater apply` for the full surface"
            ),
        }
        i += 1;
    }
    let name = name.ok_or_else(|| anyhow!("missing task name"))?;
    apply_source(None, Some(name), None, inventory, host, user, password, key, port, dry_run, shell, false)
        .await
}


/// Normalize `uname -m` output to crater's arch naming (matches dist/ names).
fn norm_arch(uname_m: &str) -> String {
    match uname_m.trim() {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => other,
    }
    .to_string()
}

/// Where to look for a bundled musl static binary `crater-linux-<arch>`:
/// `$CRATER_AGENT_DIR`, beside the control binary (+ its `dist/`), and `./dist/`.
fn musl_candidates(arch: &str) -> Vec<PathBuf> {
    let name = format!("crater-linux-{arch}");
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("CRATER_AGENT_DIR") {
        v.push(PathBuf::from(d).join(&name));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join(&name));
            v.push(dir.join("dist").join(&name));
            if let Some(parent) = dir.parent() {
                v.push(parent.join("dist").join(&name));
            }
        }
    }
    v.push(PathBuf::from("dist").join(&name));
    v
}

/// Choose the agent binary to ship to `exec`'s target. Order:
/// 1. explicit `--agent-bin`; 2. a bundled musl static for the target's arch
/// (portable, also dodges glibc skew on the same arch); 3. the control binary
/// iff the target arch matches; else error with guidance.
async fn select_agent_binary(
    exec: &dyn Executor,
    agent_bin: Option<&Path>,
) -> Result<(PathBuf, String)> {
    if let Some(p) = agent_bin {
        return Ok((p.to_path_buf(), "explicit --agent-bin".into()));
    }
    let target_arch = norm_arch(&exec.run("uname -m").await?.stdout);
    for cand in musl_candidates(&target_arch) {
        if cand.is_file() {
            return Ok((cand, format!("bundled musl static for {target_arch}")));
        }
    }
    if target_arch == std::env::consts::ARCH {
        let exe = std::env::current_exe()
            .map_err(|e| anyhow!("cannot locate current crater binary: {e}"))?;
        return Ok((exe, format!("control binary (same arch {target_arch})")));
    }
    anyhow::bail!(
        "no agent binary for target arch '{target_arch}' (control is '{}'). Build one with \
         `scripts/build-musl.sh {target_arch}` and pass --agent-bin, or run with --shell.",
        std::env::consts::ARCH
    )
}

/// Self-bootstrap agent mode (D-019/D-027, the default): push the crater binary
/// + the lowered plan to the target, then run `crater agent --plan` THERE so the
/// plan executes locally in one shot — fewer SSH round-trips, and the foundation
/// for OCI unpack / richer local logic. The binary is cached on the target (by
/// sha256), so it's pushed once per version; only the plan file is transient.
/// Push the crater binary to the target (cached by sha256 at
/// `/var/lib/crater/crater`) and return that path. Shared by component
/// (`--plan`) and task (`--task-plan`) agent runs.
async fn push_agent_binary(exec: &dyn Executor, agent_bin: Option<&Path>) -> Result<&'static str> {
    // Pick the binary to ship: explicit override > a bundled musl static
    // matching the target's arch > the control binary (only if same arch).
    let (bin_path, how) = select_agent_binary(exec, agent_bin).await?;
    let bytes = std::fs::read(&bin_path)
        .map_err(|e| anyhow!("read agent binary {}: {e}", bin_path.display()))?;
    let want = crater_core::bundle::sha256_hex(&bytes);
    let remote_bin = "/var/lib/crater/crater";
    let cached = exec
        .run(&format!("sha256sum {remote_bin} 2>/dev/null | cut -d' ' -f1"))
        .await?;
    if cached.ok() && cached.stdout.trim() == want {
        info!("[{}] agent: binary cached (sha256 match), reusing [{how}]", exec.label());
    } else {
        info!(
            "[{}] agent: pushing {} ({} bytes) [{how}]",
            exec.label(),
            bin_path.display(),
            bytes.len()
        );
        exec.run("mkdir -p /var/lib/crater").await?;
        exec.write_file(remote_bin, &bytes).await?;
        exec.run(&format!("chmod +x {remote_bin}")).await?;
    }
    Ok(remote_bin)
}

/// Forward an agent run's output verbatim and map a failed exit to an error.
fn forward_agent_output(out: &crater_core::executor::CmdOutput) -> Result<()> {
    print!("{}", out.stdout);
    if !out.stderr.trim().is_empty() {
        eprint!("{}", out.stderr);
    }
    if !out.ok() {
        if out.code == 126 || out.code == 127 {
            anyhow::bail!(
                "agent binary failed to execute on target (exit {}; likely arch/libc \
                 mismatch). Re-run with --shell for the agentless shell executor, or pass \
                 --agent-bin <musl-static-build>.",
                out.code
            );
        }
        anyhow::bail!("agent exited with code {}", out.code);
    }
    Ok(())
}


/// Run a task plan on the target via the self-bootstrap agent (D-044): push the
/// binary + the rendered task plan (steps + policy + handlers), then the target
/// runs `execute_task` locally. `--shell`/local callers use the control-plane
/// `execute_task` instead.
async fn run_task_via_agent(
    exec: &dyn Executor,
    steps: &[engine::TaskStep],
    handlers: &BTreeMap<String, Op>,
    agent_bin: Option<&Path>,
) -> Result<()> {
    let remote_bin = push_agent_binary(exec, agent_bin).await?;
    let remote_plan = "/tmp/crater-task-plan.yaml";
    exec.write_file(remote_plan, engine::task_plan_to_yaml(steps, handlers)?.as_bytes())
        .await?;
    info!("[{}] agent: executing task on target ↓", exec.label());
    let out = exec
        .run(&format!("{remote_bin} agent --task-plan {remote_plan}"))
        .await?;
    let _ = exec.run(&format!("rm -f {remote_plan}")).await;
    forward_agent_output(&out)
}

/// `crater agent --plan <file>`: run ON the target. Reads a lowered plan and
/// executes it locally (the control machine pushed the plan + this binary).
async fn run_agent(plan: Option<PathBuf>, task_plan: Option<PathBuf>) -> Result<()> {
    // Task plan (D-044): steps + handlers, run via execute_task locally.
    if let Some(tp) = task_plan {
        let text = std::fs::read_to_string(&tp)
            .map_err(|e| anyhow!("read task plan {}: {e}", tp.display()))?;
        let plan = engine::task_plan_from_yaml(&text)?;
        info!("agent: executing task ({} step(s)) locally", plan.steps.len());
        return engine::execute_task(&plan.steps, &plan.handlers, &LocalExecutor).await;
    }
    let plan_path = plan.ok_or_else(|| anyhow!("crater agent: --plan or --task-plan required"))?;
    let text = std::fs::read_to_string(&plan_path)
        .map_err(|e| anyhow!("read plan {}: {e}", plan_path.display()))?;
    let ops = engine::plan_from_yaml(&text)?;
    info!("agent: executing {} step(s) locally", ops.len());
    engine::execute(&ops, &LocalExecutor).await
}



/// `crater apply <source>` — one entry point for online & offline (D-020).
/// Auto-detect the source kind and route; the execution engine (idempotency,
/// tracing, agent/shell) is shared — online vs offline differ only in where
/// artifacts come from.
#[allow(clippy::too_many_arguments)]
async fn apply_source(
    name: Option<String>,
    source: Option<String>,
    file: Option<PathBuf>,
    inventory: Option<PathBuf>,
    host: Option<String>,
    user: String,
    password: Option<String>,
    key: Option<PathBuf>,
    port: u16,
    dry_run: bool,
    shell: bool,
    teardown: bool,
) -> Result<()> {
    let verb = if teardown { "delete" } else { "apply" };
    // `<source>` positional, else `-f`.
    let src = source
        .or_else(|| file.map(|p| p.display().to_string()))
        .ok_or_else(|| anyhow!("{verb} needs a <source>: a task.yaml, an x.oci bundle, an image ref, or a named task"))?;
    let path = PathBuf::from(&src);
    if let Some(n) = &name {
        info!("{verb}: deployment '{n}' ← {src}");
    }

    if path.is_file() && bundle::is_oci_archive(&path) {
        // Offline: OCI bundle. Targets from CLI (D-020: never inside the image)
        // — `-i inventory.yaml`, `--host a,b`, or none → local.
        info!("{verb}: {src} → offline (OCI bundle)");
        let hosts = target_hosts(inventory.as_deref(), host, &user, password, key, port)?;
        return apply_oci_bundle(&path, hosts, !dry_run, shell, teardown).await;
    }
    if path.is_file() {
        // A task file (top-level `actions:`, D-037). Component specs are gone.
        if crater_core::task::is_task_file(&path) {
            info!("{verb}: {src} → task");
            let hosts = target_hosts(inventory.as_deref(), host, &user, password, key, port)?;
            let groups = inventory_groups(inventory.as_deref())?;
            return apply_task(&path, hosts, groups, None, !dry_run, shell, teardown).await;
        }
        anyhow::bail!(
            "{src}: not a task file (needs top-level `actions:`). Component specs are no \
             longer supported — write a task."
        );
    }
    // Image reference (registry/store): has a registry path or a tag, not a file.
    if src.contains('/') || src.contains(':') {
        info!("{verb}: {src} → image (local store / registry)");
        let hosts = target_hosts(inventory.as_deref(), host, &user, password, key, port)?;
        return apply_image_ref(&src, hosts, !dry_run, teardown).await;
    }
    // Named task in the library: `crater apply <name>` → tasks/<name>.yaml
    // (D-043). This is the only bare-name path now (D-046): component shortcut
    // and component-spec fleet are gone — everything is a task.
    let named = PathBuf::from("tasks").join(format!("{src}.yaml"));
    if named.is_file() && crater_core::task::is_task_file(&named) {
        info!("{verb}: {src} → named task (tasks/{src}.yaml)");
        let hosts = target_hosts(inventory.as_deref(), host, &user, password, key, port)?;
        let groups = inventory_groups(inventory.as_deref())?;
        return apply_task(&named, hosts, groups, None, !dry_run, shell, teardown).await;
    }
    anyhow::bail!(
        "'{src}': not a file, image ref, or named task. Put a task at tasks/{src}.yaml, \
         or pass a path / -f <file> / an image reference."
    )
}

/// `crater apply <task>.yaml` (D-037): run a generic task across the targets.
/// Control flow (when-filter, needs-ordering) is in the engine. Host
/// orchestration mirrors the component pipeline (D-030/D-031): hosts grouped by
/// role-set run group-by-group (so a producer's `register` lands in `hostvars`
/// before a consumer group reads it), parallel within a group.
async fn apply_task(
    path: &Path,
    hosts: Vec<crater_core::spec::Host>,
    groups: BTreeMap<String, Vec<String>>,
    offline_blobmap: Option<BTreeMap<String, PathBuf>>,
    do_apply: bool,
    do_shell: bool,
    teardown: bool,
) -> Result<()> {
    use crater_core::task::TaskFile;
    let task = TaskFile::from_yaml_file(path)?;
    // Delete is opt-in (D-049): a task only has delete capability if it authored
    // a `teardown:`. No auto-inversion of `actions:` — real cleanup targets
    // runtime state the install steps never created.
    if teardown && task.teardown.is_empty() {
        anyhow::bail!(
            "task '{}' defines no `teardown:` — it has no delete capability \
             (delete is opt-in; author a teardown to enable it)",
            task.name
        );
    }
    let spec_dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    // `hosts` group filter (D-037-b/D-043): `all` → every target; else expand the
    // group name to a role set (nested `groups:` resolved) and keep hosts whose
    // roles intersect it. Hosts with no roles (CLI --host / local) always match.
    let hosts: Vec<crater_core::spec::Host> = if task.hosts == "all" {
        hosts
    } else {
        let mut seen = BTreeSet::new();
        let wanted = expand_group(&task.hosts, &groups, &mut seen);
        hosts
            .into_iter()
            .filter(|h| h.roles.is_empty() || h.roles.iter().any(|r| wanted.contains(r)))
            .collect()
    };
    if hosts.is_empty() {
        anyhow::bail!("task hosts='{}' matched no target host", task.hosts);
    }
    info!(
        "{} '{}': {} action(s), hosts={}, {} target(s), mode={}",
        if teardown { "teardown" } else { "task" },
        task.name,
        if teardown { task.teardown.len() } else { task.actions.len() },
        task.hosts,
        hosts.len(),
        if do_apply { "apply" } else { "dry-run" }
    );

    let mut hostvars: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    if !do_apply {
        for h in &hosts {
            run_task_on_host(&task, h, &spec_dir, &hostvars, offline_blobmap.as_ref(), false, do_shell, teardown).await?;
        }
        info!("dry-run only; omit --dry-run to execute");
        return Ok(());
    }

    let forks = forks_limit();
    for group in group_hosts_by_role(&hosts) {
        let results: Vec<Result<(String, Vec<(String, String)>)>> = futures::stream::iter(
            group
                .iter()
                .map(|h| run_task_on_host(&task, h, &spec_dir, &hostvars, offline_blobmap.as_ref(), true, do_shell, teardown)),
        )
        .buffer_unordered(forks)
        .collect()
        .await;
        let mut first_err = None;
        for r in results {
            match r {
                Ok((host_name, regs)) => {
                    for (k, v) in regs {
                        hostvars.entry(host_name.clone()).or_default().insert(k, v);
                    }
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        if let Some(e) = first_err {
            return Err(e);
        }
    }
    Ok(())
}

async fn run_task_on_host(
    task: &crater_core::task::TaskFile,
    host: &crater_core::spec::Host,
    spec_dir: &Path,
    hostvars: &BTreeMap<String, BTreeMap<String, String>>,
    offline_blobmap: Option<&BTreeMap<String, PathBuf>>,
    do_apply: bool,
    do_shell: bool,
    teardown: bool,
) -> Result<(String, Vec<(String, String)>)> {
    if host.is_local() {
        info!("▶ host {} (local)", host.name);
    } else {
        info!("▶ host {} ({})", host.name, host.address);
    }
    let exec = connect_executor(host, do_apply).await?;
    let (osf, target_arch) = if do_apply {
        (os::detect_via(exec.as_ref()).await, arch::detect_via(exec.as_ref()).await)
    } else {
        // Dry-run preview: no target connection — use the control machine's arch
        // so `place` can resolve a concrete variant for the plan.
        (OsFamily::Unknown, arch::detect_local())
    };
    let ver = task
        .vars
        .get("version")
        .cloned()
        .unwrap_or_else(|| "latest".into());
    let mut ctx = PlanContext::new(osf, ver, spec_dir.to_path_buf());
    ctx.target_arch = target_arch;
    for (k, v) in &task.vars {
        ctx.vars.insert(k.clone(), v.clone());
    }
    // Other hosts' registered facts become template vars (D-030).
    for (h, kv) in hostvars {
        for (k, v) in kv {
            ctx.vars.insert(format!("hostvars.{h}.{k}"), v.clone());
        }
    }
    for m in &task.materials {
        ctx.add_material(m.clone());
    }
    // Offline (recipe-replay, D-045): `place` pushes packed blobs from control.
    if let Some(bm) = offline_blobmap {
        ctx.offline_blobs = Some(bm.clone());
    }
    // delete → run the authored `teardown:` actions; apply → `actions`.
    let action_list = if teardown { &task.teardown } else { &task.actions };
    let steps = engine::plan_from_task(action_list, &ctx)?;
    let handlers = engine::plan_handlers(&task.handlers, &ctx)?;
    info!(
        "[{}] {} {} — {} step(s)",
        host.name,
        if teardown { "teardown" } else { "task" },
        task.name,
        steps.len()
    );
    if !do_apply {
        let ops: Vec<Op> = steps.iter().map(|s| s.op.clone()).collect();
        print_plan(&ops);
        return Ok((host.name.clone(), Vec::new()));
    }
    // Default: self-bootstrap agent runs the task plan on the target (D-044).
    // Offline (blobs on control), --shell, or local → control-plane execute_task.
    if offline_blobmap.is_some() || do_shell || host.is_local() {
        engine::execute_task(&steps, &handlers, exec.as_ref()).await?;
    } else {
        run_task_via_agent(exec.as_ref(), &steps, &handlers, None).await?;
    }

    // Capture this host's facts for later groups (D-030).
    let mut registered: Vec<(String, String)> = Vec::new();
    for reg in &task.register {
        let out = exec.run(&engine::render(&reg.cmd, &ctx.vars)?).await?;
        if !out.ok() {
            anyhow::bail!(
                "register '{}' on {} failed (exit {}): {}",
                reg.name,
                host.name,
                out.code,
                out.stderr.trim()
            );
        }
        let val = out.stdout.trim().to_string();
        info!("[{}] registered {} ({} bytes)", host.name, reg.name, val.len());
        registered.push((reg.name.clone(), val));
    }
    Ok((host.name.clone(), registered))
}




/// Group hosts by their role-set (sorted), preserving each signature's first
/// appearance order. Same-role hosts land in one group (run in parallel);
/// distinct role-sets form ordered groups (run sequentially) so a producer role
/// registers its facts before a consumer role consumes them (F17 + D-030).
fn group_hosts_by_role(hosts: &[crater_core::spec::Host]) -> Vec<Vec<&crater_core::spec::Host>> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<&crater_core::spec::Host>> = BTreeMap::new();
    for h in hosts {
        let mut roles = h.roles.clone();
        roles.sort();
        let sig = roles.join(",");
        if !groups.contains_key(&sig) {
            order.push(sig.clone());
        }
        groups.entry(sig).or_default().push(h);
    }
    order.into_iter().filter_map(|s| groups.remove(&s)).collect()
}

/// Max hosts to deploy concurrently within a group (`CRATER_FORKS`, default 10).
fn forks_limit() -> usize {
    std::env::var("CRATER_FORKS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(10)
}

/// Starter inventory written by `crater create inventory`. Comments document
/// every field; password/key are mutually exclusive (key wins, `~` expands).
const INVENTORY_TEMPLATE: &str = r#"# crater inventory —— 部署目标主机清单。
# 用法:crater apply <动作> -i <此文件>(大量机器、每台各自凭据)。
#
# 每台主机至少 name + address;认证用 password 或 key(二选一,key 优先)。
# user 默认 root,port 默认 22。roles 可选(组件/task 按 role 选主机)。
inventory:
  hosts:
    # ① 密码认证
    - name: web1
      address: 192.168.1.11
      user: root
      port: 22
      password: "changeme"
      # roles: [web]

    # ② SSH 私钥认证(适合禁用密码登录的机群;~ 会自动展开为 $HOME)
    - name: web2
      address: 192.168.1.12
      user: ubuntu
      key: ~/.ssh/id_rsa
      # roles: [web]

    # ③ 再一台
    - name: db1
      address: 192.168.1.20
      password: "changeme"
      # roles: [db]
"#;

/// `crater create inventory [path]`: write a sample inventory for the user to
/// edit. Refuses to clobber an existing file unless `--force`.
fn create_inventory(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!("{} 已存在(加 --force 覆盖)", path.display());
    }
    std::fs::write(path, INVENTORY_TEMPLATE)?;
    info!(
        "已生成 {} —— 编辑主机后用:crater apply <动作> -i {}",
        path.display(),
        path.display()
    );
    Ok(())
}

/// Expand a leading `~` to `$HOME` (std::fs / russh don't do shell expansion),
/// so an inventory `key: ~/.ssh/id_rsa` or `--key ~/...` works.
fn expand_tilde(p: &Path) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    p.to_path_buf()
}

/// Build an executor for a host: local (dry-run or `@local`), SSH key, or SSH
/// password. Shared by component (`run_host`) and task (`run_task_on_host`).
async fn connect_executor(
    host: &crater_core::spec::Host,
    do_apply: bool,
) -> Result<Box<dyn Executor>> {
    if !do_apply || host.is_local() {
        return Ok(Box::new(LocalExecutor));
    }
    if let Some(keypath) = &host.key {
        return Ok(Box::new(
            SshExecutor::connect_auth(
                &host.address,
                host.port,
                &host.user,
                &crater_core::executor::SshAuth::Key {
                    path: expand_tilde(keypath),
                    passphrase: None,
                },
            )
            .await?,
        ));
    }
    if let Some(pw) = host.password.as_deref().filter(|s| !s.is_empty()) {
        return Ok(Box::new(
            SshExecutor::connect(&host.address, host.port, &host.user, pw).await?,
        ));
    }
    anyhow::bail!("host {} needs --password or --key", host.name)
}


// ---------------------------------------------------------------------------
// build (→ local store) / save (→ oci file) / deploy
// ---------------------------------------------------------------------------

/// `crater build -f spec [-t ref]`: build the B 类 artifact(s) and store them in
/// the local store (~/.crater/store), like `docker build`. Export with `save`.
async fn build_to_store(file: &Path, tag: Option<String>, arch_filter: &[String]) -> Result<()> {
    // `crater build` only builds tasks now (D-046): a task → B 类 artifact whose
    // recipe IS the task YAML.
    if !crater_core::task::is_task_file(file) {
        anyhow::bail!(
            "{}: not a task file (needs top-level `actions:`). `crater build` builds tasks.",
            file.display()
        );
    }
    build_task_to_store(file, tag, arch_filter).await
}

/// Build a task into a B 类 OCI artifact (D-045): fetch its `binary` materials,
/// store them + the task YAML as the recipe, tag, into the local store. Loaded
/// by recipe-replay through `plan_from_task` (offline).
async fn build_task_to_store(file: &Path, tag: Option<String>, arch_filter: &[String]) -> Result<()> {
    use crater_core::component::MaterialKind;
    use crater_core::task::TaskFile;

    let task = TaskFile::from_yaml_file(file)?;
    let ver = task
        .vars
        .get("version")
        .cloned()
        .unwrap_or_else(|| "latest".into());
    let reference = tag.unwrap_or_else(|| format!("crater/{}:{ver}", task.name));
    let spec_dir = file.parent().unwrap_or_else(|| Path::new("."));
    let online = OnlineSource::with_default_mirrors();
    // Optional --arch narrowing (D-048): pack only these arches' variants.
    let want_arch: Vec<crater_core::arch::Arch> =
        arch_filter.iter().map(|s| crater_core::arch::Arch::from_uname(s)).collect();

    // Fetch binary materials, keyed by material NAME (or name@arch for an
    // arch-specific variant, D-048) — the same key `place` resolves offline.
    let mut ctx = PlanContext::new(OsFamily::Unknown, ver.clone(), spec_dir.to_path_buf());
    ctx.offline_blobs = Some(BTreeMap::new()); // rendered_url yields raw URLs
    let mut materials: Vec<(String, Vec<u8>)> = Vec::new();
    for m in &task.materials {
        if m.kind == MaterialKind::Binary {
            // Skip variants outside an explicit --arch filter (neutral always kept).
            if !want_arch.is_empty() {
                if let Some(a) = m.arch {
                    if !want_arch.contains(&a) {
                        continue;
                    }
                }
            }
            if let Some(tmpl) = &m.url_tmpl {
                let raw = ctx.rendered_url(tmpl)?;
                let url = online.rewrite(&raw);
                let key = PlanContext::material_blob_key(m);
                info!("  fetch material {key} <- {raw}");
                let (data, _) = source::fetch_best(&url)
                    .await
                    .map_err(|e| anyhow!("fetch material {key}: {e}"))?;
                materials.push((key, data));
            }
        }
    }

    // Recipe = the task YAML verbatim.
    let recipe = std::fs::read(file)?;
    let stage_root = std::env::temp_dir().join(format!("crater-taskimg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage_root);
    let stage = bundle::BundleStage::new(stage_root.clone())?;
    let ir = stage.store_component_artifact(&reference, &task.name, &ver, "task", &recipe, &materials)?;
    info!("  {} → task artifact {reference}: recipe + {} material(s)", task.name, materials.len());
    stage.write_artifact_index(&[ir])?;
    let tmp_oci = std::env::temp_dir().join(format!("crater-taskbuild-{}.oci", std::process::id()));
    bundle::pack(&stage_root, &tmp_oci)?;
    let store = ImageStore::open()?;
    let refs = store.import_all(&tmp_oci)?;
    let _ = std::fs::remove_dir_all(&stage_root);
    let _ = std::fs::remove_file(&tmp_oci);
    for r in &refs {
        info!("built {r} → 本地库(~/.crater/store)");
    }
    Ok(())
}



/// Offline deploy via the SAME pipeline as online (D-020 单管线): unpack the OCI
/// bundle, build a synthetic spec (components from the bundle's crater-manifest,
/// inventory from CLI), set `Artifacts::Offline`, and run `run_pipeline`. So
/// offline gets the same host grouping / parallelism / register / idempotency —
/// the only difference is where artifacts come from (the bundle, on control).
async fn apply_oci_bundle(
    bundle_file: &Path,
    hosts: Vec<crater_core::spec::Host>,
    do_apply: bool,
    do_shell: bool,
    teardown: bool,
) -> Result<()> {
    let dest_root = std::env::temp_dir().join(format!("crater-deploy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest_root);
    bundle::unpack(bundle_file, &dest_root)?; // unpacks the OCI layout into dest_root

    // crater task artifact (B 类, D-045) → replay via the task pipeline. (A
    // bundle is homogeneous: built from one task.)
    let recipe_dir = dest_root.join("__components");
    let mats = bundle::read_artifact_components(&dest_root, &recipe_dir)?;
    if mats.is_empty() {
        let _ = std::fs::remove_dir_all(&dest_root);
        anyhow::bail!(
            "{}: not a crater task artifact (legacy component bundles are no longer \
             supported — rebuild with `crater build -f tasks/<name>.yaml`)",
            bundle_file.display()
        );
    }
    if !mats
        .iter()
        .all(|mc| crater_core::task::is_task_file(&recipe_dir.join(&mc.name).join("component.yaml")))
    {
        let _ = std::fs::remove_dir_all(&dest_root);
        anyhow::bail!(
            "{}: legacy component artifact; rebuild as a task",
            bundle_file.display()
        );
    }
    info!("offline (task artifact): {} task(s)", mats.len());
    for mc in mats {
        let recipe_file = recipe_dir.join(&mc.name).join("component.yaml");
        apply_task(&recipe_file, hosts.clone(), Default::default(), Some(mc.blobmap), do_apply, do_shell, teardown)
            .await?;
    }
    let _ = std::fs::remove_dir_all(&dest_root);
    Ok(())
}

/// Back-compat `crater deploy --bundle x --host H`: single-host offline apply.
async fn deploy_bundle(
    bundle_file: &Path,
    host: Option<String>,
    user: &str,
    password: Option<String>,
    port: u16,
    do_apply: bool,
) -> Result<()> {
    let hosts = target_hosts(None, host, user, password, None, port)?;
    apply_oci_bundle(bundle_file, hosts, do_apply, false, false).await
}

/// The inventory's named `groups:` (from `-i`), else empty (D-043).
fn inventory_groups(inv: Option<&Path>) -> Result<BTreeMap<String, Vec<String>>> {
    match inv {
        Some(p) => Ok(CraterSpec::from_yaml_file(p)?.inventory.groups),
        None => Ok(BTreeMap::new()),
    }
}

/// Expand a group/role name to a set of leaf role names (D-043). A name found in
/// `groups` recurses over its members (nestable); otherwise it IS a role.
/// `seen` guards against cycles.
fn expand_group(
    name: &str,
    groups: &BTreeMap<String, Vec<String>>,
    seen: &mut BTreeSet<String>,
) -> BTreeSet<String> {
    if !seen.insert(name.to_string()) {
        return BTreeSet::new();
    }
    match groups.get(name) {
        Some(members) => members
            .iter()
            .flat_map(|m| expand_group(m, groups, seen))
            .collect(),
        None => {
            let mut s = BTreeSet::new();
            s.insert(name.to_string());
            s
        }
    }
}

/// Resolve deploy targets for image/oci/component sources, three layers:
///   `-i inventory.yaml`  → fleet, per-host creds (from the file);
///   `--host a,b,c`        → small fleet, ONE shared credential (user+password|key);
///   neither               → a single LOCAL host (runs on the control machine).
///
/// Heterogeneous per-host credentials are intentionally NOT expressible via
/// `--host` (it shares one credential) — use an inventory file for that.
fn target_hosts(
    inv: Option<&Path>,
    host: Option<String>,
    user: &str,
    password: Option<String>,
    key: Option<PathBuf>,
    port: u16,
) -> Result<Vec<crater_core::spec::Host>> {
    if let Some(p) = inv {
        let spec = CraterSpec::from_yaml_file(p)?;
        if spec.inventory.hosts.is_empty() {
            anyhow::bail!("inventory {} has no hosts", p.display());
        }
        Ok(spec.inventory.hosts)
    } else if let Some(h) = host {
        let hosts: Vec<_> = h
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|addr| crater_core::spec::Host {
                name: addr.to_string(),
                address: addr.to_string(),
                user: user.to_string(),
                port,
                password: password.clone(),
                key: key.clone(),
                roles: vec![],
            })
            .collect();
        if hosts.is_empty() {
            anyhow::bail!("--host given but no addresses parsed");
        }
        Ok(hosts)
    } else {
        // No target → local install on the control machine.
        Ok(vec![crater_core::spec::Host::local()])
    }
}


// ---------------------------------------------------------------------------
// Image management: images / pull / push / apply <ref>
// ---------------------------------------------------------------------------

async fn list_images() -> Result<()> {
    let store = ImageStore::open()?;
    let imgs = store.list()?;
    if imgs.is_empty() {
        info!("no images in local store ({})", store.root.display());
        return Ok(());
    }
    println!(
        "{:<48} {:<16} {:>11} {:>13}",
        "REFERENCE", "DIGEST", "DISK USAGE", "CONTENT SIZE"
    );
    for i in imgs {
        let short = i.digest.trim_start_matches("sha256:").chars().take(12).collect::<String>();
        println!(
            "{:<48} {:<16} {:>11} {:>13}",
            i.reference,
            short,
            human_size(i.disk_usage),
            human_size(i.content_size)
        );
    }
    Ok(())
}

/// Human-readable byte size, docker-style decimal units (1000-based: B/kB/MB/GB).
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes}B");
    }
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    format!("{v:.1}{}", UNITS[u])
}

async fn pull_image(reference: &str) -> Result<()> {
    let store = ImageStore::open()?;
    info!("pulling {reference} → local store ...");
    store.pull(reference).await?;
    info!("pulled {reference}");
    Ok(())
}

async fn push_image(reference: &str) -> Result<()> {
    let store = ImageStore::open()?;
    if !store.has(reference) {
        anyhow::bail!("{reference} not in local store (pull or build it first)");
    }
    info!("pushing {reference} → registry ...");
    store.push(reference).await?;
    info!("pushed {reference}");
    Ok(())
}

/// `crater apply <image-ref>`: resolve from the local store (pull on miss). A
/// **crater component artifact** (B 类, D-032) → recipe-replay via `run_pipeline`
/// (materials feed the recipe offline). A plain container image → extract its
/// rootfs layers to `/` on each host (parallel). crater-native, no runtime.
async fn apply_image_ref(
    reference: &str,
    hosts: Vec<crater_core::spec::Host>,
    do_apply: bool,
    teardown: bool,
) -> Result<()> {
    let store = ImageStore::open()?;
    if !store.has(reference) {
        info!("{reference} not in local store → pulling");
        store.pull(reference).await?;
    }

    // crater task artifact (B 类) → recipe-replay via the task pipeline (D-045).
    let manifest = store.resolve_manifest(reference)?;
    let recipe_dir = std::env::temp_dir().join(format!("crater-ref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&recipe_dir);
    if let Some(mc) = bundle::materialize_component(&manifest, &store.blobs_dir(), &recipe_dir)? {
        let recipe_file = recipe_dir.join(&mc.name).join("component.yaml");
        if crater_core::task::is_task_file(&recipe_file) {
            info!("image {reference}: crater task artifact → recipe-replay");
            let res = apply_task(&recipe_file, hosts, Default::default(), Some(mc.blobmap), do_apply, true, teardown).await;
            let _ = std::fs::remove_dir_all(&recipe_dir);
            return res;
        }
        let _ = std::fs::remove_dir_all(&recipe_dir);
        anyhow::bail!(
            "'{reference}' is a legacy component artifact; rebuild it as a task \
             (crater build -f tasks/<name>.yaml)"
        );
    }

    // Plain container image → rootfs overlay (extract all layers to /).
    let layers = store.resolve_layers(reference)?;
    info!("image {reference}: plain image, {} layer(s), {} host(s)", layers.len(), hosts.len());
    if !do_apply {
        info!("dry-run; omit --dry-run to install (extract layers to / on each host)");
        return Ok(());
    }
    let forks = forks_limit();
    let results: Vec<Result<()>> = futures::stream::iter(
        hosts.iter().map(|h| install_image_on_host(h, &layers, reference)),
    )
    .buffer_unordered(forks)
    .collect()
    .await;
    for r in results {
        r?;
    }
    Ok(())
}

async fn install_image_on_host(
    host: &crater_core::spec::Host,
    layers: &[PathBuf],
    reference: &str,
) -> Result<()> {
    let pw = host
        .password
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("host {} has no password", host.name))?;
    let exec = SshExecutor::connect(&host.address, host.port, &host.user, pw).await?;
    info!("▶ host {} ({}) ← {reference}", host.name, host.address);
    for (i, layer) in layers.iter().enumerate() {
        let data = bundle::read_file(layer)?;
        let remote = format!("/tmp/crater-layer-{i}.tar");
        exec.write_file(&remote, &data).await?;
        let out = exec
            .run(&format!("tar -xpf {remote} -C / && rm -f {remote}"))
            .await?;
        if !out.ok() {
            anyhow::bail!(
                "[{}] extract layer {}/{} failed (exit {}): {}",
                host.name,
                i + 1,
                layers.len(),
                out.code,
                out.stderr.trim()
            );
        }
        info!("[{}] extracted layer {}/{}", host.name, i + 1, layers.len());
    }
    Ok(())
}


// ---------------------------------------------------------------------------
// M4: AI copilot — natural language -> validated crater.yaml
// ---------------------------------------------------------------------------

/// systemd unit names mentioned by tasks under `tasks/` (their `service` /
/// `systemd_unit` actions). `doctor` derives per-unit journal probes from this
/// data, never hardcoded.
fn known_systemd_units(tasks_dir: &Path) -> Vec<String> {
    use crater_core::component::Action;
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(tasks_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            if let Ok(t) = crater_core::task::TaskFile::from_yaml_file(&p) {
                for step in &t.actions {
                    match &step.action {
                        Action::Service { name, .. } | Action::SystemdUnit { name, .. } => {
                            out.push(name.clone())
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

async fn ai_generate(request: &str, output: Option<PathBuf>) -> Result<()> {
    use crater_core::ai::{self, AiSettings, OpenAiCompatProvider};

    let settings = AiSettings::from_env().ok_or_else(|| {
        anyhow!(
            "AI not configured. Set CRATER_AI_ENDPOINT and CRATER_AI_MODEL (and \
             CRATER_AI_KEY if your endpoint needs one). Works with OpenAI, DeepSeek, \
             Qwen, or an on-prem OpenAI-compatible endpoint."
        )
    })?;
    println!("AI: model={} endpoint={}", settings.model, settings.endpoint);

    let provider = OpenAiCompatProvider::new(settings);
    let (yaml, task) = ai::nl_to_task(&provider, request).await?;

    println!("\n# ---- generated & validated task ----");
    println!("{yaml}");
    println!(
        "# ---- valid task '{}': {} action(s) ----",
        task.name,
        task.actions.len()
    );

    if let Some(out) = output {
        std::fs::write(&out, &yaml)?;
        println!("Wrote {}", out.display());
        println!("Next: crater apply {} (add --dry-run to preview first)", out.display());
    } else {
        println!("(Tip: -o task.yaml to save, then `crater apply task.yaml`.)");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// M5: doctor — offline rule-based diagnosis (+ optional AI)
// ---------------------------------------------------------------------------

async fn doctor(
    file: Option<PathBuf>,
    host: Option<String>,
    user: &str,
    password: Option<String>,
    port: u16,
    use_ai: bool,
) -> Result<()> {
    use crater_core::diagnose;

    // Gather the text to analyze: a local file, or collected from a host.
    let text = if let Some(f) = &file {
        std::fs::read_to_string(f).map_err(|e| anyhow!("read {}: {e}", f.display()))?
    } else if let Some(h) = &host {
        let pw = password
            .clone()
            .or_else(|| std::env::var("CRATER_SSH_PASSWORD").ok())
            .ok_or_else(|| anyhow!("--password required for --host"))?;
        let exec = SshExecutor::connect(h, port, user, &pw).await?;
        // Collect failure signals. Per-unit journals are derived from component
        // data (no hardcoded service names); the rest is product-agnostic.
        let mut probe = String::new();
        for unit in known_systemd_units(&PathBuf::from("tasks")) {
            probe.push_str(&format!(
                "echo '== journal: {unit} =='; journalctl -u {unit} --no-pager -n 50 2>/dev/null; "
            ));
        }
        probe.push_str(
            "echo '== recent errors =='; journalctl -p err --no-pager -n 100 2>/dev/null; \
             echo '== disk =='; df -h 2>/dev/null; \
             echo '== apt =='; tail -n 50 /var/log/apt/term.log 2>/dev/null",
        );
        exec.run(&probe).await?.stdout
    } else {
        return Err(anyhow!("provide --file <log> or --host <ip> to diagnose"));
    };

    let findings = diagnose::diagnose(&text);
    println!(
        "crater doctor — {} built-in rules, {} finding(s)\n",
        diagnose::rule_count(),
        findings.len()
    );
    if findings.is_empty() {
        println!("No known issue signatures matched.");
    } else {
        for (i, f) in findings.iter().enumerate() {
            println!("{}. [{}] {}", i + 1, f.category, f.cause);
            println!("   fix: {}\n", f.fix);
        }
    }

    if use_ai {
        match crater_core::ai::AiSettings::from_env() {
            Some(settings) => {
                use crater_core::ai::{AiProvider, OpenAiCompatProvider};
                println!("--- AI deeper analysis ({}) ---", settings.model);
                let provider = OpenAiCompatProvider::new(settings);
                let sys = "You are an SRE assistant. Given logs, identify the root cause \
                           and give concrete shell commands to fix it. Be concise.";
                // Cap the log size we send.
                let snippet: String = text.chars().take(6000).collect();
                match provider.complete(sys, &snippet).await {
                    Ok(ans) => println!("{ans}"),
                    Err(e) => println!("(AI analysis unavailable: {e})"),
                }
            }
            None => println!("(--ai requested but CRATER_AI_* not configured; rules above stand alone.)"),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_arch_maps_common_aliases() {
        assert_eq!(norm_arch("x86_64\n"), "x86_64");
        assert_eq!(norm_arch("amd64"), "x86_64");
        assert_eq!(norm_arch("aarch64"), "aarch64");
        assert_eq!(norm_arch("arm64"), "aarch64");
        assert_eq!(norm_arch("riscv64"), "riscv64"); // passthrough
    }

    #[test]
    fn musl_candidates_use_arch_specific_name() {
        let c = musl_candidates("aarch64");
        assert!(c.iter().all(|p| p.ends_with("crater-linux-aarch64")));
        assert!(c.iter().any(|p| p == &PathBuf::from("dist/crater-linux-aarch64")));
    }
}
