//! 本地 OCI store 的命令面:`crater images / pull / push / rmi / gc`。
//!
//! 这里**只剩仓库操作**。旧 task 管线的 `apply <image-ref>`(制品回放、
//! rootfs 铺开)随管线一起删了(D-151);新蓝图管线走 `crater pkg` 与
//! `crater install`,它们直接用 `crater_core::store`。

use anyhow::Result;
use tracing::info;

use crater_core::store::ImageStore;

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
    // 旧 build 管线的 sidecar 指纹随管线一起删了(D-151)—— 新蓝图管线不产生
    // 它们,扫一个永远为空的目录只是噪音。
    //
    // 但**目标机侧的暂存 blob(D-095)与管线无关**:新管线推物料同样落在
    // `/var/lib/crater/blobs`。差点把它一起删掉 —— 那会让"磁盘被 crater 吃满"
    // 变成一个没有出口的问题。
    let _ = cache;
    // 只在**显式给了目标**时动手 —— 一句光秃秃的 `crater gc` 不该去"清理"本机。
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
            info!("[{}] 暂存 blob: {} {tag}", host.name, human(bytes));
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
    info!("pushing {reference} → registry ...");
    store.push(reference).await?;
    info!("pushed {reference}");
    Ok(())
}
