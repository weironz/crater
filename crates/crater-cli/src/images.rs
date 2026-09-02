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
///
/// `--dry-run` reports without deleting. All four are caches/orphans: nothing
/// a deployment or a later build can't recreate.
pub(crate) async fn gc(
    cache: bool,
    dry_run: bool,
    target: crate::target::TargetOpts,
) -> Result<()> {
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
    info!(
        "build 指纹: {fp_swept} 个过期 sidecar,{} {tag}",
        human(fp_freed)
    );
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
        let short = i
            .digest
            .trim_start_matches("sha256:")
            .chars()
            .take(12)
            .collect::<String>();
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

const AT_PROJECT: &str = "application/vnd.crater.project.v1";

/// A project's locked task refs, deduped in play order (D-101 closure walk).
fn locked_refs(project: &crater_core::project::Project) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in &project.plays {
        if !out.contains(&p.source) {
            out.push(p.source.clone());
        }
    }
    out
}

pub(crate) async fn pull_image(reference: &str) -> Result<()> {
    let store = ImageStore::open()?;
    info!("pulling {reference} → local store ...");
    store.pull(reference).await?;
    // Project closure pull (D-101): fetch each locked task from the SAME
    // registry (`<registry>/<bare-lock>` — the twin `push` created), then
    // retag to the bare lock so the recipe's `source:` resolves locally.
    let manifest = store.resolve_manifest(reference)?;
    if manifest["artifactType"].as_str() == Some(AT_PROJECT) {
        let registry = crater_core::store::registry_of(reference);
        let project = store.project_recipe(&manifest)?;
        for l in locked_refs(&project) {
            let remote = format!("{registry}/{l}");
            info!("  闭包成员 {l} ← {remote}");
            store.pull(&remote).await?;
            store.retag(&remote, &l)?;
        }
    }
    info!("pulled {reference}");
    Ok(())
}

pub(crate) async fn push_image(reference: &str) -> Result<()> {
    let store = ImageStore::open()?;
    if !store.has(reference) {
        anyhow::bail!("{reference} not in local store (pull or build it first)");
    }
    // Project closure push (D-101): the recipe locks plays to BARE task refs
    // (`crater/yq:4.44.3`) — push each as `<registry-of-project-ref>/<lock>`
    // first, then the project manifest itself. Best fit: PRIVATE registries
    // (arbitrary repo paths); docker.io's namespace rules won't accept the
    // bare `crater/...` paths — use `crater save` there.
    let manifest = store.resolve_manifest(reference)?;
    if manifest["artifactType"].as_str() == Some(AT_PROJECT) {
        let registry = crater_core::store::registry_of(reference);
        let project = store.project_recipe(&manifest)?;
        for l in locked_refs(&project) {
            if !store.has(&l) {
                anyhow::bail!(
                    "项目锁定的 task 制品 '{l}' 不在本地库 —— 先 `crater build -f <project>.yaml`"
                );
            }
            let remote = format!("{registry}/{l}");
            store.retag(&l, &remote)?;
            info!("  push 闭包成员 {l} → {remote}");
            store.push(&remote).await?;
        }
    }
    info!("pushing {reference} → registry ...");
    store.push(reference).await?;
    info!("pushed {reference}");
    Ok(())
}

/// Ensure `local` is usable from the store — thin for online, FULL closure for
/// `--offline` (D-087) — fetching from `remote` (its registry twin; same as
/// `local` for direct refs) and retagging when they differ (D-101).
async fn ensure_pulled(store: &ImageStore, local: &str, remote: &str, offline: bool) -> Result<()> {
    if offline {
        if !store.has(local) || !store.has_all_layers(local) {
            info!("{local}: pulling full closure (--offline) ← {remote}");
            store.pull(remote).await?;
            if remote != local {
                store.retag(remote, local)?;
            }
        }
    } else if !store.has(local) {
        info!("{local}: thin pull(recipe + embedded;依赖 apply 时在线取)← {remote}");
        store.pull_thin(remote).await?;
        if remote != local {
            store.retag(remote, local)?;
        }
    }
    Ok(())
}

/// Materialize ONE crater task artifact from the store and run it through the
/// task pipeline. `graceful_no_teardown`: project plays skip teardown-less
/// tasks (D-098 semantics); a direct single-ref delete stays a hard error.
// 参数确实构成一个整体,而那个整体已经有名字了:`RunOpts`(见 #20)。
// 问题不是"参数多",是它在管线深处才被组装 —— 全仓 7 处各自拼一遍,
// 五个入口手抄散装参数往下传。
//
// 留 allow 而不是现在就抽:这是 D-106 降级的旧 task 管线,而且这五个函数
// **零直接测试覆盖**,把 RunOpts 从 CLI 边界穿下去是一次没有安全网的重构。
// #20 的第一步是补端到端测试,不是改签名。
#[allow(clippy::too_many_arguments)]
async fn apply_task_artifact(
    store: &ImageStore,
    reference: &str,
    hosts: Vec<crater_core::spec::Host>,
    do_apply: bool,
    do_shell: bool,
    teardown: bool,
    source: &str,
    name: Option<&str>,
    offline: bool,
    set_overrides: std::collections::BTreeMap<String, String>,
    plan: bool,
    hosts_override: Option<String>,
    var_overrides: std::collections::BTreeMap<String, String>,
    graceful_no_teardown: bool,
) -> Result<()> {
    let manifest = store.resolve_manifest(reference)?;
    let recipe_dir = std::env::temp_dir().join(format!(
        "crater-ref-{}-{}",
        std::process::id(),
        crate::build::sanitize_ref(reference)
    ));
    let _ = std::fs::remove_dir_all(&recipe_dir);
    let Some(mc) = bundle::materialize_component(&manifest, &store.blobs_dir(), &recipe_dir)?
    else {
        anyhow::bail!("'{reference}' 不是 crater task 制品(无 recipe)");
    };
    let recipe_file = recipe_dir.join(&mc.name).join("component.yaml");
    if !crater_core::task::is_task_file(&recipe_file) {
        let _ = std::fs::remove_dir_all(&recipe_dir);
        anyhow::bail!(
            "'{reference}' is a legacy component artifact; rebuild it as a task \
             (crater build -f tasks/<name>.yaml)"
        );
    }
    if teardown && graceful_no_teardown {
        let t = crater_core::task::TaskFile::from_yaml_file(&recipe_file)?;
        if t.teardown.is_empty() {
            info!("   (跳过:task '{}' 未编写 teardown)", t.name);
            let _ = std::fs::remove_dir_all(&recipe_dir);
            return Ok(());
        }
    }
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
        plan_check: plan,
    };
    let res = apply_task(
        &recipe_file,
        hosts,
        opts,
        name,
        hosts_override,
        var_overrides,
    )
    .await;
    let _ = std::fs::remove_dir_all(&recipe_dir);
    res
}

/// `crater apply <image-ref>`: resolve from the local store (pull on miss). A
/// **crater component artifact** (B 类, D-032) → recipe-replay via `run_pipeline`
/// (materials feed the recipe offline). A project artifact → registry/store-
/// direct play orchestration (D-101). A plain container image → extract its
/// rootfs layers to `/` on each host (parallel). crater-native, no runtime.
// 参数确实构成一个整体,而那个整体已经有名字了:`RunOpts`(见 #20)。
// 问题不是"参数多",是它在管线深处才被组装 —— 全仓 7 处各自拼一遍,
// 五个入口手抄散装参数往下传。
//
// 留 allow 而不是现在就抽:这是 D-106 降级的旧 task 管线,而且这五个函数
// **零直接测试覆盖**,把 RunOpts 从 CLI 边界穿下去是一次没有安全网的重构。
// #20 的第一步是补端到端测试,不是改签名。
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
    plan: bool,
) -> Result<()> {
    let store = ImageStore::open()?;
    ensure_pulled(&store, reference, reference, offline).await?;

    // crater task artifact (B 类) → recipe-replay via the task pipeline (D-045).
    let manifest = store.resolve_manifest(reference)?;
    // Project artifact (D-101): store/registry-direct orchestration — same
    // play loop as the bundle path, with each locked task materialized from
    // the local store (auto-fetched from the project's registry when absent).
    if manifest["artifactType"].as_str() == Some(AT_PROJECT) {
        let project = store.project_recipe(&manifest)?;
        let registry = crater_core::store::registry_of(reference);
        let verb = if teardown {
            "delete"
        } else if plan {
            "plan"
        } else {
            "apply"
        };
        let deployment = name
            .map(|s| s.to_string())
            .unwrap_or_else(|| project.name.clone());
        let mut order: Vec<&crater_core::project::Play> = project.plays.iter().collect();
        if teardown {
            order.reverse();
        }
        info!(
            "{verb} project '{}'(registry/store 直连): {} play(s){}",
            project.name,
            order.len(),
            if teardown { "(逆序)" } else { "" }
        );
        let total = order.len();
        for (i, play) in order.iter().enumerate() {
            let label = play.name.clone().unwrap_or_else(|| play.source.clone());
            info!(
                "── play {}/{total}: {label}(source={}, hosts={})",
                i + 1,
                play.source,
                play.hosts.as_deref().unwrap_or("<task 默认>")
            );
            if let Some(g) = &play.hosts {
                let matches = g == "all"
                    || hosts.iter().any(|h| {
                        h.roles.is_empty() || h.name == *g || h.roles.iter().any(|r| r == g)
                    });
                if !matches {
                    info!("   (跳过:hosts='{g}' 无匹配主机)");
                    continue;
                }
            }
            // Absent locally → fetch the registry twin `<registry>/<bare-lock>`.
            let remote = format!("{registry}/{}", play.source);
            ensure_pulled(&store, &play.source, &remote, offline).await?;
            apply_task_artifact(
                &store,
                &play.source,
                hosts.clone(),
                do_apply,
                do_shell,
                teardown,
                source,
                Some(&deployment),
                offline,
                set_overrides.clone(),
                plan,
                play.hosts.clone(),
                play.vars.clone(),
                true,
            )
            .await
            .map_err(|e| anyhow!("project '{}' play '{label}' 失败:{e}", project.name))?;
        }
        info!("{verb} project '{}' 完成", project.name);
        return Ok(());
    }
    if manifest["artifactType"].as_str().is_some() {
        return apply_task_artifact(
            &store,
            reference,
            hosts,
            do_apply,
            do_shell,
            teardown,
            source,
            name,
            offline,
            set_overrides,
            plan,
            None,
            BTreeMap::new(),
            false,
        )
        .await;
    }

    // Plain container image → rootfs overlay (extract all layers to /).
    let layers = store.resolve_layers(reference)?;
    info!(
        "image {reference}: plain image, {} layer(s), {} host(s)",
        layers.len(),
        hosts.len()
    );
    if !do_apply {
        info!("dry-run; omit --dry-run to install (extract layers to / on each host)");
        return Ok(());
    }
    let forks = forks_limit();
    let results: Vec<Result<()>> = futures::stream::iter(
        hosts
            .iter()
            .map(|h| install_image_on_host(h, &layers, reference)),
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
