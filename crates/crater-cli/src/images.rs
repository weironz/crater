//! Image management: `crater images / pull / push` and `apply <image-ref>` —
//! crater task artifacts replay via the task pipeline; plain images extract
//! their rootfs layers to `/` on each host (D-032/D-045).

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use futures::StreamExt;
use tracing::info;

use crater_core::bundle;
use crater_core::executor::Executor;
use crater_core::executor::SshExecutor;
use crater_core::store::ImageStore;

use crate::apply::{apply_task, forks_limit, RunOpts};

/// `crater rmi <ref>` (D-097): drop the reference; blobs stay until `gc`.
pub(crate) fn remove_image(reference: &str) -> Result<()> {
    let store = ImageStore::open()?;
    if !store.remove(reference)? {
        anyhow::bail!("'{reference}' not in local store(`crater images` 看现有引用)");
    }
    info!("removed {reference}(blob 仍按内容寻址保留,`crater gc` 回收无引用的)");
    Ok(())
}

fn human(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1}GB", bytes as f64 / 1e9)
    } else if bytes >= 1_000_000 {
        format!("{:.1}MB", bytes as f64 / 1e6)
    } else {
        format!("{:.1}kB", bytes as f64 / 1e3)
    }
}

/// `crater gc` (D-097): reclaim disk.
///   1. store: mark-and-sweep unreferenced blobs (after `rmi`/rebuilds);
///   2. stale build fingerprints (`cache/builds/` whose ref left the store);
///   3. `--cache`: the whole download cache (file/ospkg, D-096);
///   4. `--host`/`-i`: each TARGET's staged-blob cache (`/var/lib/crater/blobs`,
///      D-095) — safe, next apply re-stages.
/// `--dry-run` reports without deleting. All four are caches/orphans: nothing
/// a deployment or a later build can't recreate.
pub(crate) async fn gc(cache: bool, dry_run: bool, target: crate::target::TargetOpts) -> Result<()> {
    let tag = if dry_run { "(dry-run)" } else { "" };
    let store = ImageStore::open()?;
    // 1. store blobs.
    let (swept, freed) = store.gc(dry_run)?;
    info!("store: {swept} 个无引用 blob,{} {tag}", human(freed));
    // 2. stale build fingerprints: sidecar files whose sanitized ref no longer
    //    matches any stored reference.
    let live: std::collections::BTreeSet<String> = store
        .list()?
        .iter()
        .map(|i| crate::build::sanitize_ref(&i.reference))
        .collect();
    let builds_dir = crate::build::cache_dir().join("builds");
    let (mut fp_swept, mut fp_freed) = (0usize, 0u64);
    if let Ok(rd) = std::fs::read_dir(&builds_dir) {
        for e in rd.flatten() {
            if !live.contains(&e.file_name().to_string_lossy().to_string()) {
                fp_freed += e.metadata().map(|m| m.len()).unwrap_or(0);
                fp_swept += 1;
                if !dry_run {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    info!("build 指纹: {fp_swept} 个过期 sidecar,{} {tag}", human(fp_freed));
    // 3. download cache (opt-in: it SAVES future fetches; only --cache drops it).
    if cache {
        let mut dl_freed = 0u64;
        for sub in ["file", "ospkg"] {
            let dir = crate::build::cache_dir().join(sub);
            if let Ok(rd) = std::fs::read_dir(&dir) {
                for e in rd.flatten() {
                    dl_freed += e.metadata().map(|m| m.len()).unwrap_or(0);
                }
            }
            if !dry_run {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
        info!("下载缓存: {} {tag}", human(dl_freed));
    }
    // 4. targets' staged-blob cache (only when targets were given explicitly —
    //    a bare `crater gc` must not "clean" localhost).
    if target.has_explicit_targets() {
        for host in target.hosts()? {
            let exec = crate::target::connect_executor(&host, true).await?;
            let du = exec
                .run("du -sb /var/lib/crater/blobs 2>/dev/null | cut -f1")
                .await?;
            let bytes: u64 = du.stdout.trim().parse().unwrap_or(0);
            if !dry_run {
                exec.run("rm -rf /var/lib/crater/blobs").await?;
            }
            info!("[{}] staged blobs: {} {tag}", host.name, human(bytes));
        }
    }
    Ok(())
}

pub(crate) async fn list_images() -> Result<()> {
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
pub(crate) fn human_size(bytes: u64) -> String {
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

pub(crate) async fn pull_image(reference: &str) -> Result<()> {
    let store = ImageStore::open()?;
    info!("pulling {reference} → local store ...");
    store.pull(reference).await?;
    info!("pulled {reference}");
    Ok(())
}

pub(crate) async fn push_image(reference: &str) -> Result<()> {
    let store = ImageStore::open()?;
    if !store.has(reference) {
        anyhow::bail!("{reference} not in local store (pull or build it first)");
    }
    // push is single-ref: a project artifact without its task artifacts is
    // unusable on the other side — point at save/.oci until multi-ref push lands.
    if store.resolve_manifest(reference)?["artifactType"].as_str()
        == Some("application/vnd.crater.project.v1")
    {
        anyhow::bail!(
            "'{reference}' 是项目制品,registry 分发未实现(它引用的 task 制品不会一起 push)。\
             离线分发用 `crater save {reference} -o env.oci`"
        );
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
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_image_ref(
    reference: &str,
    hosts: Vec<crater_core::spec::Host>,
    do_apply: bool,
    do_shell: bool,
    teardown: bool,
    source: &str,
    name: Option<&str>,
    offline: bool,
    set_overrides: std::collections::BTreeMap<String, String>,
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
    // A project artifact's closure lives in its referenced task artifacts —
    // store/registry-direct apply isn't wired yet (D-098 后续). Guard so it
    // never falls through to the plain-image rootfs-extract path.
    if manifest["artifactType"].as_str() == Some("application/vnd.crater.project.v1") {
        anyhow::bail!(
            "'{reference}' 是项目制品:导出后整包部署(crater save {reference} -o env.oci && \
             crater apply env.oci -i inv.yaml),在线则直接 apply -f <project>.yaml"
        );
    }
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
                // Honor --shell; default is the agent path — blobs are staged
                // onto the target first (D-095), no more forced control-plane.
                do_shell,
                teardown,
                source: source.to_string(),
                set_overrides,
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

pub(crate) async fn install_image_on_host(
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
