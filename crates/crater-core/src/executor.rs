//! Command executors. The same binary acts as control plane and, pushed to a
//! node, as the `crater agent`. M1 implements [`LocalExecutor`]; the SSH
//! executor (russh) is scaffolded and lands next.

use async_trait::async_trait;

#[derive(Debug, Clone)]
pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

#[async_trait]
pub trait Executor: Send + Sync {
    async fn run(&self, cmd: &str) -> crate::Result<CmdOutput>;
}

/// Runs on the local machine — useful for dev and for the `crater agent`
/// self-bootstrap mode executing on the target node.
pub struct LocalExecutor;

#[async_trait]
impl Executor for LocalExecutor {
    async fn run(&self, cmd: &str) -> crate::Result<CmdOutput> {
        use tokio::process::Command;
        let out = if cfg!(windows) {
            Command::new("powershell")
                .args(["-NoProfile", "-Command", cmd])
                .output()
                .await?
        } else {
            Command::new("sh").args(["-c", cmd]).output().await?
        };
        Ok(CmdOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Scaffold for the SSH executor. Implemented next (russh + russh-sftp).
pub struct SshExecutor {
    pub host: String,
    pub port: u16,
    pub user: String,
}

#[async_trait]
impl Executor for SshExecutor {
    async fn run(&self, _cmd: &str) -> crate::Result<CmdOutput> {
        anyhow::bail!(
            "SshExecutor for {}@{}:{} not yet implemented (M1 TODO: russh)",
            self.user,
            self.host,
            self.port
        )
    }
}
