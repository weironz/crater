//! `crater` CLI — a declarative remote-execution engine (task model).
//!
//! Forms (executes by default; pass --dry-run to only print the plan):
//!   crater apply <task>.yaml [--host a,b | -i inv.yaml] [--key K] [--dry-run|--shell]
//!   crater apply <name>                       # named task → tasks/<name>.yaml
//!   crater <name> [flags]                      # shorthand for `crater apply <name>`
//!   crater apply <image-ref|x.oci> --host H    # deploy an image / offline artifact
//!   crater delete <source> [--host|-i]         # uninstall via the task's teardown: (D-049)
//!   crater task list|show <name>|history       # deployment state; --verify = drift check (D-051/055)
//!   crater ui [--bind H --port N]               # read-only web dashboard (Axum + htmx, D-054)
//!   crater build -f task.yaml [-t ref]         # → B 类 OCI artifact in the local store
//!   crater save <ref> -o x.oci                 # export a stored artifact to a file
//!   crater ai "<request>" [-o task.yaml]       # NL → validated task
//!   crater doctor --file log.txt | --host H    # offline rule-based diagnosis
//!   crater run --host H --password P -- <cmd>  # ad-hoc (ansible -m shell style)
//!   crater agent --task-plan <file>            # internal (runs on the target node)

mod ui;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use tracing::{info, warn};

use crater_core::arch;
use crater_core::bundle;
use crater_core::engine::{self, Op, PlanContext};
use crater_core::executor::{Executor, LocalExecutor, SshExecutor};
use crater_core::os::{self, OsFamily};
use crater_core::source::{self, OnlineSource};
use crater_core::spec::CraterSpec;
use crater_core::state::{self, Marker, StateStore};
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
        /// `<source>`, or a `<name>` deployment label when a second positional
        /// `<source>` follows: `crater apply yq-a yq -i invA` deploys task `yq`
        /// under deployment `yq-a` (D-052: distinguishes independent rollouts of
        /// the same task in `task list`; default deployment = task name).
        arg1: Option<String>,
        /// `<source>` (image ref | x.oci | spec.yaml | component) when the first
        /// positional is a name.
        arg2: Option<String>,
        #[arg(short, long)]
        file: Option<PathBuf>,
        #[command(flatten)]
        target: TargetOpts,
        /// Print the plan without executing.
        #[arg(long)]
        dry_run: bool,
        /// Force the agentless shell executor instead of the default agent.
        #[arg(long)]
        shell: bool,
        /// For an image/artifact `<source>`: pull the FULL closure (all material
        /// layers) and replay it air-gapped (D-087). Default is thin-online —
        /// pull only the recipe + self-authored files, fetch dependencies online
        /// at apply. Ignored for `.oci` bundles (already full) and task files.
        #[arg(long)]
        offline: bool,
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
        #[command(flatten)]
        target: TargetOpts,
        /// Print the teardown plan without executing.
        #[arg(long)]
        dry_run: bool,
        /// Force the agentless shell executor instead of the default agent.
        #[arg(long)]
        shell: bool,
    },
    /// Inspect deployment state (D-051): what crater put where, and history.
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Serve a web dashboard over the deployment state (D-054). Axum + htmx,
    /// pure Rust, htmx embedded (works offline). Default binds localhost only.
    /// Write actions (Verify/Heal, D-058) use `./inventory.yaml` when present.
    Ui {
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        #[arg(long, default_value_t = 8080)]
        port: u16,
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
        /// Override a build-stage param without editing the yaml (D-089), e.g.
        /// `--set version=4.55.1`. Repeatable. Overrides the param's `default`
        /// (and the default tag's `<version>`), so a CI/justfile builds any
        /// version from one source. Drives material URLs + the recipe baked in.
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// Export a stored artifact/image to an oci-archive file (like `docker save`).
    Save {
        /// Reference in the local store (`crater images` to list).
        reference: String,
        /// Output file, e.g. yq.oci
        #[arg(short, long)]
        output: PathBuf,
    },
    /// Inspect a task/OCI's input contract (D-081): params (description/default/
    /// required/stage), the inventory roles it needs, and its materials. Reads a
    /// task file's `params:` or an artifact's embedded (flattened) recipe.
    Inspect {
        /// A task file (`tasks/x.yaml`) or a built artifact ref (`crater/k8s-ha:1.36.1`).
        source: String,
        /// Emit a starter inventory.yaml (required groups + apply-stage params).
        #[arg(long)]
        gen_inventory: bool,
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
    /// lowered task plan locally (pushed here by the control machine). Not for
    /// humans — invoked as `crater agent --task-plan <file>` by `apply`/`delete`
    /// over the agent path (D-019/D-044). Hidden from help.
    #[command(hide = true)]
    Agent {
        /// Path to a serialized task plan (steps + handlers, D-044) to run locally.
        #[arg(long)]
        task_plan: PathBuf,
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
enum TaskCmd {
    /// List deployed tasks, **one row per task** (hosts are an attribute, like
    /// `helm list`). From the control DB by default; `--host`/`-i` reads the
    /// authoritative markers on the targets. Drill into one with `task show`.
    List {
        #[command(flatten)]
        target: TargetOpts,
        /// Drift check: re-run each deployment's verify phase on the target and
        /// report ok/DRIFT (needs `--host`/`-i` to connect).
        #[arg(long)]
        verify: bool,
    },
    /// Show one task's per-host instances (version/applied/source per host).
    Show {
        /// Task name (as in `task list`).
        name: String,
        #[command(flatten)]
        target: TargetOpts,
        /// Drift check: re-run the verify phase per host (needs `--host`/`-i`).
        #[arg(long)]
        verify: bool,
    },
    /// Recent apply/delete history (from the control-side DB).
    History {
        #[arg(long, default_value_t = 20)]
        limit: usize,
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

/// The connection / target-selection six-tuple shared by every fleet command
/// (`apply`/`delete`/`task list`/`task show`). `#[command(flatten)]` keeps each
/// subcommand's surface identical while defining the flags + resolver once.
#[derive(clap::Args, Clone, Debug)]
struct TargetOpts {
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
}

impl TargetOpts {
    /// Resolve to a concrete host list: inventory > `--host` > localhost.
    fn hosts(&self) -> Result<Vec<crater_core::spec::Host>> {
        target_hosts(
            self.inventory.as_deref(),
            self.host.clone(),
            &self.user,
            self.password.clone(),
            self.key.clone(),
            self.port,
        )
    }

    /// Like [`hosts`], but a task with no CLI target falls back to its embedded
    /// inventory before localhost (D-084).
    fn task_hosts(&self, task_path: &Path) -> Result<Vec<crater_core::spec::Host>> {
        task_hosts(
            task_path,
            self.inventory.as_deref(),
            self.host.clone(),
            &self.user,
            self.password.clone(),
            self.key.clone(),
            self.port,
        )
    }
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
            target,
            dry_run,
            shell,
            offline,
        } => {
            // Two positional forms: `apply <source>` or `apply <name> <source>`.
            let (name, source) = match (arg1, arg2) {
                (Some(a), Some(b)) => (Some(a), Some(b)),
                (Some(a), None) => (None, Some(a)),
                (None, _) => (None, None),
            };
            apply_source(name, source, file, target, dry_run, shell, false, offline).await
        }
        Cmd::Delete {
            source,
            file,
            target,
            dry_run,
            shell,
        } => apply_source(None, source, file, target, dry_run, shell, true, false).await,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List { target, verify } => task_list(target, verify).await,
            TaskCmd::Show { name, target, verify } => task_show(&name, target, verify).await,
            TaskCmd::History { limit } => task_history(limit).await,
        },
        Cmd::Ui { bind, port } => ui::serve(&bind, port).await,
        Cmd::Build { file, tag, arch, set } => build_to_store(&file, tag, &arch, &set).await,
        Cmd::Inspect { source, gen_inventory } => inspect_source(&source, gen_inventory).await,
        Cmd::Save { reference, output } => {
            ImageStore::open()?.export_oci_archive(&reference, &output)?;
            info!("saved {reference} → {}", output.display());
            Ok(())
        }
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
        Cmd::Agent { task_plan } => run_agent(&task_plan).await,
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
    let target = TargetOpts { inventory, host, user, password, key, port };
    apply_source(None, Some(name), None, target, dry_run, shell, false, false).await
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

/// `crater agent --task-plan <file>`: run ON the target. Reads a lowered task
/// plan (steps + handlers, D-044) and executes it locally via `execute_task`
/// (the control machine pushed the plan + this binary).
async fn run_agent(task_plan: &Path) -> Result<()> {
    let text = std::fs::read_to_string(task_plan)
        .map_err(|e| anyhow!("read task plan {}: {e}", task_plan.display()))?;
    let plan = engine::task_plan_from_yaml(&text)?;
    info!("agent: executing task ({} step(s)) locally", plan.steps.len());
    // Agent runs one host locally — no cross-host coordination (D-077).
    engine::execute_task(&plan.steps, &plan.handlers, &LocalExecutor, None).await
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
    target: TargetOpts,
    dry_run: bool,
    shell: bool,
    teardown: bool,
    offline: bool,
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
        let hosts = target.hosts()?;
        return apply_oci_bundle(&path, hosts, !dry_run, shell, teardown, &src, name.as_deref()).await;
    }
    if path.is_file() && crater_core::project::is_project_file(&path) {
        // A project (top-level `plays:`, D-083): orchestrate plays in order.
        info!("{verb}: {src} → project");
        return apply_project(&path, name.as_deref(), &target, dry_run, shell, teardown).await;
    }
    if path.is_file() {
        // A task file (top-level `actions:`, D-037). Component specs are gone.
        if crater_core::task::is_task_file(&path) {
            info!("{verb}: {src} → task");
            let hosts = target.task_hosts(&path)?;
            let opts = RunOpts {
                offline_blobmap: None,
                offline: false,
                do_apply: !dry_run,
                do_shell: shell,
                teardown,
                source: src.clone(),
            };
            return apply_task(&path, hosts, opts, name.as_deref(), None, BTreeMap::new()).await;
        }
        anyhow::bail!(
            "{src}: not a task file (needs top-level `actions:`). Component specs are no \
             longer supported — write a task."
        );
    }
    // Image reference (registry/store): has a registry path or a tag, not a file.
    if src.contains('/') || src.contains(':') {
        info!("{verb}: {src} → image (local store / registry)");
        let hosts = target.hosts()?;
        return apply_image_ref(&src, hosts, !dry_run, teardown, &src, name.as_deref(), offline).await;
    }
    // Named task/project: `crater apply <name>` → first match of <name>.yaml under
    // library/ (then tasks/ for back-compat). D-043/D-085.
    if let Some(named) = find_named(&src) {
        if crater_core::project::is_project_file(&named) {
            info!("{verb}: {src} → named project ({})", named.display());
            return apply_project(&named, name.as_deref(), &target, dry_run, shell, teardown).await;
        }
        if crater_core::task::is_task_file(&named) {
            info!("{verb}: {src} → named task ({})", named.display());
            let hosts = target.task_hosts(&named)?;
            let opts = RunOpts {
                offline_blobmap: None,
                offline: false,
                do_apply: !dry_run,
                do_shell: shell,
                teardown,
                source: src.clone(),
            };
            return apply_task(&named, hosts, opts, name.as_deref(), None, BTreeMap::new()).await;
        }
    }
    anyhow::bail!(
        "'{src}': not a file, image ref, or named task/project. Put it under library/<name>.yaml, \
         or pass a path / -f <file> / an image reference."
    )
}

/// `crater apply <project>.yaml` (D-083): run a project's plays in order (delete
/// runs them in REVERSE). Each play resolves its `source` to a task (path or
/// `tasks/<source>.yaml`) and applies it with the play's `hosts`/`vars` overrides.
/// A barrier between plays (each `apply_task` completes first), so ordering like
/// host-init → k8s → cni holds. All plays share one deployment label (the project
/// name, or `--name`), so `task list` groups them.
#[allow(clippy::too_many_arguments)]
async fn apply_project(
    path: &Path,
    name: Option<&str>,
    target: &TargetOpts,
    dry_run: bool,
    shell: bool,
    teardown: bool,
) -> Result<()> {
    use crater_core::project::Project;
    let project = Project::from_yaml_file(path)?;
    let verb = if teardown { "delete" } else { "apply" };
    if project.plays.is_empty() {
        anyhow::bail!("project '{}' 没有 plays", project.name);
    }
    let mut order: Vec<&crater_core::project::Play> = project.plays.iter().collect();
    if teardown {
        order.reverse(); // tear down in reverse: e.g. k8s before host baseline.
    }
    let deployment = name.map(|s| s.to_string()).unwrap_or_else(|| project.name.clone());
    info!(
        "{verb} project '{}': {} play(s){}",
        project.name,
        order.len(),
        if teardown { "(逆序)" } else { "" }
    );
    let total = order.len();
    for (i, play) in order.iter().enumerate() {
        let label = play.name.clone().unwrap_or_else(|| play.source.clone());
        // Resolve source: an explicit path, else a named task/project under library/.
        let src_path = find_named(&play.source).ok_or_else(|| {
            anyhow!(
                "project '{}' play '{label}':source '{}' 未找到(路径或 library/**/{}.yaml)",
                project.name, play.source, play.source
            )
        })?;
        info!(
            "── play {}/{total}: {label}(source={}, hosts={})",
            i + 1,
            play.source,
            play.hosts.as_deref().unwrap_or("<task 默认>")
        );
        let hosts = target.hosts()?;
        // Skip a play whose target group has no hosts (e.g. HA with no separate
        // workers) — don't abort the whole project (D-083).
        if let Some(g) = &play.hosts {
            let matches = g == "all"
                || hosts.iter().any(|h| h.roles.is_empty() || h.name == *g || h.roles.iter().any(|r| r == g));
            if !matches {
                info!("   (跳过:hosts='{g}' 无匹配主机)");
                continue;
            }
        }
        let opts = RunOpts {
            offline_blobmap: None,
            offline: false,
            do_apply: !dry_run,
            do_shell: shell,
            teardown,
            source: play.source.clone(),
        };
        apply_task(
            &src_path, hosts, opts, Some(&deployment),
            play.hosts.clone(), play.vars.clone(),
        )
        .await
        .map_err(|e| anyhow!("project '{}' play '{label}' 失败:{e}", project.name))?;
    }
    info!("{verb} project '{}' 完成", project.name);
    Ok(())
}

/// How to run a task — the mode flags every apply/delete entry point chooses
/// (CLI 重构 2/3). Named fields instead of a positional bool-soup at call sites.
struct RunOpts {
    /// Packed material blobs (recipe-replay, D-045); None = pure online.
    offline_blobmap: Option<BTreeMap<String, PathBuf>>,
    // Strict-offline (air-gap): missing blob = error, not online fetch (D-087).
    // A thin-online ref carries a partial blobmap with this `false`.
    offline: bool,
    do_apply: bool,
    do_shell: bool,
    teardown: bool,
    source: String,
}

/// Shared read-only context for ONE task run, fixed once `apply_task` has
/// parsed/expanded the task and grouped the targets. Per-host calls only add
/// what genuinely varies: the host, the between-groups `hostvars` snapshot,
/// and the per-group coordinator.
struct RunContext {
    task: crater_core::task::TaskFile,
    spec_dir: PathBuf,
    /// role → space-joined member addresses, for `{{ groups.<role> }}` (D-071).
    role_addrs: BTreeMap<String, String>,
    /// role → (name, addr) members, for the template layer's `groups` (D-075).
    role_members: BTreeMap<String, Vec<(String, String)>>,
    /// Ordered (name, roles) of all targets, for `run_once` gating (D-077).
    target_hosts: Vec<(String, Vec<String>)>,
    /// Grouping label for `task list` (D-052), default = task name.
    deployment: String,
    opts: RunOpts,
}

/// `crater apply <task>.yaml` (D-037): run a generic task across the targets.
/// Control flow (when-filter, needs-ordering) is in the engine. Host
/// orchestration mirrors the component pipeline (D-030/D-031): hosts grouped by
/// role-set run group-by-group (so a producer's `register` lands in `hostvars`
/// before a consumer group reads it), parallel within a group.
async fn apply_task(
    path: &Path,
    hosts: Vec<crater_core::spec::Host>,
    opts: RunOpts,
    name: Option<&str>,
    // Project-play overrides (D-083): retarget the task's group / overlay vars.
    hosts_override: Option<String>,
    var_overrides: BTreeMap<String, String>,
) -> Result<()> {
    use crater_core::task::TaskFile;
    let mut task = TaskFile::from_yaml_file(path)?;
    if let Some(h) = &hosts_override {
        task.hosts = h.clone();
    }
    for (k, v) in &var_overrides {
        task.vars.insert(k.clone(), v.clone());
    }
    // Flatten role bundles (D-080): online from a task file → roles read from
    // ./roles; offline from an OCI → recipe is already flat (expanded at build),
    // so this is a no-op (no `action: role` bundles remain).
    task.expand_roles(&roles_dir_for(path.parent().unwrap_or_else(|| Path::new("."))))?;
    // Param-contract validation happens per-host in run_task_on_host (against the
    // merged task ⊕ inventory vars, D-082) — so inventory-supplied required params
    // count and errors are reported before that host plans.
    // Optional deployment/grouping label (D-052), default = task name. Only used
    // for `task list` grouping; apply/delete behavior is identical regardless.
    let deployment = name.unwrap_or(&task.name).to_string();
    // Delete is opt-in (D-049): a task only has delete capability if it authored
    // a `teardown:`. No auto-inversion of `actions:` — real cleanup targets
    // runtime state the install steps never created.
    if opts.teardown && task.teardown.is_empty() {
        anyhow::bail!(
            "task '{}' defines no `teardown:` — it has no delete capability \
             (delete is opt-in; author a teardown to enable it)",
            task.name
        );
    }
    let spec_dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    // `hosts` filter (D-037-b/D-043/D-077/D-084): `all` → every target; else keep
    // hosts matching that **group name** (a role, derived transitively from
    // inventory groups) OR that **host name** (ansible-style). Hosts with no roles
    // (CLI --host / local) always match.
    let hosts: Vec<crater_core::spec::Host> = if task.hosts == "all" {
        hosts
    } else {
        hosts
            .into_iter()
            .filter(|h| {
                h.roles.is_empty()
                    || h.name == task.hosts
                    || h.roles.iter().any(|r| r == &task.hosts)
            })
            .collect()
    };
    if hosts.is_empty() {
        anyhow::bail!("task hosts='{}' matched no target host", task.hosts);
    }
    info!(
        "{} '{}': {} action(s), hosts={}, {} target(s), mode={}",
        if opts.teardown { "teardown" } else { "task" },
        task.name,
        if opts.teardown { task.teardown.len() } else { task.actions.len() },
        task.hosts,
        hosts.len(),
        if opts.do_apply { "apply" } else { "dry-run" }
    );

    let mut hostvars: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    // Role → member hosts. `role_members` (name+addr) feeds the template layer's
    // structured `groups.<role>` (D-075); `role_addrs` is the space-joined-address
    // form for the simple `{{ groups.<role> }}` substitution in cmds (D-071).
    let role_members: BTreeMap<String, Vec<(String, String)>> = {
        let mut acc: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for h in &hosts {
            for r in &h.roles {
                acc.entry(r.clone()).or_default().push((h.name.clone(), h.address.clone()));
            }
        }
        acc
    };
    let role_addrs: BTreeMap<String, String> = role_members
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().map(|(_, ip)| ip.clone()).collect::<Vec<_>>().join(" ")))
        .collect();
    // host name → roles, so a host's registered facts can also be published under
    // its roles (`hostvars.<role>.<name>`, D-071) for singleton roles like the init node.
    let name_roles: BTreeMap<String, Vec<String>> =
        hosts.iter().map(|h| (h.name.clone(), h.roles.clone())).collect();
    // Ordered (name, roles) of all targets (D-077), for `run_once` gating: a
    // run_once step runs only on the first target matching its when_role.
    let target_hosts: Vec<(String, Vec<String>)> =
        hosts.iter().map(|h| (h.name.clone(), h.roles.clone())).collect();

    // From here on the run-wide context is fixed; only host / hostvars / coord
    // vary per call.
    let rc = RunContext { task, spec_dir, role_addrs, role_members, target_hosts, deployment, opts };

    if !rc.opts.do_apply {
        for h in &hosts {
            run_task_on_host(&rc, h, &hostvars, None).await?;
        }
        info!("dry-run only; omit --dry-run to execute");
        return Ok(());
    }

    let mut applied_hosts: Vec<String> = Vec::new();
    let forks = forks_limit();

    // Merge one host's registered facts into the shared hostvars, published both
    // as `hostvars.<host>.<name>` and `hostvars.<role>.<name>` (D-030/D-071).
    fn merge_regs(
        hostvars: &mut BTreeMap<String, BTreeMap<String, String>>,
        name_roles: &BTreeMap<String, Vec<String>>,
        host_name: &str,
        regs: Vec<(String, String)>,
    ) {
        let roles = name_roles.get(host_name).cloned().unwrap_or_default();
        for (k, v) in regs {
            hostvars.entry(host_name.to_string()).or_default().insert(k.clone(), v.clone());
            for r in &roles {
                hostvars.entry(r.clone()).or_default().insert(k.clone(), v.clone());
            }
        }
    }

    for group in group_hosts_by_role(&hosts) {
        // serial_roles (D-071): a group whose hosts hold a serial role runs one at
        // a time — e.g. control-plane joins must not race on etcd quorum.
        let serial = group
            .iter()
            .any(|h| h.roles.iter().any(|r| rc.task.serial_roles.contains(r)));
        if serial {
            // Sequential WITH progressive fact propagation (D-077): each host's
            // registered facts are merged into hostvars BEFORE the next host plans.
            // (Prefer per-step `throttle` over serial_roles — it parallelizes prep.)
            for h in group {
                let (host_name, regs) = run_task_on_host(&rc, h, &hostvars, None).await?;
                merge_regs(&mut hostvars, &name_roles, &host_name, regs);
                applied_hosts.push(host_name);
            }
        } else {
            // Independent hosts → run in PARALLEL, coordinated by a shared
            // HostCoord (D-077): a step awaiting a cross-host fact blocks until its
            // producer publishes (fail-fast if the producer errors), and a
            // `throttle`d step is capped to N-at-once. Seeded with prior-group facts.
            let mut seed: BTreeMap<String, String> = BTreeMap::new();
            for (scope, kv) in &hostvars {
                for (k, v) in kv {
                    seed.insert(format!("hostvars.{scope}.{k}"), v.clone());
                }
            }
            let coord = engine::HostCoord::new(seed, group.len());
            let (rc_ref, hostvars_ref, coord_ref) = (&rc, &hostvars, &coord);
            let results: Vec<Result<(String, Vec<(String, String)>)>> = futures::stream::iter(
                group.iter().map(|h| async move {
                    // Signal the coordinator on finish so peers awaiting this host's
                    // facts fail fast on error / never-produced rather than blocking
                    // to the timeout (D-077). Facts (if any) are published inside.
                    let r = run_task_on_host(rc_ref, h, hostvars_ref, Some(coord_ref)).await;
                    if r.is_err() {
                        coord_ref.mark_aborted();
                    }
                    coord_ref.host_done();
                    r
                }),
            )
            .buffer_unordered(forks)
            .collect()
            .await;
            let mut first_err = None;
            for r in results {
                match r {
                    Ok((host_name, regs)) => {
                        merge_regs(&mut hostvars, &name_roles, &host_name, regs);
                        applied_hosts.push(host_name);
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
    }

    // Record to the control-side aggregate DB (D-051). Best-effort: the markers
    // on the targets are authoritative; the DB is a cache/history for list/UI.
    if let Err(e) = record_deployments(&rc.task, &rc.opts.source, &rc.deployment, rc.opts.teardown, &applied_hosts).await {
        warn!("state DB update failed (targets' markers are authoritative): {e}");
    }
    Ok(())
}

/// A gathered deployment instance + optional drift status (Some(true)=ok,
/// Some(false)=DRIFT, None=not checked / no verify phase).
struct DepRow {
    dep: crater_core::state::Deployment,
    status: Option<bool>,
}

/// Gather deployment instances (one per host×task) from the right source:
/// the control DB, or — when a target is given — the authoritative markers read
/// off the hosts (D-051). With `verify`, re-run each deployment's verify phase
/// on the target to detect drift (D-055; requires `--host`/`-i`).
async fn gather_deployments(
    target: &TargetOpts,
    verify: bool,
) -> Result<Vec<DepRow>> {
    use crater_core::state::Deployment;
    if target.inventory.is_some() || target.host.is_some() {
        let hosts = target.hosts()?;
        // Persist verify results so the read-only UI can show drift (D-055).
        let store = if verify { state::TursoStore::open().await.ok() } else { None };
        let now = state::now_epoch();
        let mut out = Vec::new();
        for h in &hosts {
            let exec = connect_executor(h, true).await?;
            for m in state::read_markers(exec.as_ref()).await.unwrap_or_default() {
                let status = if verify {
                    let s = verify_on_host(exec.as_ref(), &m.source).await;
                    if let Some(st) = &store {
                        let _ = st.record_verify(&h.name, &m.name, s, now).await;
                    }
                    s
                } else {
                    None
                };
                let (status_str, checked) = match status {
                    Some(true) => ("ok".to_string(), now),
                    Some(false) => ("drift".to_string(), now),
                    None => ("unknown".to_string(), 0),
                };
                out.push(DepRow {
                    dep: Deployment {
                        host: h.name.clone(),
                        name: m.name.clone(),
                        deployment: if m.deployment.is_empty() { m.name } else { m.deployment },
                        version: m.version,
                        source: m.source,
                        applied_at: m.applied_at,
                        status: status_str,
                        checked_at: checked,
                    },
                    status,
                });
            }
        }
        Ok(out)
    } else {
        Ok(state::TursoStore::open()
            .await?
            .list_deployments()
            .await?
            .into_iter()
            .map(|dep| DepRow { dep, status: None })
            .collect())
    }
}

/// Drift check (D-055): resolve the task from its `source`, re-run only its
/// **verify-phase** actions on the host (read-only), report ok/DRIFT. Returns
/// `None` when the source can't be resolved locally or the task has no verify
/// phase (no health probe to judge by).
/// Resolve a deployment's recorded `source` (named task or task-file path) to a
/// local task file. Returns None for artifact-ref sources (not resolvable here).
fn resolve_task_path(source: &str) -> Option<PathBuf> {
    use crater_core::task::is_task_file;
    let p = PathBuf::from(source);
    if p.is_file() && is_task_file(&p) {
        return Some(p);
    }
    let named = PathBuf::from("tasks").join(format!("{source}.yaml"));
    if named.is_file() && is_task_file(&named) {
        return Some(named);
    }
    None
}

async fn verify_on_host(exec: &dyn Executor, source: &str) -> Option<bool> {
    use crater_core::engine::{self, Op, PlanContext, Phase};
    use crater_core::task::TaskFile;
    let path = resolve_task_path(source)?;
    let task = TaskFile::from_yaml_file(&path).ok()?;
    let spec_dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    let ver = task.effective_vars().get("version").cloned().unwrap_or_else(|| "latest".into());
    let mut ctx = PlanContext::new(os::detect_via(exec).await, ver, spec_dir);
    ctx.target_arch = arch::detect_via(exec).await;
    for (k, v) in &task.effective_vars() {
        ctx.vars.insert(k.clone(), v.clone());
    }
    for m in &task.materials {
        ctx.add_material(m.clone());
    }
    // Verify-phase actions only, `needs` cleared (we run them standalone).
    let verify_actions: Vec<_> = task
        .actions
        .iter()
        .filter(|a| a.phase == Phase::Verify)
        .map(|a| {
            let mut a = a.clone();
            a.needs.clear();
            a
        })
        .collect();
    if verify_actions.is_empty() {
        return None; // no health probe to judge drift by
    }
    let steps = engine::plan_from_task(&verify_actions, &ctx).ok()?;
    for s in &steps {
        if let Op::Shell { cmd, .. } = &s.op {
            match exec.run(cmd).await {
                Ok(o) if o.ok() => {}
                _ => return Some(false), // a verify check failed → drift
            }
        }
    }
    Some(true)
}

fn status_label(s: Option<bool>) -> &'static str {
    match s {
        Some(true) => "ok",
        Some(false) => "DRIFT",
        None => "?",
    }
}

/// `crater task list` (D-051/052/053): **deployment-centric** — one row per
/// deployment, hosts aggregated as a count. `--verify` adds a drift STATUS.
#[allow(clippy::too_many_arguments)]
async fn task_list(target: TargetOpts, verify: bool) -> Result<()> {
    let from_targets = target.inventory.is_some() || target.host.is_some();
    if verify && !from_targets {
        anyhow::bail!("--verify needs --host or -i (it re-runs the verify phase on the targets)");
    }
    let rows = gather_deployments(&target, verify).await?;
    if rows.is_empty() {
        if from_targets {
            info!("no deployments found on the target(s)");
        } else {
            info!("no deployments recorded in the control DB (~/.crater/state.db); use --host/-i to read targets directly");
        }
        return Ok(());
    }
    // Aggregate by deployment label: tasks, versions, host count, latest, drift.
    struct Agg {
        tasks: BTreeSet<String>,
        versions: BTreeSet<String>,
        hosts: usize,
        last: i64,
        ok: usize,
        drift: usize,
    }
    let mut by_dep: BTreeMap<String, Agg> = BTreeMap::new();
    for r in rows {
        let e = by_dep.entry(r.dep.deployment).or_insert_with(|| Agg {
            tasks: BTreeSet::new(),
            versions: BTreeSet::new(),
            hosts: 0,
            last: 0,
            ok: 0,
            drift: 0,
        });
        e.tasks.insert(r.dep.name);
        e.versions.insert(r.dep.version);
        e.hosts += 1;
        e.last = e.last.max(r.dep.applied_at);
        match r.status {
            Some(true) => e.ok += 1,
            Some(false) => e.drift += 1,
            None => {}
        }
    }
    let join_or_mixed = |s: BTreeSet<String>| {
        if s.len() == 1 {
            s.into_iter().next().unwrap()
        } else {
            format!("{} (mixed)", s.into_iter().collect::<Vec<_>>().join(","))
        }
    };
    if verify {
        println!("{:<16} {:<12} {:<14} {:>6}  {:<14} {}", "DEPLOYMENT", "TASK", "VERSION", "HOSTS", "STATUS", "LAST APPLIED (UTC)");
    } else {
        println!("{:<16} {:<12} {:<14} {:>6}  {}", "DEPLOYMENT", "TASK", "VERSION", "HOSTS", "LAST APPLIED (UTC)");
    }
    for (dep, a) in by_dep {
        if verify {
            let status = if a.drift > 0 {
                format!("DRIFT {}/{}", a.drift, a.hosts)
            } else {
                format!("ok {}/{}", a.ok, a.hosts)
            };
            println!(
                "{:<16} {:<12} {:<14} {:>6}  {:<14} {}",
                dep, join_or_mixed(a.tasks), join_or_mixed(a.versions), a.hosts, status, state::fmt_epoch(a.last)
            );
        } else {
            println!(
                "{:<16} {:<12} {:<14} {:>6}  {}",
                dep, join_or_mixed(a.tasks), join_or_mixed(a.versions), a.hosts, state::fmt_epoch(a.last)
            );
        }
    }
    info!("(host names: crater task show <deployment>)");
    Ok(())
}

/// `crater task show <name>` (D-051): one deployment's per-host instances;
/// `--verify` adds per-host drift status.
#[allow(clippy::too_many_arguments)]
async fn task_show(name: &str, target: TargetOpts, verify: bool) -> Result<()> {
    if verify && target.inventory.is_none() && target.host.is_none() {
        anyhow::bail!("--verify needs --host or -i (it re-runs the verify phase on the targets)");
    }
    let mut rows = gather_deployments(&target, verify).await?;
    rows.retain(|r| r.dep.deployment == name || r.dep.name == name);
    if rows.is_empty() {
        info!("deployment '{name}' has no recorded instances");
        return Ok(());
    }
    if verify {
        println!("{:<16} {:<12} {:<10} {:<8} {:<20} {}", "HOST", "TASK", "VERSION", "STATUS", "APPLIED (UTC)", "SOURCE");
        for r in rows {
            println!(
                "{:<16} {:<12} {:<10} {:<8} {:<20} {}",
                r.dep.host, r.dep.name, r.dep.version, status_label(r.status), state::fmt_epoch(r.dep.applied_at), r.dep.source
            );
        }
    } else {
        println!("{:<16} {:<12} {:<10} {:<20} {}", "HOST", "TASK", "VERSION", "APPLIED (UTC)", "SOURCE");
        for r in rows {
            println!(
                "{:<16} {:<12} {:<10} {:<20} {}",
                r.dep.host, r.dep.name, r.dep.version, state::fmt_epoch(r.dep.applied_at), r.dep.source
            );
        }
    }
    Ok(())
}

/// `crater task history` (D-051): recent apply/delete runs from the control DB.
async fn task_history(limit: usize) -> Result<()> {
    let store = state::TursoStore::open().await?;
    let runs = store.history(limit).await?;
    if runs.is_empty() {
        info!("no history recorded in the control DB (~/.crater/state.db)");
        return Ok(());
    }
    println!("{:<20} {:<8} {:<14} {:<12} {:<16} {}", "WHEN (UTC)", "ACTION", "DEPLOYMENT", "TASK", "HOST", "RESULT");
    for r in runs {
        println!(
            "{:<20} {:<8} {:<14} {:<12} {:<16} {}",
            state::fmt_epoch(r.ts), r.action, r.deployment, r.task, r.host, r.result
        );
    }
    Ok(())
}

/// Record apply/delete outcomes to the control-side aggregate DB (D-051).
async fn record_deployments(
    task: &crater_core::task::TaskFile,
    source: &str,
    deployment: &str,
    teardown: bool,
    hosts: &[String],
) -> Result<()> {
    if hosts.is_empty() {
        return Ok(());
    }
    let store = state::TursoStore::open().await?;
    let ver = task.effective_vars().get("version").cloned().unwrap_or_else(|| "latest".into());
    let ts = state::now_epoch();
    for h in hosts {
        if teardown {
            store.record_delete(h, &task.name, ts).await?;
        } else {
            let m = Marker {
                name: task.name.clone(),
                deployment: deployment.to_string(),
                version: ver.clone(),
                source: source.to_string(),
                applied_at: ts,
            };
            store.record_apply(h, &m).await?;
        }
    }
    Ok(())
}

/// Plan + execute one task on one host. The run-wide fixed inputs live in
/// `rc` (CLI 重构 2/3); only the genuinely per-call ones remain parameters:
/// the host, the between-groups `hostvars` snapshot, the per-group coordinator.
async fn run_task_on_host(
    rc: &RunContext,
    host: &crater_core::spec::Host,
    hostvars: &BTreeMap<String, BTreeMap<String, String>>,
    coord: Option<&engine::HostCoord>,
) -> Result<(String, Vec<(String, String)>)> {
    let RunContext { task, spec_dir, role_addrs, role_members, target_hosts, deployment, opts } = rc;
    let RunOpts { offline_blobmap, offline, do_apply, do_shell, teardown, source } = opts;
    let (offline, do_apply, do_shell, teardown) = (*offline, *do_apply, *do_shell, *teardown);
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
        .effective_vars()
        .get("version")
        .cloned()
        .unwrap_or_else(|| "latest".into());
    let mut ctx = PlanContext::new(osf, ver, spec_dir.to_path_buf());
    ctx.target_arch = target_arch;
    ctx.roles_dir = roles_dir_for(spec_dir); // legacy thin roles, relative to delivery (D-086)
    for (k, v) in &task.effective_vars() {
        ctx.vars.insert(k.clone(), v.clone());
    }
    // Inventory vars (D-082) override task param defaults: host.vars is the merged
    // global ⊕ group ⊕ host set (resolved in target_hosts). This is how apply-stage
    // env config (vip/subnet/…) comes from inventory rather than baked in the OCI.
    for (k, v) in &host.vars {
        ctx.vars.insert(k.clone(), v.clone());
    }
    // Validate the param contract against the merged vars (D-081/082): every
    // required param must now have a value (default / inventory). Fail before planning.
    task.validate_params(&ctx.vars, None)?;
    // Other hosts' registered facts become template vars (D-030).
    for (h, kv) in hostvars {
        for (k, v) in kv {
            ctx.vars.insert(format!("hostvars.{h}.{k}"), v.clone());
        }
    }
    // Role membership (D-071): `{{ groups.<role> }}` = member addresses; the
    // target's own roles drive `when_role` step filtering.
    for (role, addrs) in role_addrs {
        ctx.vars.insert(format!("groups.{role}"), addrs.clone());
    }
    ctx.groups = role_members.clone(); // structured groups for the template layer (D-075)
    ctx.host_roles = host.roles.clone();
    // run_once gating (D-077): this host's identity + the ordered target list.
    ctx.self_host = host.name.clone();
    ctx.target_hosts = target_hosts.to_vec();
    // Facts THIS host will itself register (mirrors the register gating below):
    // a step must never await a fact its own host produces — the register runs
    // after the step, so awaiting it would deadlock (D-077). Keyed exactly as the
    // consumers reference them: hostvars.<host>.<name> and hostvars.<role>.<name>.
    for reg in &task.register {
        let role_match = |roles: &[String]| {
            reg.when_role.is_empty() || reg.when_role.iter().any(|r| roles.iter().any(|h| h == r))
        };
        if !role_match(&host.roles) {
            continue;
        }
        if reg.run_once {
            let leader = target_hosts.iter().find(|(_, roles)| role_match(roles));
            if leader.is_some_and(|(name, _)| name != &host.name) {
                continue;
            }
        }
        ctx.self_produced.insert(format!("hostvars.{}.{}", host.name, reg.name));
        for r in &host.roles {
            ctx.self_produced.insert(format!("hostvars.{r}.{}", reg.name));
        }
    }
    // The target's own inventory identity, for templates that need a stable
    // unique per-host value (e.g. kubeadm `--node-name`, D-071).
    ctx.vars.insert("inventory_hostname".to_string(), host.name.clone());
    ctx.vars.insert("inventory_addr".to_string(), host.address.clone());
    for m in &task.materials {
        ctx.add_material(m.clone());
    }
    // Offline (recipe-replay, D-045): `place` pushes packed blobs from control.
    // `offline` distinguishes strict air-gap (missing blob = error) from
    // thin-online (partial blobmap, missing material fetched online) — D-087.
    if let Some(bm) = offline_blobmap {
        ctx.offline_blobs = Some(bm.clone());
        ctx.offline = offline;
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
        engine::execute_task(&steps, &handlers, exec.as_ref(), coord).await?;
    } else {
        // Agent path runs the plan on the target without the control-side coord;
        // cross-host throttle/awaited-facts (D-077) only apply to control-plane
        // execute (the offline branch above, which HA always takes).
        run_task_via_agent(exec.as_ref(), &steps, &handlers, None).await?;
    }

    // Record on-target deployment state (D-051): apply writes the marker,
    // delete removes it. Best-effort — the deployment already succeeded.
    let marker_res = if teardown {
        state::remove_marker(exec.as_ref(), &task.name).await
    } else {
        let m = Marker {
            name: task.name.clone(),
            deployment: deployment.to_string(),
            version: ctx.version.clone(),
            source: source.to_string(),
            applied_at: state::now_epoch(),
        };
        state::write_marker(exec.as_ref(), &m).await
    };
    if let Err(e) = marker_res {
        warn!("[{}] state marker update failed (deployment still applied): {e}", host.name);
    }

    // Capture this host's facts for later groups (D-030). Apply only — teardown
    // has no fact consumers, and the register cmds (e.g. `kubeadm token create`)
    // would run against tooling teardown just removed (D-077: it deleted kubeadm).
    let mut registered: Vec<(String, String)> = Vec::new();
    for reg in &task.register {
        if teardown {
            break;
        }
        // when_role (D-071): only gather this fact on hosts holding the role
        // (e.g. the join command is produced on the control-plane, not workers).
        let role_match = |roles: &[String]| {
            reg.when_role.is_empty() || reg.when_role.iter().any(|r| roles.iter().any(|h| h == r))
        };
        if !role_match(&host.roles) {
            continue;
        }
        // run_once (D-077): gather only on the first target matching when_role
        // (the implicit init node) — `kubeadm token create` / upload-certs once.
        if reg.run_once {
            let leader = target_hosts.iter().find(|(_, roles)| role_match(roles));
            if leader.is_some_and(|(name, _)| name != &host.name) {
                continue;
            }
        }
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
    // Publish to the group coordinator (D-077) so concurrently-running peers
    // awaiting these facts (e.g. a control-plane join awaiting the init node's
    // token) unblock immediately — keyed as the consumers reference them.
    if let Some(c) = coord {
        let mut kv: Vec<(String, String)> = Vec::new();
        for (name, val) in &registered {
            kv.push((format!("hostvars.{}.{name}", host.name), val.clone()));
            for r in &host.roles {
                kv.push((format!("hostvars.{r}.{name}"), val.clone()));
            }
        }
        if !kv.is_empty() {
            c.publish(&kv).await;
        }
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
# user 默认 root,port 默认 22。
#
# 角色/成员由 groups 决定(仿 kubekey/Ansible):每个组列 hosts:(主机名)
# 和/或 groups:(嵌套子组),可嵌套。host 的角色 = 所属组(含嵌套向上传播),
# 不在 host 上重复写。task 的 when_role/hosts 按组名匹配。
#
# 三级 vars(全局 inventory.vars < 组 groups.<g>.vars < 主机 hosts[].vars),
# 覆盖 task 的 params 默认 —— 环境配置(vip/网段等)放这里,让 OCI 与环境无关。
inventory:
  # vars:                 # 全局(对所有主机生效)
  #   vip: "192.168.1.100"
  hosts:
    # ① 密码认证
    - name: web1
      address: 192.168.1.11
      user: root
      port: 22
      password: "changeme"

    # ② SSH 私钥认证(适合禁用密码登录的机群;~ 会自动展开为 $HOME)
    - name: web2
      address: 192.168.1.12
      user: ubuntu
      key: ~/.ssh/id_rsa

    # ③ 再一台
    - name: db1
      address: 192.168.1.20
      password: "changeme"

  groups:
    # 组成员 = 主机名;run_once 步骤取组内首台(如 k8s init 节点)。
    web:
      hosts: [web1, web2]
      # vars:               # 组级 vars(覆盖全局,被主机 vars 覆盖)
      #   listen_port: "8080"
    db:
      hosts: [db1]
    # 嵌套:组也能包含其他组,角色向上传播(web1 同时拥有 app 角色)。
    app:
      groups: [web, db]
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

/// Parse `--set key=val` overrides (D-089) into a map. Each must contain `=`.
fn parse_set_overrides(set: &[String]) -> Result<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    for kv in set {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| anyhow!("--set '{kv}': expected KEY=VAL"))?;
        m.insert(k.trim().to_string(), v.to_string());
    }
    Ok(m)
}

/// `crater build -f spec [-t ref]`: build the B 类 artifact(s) and store them in
/// the local store (~/.crater/store), like `docker build`. Export with `save`.
async fn build_to_store(file: &Path, tag: Option<String>, arch_filter: &[String], set: &[String]) -> Result<()> {
    // `crater build` only builds tasks now (D-046): a task → B 类 artifact whose
    // recipe IS the task YAML.
    if !crater_core::task::is_task_file(file) {
        anyhow::bail!(
            "{}: not a task file (needs top-level `actions:`). `crater build` builds tasks.",
            file.display()
        );
    }
    let overrides = parse_set_overrides(set)?;
    build_task_to_store(file, tag, arch_filter, &overrides).await
}

/// `crater inspect <source>` (D-081): print a task/OCI's input contract — params
/// (description/default/required/stage), the inventory roles it needs, and its
/// materials. `--gen-inventory` emits a starter inventory instead. Source is a
/// task file (its `params:`) or an artifact ref (its embedded, flattened recipe).
async fn inspect_source(source: &str, gen_inventory: bool) -> Result<()> {
    use crater_core::component::MaterialKind;
    use crater_core::task::{ParamStage, TaskFile};

    // Resolve the task: a local file (explicit path OR named under library/, then
    // load + expand roles) or an OCI artifact (recipe already flat, D-080).
    let local = find_named(source).filter(|f| crater_core::task::is_task_file(f));
    let task: TaskFile = if let Some(f) = local {
        let mut t = TaskFile::from_yaml_file(&f)?;
        t.expand_roles(&roles_dir_for(f.parent().unwrap_or_else(|| Path::new("."))))?;
        t
    } else {
        let store = ImageStore::open()?;
        if !store.has(source) {
            info!("{source} not in local store → pulling");
            store.pull(source).await?;
        }
        let manifest = store.resolve_manifest(source)?;
        let dir = std::env::temp_dir().join(format!("crater-inspect-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mc = bundle::materialize_component(&manifest, &store.blobs_dir(), &dir)?
            .ok_or_else(|| anyhow!("{source} is not a crater task artifact (no recipe)"))?;
        let t = TaskFile::from_yaml_file(&dir.join(&mc.name).join("component.yaml"))?;
        let _ = std::fs::remove_dir_all(&dir);
        t
    };

    let ev = task.effective_vars();
    let roles = task.roles_needed();

    // The contract = DECLARED params only. Plain `vars` (e.g. component versions)
    // are internal defaults — overridable but not advertised as user inputs.
    struct P {
        name: String,
        stage: ParamStage,
        required: bool,
        default: Option<String>,
        desc: Option<String>,
    }
    let mut params: Vec<P> = task
        .params
        .iter()
        .map(|(k, p)| P {
            name: k.clone(),
            stage: p.stage,
            required: p.required,
            default: p.default.clone(),
            desc: p.description.clone(),
        })
        .collect();
    params.sort_by(|a, b| a.name.cmp(&b.name));
    // Internal vars not declared as params (count only, for the summary note).
    let internal_vars = task.vars.keys().filter(|k| !task.params.contains_key(*k)).count();

    if gen_inventory {
        // Starter inventory: apply-stage params under vars, required groups.
        println!("inventory:");
        let apply: Vec<&P> = params.iter().filter(|p| p.stage == ParamStage::Apply).collect();
        if !apply.is_empty() {
            println!("  vars:");
            for p in &apply {
                let val = p.default.clone().unwrap_or_else(|| "TODO".into());
                let note = p.desc.as_deref().map(|d| format!("   # {d}")).unwrap_or_default();
                println!("    {}: \"{}\"{}", p.name, val, note);
            }
        }
        println!("  hosts:");
        println!("    - name: node1");
        println!("      address: 192.168.1.11");
        println!("      password: \"changeme\"");
        if !roles.is_empty() {
            println!("  groups:");
            for r in &roles {
                println!("    {r}:");
                println!("      hosts: [node1]");
            }
        }
        return Ok(());
    }

    // Contract summary.
    let ver = ev.get("version").map(|v| format!("  v{v}")).unwrap_or_default();
    let desc = task.description.as_deref().map(|d| format!("  — {d}")).unwrap_or_default();
    println!("{}{}{}", task.name, ver, desc);
    println!("hosts: {}", task.hosts);
    println!(
        "角色(inventory 需定义): {}",
        if roles.is_empty() { "(无 / 全部)".to_string() } else { roles.join(", ") }
    );
    if params.is_empty() {
        println!("参数: (无声明的 params)");
    } else {
        println!("参数(契约):");
        for p in &params {
            let stage = match p.stage {
                ParamStage::Build => "build",
                ParamStage::Apply => "apply",
            };
            let req = if p.required && p.default.is_none() { "必填" } else { "选填" };
            let def = p.default.as_deref().map(|d| format!(" = {d}")).unwrap_or_default();
            let d = p.desc.as_deref().map(|d| format!("   {d}")).unwrap_or_default();
            println!("  {:<22} ({stage}, {req}){def}{d}", p.name);
        }
    }
    if internal_vars > 0 {
        println!("(另有 {internal_vars} 个内部 vars 默认,可覆盖但非对外契约)");
    }
    let (mut nf, mut ni, mut no) = (0u32, 0u32, 0u32);
    for m in &task.materials {
        match m.kind {
            MaterialKind::File => nf += 1,
            MaterialKind::Image => ni += 1,
            MaterialKind::OsPackage => no += 1,
        }
    }
    println!("materials: {} ({nf} file, {ni} image, {no} os_package)", task.materials.len());
    Ok(())
}

/// Build a task into a B 类 OCI artifact (D-045): fetch its `binary` materials,
/// store them + the task YAML as the recipe, tag, into the local store. Loaded
/// by recipe-replay through `plan_from_task` (offline).
/// Make an image ref safe as a temp-file fragment.
fn sanitize_ref(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// Resolve an OS-package dependency closure in `base` via **buildah** (daemonless)
/// and return it as a tar of the .deb/.rpm files (D-062). Mirrors KubeKey's
/// recipe but daemonless and without ISO/repo-metadata (apply uses
/// `apt-get install ./*.deb`). Requires `buildah` on the build machine.
fn build_os_package_repo(base: &str, family: &str, pkgs: &[String]) -> Result<Vec<u8>> {
    use std::process::Command;
    if Command::new("buildah").arg("--version").output().is_err() {
        anyhow::bail!("buildah not found — install it on the build machine for os_package (daemonless, no dockerd)");
    }
    let run = |args: &[&str]| -> Result<std::process::Output> {
        let out = Command::new("buildah").args(args).output()?;
        Ok(out)
    };
    let ctr = String::from_utf8(run(&["from", base])?.stdout)?.trim().to_string();
    if ctr.is_empty() {
        anyhow::bail!("buildah from {base} failed");
    }
    let cleanup = |c: &str| {
        let _ = Command::new("buildah").args(["umount", c]).output();
        let _ = Command::new("buildah").args(["rm", c]).output();
    };
    let pkglist = pkgs.join(" ");
    // Resolve closure → download .deb/.rpm into /repo inside the container.
    let script = if family == "debian" {
        format!(
            "set -e; export DEBIAN_FRONTEND=noninteractive; apt-get update -qq; \
             apt-get install -y --no-install-recommends dpkg-dev wget >/dev/null; \
             mkdir -p /repo; cd /repo; \
             apt-get install --reinstall --print-uris -y {pkglist} | awk -F\"'\" '{{print $2}}' | grep -v '^$' | sort -u > /urls; \
             wget -q -i /urls"
        )
    } else {
        format!(
            "set -e; mkdir -p /repo; yum install -y yum-utils createrepo >/dev/null 2>&1 || true; \
             (repotrack -p /repo {pkglist} || yumdownloader --resolve --destdir=/repo {pkglist})"
        )
    };
    let st = Command::new("buildah").args(["run", &ctr, "--", "bash", "-c", &script]).status()?;
    if !st.success() {
        cleanup(&ctr);
        anyhow::bail!("buildah closure resolution failed in {base}");
    }
    let mnt = String::from_utf8(run(&["mount", &ctr])?.stdout)?.trim().to_string();
    if mnt.is_empty() {
        cleanup(&ctr);
        anyhow::bail!("buildah mount failed");
    }
    let tar = std::env::temp_dir().join(format!("crater-osrepo-{}-{}.tar", std::process::id(), sanitize_ref(base)));
    let st = Command::new("tar").args(["cf", tar.to_str().unwrap(), "-C", &format!("{mnt}/repo"), "."]).status()?;
    cleanup(&ctr);
    if !st.success() {
        anyhow::bail!("tar of package closure failed");
    }
    let data = std::fs::read(&tar)?;
    let _ = std::fs::remove_file(&tar);
    Ok(data)
}

async fn build_task_to_store(
    file: &Path,
    tag: Option<String>,
    arch_filter: &[String],
    overrides: &BTreeMap<String, String>,
) -> Result<()> {
    use crater_core::component::MaterialKind;
    use crater_core::task::TaskFile;

    let mut task = TaskFile::from_yaml_file(file)?;
    // `--set key=val` (D-089): override build-stage params without editing the
    // yaml — injected into task.vars (which `effective_vars` ranks above param
    // defaults), so material URLs + the default tag's <version> + the baked
    // recipe all use the overridden value. Must precede expand_roles (role
    // params render against these vars) and material fetching.
    for (k, v) in overrides {
        task.vars.insert(k.clone(), v.clone());
    }
    // Flatten role bundles BEFORE collecting materials (D-080): role closures are
    // hoisted into task.materials (so they get packed) and role actions spliced
    // into the recipe — making the OCI self-contained (no role files needed offline).
    task.expand_roles(&roles_dir_for(file.parent().unwrap_or_else(|| Path::new("."))))?;
    // Never bake an embedded inventory (creds!) into the distributable OCI (D-084).
    task.inventory = None;
    // Build-stage params (version-like, affect materials) must be resolved now (D-081).
    task.validate_params(&task.effective_vars(), Some(crater_core::task::ParamStage::Build))?;
    let ver = task
        .effective_vars()
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
    // Task vars drive url_tmpl/ref rendering (D-064); without this only {{version}}
    // and {{arch}} resolve and e.g. {{containerd_ver}} leaks literally into the URL.
    for (k, v) in &task.effective_vars() {
        ctx.vars.insert(k.clone(), v.clone());
    }
    ctx.offline = true; // build wants raw URLs (no mirror rewrite) from rendered_url (D-087)
    let mut materials: Vec<(String, bool, Vec<u8>)> = Vec::new(); // (key, embedded?, bytes) — D-087
    for m in &task.materials {
        if m.kind == MaterialKind::File {
            // Skip variants outside an explicit --arch filter (neutral always kept).
            if !want_arch.is_empty() {
                if let Some(a) = m.arch {
                    if !want_arch.contains(&a) {
                        continue;
                    }
                }
            }
            let key = PlanContext::material_blob_key(m);
            if let Some(tmpl) = &m.url_tmpl {
                // Expose the material's own arch as {{arch}} so url_tmpl has a
                // single source of truth for arch (D-064).
                if let Some(a) = m.arch {
                    ctx.vars.insert("arch".to_string(), a.as_str().to_string());
                }
                let raw = ctx.rendered_url(tmpl)?;
                let url = online.rewrite(&raw);
                info!("  fetch material {key} <- {raw}");
                let (data, _) = source::fetch_best(&url)
                    .await
                    .map_err(|e| anyhow!("fetch material {key}: {e}"))?;
                materials.push((key, false, data)); // has url_tmpl → dependency layer (D-087)
            } else if let Some(src) = &m.src {
                // Hand-authored local file (D-066): read from the task dir and
                // pack verbatim, same blob key `place` resolves offline.
                let path = spec_dir.join(src);
                info!("  read material {key} <- {}", path.display());
                let data = std::fs::read(&path)
                    .map_err(|e| anyhow!("read material {key} from {}: {e}", path.display()))?;
                materials.push((key, true, data)); // self-authored src, no online source → embedded (D-087)
            } else {
                return Err(anyhow!("file material '{key}' has neither `url_tmpl` nor `src`"));
            }
        } else if m.kind == MaterialKind::Image {
            // kind: image (D-061) — pull the image and pack it as a self-
            // contained oci-archive blob; apply imports it into the runtime.
            let reference = m
                .reference
                .as_ref()
                .ok_or_else(|| anyhow!("image material '{}' has no `ref`", m.name))?;
            let reference = ctx.rendered_url(reference)?; // substitute {{version}} (offline ctx → no mirror rewrite)
            let key = PlanContext::material_blob_key(m);
            info!("  pull image material {key} <- {reference}");
            let store = ImageStore::open()?;
            store.pull(&reference).await.map_err(|e| anyhow!("pull image {reference}: {e}"))?;
            let tmp = std::env::temp_dir().join(format!("crater-img-{}-{}.tar", std::process::id(), sanitize_ref(&key)));
            store.export_oci_archive(&reference, &tmp).map_err(|e| anyhow!("export image {reference}: {e}"))?;
            let data = std::fs::read(&tmp)?;
            let _ = std::fs::remove_file(&tmp);
            info!("    packed {} ({} bytes)", reference, data.len());
            materials.push((key, false, data)); // image has ref → dependency layer (D-087)
        } else if m.kind == MaterialKind::OsPackage {
            // kind: os_package (D-062) — resolve the .deb/.rpm dependency closure
            // in the target OS via buildah (daemonless), pack as a tar blob;
            // apply installs it locally (apt-get install ./*.deb / dnf ./*.rpm).
            let base = m
                .base
                .as_ref()
                .ok_or_else(|| anyhow!("os_package material '{}' has no `base` (OS image, e.g. ubuntu:24.04)", m.name))?;
            // pick the package list matching the base OS family.
            let family = if base.contains("ubuntu") || base.contains("debian") { "debian" } else { "rhel" };
            let pkgs = m.packages.get(family).cloned().unwrap_or_default();
            if pkgs.is_empty() {
                anyhow::bail!("os_package material '{}' has no packages for family '{family}'", m.name);
            }
            let key = PlanContext::material_blob_key(m);
            info!("  build os_package {key} <- {base} [{}] (buildah)", pkgs.join(" "));
            let data = build_os_package_repo(base, family, &pkgs)?;
            info!("    packed closure ({} bytes)", data.len());
            materials.push((key, false, data)); // os_package source → dependency layer (D-087)
        }
    }

    // Recipe = the ROLE-EXPANDED task (flat, self-contained), not the raw file —
    // so offline replay needs no role files (D-080). Note: drops source comments.
    let recipe = serde_yaml::to_string(&task)?.into_bytes();
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
    source: &str,
    name: Option<&str>,
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
        let opts = RunOpts {
            offline_blobmap: Some(mc.blobmap),
            offline: true, // a .oci bundle is the full closure → strict air-gap
            do_apply,
            do_shell,
            teardown,
            source: source.to_string(),
        };
        apply_task(&recipe_file, hosts.clone(), opts, name, None, BTreeMap::new()).await?;
    }
    let _ = std::fs::remove_dir_all(&dest_root);
    Ok(())
}

/// Resolve deploy targets for image/oci/component sources, three layers:
///   `-i inventory.yaml`  → fleet, per-host creds (from the file);
///   `--host a,b,c`        → small fleet, ONE shared credential (user+password|key);
///   neither               → a single LOCAL host (runs on the control machine).
///
/// Heterogeneous per-host credentials are intentionally NOT expressible via
/// `--host` (it shares one credential) — use an inventory file for that.
/// The roles dir for a delivery (D-086): the task/project file's sibling `roles/`
/// (self-contained delivery), falling back to `./roles` (cwd) for back-compat.
fn roles_dir_for(spec_dir: &Path) -> PathBuf {
    let local = spec_dir.join("roles");
    if local.is_dir() {
        local
    } else {
        PathBuf::from("roles")
    }
}

/// Resolve a bare `<name>` to a task/project file (D-085): an explicit path, else
/// the first `<name>.yaml` found under `library/` (then `tasks/` for back-compat).
fn find_named(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(name);
    if p.is_file() {
        return Some(p);
    }
    for root in ["library", "tasks"] {
        if let Some(f) = find_yaml_under(Path::new(root), name) {
            return Some(f);
        }
    }
    None
}

/// Recursively find `<name>.yaml` under `dir` (first match, dirs after files).
fn find_yaml_under(dir: &Path, name: &str) -> Option<PathBuf> {
    let target = format!("{name}.yaml");
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path.file_name().and_then(|n| n.to_str()) == Some(target.as_str()) {
            return Some(path);
        }
    }
    subdirs.sort();
    subdirs.into_iter().find_map(|d| find_yaml_under(&d, name))
}

/// Resolve targets for a TASK file (D-084): an explicit `-i`/`--host` wins; else
/// the task's own embedded `inventory:` (self-contained single file); else local.
fn task_hosts(
    task_path: &Path,
    inv: Option<&Path>,
    host: Option<String>,
    user: &str,
    password: Option<String>,
    key: Option<PathBuf>,
    port: u16,
) -> Result<Vec<crater_core::spec::Host>> {
    if inv.is_some() || host.is_some() {
        return target_hosts(inv, host, user, password, key, port);
    }
    // No CLI target → use the task's embedded inventory if it has hosts, else local.
    if let Some(mut emb) = crater_core::task::TaskFile::from_yaml_file(task_path)?.inventory {
        if !emb.hosts.is_empty() {
            emb.resolve(); // derive roles + merge the three var levels (D-077/082)
            info!("  目标取自任务内嵌 inventory({} 台)", emb.hosts.len());
            return Ok(emb.hosts);
        }
    }
    target_hosts(None, None, user, password, key, port) // → localhost
}

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
        let mut inv = spec.inventory;
        if inv.hosts.is_empty() {
            anyhow::bail!("inventory {} has no hosts", p.display());
        }
        // Derive roles from groups (D-077) + merge the three var levels into each
        // host (D-082: global ⊕ group ⊕ host).
        inv.resolve();
        Ok(inv.hosts)
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
                vars: BTreeMap::new(),
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
    // Pad REFERENCE to the widest ref (not a fixed 48) so long registry paths
    // don't push the other columns out of alignment.
    let refw = imgs
        .iter()
        .map(|i| i.reference.len())
        .max()
        .unwrap_or(0)
        .max("REFERENCE".len());
    println!(
        "{:<refw$} {:<16} {:>11} {:>13}",
        "REFERENCE", "DIGEST", "DISK USAGE", "CONTENT SIZE"
    );
    for i in imgs {
        let short = i.digest.trim_start_matches("sha256:").chars().take(12).collect::<String>();
        println!(
            "{:<refw$} {:<16} {:>11} {:>13}",
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
    source: &str,
    name: Option<&str>,
    offline: bool,
) -> Result<()> {
    let store = ImageStore::open()?;
    // D-087: strict-offline (air-gap) needs the FULL closure locally — re-pull if
    // a prior thin pull left `dependency` layers absent. Thin-online (default)
    // pulls only recipe + `embedded` files; dependencies are fetched online at
    // apply, so an absent local copy just needs a thin pull.
    if offline {
        if !store.has(reference) || !store.has_all_layers(reference) {
            info!("{reference}: pulling full closure (--offline)");
            store.pull(reference).await?;
        }
    } else if !store.has(reference) {
        info!("{reference}: thin pull (recipe + embedded files; dependencies fetched online)");
        store.pull_thin(reference).await?;
    }

    // crater task artifact (B 类) → recipe-replay via the task pipeline (D-045).
    let manifest = store.resolve_manifest(reference)?;
    let recipe_dir = std::env::temp_dir().join(format!("crater-ref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&recipe_dir);
    if let Some(mc) = bundle::materialize_component(&manifest, &store.blobs_dir(), &recipe_dir)? {
        let recipe_file = recipe_dir.join(&mc.name).join("component.yaml");
        if crater_core::task::is_task_file(&recipe_file) {
            info!("image {reference}: crater task artifact → recipe-replay");
            let opts = RunOpts {
                offline_blobmap: Some(mc.blobmap),
                offline, // --offline = strict; default thin-online fetches deps live (D-088)
                do_apply,
                do_shell: true,
                teardown,
                source: source.to_string(),
            };
            let res = apply_task(&recipe_file, hosts, opts, name, None, BTreeMap::new()).await;
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
                        Action::Service { name, .. } => out.push(name.clone()),
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
