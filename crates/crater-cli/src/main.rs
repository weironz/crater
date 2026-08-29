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

mod agent;
mod apply;
mod blueprint;
mod build;
mod closure;
mod deployments;
mod fmt_cmd;
mod images;
mod inspect_bp;
mod material_ctx;
mod lint;
mod schema_cmd;
mod stack_cmd;
mod target;
mod types_cmd;
mod ui;
mod ui_contract;
mod ui_edit;
mod ui_run;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use tracing::info;

use target::TargetOpts;

use crater_core::executor::{Executor, SshExecutor};
use crate::blueprint::StackMode;
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
        /// Override an **apply-stage** param (`stage: apply`, e.g. vip/subnet):
        /// `--set vip=192.168.73.14`. Repeatable; highest priority (above
        /// inventory vars). Build-stage params (e.g. version) are REJECTED here —
        /// a built OCI is a frozen closure; rebuild with `crater build --set`
        /// (D-093). `crater inspect <source>` shows each param's stage.
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// Preview what an apply WOULD change (terraform-style, D-100): connect to
    /// the targets, probe each step's read-only idempotency check, and report
    /// ✓ ok / ~ would-change / ? unknown(no probe) / - skip(preflight/verify).
    /// Executes nothing. (`apply --dry-run` is the offline/static variant —
    /// it prints the plan without connecting.)
    Plan {
        /// `<source>` like apply: task.yaml | x.oci | image ref | named task/project.
        source: Option<String>,
        #[arg(short, long)]
        file: Option<PathBuf>,
        #[command(flatten)]
        target: TargetOpts,
        /// For an image/artifact source: probe against the FULL local closure.
        #[arg(long)]
        offline: bool,
        /// Apply-stage param overrides, same gate as `apply --set` (D-093).
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
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
        /// Override an apply-stage param for teardown rendering (same gate as
        /// `apply --set`, D-093) — supply the same values the deploy used.
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
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
        /// Access token (D-099): requests must present it (first visit
        /// `http://host:port/?token=<t>` sets a cookie). REQUIRED when binding
        /// beyond localhost — the UI can apply/delete deployments.
        #[arg(long)]
        token: Option<String>,
    },
    /// Build a task into a B 类 OCI artifact in the local store (like
    /// `docker build`). Export to a file with `crater save`.
    Build {
        /// Task file to build (its `materials` are fetched and packed), or a
        /// blueprint — a blueprint builds an **offline closure** to `--output`.
        #[arg(short, long)]
        file: PathBuf,
        /// Blueprint pipeline: write the offline closure here (e.g. `k8s.closure.tar`).
        /// Deploy it with `crater apply -f <blueprint> --closure <file>`.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Blueprint pipeline: bake only the variants matching this target
        /// profile, e.g. `--for arch=amd64 --for distro=ubuntu`. Omit to bake
        /// **every** declared variant (safest for air-gap).
        #[arg(long = "for", value_name = "KEY=VAL")]
        profile: Vec<String>,
        /// Reference (tag) for the artifact, e.g. `192.168.1.5:5000/yq:1.0`.
        /// Defaults to `crater/<name>:<version>`.
        #[arg(short = 't', long)]
        tag: Option<String>,
        /// Restrict packed material arches (D-048), e.g. `--arch amd64` or
        /// `--arch amd64,arm64`. Default: pack every declared arch variant.
        #[arg(long, value_delimiter = ',')]
        arch: Vec<String>,
        /// Bypass the build caches (D-096): re-fetch every material and rebuild
        /// even if the ref exists with an unchanged source fingerprint. Use when
        /// an upstream TAG moved (e.g. `latest`) — the fingerprint only sees the
        /// declared source, not remote content.
        #[arg(long)]
        no_cache: bool,
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
    /// Move a whole top-level section out to its own file (or bring it back).
    /// Mechanical and reversible — the merged result is equivalent to writing
    /// everything in one file, which is what separates this from `include`.
    Fmt {
        /// The blueprint's root file.
        file: PathBuf,
        /// Section to externalise into `<stem>.<section>.yaml`.
        #[arg(long)]
        split: Option<String>,
        /// Merge every externalised section back into the root file.
        #[arg(long)]
        join: bool,
    },
    /// Generate a JSON Schema for blueprints — editor completion, hover docs and
    /// inline validation. Pass `-f` to self-specialise it to one blueprint
    /// (its own material names and custom types become completions).
    Schema {
        /// Blueprint to self-specialise against.
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Output path (default `.crater/schema.json`).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Print to stdout instead of writing a file.
        #[arg(long = "stdout")]
        to_stdout: bool,
    },
    /// Show the built-in resource types and their fields — the answer to
    /// "what fields does `systemd_unit` take, and which are required?".
    /// Renders the same registry lint errors and the JSON Schema are generated
    /// from, so the three can never contradict each other.
    Types {
        /// A type name for the full field card; omit to list everything.
        name: Option<String>,
        /// Machine-readable output (for editors / schema generation).
        #[arg(long)]
        json: bool,
    },
    /// Lint blueprints — zero-connection static checks (D-107). Catches the whole
    /// class of errors Ansible only surfaces after connecting and reaching that line:
    /// misspelled module/argument/param names (with suggestions), out-of-scope CEL
    /// variables, undeclared materials, unbalanced cross-host facts.
    Lint {
        /// Files or directories to scan (recursively). Default: current directory.
        #[arg(default_value = ".")]
        paths: Vec<PathBuf>,
        /// Treat warnings as failures too (for CI).
        #[arg(long)]
        strict: bool,
        /// Machine-readable output for CI / editor integration.
        #[arg(long)]
        json: bool,
        /// Report per-section line counts. Informational only — line count does
        /// not measure complexity, so this never warns and never fails.
        #[arg(long)]
        stats: bool,
    },
    /// Run a named procedure from a blueprint — the "dance" a blueprint declares
    /// (bootstrap a cluster, roll an upgrade). Unlike `apply`, which converges
    /// resources host-by-host, a procedure is fleet-level: its steps span hosts
    /// and pass facts between them.
    Procedure {
        /// Procedure name (see `crater inspect`).
        name: String,
        #[arg(short, long)]
        file: Option<PathBuf>,
        #[command(flatten)]
        target: TargetOpts,
        /// Procedure params + deploy-stage overrides: `--set to=1.37.0`.
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// Retire a blueprint (or a stack, in reverse order) — remove every resource
    /// it declares. **Previews by default**: without `--yes` it only prints what
    /// would be removed and touches nothing.
    ///
    /// There is no `teardown:` section in a blueprint — retirement is derived
    /// from the five verbs, run in reverse declaration order.
    Destroy {
        /// Blueprint or stack file.
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Same, positionally.
        source: Option<String>,
        #[command(flatten)]
        target: TargetOpts,
        /// Actually remove. Without this the command is a read-only preview.
        #[arg(long)]
        yes: bool,
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// Verify a deployed blueprint — read-only drift check against the recorded
    /// state. Answers "is reality still what we deployed?", which `plan` cannot:
    /// without a record, "never deployed" and "drifted" look identical.
    Verify {
        /// Blueprint file (new IR pipeline).
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Or positionally.
        source: Option<String>,
        #[command(flatten)]
        target: TargetOpts,
        /// Deploy-stage param overrides (must match what was applied).
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
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
    /// Remove a reference from the local store, like `docker rmi` (D-097).
    /// Blobs are content-addressed and possibly shared — they stay until
    /// `crater gc` sweeps the unreferenced ones.
    Rmi {
        /// Reference to remove, e.g. `crater/yq:4.40.5`.
        reference: String,
    },
    /// Garbage-collect crater storage (D-097): sweep store blobs nothing
    /// references and stale build fingerprints. `--cache` also wipes the
    /// download cache; `--host`/`-i` additionally clears the staged-blob cache
    /// on TARGETS (`/var/lib/crater/blobs`, D-095 — re-staged on next apply).
    Gc {
        /// Also wipe the download cache (~/.crater/cache/{file,ospkg}).
        #[arg(long)]
        cache: bool,
        /// Report what would be freed, delete nothing.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        target: TargetOpts,
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

/// 命令行给的 source 是不是**新 IR blueprint 文件**。是则走五动词管线,
/// 否则(旧 task / 镜像 ref / .oci / 命名 task)交回原管线 —— 两条管线并存到迁移完成。
fn blueprint_source(file: &Option<PathBuf>, source: &Option<String>) -> Option<PathBuf> {
    let candidate = file.clone().or_else(|| source.as_ref().map(PathBuf::from))?;
    (candidate.is_file() && blueprint::is_blueprint_file(&candidate)).then_some(candidate)
}

/// `k8s-ha.blueprint.yaml` → `k8s-ha.closure.tar`;`platform.stack.yaml` → `platform.closure.tar`。
fn default_closure_path(file: &Path, kind_suffix: &str) -> PathBuf {
    let stem = file.file_stem().unwrap_or_default().to_string_lossy().into_owned();
    PathBuf::from(format!("{}.closure.tar", stem.trim_end_matches(kind_suffix)))
}

/// 同上,但认的是**栈**。栈与蓝图靠形状分辨(`stack:` + `uses:`),不靠文件名。
fn stack_source(file: &Option<PathBuf>, source: &Option<String>) -> Option<PathBuf> {
    let candidate = file.clone().or_else(|| source.as_ref().map(PathBuf::from))?;
    (candidate.is_file() && stack_cmd::is_stack_file(&candidate)).then_some(candidate)
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
            set,
        } => {
            // 新 IR blueprint 先分流(同 plan);其余走原管线。
            let probe = arg2.clone().or_else(|| arg1.clone());
            if let Some(p) = stack_source(&file, &probe) {
                let m = if dry_run { StackMode::Plan } else { StackMode::Apply };
                return stack_cmd::run(&p, &target, &set, m).await;
            }
            if let Some(p) = blueprint_source(&file, &probe) {
                if dry_run {
                    return blueprint::plan_blueprint(&p, &target, &set).await;
                }
                return blueprint::apply_blueprint(&p, &target, &set).await;
            }
            // Two positional forms: `apply <source>` or `apply <name> <source>`.
            let (name, source) = match (arg1, arg2) {
                (Some(a), Some(b)) => (Some(a), Some(b)),
                (Some(a), None) => (None, Some(a)),
                (None, _) => (None, None),
            };
            apply::apply_source(name, source, file, target, dry_run, shell, false, offline, &set, false).await
        }
        Cmd::Plan { source, file, target, offline, set } => {
            // 按文件格式分流:栈 → 逐蓝图;新 IR blueprint → 五动词管线;旧 task → 原管线。
            if let Some(p) = stack_source(&file, &source) {
                return stack_cmd::run(&p, &target, &set, StackMode::Plan).await;
            }
            match blueprint_source(&file, &source) {
                Some(p) => blueprint::plan_blueprint(&p, &target, &set).await,
                None => {
                    apply::apply_source(None, source, file, target, false, false, false, offline, &set, true)
                        .await
                }
            }
        }
        Cmd::Delete {
            source,
            file,
            target,
            dry_run,
            shell,
            set,
        } => apply::apply_source(None, source, file, target, dry_run, shell, true, false, &set, false).await,
        Cmd::Task { cmd } => match cmd {
            TaskCmd::List { target, verify } => deployments::task_list(target, verify).await,
            TaskCmd::Show { name, target, verify } => deployments::task_show(&name, target, verify).await,
            TaskCmd::History { limit } => deployments::task_history(limit).await,
        },
        Cmd::Ui { bind, port, token } => ui::serve(&bind, port, token).await,
        Cmd::Build { file, output, profile, tag, arch, no_cache, set } => {
            // 与 apply/plan 同一条按文件格式分派的路子:栈/蓝图烤闭包,task 进 store。
            if stack_cmd::is_stack_file(&file) {
                let out = output.unwrap_or_else(|| default_closure_path(&file, ".stack"));
                return closure::build_stack(&file, &out, &profile, &set).await;
            }
            if blueprint::is_blueprint_file(&file) {
                let out = output.unwrap_or_else(|| default_closure_path(&file, ".blueprint"));
                return closure::build(&file, &out, &profile, &set).await;
            }
            if output.is_some() || !profile.is_empty() {
                anyhow::bail!("`--output` / `--for` 只用于蓝图闭包;task 构建请用 `-t/--tag`");
            }
            build::build_to_store(&file, tag, &arch, &set, no_cache).await
        }
        Cmd::Inspect { source, gen_inventory } => {
            // 与 apply/plan 同一条按文件格式分派:蓝图/栈走 IR 的输入契约视图,
            // 其余(task 文件、OCI ref)仍走旧管线。
            let p = PathBuf::from(&source);
            if p.is_file() && (blueprint::is_blueprint_file(&p) || stack_cmd::is_stack_file(&p)) {
                if gen_inventory {
                    anyhow::bail!("`--gen-inventory` 暂只支持旧 task;蓝图请照 `需要的机群` 一节手写");
                }
                return inspect_bp::run(&p);
            }
            build::inspect_source(&source, gen_inventory).await
        }
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
        Cmd::Images => images::list_images().await,
        Cmd::Pull { reference } => images::pull_image(&reference).await,
        Cmd::Push { reference } => images::push_image(&reference).await,
        Cmd::Load { file, as_ref } => {
            let r = ImageStore::open()?.import_oci_archive(&file, as_ref.as_deref())?;
            info!("loaded {} → {r}", file.display());
            Ok(())
        }
        Cmd::Rmi { reference } => images::remove_image(&reference),
        Cmd::Gc { cache, dry_run, target } => images::gc(cache, dry_run, target).await,
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
            CreateWhat::Inventory { path, force } => target::create_inventory(&path, force),
        },
        Cmd::Fmt { file, split, join } => fmt_cmd::run(&file, split.as_deref(), join),
        Cmd::Types { name, json } => types_cmd::run(name.as_deref(), json),
        Cmd::Schema { file, output, to_stdout } => {
            schema_cmd::run(file.as_deref(), output.as_deref(), to_stdout)
        }
        Cmd::Lint { paths, strict, json, stats } => lint::run(&paths, strict, json, stats),
        Cmd::Procedure { name, file, target, set } => match blueprint_source(&file, &None) {
            Some(p) => blueprint::run_procedure(&p, &name, &target, &set).await,
            None => anyhow::bail!("`crater procedure` 需要 `-f <blueprint.yaml>`"),
        },
        Cmd::Destroy { file, source, target, yes, set } => {
            if let Some(p) = stack_source(&file, &source) {
                return stack_cmd::destroy(&p, &target, &set, yes).await;
            }
            match blueprint_source(&file, &source) {
                Some(p) => blueprint::destroy_blueprint(&p, &target, &set, yes).await,
                None => anyhow::bail!(
                    "`crater destroy` 只支持新 IR blueprint 与 stack;\
                     旧 task 的删除用 `crater delete`"
                ),
            }
        }
        Cmd::Verify { file, source, target, set } => {
            if let Some(p) = stack_source(&file, &source) {
                return stack_cmd::run(&p, &target, &set, StackMode::Verify).await;
            }
            match blueprint_source(&file, &source) {
                Some(p) => blueprint::verify_blueprint(&p, &target, &set).await,
                None => anyhow::bail!(
                    "`crater verify` 目前只支持新 IR blueprint 与 stack;\
                     旧 task 的漂移检测用 `crater task list --verify`"
                ),
            }
        }
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
        Cmd::Agent { task_plan } => agent::run_agent(&task_plan).await,
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
    let target = TargetOpts { inventory, host, user, password, key, port, parallel: 1, closure: None };
    apply::apply_source(None, Some(name), None, target, dry_run, shell, false, false, &[], false).await
}




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


