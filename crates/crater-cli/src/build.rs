//! `crater build / inspect`: task → B 类 OCI artifact in the local store
//! (D-045/D-046), `--set` build-stage overrides (D-089), os_package repo
//! synthesis (D-048/D-061), and the artifact/param contract inspector (D-081).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
pub(crate) async fn build_to_store(
    file: &Path,
    tag: Option<String>,
    arch_filter: &[String],
    set: &[String],
    no_cache: bool,
) -> Result<()> {
    let overrides = parse_set_overrides(set)?;
    // A project (top-level `plays:`, D-098): build every play's task artifact,
    // then a project artifact whose recipe locks each play to its built ref.
    if crater_core::project::is_project_file(file) {
        return build_project_to_store(file, tag, arch_filter, &overrides, no_cache).await;
    }
    // Otherwise a task (D-046): task → B 类 artifact whose recipe IS the task YAML.
    if !crater_core::task::is_task_file(file) {
        anyhow::bail!(
            "{}: not a task file (needs top-level `actions:`) or project (top-level `plays:`).",
            file.display()
        );
    }
    build_task_to_store(file, tag, arch_filter, &overrides, no_cache).await?;
    Ok(())
}

/// `crater build -f project.yaml` (D-098): the offline story for a whole
/// environment. Each play's task is built into the store (D-096 caches apply
/// per task), then a PROJECT artifact is stored whose recipe is the project
/// with every play `source` REWRITTEN to the built task ref (a lock). `crater
/// save <project-ref>` then exports project + all task closures as one .oci.
async fn build_project_to_store(
    file: &Path,
    tag: Option<String>,
    arch_filter: &[String],
    overrides: &BTreeMap<String, String>,
    no_cache: bool,
) -> Result<()> {
    use crater_core::project::Project;
    let mut project = Project::from_yaml_file(file)?;
    if project.plays.is_empty() {
        anyhow::bail!("project '{}' 没有 plays", project.name);
    }
    info!("build project '{}': {} play(s)", project.name, project.plays.len());
    // Build each play's task. Play vars participate as build overrides (they
    // may pin versions → affect materials), CLI --set wins over play vars.
    // Same default ref reached from two plays must mean the same inputs —
    // otherwise the lock would silently point both at the second build.
    let mut built: BTreeMap<String, (PathBuf, BTreeMap<String, String>)> = BTreeMap::new();
    let total = project.plays.len();
    for (i, play) in project.plays.iter_mut().enumerate() {
        let label = play.name.clone().unwrap_or_else(|| play.source.clone());
        let task_file = find_named(&play.source)
            .filter(|f| crater_core::task::is_task_file(f))
            .ok_or_else(|| anyhow!("play '{label}': task '{}' 未找到(路径或 library/**/{}.yaml)", play.source, play.source))?;
        let mut play_overrides = play.vars.clone();
        for (k, v) in overrides {
            play_overrides.insert(k.clone(), v.clone());
        }
        info!("── build play {}/{total}: {label}({})", i + 1, task_file.display());
        let task_ref =
            build_task_to_store(&task_file, None, arch_filter, &play_overrides, no_cache).await?;
        if let Some((prev_file, prev_vars)) = built.get(&task_ref) {
            if (prev_file, prev_vars) != (&task_file, &play_overrides) {
                anyhow::bail!(
                    "play '{label}' 与之前的 play 构建出同一个 ref '{task_ref}' 但输入不同 \
                     (任务文件或 vars 不一致)—— 锁定会指向后者。请用不同 version(进默认 tag)区分"
                );
            }
        }
        built.insert(task_ref.clone(), (task_file, play_overrides));
        play.source = task_ref; // the lock: offline replay resolves by ref
    }
    // Store the project artifact (recipe = locked project, no material layers).
    let reference = tag.unwrap_or_else(|| format!("crater/{}:latest", project.name));
    let recipe = serde_yaml::to_string(&project)?.into_bytes();
    let stage_root = std::env::temp_dir().join(format!("crater-projimg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage_root);
    let stage = bundle::BundleStage::new(stage_root.clone())?;
    let ir = stage.store_project_artifact(&reference, &project.name, &recipe)?;
    stage.write_artifact_index(&[ir])?;
    let tmp_oci = std::env::temp_dir().join(format!("crater-projbuild-{}.oci", std::process::id()));
    bundle::pack(&stage_root, &tmp_oci)?;
    ImageStore::open()?.import_all(&tmp_oci)?;
    let _ = std::fs::remove_dir_all(&stage_root);
    let _ = std::fs::remove_file(&tmp_oci);
    info!("built project {reference}(锁定 {} 个 task 制品)→ 本地库;`crater save {reference} -o <x>.oci` 导出整套", built.len());
    Ok(())
}

// ---------------------------------------------------------------------------
// Build caches (D-096): download cache + whole-build fingerprint
// ---------------------------------------------------------------------------

/// `~/.crater/cache` — build-time caches, content/source-addressed. Safe to
/// `rm -rf` anytime (it only ever saves refetches/rebuilds).
pub(crate) fn cache_dir() -> PathBuf {
    ImageStore::home().join("cache")
}

/// Cache key for a `file` material download: the DECLARED `sha256` when present
/// (content-addressed — survives URL/mirror changes), else the hash of the
/// rendered source URL (refetched only when the URL itself changes).
pub(crate) fn file_cache_key(declared_sha: Option<&str>, raw_url: &str) -> String {
    match declared_sha {
        Some(s) => s.to_string(),
        None => crater_core::bundle::sha256_hex(raw_url.as_bytes()),
    }
}

/// Read a cached blob; verify against a declared sha256 when given (a corrupt
/// cache entry is dropped, not trusted).
fn cache_get(path: &Path, declared_sha: Option<&str>) -> Option<Vec<u8>> {
    let data = std::fs::read(path).ok()?;
    if let Some(want) = declared_sha {
        if crater_core::bundle::sha256_hex(&data) != want {
            let _ = std::fs::remove_file(path);
            return None;
        }
    }
    Some(data)
}

fn cache_put(path: &Path, data: &[u8]) {
    // Best-effort: a failed cache write must never fail the build.
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, data);
}

/// Whole-build fingerprint (D-096): hash of the role-expanded recipe + every
/// material's SOURCE descriptor (rendered URL/ref, local-file content hash,
/// os_package base+packages) + the arch filter. Same fingerprint + ref already
/// in store ⇒ rebuilding would reproduce the same artifact, so build skips.
/// NOTE: an upstream tag/URL whose CONTENT moved is invisible here — pin
/// versions, or escape with `--no-cache`.
fn build_fingerprint(recipe: &[u8], material_descs: &[String], arch_filter: &[String]) -> String {
    let mut parts = vec![crater_core::bundle::sha256_hex(recipe)];
    parts.extend_from_slice(material_descs);
    parts.push(format!("arch={}", arch_filter.join(",")));
    crater_core::bundle::sha256_hex(parts.join("\n").as_bytes())
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

/// Build one task into the store; returns the artifact REFERENCE (the default
/// `crater/<name>:<version>` or `tag`) — project builds lock plays to it.
pub(crate) async fn build_task_to_store(
    file: &Path,
    tag: Option<String>,
    arch_filter: &[String],
    overrides: &BTreeMap<String, String>,
    no_cache: bool,
) -> Result<String> {
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

    // Whole-build cache (D-096): fingerprint the SOURCES (recipe + material
    // descriptors) before any fetching. Ref already built from identical
    // sources → skip the whole build. Descriptor rendering mirrors the fetch
    // loop below (same order, same `{{arch}}` var handling) so both agree.
    let recipe = serde_yaml::to_string(&task)?.into_bytes();
    let mut material_descs: Vec<String> = Vec::new();
    let mut image_refs: Vec<String> = Vec::new();
    for m in &task.materials {
        if m.kind == MaterialKind::File {
            if !want_arch.is_empty() {
                if let Some(a) = m.arch {
                    if !want_arch.contains(&a) {
                        continue;
                    }
                }
            }
            let key = PlanContext::material_blob_key(m);
            if let Some(tmpl) = &m.url_tmpl {
                if let Some(a) = m.arch {
                    ctx.vars.insert("arch".to_string(), a.as_str().to_string());
                }
                let raw = ctx.rendered_url(tmpl)?;
                material_descs.push(format!(
                    "file:{key}:{raw}:{}",
                    m.sha256.as_deref().unwrap_or("")
                ));
            } else if let Some(src) = &m.src {
                // Local file: the CONTENT is the source — edits invalidate.
                let data = std::fs::read(spec_dir.join(src))
                    .map_err(|e| anyhow!("read material {key} from {}: {e}", spec_dir.join(src).display()))?;
                material_descs.push(format!("src:{key}:{}", crater_core::bundle::sha256_hex(&data)));
            }
        } else if m.kind == MaterialKind::Image {
            if let Some(r) = &m.reference {
                let rendered = ctx.rendered_url(r)?;
                material_descs.push(format!("img:{}:{rendered}", m.name));
                // Collected here (same ordered walk, same `{{arch}}` state as
                // the fetch loop) for the concurrent pre-pull below (D-078④).
                image_refs.push(rendered);
            }
        } else if m.kind == MaterialKind::OsPackage {
            let pkgs: Vec<String> = m.packages.values().flatten().cloned().collect();
            material_descs.push(format!(
                "pkg:{}:{}:{}",
                m.name,
                m.base.as_deref().unwrap_or(""),
                pkgs.join(",")
            ));
        }
    }
    let fingerprint = build_fingerprint(&recipe, &material_descs, arch_filter);
    let fp_file = cache_dir().join("builds").join(sanitize_ref(&reference));
    if !no_cache
        && ImageStore::open()?.has(&reference)
        && std::fs::read_to_string(&fp_file).is_ok_and(|s| s.trim() == fingerprint)
    {
        info!("{reference} 已在本地库且源未变(指纹 {})— 构建缓存命中,跳过(--no-cache 强制重建)", &fingerprint[..12]);
        return Ok(reference);
    }

    // Parallel image pre-pull (D-078④): pull all image materials concurrently
    // — the per-image cost is mostly TLS + manifest round-trips, which stack
    // nicely. Blob writes are content-addressed (safe); index tagging is
    // serialized by the store's index lock. The pack loop below then hits the
    // already-pulled fast path (D-078① digest skip).
    if image_refs.len() > 1 {
        use futures::StreamExt;
        info!("  pre-pull {} image material(s) (parallel)", image_refs.len());
        let store = ImageStore::open()?;
        let results: Vec<Result<()>> = futures::stream::iter(image_refs.iter().map(|r| {
            let store = &store;
            async move {
                store.pull(r).await.map_err(|e| anyhow!("pull image {r}: {e}"))
            }
        }))
        .buffer_unordered(4)
        .collect()
        .await;
        for r in results {
            r?;
        }
    }

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
                // Download cache (D-096): addressed by declared sha256 (content)
                // or by the rendered URL (source). `--no-cache` forces a refetch.
                let ckey = file_cache_key(m.sha256.as_deref(), &raw);
                let cpath = cache_dir().join("file").join(&ckey);
                let cached = if no_cache { None } else { cache_get(&cpath, m.sha256.as_deref()) };
                let data = match cached {
                    Some(data) => {
                        info!("  material {key} <- 缓存({})", cpath.display());
                        data
                    }
                    None => {
                        let url = online.rewrite(&raw);
                        info!("  fetch material {key} <- {raw}");
                        let (data, _) = source::fetch_best(&url)
                            .await
                            .map_err(|e| anyhow!("fetch material {key}: {e}"))?;
                        // Verify a declared digest BEFORE caching/packing.
                        if let Some(want) = &m.sha256 {
                            let got = crater_core::bundle::sha256_hex(&data);
                            if &got != want {
                                anyhow::bail!("material {key}: sha256 不符(声明 {want},实际 {got})");
                            }
                        }
                        cache_put(&cpath, &data);
                        data
                    }
                };
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
            let store = ImageStore::open()?;
            if image_refs.len() > 1 {
                // Already fetched by the parallel pre-pull (D-078④) — packing
                // reads straight from the store, no second manifest round-trip.
                info!("  image material {key} <- {reference}(已预拉)");
            } else {
                info!("  pull image material {key} <- {reference}");
                store.pull(&reference).await.map_err(|e| anyhow!("pull image {reference}: {e}"))?;
            }
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
            // Download cache (D-096): the buildah closure is minutes of work;
            // addressed by hash(base + family + packages).
            let ckey = crater_core::bundle::sha256_hex(
                format!("{base}|{family}|{}", pkgs.join(",")).as_bytes(),
            );
            let cpath = cache_dir().join("ospkg").join(format!("{ckey}.tar"));
            let cached = if no_cache { None } else { cache_get(&cpath, None) };
            let data = match cached {
                Some(data) => {
                    info!("  os_package {key} <- 缓存({})", cpath.display());
                    data
                }
                None => {
                    info!("  build os_package {key} <- {base} [{}] (buildah)", pkgs.join(" "));
                    let data = build_os_package_repo(base, family, &pkgs)?;
                    info!("    packed closure ({} bytes)", data.len());
                    cache_put(&cpath, &data);
                    data
                }
            };
            materials.push((key, false, data)); // os_package source → dependency layer (D-087)
        }
    }

    // Recipe = the ROLE-EXPANDED task (flat, self-contained), not the raw file —
    // so offline replay needs no role files (D-080). Note: drops source comments.
    // (Serialized once above for the build fingerprint, D-096.)
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
    // Record the source fingerprint (D-096) so an unchanged rebuild can skip.
    cache_put(&fp_file, fingerprint.as_bytes());
    Ok(reference)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D-096: declared sha256 wins (content-addressed); otherwise the key is
    /// derived from the rendered URL (source-addressed).
    #[test]
    fn file_cache_key_prefers_declared_sha() {
        assert_eq!(file_cache_key(Some("abc123"), "https://x/v1"), "abc123");
        let by_url = file_cache_key(None, "https://x/v1");
        assert_eq!(by_url, crater_core::bundle::sha256_hex(b"https://x/v1"));
        assert_ne!(by_url, file_cache_key(None, "https://x/v2"));
    }

    /// D-096: the build fingerprint reacts to every source input — recipe,
    /// material descriptors, arch filter — and only to them.
    #[test]
    fn build_fingerprint_tracks_all_sources() {
        let base = build_fingerprint(b"recipe", &["file:a:url:".into()], &[]);
        assert_eq!(base, build_fingerprint(b"recipe", &["file:a:url:".into()], &[]));
        assert_ne!(base, build_fingerprint(b"recipe2", &["file:a:url:".into()], &[]));
        assert_ne!(base, build_fingerprint(b"recipe", &["file:a:url2:".into()], &[]));
        assert_ne!(base, build_fingerprint(b"recipe", &["file:a:url:".into()], &["amd64".into()]));
    }

    /// D-096: a corrupt cache entry (declared sha mismatch) is dropped, not used.
    #[test]
    fn cache_get_drops_corrupt_entries() {
        let p = std::env::temp_dir().join(format!("crater-cache-test-{}", std::process::id()));
        std::fs::write(&p, b"good content").unwrap();
        let sha = crater_core::bundle::sha256_hex(b"good content");
        assert_eq!(cache_get(&p, Some(&sha)).unwrap(), b"good content");
        std::fs::write(&p, b"tampered").unwrap();
        assert!(cache_get(&p, Some(&sha)).is_none(), "corrupt entry rejected");
        assert!(!p.exists(), "corrupt entry deleted");
    }
}
