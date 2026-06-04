//! Self-bootstrap agent (D-044): select/push the static agent binary to the
//! target, ship the lowered task plan, and `crater agent` itself (target side).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use tracing::info;

use crater_core::engine::{self, Op};
use crater_core::executor::{Executor, LocalExecutor};

/// Normalize `uname -m` output to crater's arch naming (matches dist/ names).
pub(crate) fn norm_arch(uname_m: &str) -> String {
    match uname_m.trim() {
        "x86_64" | "amd64" => "x86_64",
        "aarch64" | "arm64" => "aarch64",
        other => other,
    }
    .to_string()
}

/// Where to look for a bundled musl static binary `crater-linux-<arch>`:
/// `$CRATER_AGENT_DIR`, beside the control binary (+ its `dist/`), and `./dist/`.
pub(crate) fn musl_candidates(arch: &str) -> Vec<PathBuf> {
    let name = format!("crater-linux-{arch}");
    let mut v = Vec::new();
    if let Ok(d) = std::env::var("CRATER_AGENT_DIR") {
        v.push(PathBuf::from(d).join(&name));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(dir.join(&name));
            v.push(dir.join("dist").join(&name));
            if let Some(parent) = dir.parent() {
                v.push(parent.join("dist").join(&name));
            }
        }
    }
    v.push(PathBuf::from("dist").join(&name));
    v
}

/// Choose the agent binary to ship to `exec`'s target. Order:
/// 1. explicit `--agent-bin`; 2. a bundled musl static for the target's arch
/// (portable, also dodges glibc skew on the same arch); 3. the control binary
/// iff the target arch matches; else error with guidance.
pub(crate) async fn select_agent_binary(
    exec: &dyn Executor,
    agent_bin: Option<&Path>,
) -> Result<(PathBuf, String)> {
    if let Some(p) = agent_bin {
        return Ok((p.to_path_buf(), "explicit --agent-bin".into()));
    }
    let target_arch = norm_arch(&exec.run("uname -m").await?.stdout);
    for cand in musl_candidates(&target_arch) {
        if cand.is_file() {
            return Ok((cand, format!("bundled musl static for {target_arch}")));
        }
    }
    if target_arch == std::env::consts::ARCH {
        let exe = std::env::current_exe()
            .map_err(|e| anyhow!("cannot locate current crater binary: {e}"))?;
        return Ok((exe, format!("control binary (same arch {target_arch})")));
    }
    anyhow::bail!(
        "no agent binary for target arch '{target_arch}' (control is '{}'). Build one with \
         `scripts/build-musl.sh {target_arch}` and pass --agent-bin, or run with --shell.",
        std::env::consts::ARCH
    )
}

/// Self-bootstrap agent mode (D-019/D-027, the default): push the crater binary
/// + the lowered plan to the target, then run `crater agent --plan` THERE so the
/// plan executes locally in one shot — fewer SSH round-trips, and the foundation
/// for OCI unpack / richer local logic. The binary is cached on the target (by
/// sha256), so it's pushed once per version; only the plan file is transient.
/// Push the crater binary to the target (cached by sha256 at
/// `/var/lib/crater/crater`) and return that path. Shared by component
/// (`--plan`) and task (`--task-plan`) agent runs.
pub(crate) async fn push_agent_binary(exec: &dyn Executor, agent_bin: Option<&Path>) -> Result<&'static str> {
    // Pick the binary to ship: explicit override > a bundled musl static
    // matching the target's arch > the control binary (only if same arch).
    let (bin_path, how) = select_agent_binary(exec, agent_bin).await?;
    let bytes = std::fs::read(&bin_path)
        .map_err(|e| anyhow!("read agent binary {}: {e}", bin_path.display()))?;
    let want = crater_core::bundle::sha256_hex(&bytes);
    let remote_bin = "/var/lib/crater/crater";
    let cached = exec
        .run(&format!("sha256sum {remote_bin} 2>/dev/null | cut -d' ' -f1"))
        .await?;
    if cached.ok() && cached.stdout.trim() == want {
        info!("[{}] agent: binary cached (sha256 match), reusing [{how}]", exec.label());
    } else {
        info!(
            "[{}] agent: pushing {} ({} bytes) [{how}]",
            exec.label(),
            bin_path.display(),
            bytes.len()
        );
        exec.run("mkdir -p /var/lib/crater").await?;
        exec.write_file(remote_bin, &bytes).await?;
        exec.run(&format!("chmod +x {remote_bin}")).await?;
    }
    Ok(remote_bin)
}

/// Forward an agent run's output verbatim and map a failed exit to an error.
pub(crate) fn forward_agent_output(out: &crater_core::executor::CmdOutput) -> Result<()> {
    print!("{}", out.stdout);
    if !out.stderr.trim().is_empty() {
        eprint!("{}", out.stderr);
    }
    if !out.ok() {
        if out.code == 126 || out.code == 127 {
            anyhow::bail!(
                "agent binary failed to execute on target (exit {}; likely arch/libc \
                 mismatch). Re-run with --shell for the agentless shell executor, or pass \
                 --agent-bin <musl-static-build>.",
                out.code
            );
        }
        anyhow::bail!("agent exited with code {}", out.code);
    }
    Ok(())
}


/// Run a task plan on the target via the self-bootstrap agent (D-044): push the
/// binary + the rendered task plan (steps + policy + handlers), then the target
/// runs `execute_task` locally. `--shell`/local callers use the control-plane
/// `execute_task` instead.
pub(crate) async fn run_task_via_agent(
    exec: &dyn Executor,
    steps: &[engine::TaskStep],
    handlers: &BTreeMap<String, Op>,
    agent_bin: Option<&Path>,
) -> Result<()> {
    let remote_bin = push_agent_binary(exec, agent_bin).await?;
    let remote_plan = "/tmp/crater-task-plan.yaml";
    exec.write_file(remote_plan, engine::task_plan_to_yaml(steps, handlers)?.as_bytes())
        .await?;
    info!("[{}] agent: executing task on target ↓", exec.label());
    let out = exec
        .run(&format!("{remote_bin} agent --task-plan {remote_plan}"))
        .await?;
    let _ = exec.run(&format!("rm -f {remote_plan}")).await;
    forward_agent_output(&out)
}

/// `crater agent --task-plan <file>`: run ON the target. Reads a lowered task
/// plan (steps + handlers, D-044) and executes it locally via `execute_task`
/// (the control machine pushed the plan + this binary).
pub(crate) async fn run_agent(task_plan: &Path) -> Result<()> {
    let text = std::fs::read_to_string(task_plan)
        .map_err(|e| anyhow!("read task plan {}: {e}", task_plan.display()))?;
    let plan = engine::task_plan_from_yaml(&text)?;
    info!("agent: executing task ({} step(s)) locally", plan.steps.len());
    // Agent runs one host locally — no cross-host coordination (D-077).
    engine::execute_task(&plan.steps, &plan.handlers, &LocalExecutor, None).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_arch_maps_common_aliases() {
        assert_eq!(norm_arch("x86_64\n"), "x86_64");
        assert_eq!(norm_arch("amd64"), "x86_64");
        assert_eq!(norm_arch("aarch64"), "aarch64");
        assert_eq!(norm_arch("arm64"), "aarch64");
        assert_eq!(norm_arch("riscv64"), "riscv64"); // passthrough
    }

    #[test]
    fn musl_candidates_use_arch_specific_name() {
        let c = musl_candidates("aarch64");
        assert!(c.iter().all(|p| p.ends_with("crater-linux-aarch64")));
        assert!(c.iter().any(|p| p == &PathBuf::from("dist/crater-linux-aarch64")));
    }
}
