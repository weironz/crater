//! `crater` CLI.
//!
//! Forms (executes by default; pass --dry-run to only print the plan):
//!   crater <component> [--host H --user U --password P --port N] [--version X] [--os debian|rhel] [--dry-run]
//!   crater apply -f crater.yaml [--dry-run]
//!   crater build -f spec.yaml -o x.bundle                              (online: make offline bundle)
//!   crater deploy --bundle x.bundle --host H --password P [--dry-run]  (offline)
//!   crater ai "<request>" [-o crater.yaml]                             (M4)
//!   crater doctor --file log.txt | --host H --password P [--ai]        (M5)
//!   crater run --host H --password P -- <cmd>                          (ad-hoc, ansible -m shell)
//!   crater agent --plan <file>                                         (internal, runs on the node)

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use tracing::info;

use crater_core::bundle::{self, BundleStage, Manifest, ManifestComponent, BUNDLE_FORMAT_VERSION};
use crater_core::component::ComponentDescriptor;
use crater_core::dag::{self, DepNode};
use crater_core::engine::{
    self, build_plan, collect_downloads, collect_materials, Op, Phase, PlanContext,
};
use crater_core::executor::{Executor, LocalExecutor, SshExecutor};
use crater_core::os::{self, OsFamily};
use crater_core::source::{self, OnlineSource};
use crater_core::spec::CraterSpec;
use crater_core::store::ImageStore;

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
    /// Apply a source — one command for online & offline (D-020). `<source>`
    /// auto-detects: a spec `.yaml` (online), an OCI archive `.oci` (offline
    /// load+install), or a component name (online shortcut). Same engine &
    /// idempotency either way; offline just sources artifacts from the bundle.
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
    /// Build an offline bundle from a spec (run on an online control machine).
    Build {
        #[arg(short, long)]
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
        /// Wrap the component's files into an OCI rootfs image (crater build);
        /// deploy installs it by extracting the layer — no container runtime.
        #[arg(long)]
        image: bool,
    },
    /// Deploy an offline bundle to a target (zero network on the target).
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
    /// AI copilot: natural language -> validated crater.yaml (M4).
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
    /// Import an oci-archive file (e.g. `build --image` output) into the store.
    Load {
        /// Path to the .oci archive.
        file: PathBuf,
        /// Tag to store it under, e.g. 192.168.73.5:5000/yq:4.53.2
        #[arg(long = "as")]
        as_ref: String,
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
        /// Path to the serialized plan to execute locally.
        #[arg(long)]
        plan: Option<PathBuf>,
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
            apply_source(name, source, file, inventory, host, user, password, key, port, dry_run, shell)
                .await
        }
        Cmd::Build {
            file,
            output,
            image,
        } => {
            if image {
                build_image_bundle(&file, &output).await
            } else {
                build_bundle(&file, &output).await
            }
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
            ImageStore::open()?.import_oci_archive(&file, &as_ref)?;
            info!("loaded {} → {as_ref}", file.display());
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
        Cmd::Agent { plan } => run_agent(plan).await,
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
    /// Escape hatch: force the agentless shell executor instead of the default
    /// self-bootstrap agent (use when the target can't run the crater binary).
    shell: bool,
    /// Override the binary shipped in agent mode (e.g. a musl static build).
    agent_bin: Option<PathBuf>,
}

fn parse_flags(rest: &[String]) -> Result<ShortcutFlags> {
    let mut f = ShortcutFlags {
        user: "root".into(),
        port: 22,
        components_dir: PathBuf::from("components"),
        password: std::env::var("CRATER_SSH_PASSWORD").ok(),
        do_apply: true, // execute by default; --dry-run flips it off
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
            "--dry-run" => {
                f.do_apply = false;
                i += 1;
            }
            "--shell" => {
                f.shell = true;
                i += 1;
            }
            "--agent" => {
                // Agent is now the default; accept the flag as a no-op for
                // back-compat.
                i += 1;
            }
            "--agent-bin" => {
                f.agent_bin = rest.get(i + 1).map(PathBuf::from);
                i += 2;
            }
            other => return Err(anyhow!("unknown flag: {other}")),
        }
    }
    Ok(f)
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

/// Resolve a user-typed name (which may be an alias) to a real component dir
/// name. The engine holds ZERO product knowledge: aliases are declared in each
/// component's `aliases:` field (data), and we build the map by scanning
/// `components/`. `crater es` works because `elasticsearch/component.yaml`
/// declares `es` as an alias, not because the code knows what `es` is.
fn resolve_component(name: &str, components_dir: &Path) -> String {
    // Exact directory match wins — no scan needed.
    if components_dir.join(name).join("component.yaml").is_file() {
        return name.to_string();
    }
    if let Ok(rd) = std::fs::read_dir(components_dir) {
        for e in rd.flatten() {
            let yaml = e.path().join("component.yaml");
            if !yaml.is_file() {
                continue;
            }
            if let Ok(desc) = ComponentDescriptor::from_yaml_file(&yaml) {
                if desc.aliases.iter().any(|a| a == name) {
                    return desc.name;
                }
            }
        }
    }
    // No match: hand back the original so the caller fails with a clear error.
    name.to_string()
}

async fn deploy_shortcut(args: Vec<String>) -> Result<()> {
    let mut it = args.into_iter();
    let raw = it.next().ok_or_else(|| anyhow!("missing component name"))?;
    let rest: Vec<String> = it.collect();
    let f = parse_flags(&rest)?;
    let name = resolve_component(&raw, &f.components_dir);

    let component_dir = f.components_dir.join(&name);
    let desc = ComponentDescriptor::from_yaml_file(&component_dir.join("component.yaml"))
        .map_err(|e| anyhow!("failed to load component '{name}': {e}"))?;

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

    // Agent is the default execution model (D-027); --shell forces the
    // agentless shell executor. A local target (no --host) always runs locally.
    let is_remote = f.host.is_some();
    let use_shell = f.shell || !is_remote;
    let mode = if !f.do_apply {
        "dry-run"
    } else if use_shell {
        "apply via shell"
    } else {
        "apply via agent"
    };
    info!(
        "{} v{ver} → {} ({}, os={}, {} steps)",
        desc.name,
        exec.label(),
        mode,
        osf.as_str(),
        plan.len()
    );

    if !f.do_apply {
        print_plan(&plan);
        info!("dry-run only (--dry-run); omit it to execute");
        return Ok(());
    }
    execute_plan(&plan, exec.as_ref(), use_shell, f.agent_bin.as_deref()).await
}

/// Dispatch a plan to the chosen executor. Default is the self-bootstrap agent
/// (D-027); `use_shell` falls back to the agentless shell executor.
async fn execute_plan(
    plan: &[Op],
    exec: &dyn Executor,
    use_shell: bool,
    agent_bin: Option<&Path>,
) -> Result<()> {
    if use_shell {
        engine::execute(plan, exec).await
    } else {
        run_via_agent(exec, plan, agent_bin).await
    }
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
async fn run_via_agent(exec: &dyn Executor, plan: &[Op], agent_bin: Option<&Path>) -> Result<()> {
    // Pick the binary to ship: explicit override > a bundled musl static
    // matching the target's arch > the control binary (only if same arch).
    let (bin_path, how) = select_agent_binary(exec, agent_bin).await?;
    let bytes = std::fs::read(&bin_path)
        .map_err(|e| anyhow!("read agent binary {}: {e}", bin_path.display()))?;
    let want = crater_core::bundle::sha256_hex(&bytes);

    // Same crater binary, cached on the target as `crater` (it's the exact same
    // executable as the control side; we just invoke its `agent` subcommand).
    let remote_bin = "/var/lib/crater/crater";
    let remote_plan = "/tmp/crater-plan.yaml";

    // Push the binary only if the target doesn't already have this exact version.
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
    exec.write_file(remote_plan, engine::plan_to_yaml(plan)?.as_bytes())
        .await?;

    info!("[{}] agent: executing on target ↓", exec.label());
    let out = exec
        .run(&format!("{remote_bin} agent --plan {remote_plan}"))
        .await?;
    // Forward the agent's output verbatim (already tracing-formatted on the target).
    print!("{}", out.stdout);
    if !out.stderr.trim().is_empty() {
        eprint!("{}", out.stderr);
    }
    // Plan is transient; the cached binary stays for reuse.
    let _ = exec.run(&format!("rm -f {remote_plan}")).await;
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

/// `crater agent --plan <file>`: run ON the target. Reads a lowered plan and
/// executes it locally (the control machine pushed the plan + this binary).
async fn run_agent(plan: Option<PathBuf>) -> Result<()> {
    let plan_path = plan.ok_or_else(|| anyhow!("crater agent: --plan <file> is required"))?;
    let text = std::fs::read_to_string(&plan_path)
        .map_err(|e| anyhow!("read plan {}: {e}", plan_path.display()))?;
    let ops = engine::plan_from_yaml(&text)?;
    info!("agent: executing {} step(s) locally", ops.len());
    engine::execute(&ops, &LocalExecutor).await
}

/// Resolve a component ref to its descriptor + the directory used for template
/// lookups. Inline recipes (Path B) need no `components/` entry; otherwise load
/// `components/<name>/component.yaml` (`components/` is an optional reuse lib).
fn resolve_descriptor(
    cref: &crater_core::spec::ComponentRef,
    components_dir: &Path,
    spec_dir: &Path,
) -> Result<(ComponentDescriptor, PathBuf)> {
    if cref.is_inline() {
        // Templates (if any) resolve relative to the spec file's directory.
        Ok((cref.to_inline_descriptor(), spec_dir.to_path_buf()))
    } else {
        let dir = components_dir.join(&cref.name);
        let desc = ComponentDescriptor::from_yaml_file(&dir.join("component.yaml"))
            .map_err(|e| anyhow!("component '{}': {e}", cref.name))?;
        Ok((desc, dir))
    }
}

/// Order selected components by their `requires` DAG (deps first). Edges to
/// components not in this spec are ignored (lenient); cycles error out.
fn order_components(spec: &CraterSpec, components_dir: &Path) -> Result<Vec<String>> {
    let selected: BTreeSet<String> = spec.components.iter().map(|c| c.name.clone()).collect();
    let mut nodes = Vec::new();
    for cref in &spec.components {
        // `requires` comes from the inline recipe or the on-disk descriptor.
        let requires_all = if cref.is_inline() {
            cref.requires.clone()
        } else {
            ComponentDescriptor::from_yaml_file(
                &components_dir.join(&cref.name).join("component.yaml"),
            )?
            .requires
        };
        let requires = requires_all
            .into_iter()
            .filter(|r| selected.contains(r))
            .collect();
        nodes.push(DepNode {
            name: cref.name.clone(),
            requires,
        });
    }
    dag::topo_sort(&nodes)
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
) -> Result<()> {
    // `<source>` positional, else `-f`.
    let src = source
        .or_else(|| file.map(|p| p.display().to_string()))
        .ok_or_else(|| anyhow!("apply needs a <source>: a spec.yaml, an x.oci bundle, an image ref, or a component name"))?;
    let path = PathBuf::from(&src);
    if let Some(n) = &name {
        info!("apply: deployment '{n}' ← {src}");
    }

    if path.is_file() && bundle::is_oci_archive(&path) {
        // Offline: OCI bundle. Targets from CLI (D-020: never inside the image)
        // — `-i inventory.yaml`, `--host a,b`, or none → local.
        info!("apply: {src} → offline (OCI bundle)");
        let hosts = target_hosts(inventory.as_deref(), host, &user, password, key, port)?;
        return apply_oci_bundle(&path, hosts, !dry_run, shell).await;
    }
    if path.is_file() {
        // A task file (top-level `actions:`, D-037) → generic action layer;
        // otherwise a legacy declarative spec (inventory inside).
        if crater_core::task::is_task_file(&path) {
            info!("apply: {src} → task");
            let hosts = target_hosts(inventory.as_deref(), host, &user, password, key, port)?;
            return apply_task(&path, hosts, !dry_run, shell).await;
        }
        info!("apply: {src} → online (spec)");
        return apply_spec(&path, !dry_run, shell).await;
    }
    // Image reference (registry/store): has a registry path or a tag, not a file.
    if src.contains('/') || src.contains(':') {
        info!("apply: {src} → image (local store / registry)");
        let hosts = target_hosts(inventory.as_deref(), host, &user, password, key, port)?;
        return apply_image_ref(&src, hosts, !dry_run).await;
    }
    // Online: bare component name → shortcut (build its flag args). Comma-hosts
    // and key auth route through the unified pipeline instead (below) when given.
    if inventory.is_some() || host.as_deref().map(|h| h.contains(',')).unwrap_or(false) || key.is_some() {
        info!("apply: {src} → online (component, fleet)");
        let hosts = target_hosts(inventory.as_deref(), host, &user, password, key, port)?;
        let spec = CraterSpec {
            inventory: crater_core::spec::Inventory { hosts },
            components: vec![component_ref(&src, "")],
            offline: false,
            ai: None,
        };
        return run_pipeline(&spec, &Artifacts::Online, &PathBuf::from("components"), Path::new("."), !dry_run, shell).await;
    }
    info!("apply: {src} → online (component)");
    let mut args = vec![src];
    if let Some(h) = host {
        args.push("--host".into());
        args.push(h);
    }
    args.push("--user".into());
    args.push(user);
    if let Some(pw) = password {
        args.push("--password".into());
        args.push(pw);
    }
    args.push("--port".into());
    args.push(port.to_string());
    if dry_run {
        args.push("--dry-run".into());
    }
    if shell {
        args.push("--shell".into());
    }
    deploy_shortcut(args).await
}

async fn apply_spec(file: &Path, do_apply: bool, do_shell: bool) -> Result<()> {
    let spec = CraterSpec::from_yaml_file(file)?;
    let spec_dir = file.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    // Online: artifacts fetched by the target; recipes from ./components.
    run_pipeline(
        &spec,
        &Artifacts::Online,
        &PathBuf::from("components"),
        &spec_dir,
        do_apply,
        do_shell,
    )
    .await
}

/// `crater apply <task>.yaml` (D-037): run a generic task across the targets.
/// All control flow (when-filter, needs-ordering) is in the engine; this just
/// fans the lowered plan out to each host.
async fn apply_task(
    path: &Path,
    hosts: Vec<crater_core::spec::Host>,
    do_apply: bool,
    do_shell: bool,
) -> Result<()> {
    use crater_core::task::TaskFile;
    let task = TaskFile::from_yaml_file(path)?;
    let spec_dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    info!(
        "task '{}': {} action(s), hosts={}, {} target(s), mode={}",
        task.name,
        task.actions.len(),
        task.hosts,
        hosts.len(),
        if do_apply { "apply" } else { "dry-run" }
    );
    let forks = forks_limit();
    let results: Vec<Result<()>> = futures::stream::iter(
        hosts
            .iter()
            .map(|h| run_task_on_host(&task, h, &spec_dir, do_apply, do_shell)),
    )
    .buffer_unordered(forks)
    .collect()
    .await;
    for r in results {
        r?;
    }
    Ok(())
}

async fn run_task_on_host(
    task: &crater_core::task::TaskFile,
    host: &crater_core::spec::Host,
    spec_dir: &Path,
    do_apply: bool,
    do_shell: bool,
) -> Result<()> {
    if host.is_local() {
        info!("▶ host {} (local)", host.name);
    } else {
        info!("▶ host {} ({})", host.name, host.address);
    }
    let exec = connect_executor(host, do_apply).await?;
    let osf = if do_apply {
        os::detect_via(exec.as_ref()).await
    } else {
        OsFamily::Unknown
    };
    let ver = task
        .vars
        .get("version")
        .cloned()
        .unwrap_or_else(|| "latest".into());
    let mut ctx = PlanContext::new(osf, ver, spec_dir.to_path_buf());
    for (k, v) in &task.vars {
        ctx.vars.insert(k.clone(), v.clone());
    }
    for m in &task.materials {
        ctx.materials.insert(m.name.clone(), m.clone());
    }
    let plan = engine::plan_from_task(&task.actions, &ctx)?;
    info!("[{}] task {} — {} step(s)", host.name, task.name, plan.len());
    if !do_apply {
        print_plan(&plan);
        return Ok(());
    }
    // Local always runs directly; remote uses agent unless --shell (D-027).
    let use_shell = do_shell || host.is_local();
    execute_plan(&plan, exec.as_ref(), use_shell, None).await
}

/// The single deploy pipeline shared by online & offline (D-020): order
/// components by DAG, group hosts by role (parallel within / sequential across),
/// run each host, merge registered facts. `artifacts` is the only online/offline
/// difference. `components_dir` is `./components` (online) or the bundle's staged
/// components (offline).
async fn run_pipeline(
    spec: &CraterSpec,
    artifacts: &Artifacts,
    components_dir: &Path,
    spec_dir: &Path,
    do_apply: bool,
    do_shell: bool,
) -> Result<()> {
    let ordered = order_components(spec, components_dir)?;
    let by_name: BTreeMap<String, &crater_core::spec::ComponentRef> =
        spec.components.iter().map(|c| (c.name.clone(), c)).collect();
    info!(
        "{} host(s), component(s) [{}], {}, mode={}",
        spec.inventory.hosts.len(),
        ordered.join(", "),
        if artifacts.is_offline() { "offline" } else { "online" },
        if do_apply { "apply" } else { "dry-run" }
    );

    if spec.inventory.hosts.is_empty() {
        for cname in &ordered {
            let cref = by_name[cname];
            let (desc, component_dir) = resolve_descriptor(cref, components_dir, spec_dir)?;
            let ver = cref
                .version
                .clone()
                .or_else(|| desc.version_default.clone())
                .unwrap_or_else(|| "latest".into());
            let ctx = PlanContext::new(OsFamily::Unknown, ver.clone(), component_dir);
            let plan = build_plan(&desc, &ctx)?;
            info!("{} v{ver} — {} steps (no inventory → dry-run):", cref.name, plan.len());
            print_plan(&plan);
        }
        return Ok(());
    }

    // Cross-node facts (D-030): hostvars[host][name], populated by `register:`
    // and injected as `hostvars.<host>.<name>` template vars for later groups.
    let mut hostvars: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    if !do_apply {
        // Dry-run: print each host's plans sequentially (no execution).
        for host in &spec.inventory.hosts {
            run_host(host, &ordered, &by_name, artifacts, components_dir, spec_dir, &hostvars, false, do_shell)
                .await?;
        }
        info!("dry-run only; omit --dry-run to execute over SSH");
        return Ok(());
    }

    // F17: hosts grouped by role-set. Groups run sequentially (so a producer
    // role registers before a consumer role reads it), but hosts WITHIN a group
    // run in parallel — same-role peers are independent. Each host returns its
    // registered facts, merged into hostvars after the whole group finishes.
    let forks = forks_limit();
    for group in group_hosts_by_role(&spec.inventory.hosts) {
        if group.len() > 1 {
            let mut roles = group[0].roles.clone();
            roles.sort();
            info!(
                "▷ group [{}] — {} hosts in parallel (forks={forks})",
                roles.join(","),
                group.len()
            );
        }
        let results: Vec<Result<(String, Vec<(String, String)>)>> = futures::stream::iter(
            group.iter().map(|host| {
                run_host(host, &ordered, &by_name, artifacts, components_dir, spec_dir, &hostvars, true, do_shell)
            }),
        )
        .buffer_unordered(forks)
        .collect()
        .await;

        // Merge this group's registered facts; surface the first host error.
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

/// Where a host gets its artifacts — the ONLY thing that differs between online
/// and offline. The host-pipeline (grouping/parallel/agent/register/idempotency)
/// is identical for both (D-020 单管线).
enum Artifacts {
    /// Online: the target fetches (curl/apt); agent default applies.
    Online,
    /// Offline (OCI bundle, on the control machine): `download`→push-from-blob,
    /// or a rootfs image extracted to `/`. Runs via the shell executor since the
    /// blobs live on control. `blobmap`: url→blob; `rootfs`: component→layer blob.
    Offline {
        blobmap: BTreeMap<String, PathBuf>,
        rootfs: BTreeMap<String, PathBuf>,
    },
}

impl Artifacts {
    fn is_offline(&self) -> bool {
        matches!(self, Artifacts::Offline { .. })
    }
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

/// Deploy one host: connect, detect OS, run its role's components (agent/shell),
/// and return its registered facts (for hostvars). Read-only over `hostvars` —
/// the caller merges results after the group, so parallel peers don't race.
#[allow(clippy::too_many_arguments)]
async fn run_host(
    host: &crater_core::spec::Host,
    ordered: &[String],
    by_name: &BTreeMap<String, &crater_core::spec::ComponentRef>,
    artifacts: &Artifacts,
    components_dir: &Path,
    spec_dir: &Path,
    hostvars: &BTreeMap<String, BTreeMap<String, String>>,
    do_apply: bool,
    do_shell: bool,
) -> Result<(String, Vec<(String, String)>)> {
    if host.is_local() {
        info!("▶ host {} (local)", host.name);
    } else {
        info!("▶ host {} ({})", host.name, host.address);
    }
    let exec = connect_executor(host, do_apply).await?;
    let osf = if do_apply {
        os::detect_via(exec.as_ref()).await
    } else {
        OsFamily::Unknown
    };

    let mut registered: Vec<(String, String)> = Vec::new();
    for cname in ordered {
        let cref = by_name[cname];
        if !host.roles.is_empty() && !host.roles.contains(&cref.name) {
            continue;
        }
        let (desc, component_dir) = resolve_descriptor(cref, components_dir, spec_dir)?;
        let ver = cref
            .version
            .clone()
            .or_else(|| desc.version_default.clone())
            .unwrap_or_else(|| "latest".into());
        let mut ctx = PlanContext::new(osf, ver.clone(), component_dir);
        // Other hosts' registered facts become template vars.
        for (h, kv) in hostvars {
            for (k, v) in kv {
                ctx.vars.insert(format!("hostvars.{h}.{k}"), v.clone());
            }
        }
        // Build the plan — the ONLY online/offline difference (D-020).
        let plan = match artifacts {
            Artifacts::Online => build_plan(&desc, &ctx)?,
            Artifacts::Offline { blobmap, rootfs } => {
                if let Some(layer) = rootfs.get(&cref.name) {
                    // rootfs image: push the layer, extract to /, then replay the
                    // residual (non-file) install + verify.
                    let mut ops = vec![
                        Op::PushFile {
                            phase: Phase::Install,
                            describe: format!("push rootfs layer ({})", cref.name),
                            local_path: layer.clone(),
                            dest: "/tmp/crater-rootfs.tar".into(),
                            mode: None,
                        },
                        Op::Shell {
                            phase: Phase::Install,
                            describe: "extract rootfs to /".into(),
                            cmd: "tar -xpf /tmp/crater-rootfs.tar -C / && rm -f /tmp/crater-rootfs.tar".into(),
                            soft_fail: false,
                            check: None,
                        },
                    ];
                    let mut resid = desc.clone();
                    resid.install.retain(|a| !a.produces_files());
                    resid.preflight.clear();
                    ops.extend(build_plan(&resid, &ctx)?);
                    ops
                } else {
                    ctx.offline_blobs = Some(blobmap.clone());
                    build_plan(&desc, &ctx)?
                }
            }
        };
        info!("[{}] {} v{ver} — {} steps", host.name, cref.name, plan.len());
        if !do_apply {
            print_plan(&plan);
            continue;
        }
        // Agent default online (D-027); offline pins to shell (blobs on control);
        // a local host always runs directly (no agent to bootstrap over SSH).
        let use_shell = do_shell || artifacts.is_offline() || host.is_local();
        execute_plan(&plan, exec.as_ref(), use_shell, None).await?;
        // Capture this component's facts on this host for later groups.
        for reg in &desc.register {
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
    }
    Ok((host.name.clone(), registered))
}

// ---------------------------------------------------------------------------
// M2: offline bundle build / deploy
// ---------------------------------------------------------------------------

async fn build_bundle(spec_file: &Path, out: &Path) -> Result<()> {
    let spec = CraterSpec::from_yaml_file(spec_file)?;
    let components_dir = PathBuf::from("components");

    let stage_root = std::env::temp_dir().join(format!("crater-build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage_root);
    let stage = BundleStage::new(stage_root.clone())?;

    let online = OnlineSource::with_default_mirrors();
    let mut manifest_components = Vec::new();
    let mut blobs = Vec::new();
    let mut images: Vec<bundle::ImageRef> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut seen_images: BTreeSet<String> = BTreeSet::new();

    println!("Building bundle from {} ...", spec_file.display());
    let spec_dir = spec_file.parent().unwrap_or_else(|| Path::new("."));
    for cref in &spec.components {
        let (desc, cdir) = resolve_descriptor(cref, &components_dir, spec_dir)?;
        let ver = cref
            .version
            .clone()
            .or_else(|| desc.version_default.clone())
            .unwrap_or_else(|| "latest".into());

        // Stage the recipe: inline recipes are serialized into the bundle;
        // on-disk components copy their whole dir (templates included).
        let staged = stage.components_dir().join(&cref.name);
        if cref.is_inline() {
            std::fs::create_dir_all(&staged)?;
            std::fs::write(staged.join("component.yaml"), desc.to_yaml()?)?;
        } else {
            copy_dir_all(&cdir, &staged)?;
        }
        manifest_components.push(ManifestComponent {
            name: cref.name.clone(),
            version: ver.clone(),
        });

        // Offline ctx so rendered_url() yields RAW urls (the manifest keys).
        let mut ctx = PlanContext::new(OsFamily::Unknown, ver.clone(), cdir.clone());
        ctx.offline_blobs = Some(BTreeMap::new());
        let downloads = collect_downloads(&desc, &ctx)?;

        for (raw_url, _dest) in downloads {
            if !seen.insert(raw_url.clone()) {
                continue;
            }
            // raw_url is the non-rewritten URL (offline ctx). For non-GitHub
            // hosts, apply registry mirror rewrite; fetch_best tries direct
            // then CN GitHub mirrors with fallback.
            let primary = online.rewrite(&raw_url);
            println!("  fetch {raw_url}");
            let (data, used) = source::fetch_best(&primary)
                .await
                .map_err(|e| anyhow!("fetch {raw_url}: {e}"))?;
            if used != primary {
                println!("    (via {used})");
            }
            let entry = stage.store_blob(&raw_url, &data)?;
            println!("    -> {} bytes, sha256={}", entry.size, &entry.sha256[..16]);
            blobs.push(entry);
        }

        // Container images declared by the component (D-018 ②): pull into the
        // OCI layout so the target can load them with zero network.
        for image in &desc.images {
            if !seen_images.insert(image.clone()) {
                continue;
            }
            println!("  pull image {image}");
            let ir = stage.pull_image(image).await?;
            println!("    -> manifest sha256={}", &ir.manifest_digest[..16]);
            images.push(ir);
        }
    }

    let name = spec
        .components
        .first()
        .map(|c| c.name.clone())
        .unwrap_or_else(|| "bundle".into());
    let manifest = Manifest {
        format_version: BUNDLE_FORMAT_VERSION,
        name,
        components: manifest_components,
        blobs,
        images,
        rootfs: vec![],
    };
    stage.write_manifest(&manifest)?;
    stage.verify(&manifest)?;

    bundle::pack(&stage_root, out)?;
    let _ = std::fs::remove_dir_all(&stage_root);
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    println!(
        "Wrote {} ({} component(s), {} blob(s), {} image(s), {} bytes).",
        out.display(),
        manifest.components.len(),
        manifest.blobs.len(),
        manifest.images.len(),
        size
    );
    Ok(())
}

/// `crater build --image`: wrap each component into a **B 类 OCI artifact**
/// (D-032, `artifactType: application/vnd.crater.component.v1`) — recipe layer +
/// one material layer per `download` (annotated source-url) + self-describing
/// annotations. NOT a runnable image (no fake rootfs config). Loaded by
/// recipe-replay (materials feed the recipe's download actions offline), so the
/// full recipe works — including systemd/run_cmd, no "bakeable-only" limit.
async fn build_image_bundle(spec_file: &Path, out: &Path) -> Result<()> {
    use crater_core::component::Action;

    let spec = CraterSpec::from_yaml_file(spec_file)?;
    let components_dir = PathBuf::from("components");
    let spec_dir = spec_file.parent().unwrap_or_else(|| Path::new("."));
    let stage_root = std::env::temp_dir().join(format!("crater-img-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage_root);
    let stage = bundle::BundleStage::new(stage_root.clone())?;
    let online = OnlineSource::with_default_mirrors();

    info!("building OCI component artifact(s) from {}", spec_file.display());
    let mut artifacts = Vec::new();
    for cref in &spec.components {
        let (desc, cdir) = resolve_descriptor(cref, &components_dir, spec_dir)?;
        let ver = cref
            .version
            .clone()
            .or_else(|| desc.version_default.clone())
            .unwrap_or_else(|| "latest".into());

        // Materials = the component's declared `materials:` closure (D-034),
        // keyed by logical NAME — the same key the recipe's `place` action
        // resolves against in offline replay. Reading the declaration (not
        // scraping install actions) is the whole point: nothing hidden in a
        // run_cmd can be missed. Legacy `download` actions are still packed by
        // their raw URL for back-compat. write_file/extract come from the recipe.
        let mut ctx = PlanContext::new(OsFamily::Unknown, ver.clone(), cdir.clone());
        ctx.offline_blobs = Some(BTreeMap::new()); // rendered_url yields raw URLs
        let mut materials: Vec<(String, Vec<u8>)> = Vec::new();
        for (name, raw) in collect_materials(&desc, &ctx)? {
            let url = online.rewrite(&raw);
            info!("  fetch material {name} <- {raw}");
            let (data, _) = source::fetch_best(&url)
                .await
                .map_err(|e| anyhow!("fetch material {name}: {e}"))?;
            materials.push((name, data));
        }
        for a in &desc.install {
            if let Action::Download { url_tmpl, .. } = a {
                let raw = ctx.rendered_url(url_tmpl)?;
                let url = online.rewrite(&raw);
                info!("  fetch {raw}");
                let (data, _) = source::fetch_best(&url)
                    .await
                    .map_err(|e| anyhow!("fetch {raw}: {e}"))?;
                materials.push((raw, data));
            }
        }
        let recipe = desc.to_yaml()?;
        let reference = format!("crater/{}:{ver}", cref.name);
        let ir = stage.store_component_artifact(
            &reference,
            &cref.name,
            &ver,
            "process",
            recipe.as_bytes(),
            &materials,
        )?;
        info!("  {} → artifact {reference}: recipe + {} material(s)", cref.name, materials.len());
        artifacts.push(ir);
    }

    stage.write_artifact_index(&artifacts)?;
    bundle::pack(&stage_root, out)?;
    let _ = std::fs::remove_dir_all(&stage_root);
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    info!(
        "wrote {} ({} component artifact(s), {} bytes) — OCI artifact ({})",
        out.display(),
        artifacts.len(),
        size,
        crater_core::bundle::AT_COMPONENT
    );
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
) -> Result<()> {
    use crater_core::spec::{ComponentRef, CraterSpec, Inventory};

    let dest_root = std::env::temp_dir().join(format!("crater-deploy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest_root);
    let stage = bundle::unpack(bundle_file, &dest_root)?;

    // B 类 artifact bundle (D-032)? → recipe-replay (materials feed the recipe).
    let recipe_dir = dest_root.join("__components");
    let mats = bundle::read_artifact_components(&dest_root, &recipe_dir)?;
    if !mats.is_empty() {
        info!("offline (B 类 artifact): {} component(s)", mats.len());
        let mut art_blobmap: BTreeMap<String, PathBuf> = BTreeMap::new();
        let mut components: Vec<ComponentRef> = Vec::new();
        for mc in mats {
            art_blobmap.extend(mc.blobmap);
            components.push(component_ref(&mc.name, &mc.version));
        }
        let spec = CraterSpec {
            inventory: Inventory { hosts },
            components,
            offline: true,
            ai: None,
        };
        let artifacts = Artifacts::Offline {
            blobmap: art_blobmap,
            rootfs: BTreeMap::new(),
        };
        let res = run_pipeline(&spec, &artifacts, &recipe_dir, &recipe_dir, do_apply, do_shell).await;
        let _ = std::fs::remove_dir_all(&dest_root);
        return res;
    }

    // Legacy recipe-replay bundle (crater-manifest + rootfs/blob layers).
    let manifest = stage.read_manifest()?;
    stage.verify(&manifest)?; // content-addressed: digests are the integrity check
    info!(
        "bundle {} — {} component(s), {} blob(s), {} rootfs image(s), checksums OK",
        manifest.name,
        manifest.components.len(),
        manifest.blobs.len(),
        manifest.rootfs.len()
    );

    let mut blobmap: BTreeMap<String, PathBuf> = BTreeMap::new();
    for b in &manifest.blobs {
        blobmap.insert(b.source_url.clone(), stage.blob_path(&b.sha256));
    }
    let mut rootfs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for ir in &manifest.rootfs {
        // reference is "crater/<component>:<ver>".
        let name = ir
            .reference
            .trim_start_matches("crater/")
            .split(':')
            .next()
            .unwrap_or(&ir.reference)
            .to_string();
        let layer_digest = stage.layer_of(&ir.manifest_digest)?;
        rootfs.insert(name, stage.blob_path(&layer_digest));
    }
    let artifacts = Artifacts::Offline { blobmap, rootfs };

    // Synthetic spec: recipes resolve from the bundle's staged components.
    let components: Vec<ComponentRef> = manifest
        .components
        .iter()
        .map(|mc| ComponentRef {
            name: mc.name.clone(),
            version: Some(mc.version.clone()),
            params: Default::default(),
            requires: vec![],
            images: vec![],
            materials: vec![],
            supported_os: vec![],
            preflight: vec![],
            install: vec![],
            verify: vec![],
            register: vec![],
        })
        .collect();
    let spec = CraterSpec {
        inventory: Inventory { hosts },
        components,
        offline: true,
        ai: None,
    };

    let res = run_pipeline(
        &spec,
        &artifacts,
        &stage.components_dir(),
        &dest_root,
        do_apply,
        do_shell,
    )
    .await;
    let _ = std::fs::remove_dir_all(&dest_root);
    res
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
    apply_oci_bundle(bundle_file, hosts, do_apply, false).await
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

/// A bare ComponentRef (name + version) whose recipe resolves from disk/bundle.
fn component_ref(name: &str, version: &str) -> crater_core::spec::ComponentRef {
    crater_core::spec::ComponentRef {
        name: name.to_string(),
        // Empty version → None, so version_default (or "latest") applies.
        version: (!version.is_empty()).then(|| version.to_string()),
        params: Default::default(),
        requires: vec![],
        images: vec![],
        materials: vec![],
        supported_os: vec![],
        preflight: vec![],
        install: vec![],
        verify: vec![],
        register: vec![],
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
    println!("{:<48} {:<20} {}", "REFERENCE", "DIGEST", "SIZE");
    for i in imgs {
        let short = i.digest.trim_start_matches("sha256:").chars().take(12).collect::<String>();
        println!("{:<48} {:<20} {}", i.reference, short, i.size);
    }
    Ok(())
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
) -> Result<()> {
    use crater_core::spec::{CraterSpec, Inventory};

    let store = ImageStore::open()?;
    if !store.has(reference) {
        info!("{reference} not in local store → pulling");
        store.pull(reference).await?;
    }

    // B 类 crater component artifact → recipe-replay (the unified pipeline).
    let manifest = store.resolve_manifest(reference)?;
    let recipe_dir = std::env::temp_dir().join(format!("crater-ref-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&recipe_dir);
    if let Some(mc) = bundle::materialize_component(&manifest, &store.blobs_dir(), &recipe_dir)? {
        info!("image {reference}: crater component artifact → recipe-replay");
        let spec = CraterSpec {
            inventory: Inventory { hosts },
            components: vec![component_ref(&mc.name, &mc.version)],
            offline: true,
            ai: None,
        };
        let artifacts = Artifacts::Offline {
            blobmap: mc.blobmap,
            rootfs: BTreeMap::new(),
        };
        let res = run_pipeline(&spec, &artifacts, &recipe_dir, &recipe_dir, do_apply, true).await;
        let _ = std::fs::remove_dir_all(&recipe_dir);
        return res;
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

fn copy_dir_all(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// M4: AI copilot — natural language -> validated crater.yaml
// ---------------------------------------------------------------------------

/// All systemd unit names declared across every component under `components/`.
/// `doctor` probes these instead of hardcoding any product's units: the names
/// live in component data, so a new component is diagnosable without code edits.
fn known_systemd_units(components_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(components_dir) {
        for e in rd.flatten() {
            let yaml = e.path().join("component.yaml");
            if !yaml.is_file() {
                continue;
            }
            if let Ok(desc) = ComponentDescriptor::from_yaml_file(&yaml) {
                out.extend(desc.systemd_units());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

/// List component names available under `components/`.
fn list_components(components_dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(components_dir) {
        for e in rd.flatten() {
            if e.path().join("component.yaml").is_file() {
                if let Some(name) = e.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out.sort();
    out
}

async fn ai_generate(request: &str, output: Option<PathBuf>) -> Result<()> {
    use crater_core::ai::{self, AiSettings, OpenAiCompatProvider};

    let components_dir = PathBuf::from("components");
    let available = list_components(&components_dir);
    if available.is_empty() {
        return Err(anyhow!("no components found under {}", components_dir.display()));
    }

    let settings = AiSettings::from_env().ok_or_else(|| {
        anyhow!(
            "AI not configured. Set CRATER_AI_ENDPOINT and CRATER_AI_MODEL (and \
             CRATER_AI_KEY if your endpoint needs one). Works with OpenAI, DeepSeek, \
             Qwen, or an on-prem OpenAI-compatible endpoint."
        )
    })?;
    println!(
        "AI: model={} endpoint={} | components: {}",
        settings.model,
        settings.endpoint,
        available.join(", ")
    );

    let provider = OpenAiCompatProvider::new(settings);
    let (yaml, spec) = ai::nl_to_spec(&provider, &available, request).await?;

    println!("\n# ---- generated & validated crater.yaml ----");
    println!("{yaml}");
    println!(
        "# ---- valid: {} host(s), {} component(s) ----",
        spec.inventory.hosts.len(),
        spec.components.len()
    );

    if let Some(out) = output {
        std::fs::write(&out, &yaml)?;
        println!("Wrote {}", out.display());
        println!("Next: crater apply -f {} (add --dry-run to preview first)", out.display());
    } else {
        println!("(Tip: -o crater.yaml to save, then `crater apply -f crater.yaml`.)");
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
        for unit in known_systemd_units(&PathBuf::from("components")) {
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
