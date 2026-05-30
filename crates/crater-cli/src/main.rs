//! `crater` CLI.
//!
//! Forms:
//!   crater <component> [--host H --user U --password P --port N] [--version X] [--os debian|rhel] [--apply]
//!   crater apply -f crater.yaml [--apply]
//!   crater build -f spec.yaml -o x.bundle                              (online: make offline bundle)
//!   crater deploy --bundle x.bundle --host H --password P [--apply]    (offline)
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
    /// Apply a declarative spec file (crater.yaml).
    Apply {
        #[arg(short, long)]
        file: PathBuf,
        #[arg(long)]
        apply: bool,
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
        #[arg(long)]
        apply: bool,
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
        Cmd::Build { file, output } => build_bundle(&file, &output).await,
        Cmd::Deploy {
            bundle,
            host,
            user,
            password,
            port,
            apply,
        } => deploy_bundle(&bundle, host, &user, password, port, apply).await,
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

/// Map user-friendly aliases to actual component directory names, so the
/// literal examples `crater k8s` / `crater es` work even though the components
/// are `k3s` / `elasticsearch`.
fn resolve_alias(name: &str) -> &str {
    match name {
        "k8s" | "kubernetes" => "k3s",
        "es" => "elasticsearch",
        other => other,
    }
}

async fn deploy_shortcut(args: Vec<String>) -> Result<()> {
    let mut it = args.into_iter();
    let raw = it.next().ok_or_else(|| anyhow!("missing component name"))?;
    let name = resolve_alias(&raw).to_string();
    let rest: Vec<String> = it.collect();
    let f = parse_flags(&rest)?;

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

    println!("Component : {}", desc.name);
    println!("Version   : {ver}");
    println!("Target    : {}", exec.label());
    println!("OS family : {}", osf.as_str());
    println!("Mode      : {}", if f.do_apply { "APPLY" } else { "DRY-RUN" });
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

/// Order selected components by their `requires` DAG (deps first). Edges to
/// components not in this spec are ignored (lenient); cycles error out.
fn order_components(spec: &CraterSpec, components_dir: &Path) -> Result<Vec<String>> {
    let selected: BTreeSet<String> = spec.components.iter().map(|c| c.name.clone()).collect();
    let mut nodes = Vec::new();
    for cref in &spec.components {
        let desc = ComponentDescriptor::from_yaml_file(
            &components_dir.join(&cref.name).join("component.yaml"),
        )?;
        let requires = desc
            .requires
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
            let component_dir = components_dir.join(&cref.name);
            let desc = ComponentDescriptor::from_yaml_file(&component_dir.join("component.yaml"))?;
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
            let component_dir = components_dir.join(&cref.name);
            let desc = ComponentDescriptor::from_yaml_file(&component_dir.join("component.yaml"))?;
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
    for cref in &spec.components {
        let cdir = components_dir.join(&cref.name);
        let desc = ComponentDescriptor::from_yaml_file(&cdir.join("component.yaml"))?;
        let ver = cref
            .version
            .clone()
            .or_else(|| desc.version_default.clone())
            .unwrap_or_else(|| "latest".into());

        copy_dir_all(&cdir, &stage.components_dir().join(&cref.name))?;
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
        println!("\n(dry-run; re-run with --apply to execute.)");
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
        println!("Next: crater apply -f {} (dry-run) then add --apply", out.display());
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
        // Collect common failure signals from the box.
        let probe = "echo '== journal: docker =='; journalctl -u docker --no-pager -n 50 2>/dev/null; \
                     echo '== journal: k3s =='; journalctl -u k3s --no-pager -n 50 2>/dev/null; \
                     echo '== disk =='; df -h 2>/dev/null; \
                     echo '== apt =='; tail -n 50 /var/log/apt/term.log 2>/dev/null";
        exec.run(probe).await?.stdout
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
