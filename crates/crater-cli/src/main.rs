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
//!   crater agent --task ...                                            (internal, on the node)

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

use crater_core::bundle::{self, BundleStage, Manifest, ManifestComponent, BUNDLE_FORMAT_VERSION};
use crater_core::component::ComponentDescriptor;
use crater_core::dag::{self, DepNode};
use crater_core::engine::{self, build_plan, collect_downloads, Op, PlanContext};
use crater_core::executor::{Executor, LocalExecutor, SshExecutor};
use crater_core::os::{self, OsFamily};
use crater_core::source::{self, OnlineSource};
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
    /// Apply a declarative spec file (crater.yaml). Executes by default;
    /// pass --dry-run to only print the plan.
    Apply {
        #[arg(short, long)]
        file: PathBuf,
        /// Print the plan without executing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Build an offline bundle from a spec (run on an online control machine).
    Build {
        #[arg(short, long)]
        file: PathBuf,
        #[arg(short, long)]
        output: PathBuf,
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
    Push {
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

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .init();

    match Cli::parse().cmd {
        Cmd::Apply { file, dry_run } => apply_spec(&file, !dry_run).await,
        Cmd::Build { file, output } => build_bundle(&file, &output).await,
        Cmd::Deploy {
            bundle,
            host,
            user,
            password,
            port,
            dry_run,
        } => deploy_bundle(&bundle, host, &user, password, port, !dry_run).await,
        Cmd::Push {
            host,
            user,
            password,
            port,
            src,
            dst,
            chmod,
        } => push_file(&host, &user, password, port, &src, &dst, chmod).await,
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
    /// Execute via the self-bootstrap agent (push binary + plan, run on target).
    agent: bool,
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
            "--agent" => {
                f.agent = true;
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
/// `components/`. `crater k8s` works because `k3s/component.yaml` declares it,
/// not because the code knows what k8s is.
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

    let mode = if !f.do_apply {
        "DRY-RUN"
    } else if f.agent {
        "APPLY (self-bootstrap agent)"
    } else {
        "APPLY"
    };
    println!("Component : {}", desc.name);
    println!("Version   : {ver}");
    println!("Target    : {}", exec.label());
    println!("OS family : {}", osf.as_str());
    println!("Mode      : {mode}");
    println!("Steps     : {}", plan.len());
    println!("------------------------------------------");
    print_plan(&plan);
    println!("------------------------------------------");

    if !f.do_apply {
        println!("Dry-run only (--dry-run). Omit it to execute.");
        Ok(())
    } else if f.agent {
        run_via_agent(exec.as_ref(), &plan, f.agent_bin.as_deref()).await
    } else {
        engine::execute(&plan, exec.as_ref()).await
    }
}

/// Self-bootstrap agent mode (D-019): push the crater binary + the lowered plan
/// to the target, then run `crater agent --plan` THERE so the plan executes
/// locally in one shot — fewer SSH round-trips, and the foundation for OCI
/// unpack / richer local logic. A one-shot bootstrap: nothing is left running.
async fn run_via_agent(exec: &dyn Executor, plan: &[Op], agent_bin: Option<&Path>) -> Result<()> {
    // Which binary to ship. Default: the running control binary (works when
    // control and target share OS/arch/libc); override with --agent-bin (e.g. a
    // musl static build) for heterogeneous targets.
    let bin_path = match agent_bin {
        Some(p) => p.to_path_buf(),
        None => std::env::current_exe()
            .map_err(|e| anyhow!("cannot locate current crater binary: {e}"))?,
    };
    let bytes = std::fs::read(&bin_path)
        .map_err(|e| anyhow!("read agent binary {}: {e}", bin_path.display()))?;

    let remote_bin = "/tmp/crater-agent";
    let remote_plan = "/tmp/crater-plan.yaml";
    println!(
        "Bootstrapping agent on {}: pushing binary ({} bytes from {}) + plan ...",
        exec.label(),
        bytes.len(),
        bin_path.display()
    );
    exec.write_file(remote_bin, &bytes).await?;
    exec.run(&format!("chmod +x {remote_bin}")).await?;
    exec.write_file(remote_plan, engine::plan_to_yaml(plan)?.as_bytes())
        .await?;

    println!("--- agent output (executing locally on target) ---");
    let out = exec
        .run(&format!("{remote_bin} agent --plan {remote_plan}"))
        .await?;
    print!("{}", out.stdout);
    if !out.stderr.trim().is_empty() {
        eprintln!("{}", out.stderr.trim());
    }
    // Clean up the bootstrap artifacts — one-shot, nothing left behind.
    let _ = exec
        .run(&format!("rm -f {remote_bin} {remote_plan}"))
        .await;
    if !out.ok() {
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
    println!("[agent] executing {} step(s) locally", ops.len());
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

async fn apply_spec(file: &Path, do_apply: bool) -> Result<()> {
    let spec = CraterSpec::from_yaml_file(file)?;
    let components_dir = PathBuf::from("components");
    // Inline recipes' relative template paths resolve against the spec's dir.
    let spec_dir = file.parent().unwrap_or_else(|| Path::new("."));
    let ordered = order_components(&spec, &components_dir)?;
    let by_name: BTreeMap<String, &crater_core::spec::ComponentRef> =
        spec.components.iter().map(|c| (c.name.clone(), c)).collect();
    println!(
        "Spec: {} host(s), {} component(s), order=[{}], mode={}",
        spec.inventory.hosts.len(),
        spec.components.len(),
        ordered.join(" -> "),
        if do_apply { "APPLY" } else { "DRY-RUN" }
    );

    if spec.inventory.hosts.is_empty() {
        for cname in &ordered {
            let cref = by_name[cname];
            let (desc, component_dir) = resolve_descriptor(cref, &components_dir, spec_dir)?;
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

        for cname in &ordered {
            let cref = by_name[cname];
            if !host.roles.is_empty() && !host.roles.contains(&cref.name) {
                continue;
            }
            let (desc, component_dir) = resolve_descriptor(cref, &components_dir, spec_dir)?;
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
        println!("\n(dry-run; omit --dry-run to execute over SSH.)");
    }
    Ok(())
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
    let mut seen: BTreeSet<String> = BTreeSet::new();

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
        let downloads = collect_downloads(&desc, &ctx);

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
    };
    stage.write_manifest(&manifest)?;
    stage.verify(&manifest)?;

    bundle::pack(&stage_root, out)?;
    let _ = std::fs::remove_dir_all(&stage_root);
    let size = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    println!(
        "Wrote {} ({} component(s), {} blob(s), {} bytes).",
        out.display(),
        manifest.components.len(),
        manifest.blobs.len(),
        size
    );
    Ok(())
}

async fn deploy_bundle(
    bundle_file: &Path,
    host: Option<String>,
    user: &str,
    password: Option<String>,
    port: u16,
    do_apply: bool,
) -> Result<()> {
    let dest_root = std::env::temp_dir().join(format!("crater-deploy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest_root);
    let stage = bundle::unpack(bundle_file, &dest_root)?;
    let manifest = stage.read_manifest()?;
    stage.verify(&manifest)?; // checksum every blob before touching the target
    println!(
        "Bundle: {} — {} component(s), {} blob(s), checksums OK",
        manifest.name,
        manifest.components.len(),
        manifest.blobs.len()
    );

    let mut blobmap: BTreeMap<String, PathBuf> = BTreeMap::new();
    for b in &manifest.blobs {
        blobmap.insert(b.source_url.clone(), stage.blob_path(&b.sha256));
    }

    let exec: Box<dyn Executor> = match &host {
        Some(h) => {
            let pw = password
                .as_deref()
                .ok_or_else(|| anyhow!("--password required for --host"))?;
            println!("Connecting to {user}@{h}:{port} ...");
            Box::new(SshExecutor::connect(h, port, user, pw).await?)
        }
        None => Box::new(LocalExecutor),
    };
    let osf = if do_apply && host.is_some() {
        os::detect_via(exec.as_ref()).await
    } else {
        OsFamily::Unknown
    };

    for mc in &manifest.components {
        let cdir = stage.components_dir().join(&mc.name);
        let desc = ComponentDescriptor::from_yaml_file(&cdir.join("component.yaml"))?;
        let ctx = PlanContext::new(osf, mc.version.clone(), cdir).with_offline(blobmap.clone());
        let plan = build_plan(&desc, &ctx)?;
        println!(
            "\n--- component {} (v{}) — {} steps [{}] ---",
            mc.name,
            mc.version,
            plan.len(),
            if do_apply { "APPLY" } else { "DRY-RUN" }
        );
        print_plan(&plan);
        if do_apply {
            engine::execute(&plan, exec.as_ref()).await?;
        }
    }
    let _ = std::fs::remove_dir_all(&dest_root);
    if !do_apply {
        println!("\n(dry-run; omit --dry-run to execute.)");
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
/// `doctor` probes these instead of hardcoding `docker`/`k3s`: the unit names
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
