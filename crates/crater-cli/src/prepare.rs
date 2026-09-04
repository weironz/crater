//! Site Seed preparation: probe the real target profile, then bake the exact
//! offline closure needed there.  The resulting `.lock.yaml` is deliberately
//! small and reviewable: it binds the closure digest to the source, target
//! profile, and apply-time switches required to consume the closure.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use serde::Serialize;

use crate::closure;
use crate::target::{self, TargetOpts};

#[derive(Serialize)]
struct SiteLock {
    format_version: u32,
    source: String,
    closure: ClosureLock,
    profile: ProfileLock,
    apply: ApplyLock,
}

#[derive(Serialize)]
struct ClosureLock {
    file: String,
    sha256: String,
}

#[derive(Serialize)]
struct ProfileLock {
    arch: String,
    distro: String,
    version: String,
    os_image: String,
    hosts: Vec<String>,
}

#[derive(Serialize)]
struct ApplyLock {
    /// These are apply-stage switches: the closure was baked with their
    /// material branches enabled, so consuming it must use the same branches.
    set: Vec<String>,
}

/// Probe every selected target, reject a heterogeneous fleet, and bake a
/// closure for its exact OS image.  A single Site Seed has one apt/dnf solver
/// context by definition; mixed fleets are separate Seeds, not a silent union.
pub(crate) async fn run(
    blueprint: &Path,
    output: &Path,
    target_opts: &TargetOpts,
    sets: &[String],
) -> Result<()> {
    if !crate::blueprint::is_blueprint_file(blueprint) {
        bail!(
            "prepare 目前只接受 blueprint 文件(不接受 stack):{}",
            blueprint.display()
        );
    }
    let hosts = target_opts.exec_hosts()?;
    let mut profiles = Vec::new();
    let mut controlplanes = 0usize;
    for host in &hosts {
        if host.roles.iter().any(|r| r == "controlplane") {
            controlplanes += 1;
        }
        let exec = target::connect_executor(host, true)
            .await
            .with_context(|| format!("连接目标 {}", host.name))?;
        let os = crater_core::os::detect_info_via(exec.as_ref()).await;
        let arch = detect_arch(exec.as_ref()).await?;
        if os.distro.is_empty() || os.version.is_empty() {
            bail!("{}:无法从 /etc/os-release 确认 distro/version", host.name);
        }
        profiles.push((host.name.clone(), arch, os.distro, os.version));
    }
    let unique: BTreeSet<_> = profiles
        .iter()
        .map(|(_, arch, distro, version)| (arch.clone(), distro.clone(), version.clone()))
        .collect();
    if unique.len() != 1 {
        let got = profiles
            .iter()
            .map(|(host, arch, distro, version)| format!("{host}={arch}/{distro}/{version}"))
            .collect::<Vec<_>>()
            .join(", ");
        bail!("一个 Site Seed 只能对应一个 arch+distro+version；检测到:{got}");
    }
    let (arch, distro, version) = unique.into_iter().next().expect("non-empty hosts");
    let os_image = format!("{distro}:{version}");
    let mut bake_sets = sets.to_vec();
    require_set(&mut bake_sets, "preload_images=true")?;
    // Multi-control-plane topology needs the local Nginx material branch.
    if controlplanes > 1 {
        require_set(&mut bake_sets, "ha=true")?;
    }
    let profile = vec![format!("arch={arch}"), format!("os_image={os_image}")];
    println!(
        "Site Seed 画像: {arch}/{distro}/{version}，{} 台目标{}",
        hosts.len(),
        if controlplanes > 1 {
            "，HA 分支已启用"
        } else {
            ""
        }
    );
    closure::build(blueprint, output, &profile, &bake_sets).await?;

    let bytes =
        std::fs::read(output).with_context(|| format!("读取刚生成的闭包 {}", output.display()))?;
    let mut apply_sets = vec!["preload_images=true".to_string()];
    if controlplanes > 1 {
        apply_sets.push("ha=true".to_string());
    }
    let lock = SiteLock {
        format_version: 1,
        source: blueprint.display().to_string(),
        closure: ClosureLock {
            file: output.display().to_string(),
            sha256: crater_core::bundle::sha256_hex(&bytes),
        },
        profile: ProfileLock {
            arch,
            distro,
            version,
            os_image,
            hosts: hosts.into_iter().map(|h| h.name).collect(),
        },
        apply: ApplyLock { set: apply_sets },
    };
    let lock_path = lock_path(output);
    let yaml = serde_yaml::to_string(&lock)?;
    std::fs::write(&lock_path, yaml)
        .with_context(|| format!("写 Site Seed 锁文件 {}", lock_path.display()))?;
    println!("\nSite Seed 锁文件 → {}", lock_path.display());
    println!("断网部署:");
    println!(
        "  crater apply -f {} -i <inventory> --closure {} --set {}",
        blueprint.display(),
        output.display(),
        lock.apply.set.join(" --set ")
    );
    Ok(())
}

fn require_set(sets: &mut Vec<String>, required: &str) -> Result<()> {
    let (key, value) = required
        .split_once('=')
        .expect("required Site Seed set must be k=v");
    if let Some(existing) = sets
        .iter()
        .find(|s| s.split_once('=').is_some_and(|(k, _)| k == key))
    {
        if existing != required {
            bail!("Site Seed 必须使用 `--set {required}`，但收到了冲突的 `--set {existing}`");
        }
        return Ok(());
    }
    debug_assert!(!value.is_empty());
    sets.push(required.to_string());
    Ok(())
}

async fn detect_arch(exec: &dyn crater_core::executor::Executor) -> Result<String> {
    let out = exec.run("uname -m").await?;
    if !out.ok() {
        bail!("读取目标架构失败(exit {}):{}", out.code, out.stderr.trim());
    }
    match out.stdout.trim() {
        "x86_64" | "amd64" => Ok("amd64".into()),
        "aarch64" | "arm64" => Ok("arm64".into()),
        other => bail!("不支持的目标架构 `{other}`(当前支持 amd64、arm64)"),
    }
}

fn lock_path(output: &Path) -> PathBuf {
    let mut p = output.to_path_buf();
    p.set_extension("lock.yaml");
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_set_fills_a_missing_key_and_rejects_conflict() {
        let mut sets = vec!["version=1.36.1".into()];
        require_set(&mut sets, "preload_images=true").unwrap();
        assert!(require_set(&mut sets, "preload_images=true").is_ok());
        assert!(require_set(&mut sets, "preload_images=false").is_err());
        assert_eq!(sets, ["version=1.36.1", "preload_images=true"]);
    }

    #[test]
    fn lock_sits_beside_the_closure() {
        assert_eq!(
            lock_path(Path::new("out/k8s.site.tar")),
            PathBuf::from("out/k8s.site.lock.yaml")
        );
    }
}
