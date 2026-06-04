//! `crater build / inspect`: task → B 类 OCI artifact in the local store
//! (D-045/D-046), `--set` build-stage overrides (D-089), os_package repo
//! synthesis (D-048/D-061), and the artifact/param contract inspector (D-081).

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use tracing::info;

use crater_core::engine::PlanContext;
use crater_core::os::OsFamily;
use crater_core::bundle;
use crater_core::source::{self, OnlineSource};
use crater_core::store::ImageStore;

use crate::apply::{find_named, roles_dir_for};

/// Parse `--set key=val` overrides (D-089) into a map. Each must contain `=`.
pub(crate) fn parse_set_overrides(set: &[String]) -> Result<BTreeMap<String, String>> {
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
pub(crate) async fn build_to_store(file: &Path, tag: Option<String>, arch_filter: &[String], set: &[String]) -> Result<()> {
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
pub(crate) async fn inspect_source(source: &str, gen_inventory: bool) -> Result<()> {
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
pub(crate) fn sanitize_ref(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// Resolve an OS-package dependency closure in `base` via **buildah** (daemonless)
/// and return it as a tar of the .deb/.rpm files (D-062). Mirrors KubeKey's
/// recipe but daemonless and without ISO/repo-metadata (apply uses
/// `apt-get install ./*.deb`). Requires `buildah` on the build machine.
pub(crate) fn build_os_package_repo(base: &str, family: &str, pkgs: &[String]) -> Result<Vec<u8>> {
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

pub(crate) async fn build_task_to_store(
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
