//! Site Seed preparation: use the inventory's declared target profile to bake
//! the exact offline closure needed there. The resulting `.lock.yaml` is
//! deliberately small and reviewable: it binds the closure digest to the
//! source, target profile, and apply-time switches required to consume it.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use serde::Serialize;

use crate::closure;
use crate::target::TargetOpts;

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

/// A fully resolved static profile plus the topology switches that affect its
/// closure. It intentionally contains no connection data or target facts.
#[derive(Clone)]
pub(crate) struct SeedPlan {
    pub platform: crater_core::spec::Platform,
    pub hosts: Vec<String>,
    pub ha: bool,
    pub apply_sets: Vec<String>,
}

impl SeedPlan {
    fn profile(&self) -> Vec<String> {
        vec![
            format!("arch={}", self.platform.arch),
            format!("os_image={}", self.platform.os_image()),
        ]
    }

    fn bake_sets(&self, sets: &[String]) -> Result<Vec<String>> {
        let mut out = sets.to_vec();
        require_set(&mut out, "preload_images=true")?;
        if self.ha {
            require_set(&mut out, "ha=true")?;
        }
        Ok(out)
    }
}

fn seed_plan(platform: crater_core::spec::Platform, hosts: &[crater_core::spec::Host]) -> SeedPlan {
    let ha = hosts
        .iter()
        .filter(|host| host.roles.iter().any(|r| r == "controlplane"))
        .count()
        > 1;
    SeedPlan {
        platform,
        hosts: hosts.iter().map(|h| h.name.clone()).collect(),
        ha,
        apply_sets: if ha {
            vec!["preload_images=true".into(), "ha=true".into()]
        } else {
            vec!["preload_images=true".into()]
        },
    }
}

/// Bake a Seed for a publisher-supplied inventory. This parses only static
/// inventory data and never resolves credentials or connects to a host.
pub(crate) async fn bake_for_inventory(
    blueprint: &Path,
    output: &Path,
    inventory: &Path,
    sets: &[String],
) -> Result<SeedPlan> {
    let plan = plan_for_inventory(inventory)?;
    bake(blueprint, output, &plan, sets).await?;
    Ok(plan)
}

/// Resolve an inventory into a Seed plan without baking bytes. Publishers use
/// this to attach a previously verified closure to the matching profile.
pub(crate) fn plan_for_inventory(inventory: &Path) -> Result<SeedPlan> {
    let spec = crater_core::spec::CraterSpec::from_yaml_file(inventory)?;
    let mut inv = spec.inventory;
    let platform = inv.platform.take().ok_or_else(|| {
        anyhow::anyhow!(
            "{} 缺少 inventory.platform；不能为未声明画像的机群烤 Site Seed",
            inventory.display()
        )
    })?;
    platform.validate()?;
    if inv.hosts.is_empty() {
        bail!("inventory {} 没有 hosts", inventory.display());
    }
    inv.resolve();
    Ok(seed_plan(platform, &inv.hosts))
}

async fn bake(blueprint: &Path, output: &Path, plan: &SeedPlan, sets: &[String]) -> Result<()> {
    let platform = &plan.platform;
    println!(
        "Site Seed 画像: {}/{}/{}，{} 台目标{}",
        platform.arch,
        platform.os,
        platform.version,
        plan.hosts.len(),
        if plan.ha { "，HA 分支已启用" } else { "" }
    );
    closure::build(blueprint, output, &plan.profile(), &plan.bake_sets(sets)?).await
}

/// Bake a closure for the statically declared inventory platform. A single
/// Site Seed has one apt/dnf solver context by definition; mixed fleets are
/// separate Seeds, not a silent union. No target connection is made here:
/// preparing/downloading an offline Seed must work before the target network
/// is reachable.
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
    let plan = seed_plan(target_opts.offline_platform()?, &hosts);
    bake(blueprint, output, &plan, sets).await?;

    let bytes =
        std::fs::read(output).with_context(|| format!("读取刚生成的闭包 {}", output.display()))?;
    let lock = SiteLock {
        format_version: 1,
        source: blueprint.display().to_string(),
        closure: ClosureLock {
            file: output.display().to_string(),
            sha256: crater_core::bundle::sha256_hex(&bytes),
        },
        profile: ProfileLock {
            arch: plan.platform.arch.clone(),
            distro: plan.platform.os.clone(),
            version: plan.platform.version.clone(),
            os_image: plan.platform.os_image(),
            hosts: plan.hosts.clone(),
        },
        apply: ApplyLock {
            set: plan.apply_sets,
        },
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
