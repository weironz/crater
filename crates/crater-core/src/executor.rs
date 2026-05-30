//! Command executors. The same binary acts as control plane and, pushed to a
//! node, as the `crater agent`. Two implementations:
//! - [`LocalExecutor`]: runs on the local machine (dev / agent mode on a node).
//! - [`SshExecutor`]: agentless control-plane → target over SSH (russh 0.45).

use async_trait::async_trait;
use base64::Engine as _;

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
    /// Run a shell command on the target, capturing output and exit code.
    async fn run(&self, cmd: &str) -> crate::Result<CmdOutput>;

    /// Write bytes to a file on the target. Default impl streams the content
    /// base64-encoded through `run`, so it works for both local and SSH
    /// targets without an extra file-transfer channel.
    async fn write_file(&self, path: &str, content: &[u8]) -> crate::Result<()> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(content);
        let cmd = format!(
            "mkdir -p \"$(dirname '{path}')\" && printf %s '{b64}' | base64 -d > '{path}'"
        );
        let out = self.run(&cmd).await?;
        if out.ok() {
            Ok(())
        } else {
            anyhow::bail!(
                "write_file {path} failed (code {}): {}",
                out.code,
                out.stderr.trim()
            )
        }
    }

    /// Human-readable label for logging (e.g. `root@host:22` or `local`).
    fn label(&self) -> &str {
        "local"
    }
}

/// Executes on the local machine.
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

// ---------------------------------------------------------------------------
// SSH executor (russh 0.45)
// ---------------------------------------------------------------------------

use std::sync::Arc;

use russh::client;
use russh::ChannelMsg;

struct ClientHandler;

#[async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    // russh 0.45: the key type lives at `russh::keys::key::PublicKey`.
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // TODO(security): pin/verify host keys (known_hosts). Accept-all for now.
        Ok(true)
    }
}

/// Agentless executor: drives a remote target over SSH.
pub struct SshExecutor {
    handle: client::Handle<ClientHandler>,
    label: String,
}

impl SshExecutor {
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> crate::Result<Self> {
        let config = Arc::new(client::Config::default());
        let mut handle = client::connect(config, (host, port), ClientHandler)
            .await
            .map_err(|e| anyhow::anyhow!("ssh connect {host}:{port} failed: {e}"))?;
        // russh 0.45: authenticate_password returns Result<bool>.
        let authed = handle
            .authenticate_password(user, password)
            .await
            .map_err(|e| anyhow::anyhow!("ssh auth error for {user}@{host}: {e}"))?;
        if !authed {
            anyhow::bail!("ssh password authentication failed for {user}@{host}");
        }
        Ok(Self {
            handle,
            label: format!("{user}@{host}:{port}"),
        })
    }
}

#[async_trait]
impl Executor for SshExecutor {
    /// Override the default base64-in-one-shot writer: large blobs (e.g. a
    /// 10 MB offline artifact) produce a command too big for a single SSH exec
    /// (the channel returns no exit status -> code -1). Stream the base64 text
    /// in chunks appended to a temp file, then decode once on the target.
    async fn write_file(&self, path: &str, content: &[u8]) -> crate::Result<()> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(content);
        let tmp = format!("{path}.crater-b64");

        // Ensure parent dir exists and start a fresh temp file.
        let prep = format!("mkdir -p \"$(dirname '{path}')\" && : > '{tmp}'");
        let out = self.run(&prep).await?;
        if !out.ok() {
            anyhow::bail!("prepare {tmp} failed (code {}): {}", out.code, out.stderr.trim());
        }

        // Append base64 in chunks. The chunk travels as a single shell
        // argument, so it must stay under Linux MAX_ARG_STRLEN (~128 KB).
        // 60 KB is safely below that.
        const CHUNK: usize = 60_000; // base64 chars per round trip
        let bytes = b64.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let end = (i + CHUNK).min(bytes.len());
            let chunk = &b64[i..end];
            let cmd = format!("printf %s '{chunk}' >> '{tmp}'");
            let out = self.run(&cmd).await?;
            if !out.ok() {
                anyhow::bail!(
                    "append chunk to {tmp} failed (code {}): {}",
                    out.code,
                    out.stderr.trim()
                );
            }
            i = end;
        }

        // Decode once, then clean up the temp file.
        let fin = format!("base64 -d '{tmp}' > '{path}' && rm -f '{tmp}'");
        let out = self.run(&fin).await?;
        if !out.ok() {
            anyhow::bail!("decode {path} failed (code {}): {}", out.code, out.stderr.trim());
        }
        Ok(())
    }

    async fn run(&self, cmd: &str) -> crate::Result<CmdOutput> {
        let mut channel = self.handle.channel_open_session().await?;
        channel.exec(true, cmd).await?;

        let mut code: Option<i32> = None;
        let mut stdout: Vec<u8> = Vec::new();
        let mut stderr: Vec<u8> = Vec::new();

        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::Data { ref data } => stdout.extend_from_slice(data),
                ChannelMsg::ExtendedData { ref data, ext } => {
                    if ext == 1 {
                        stderr.extend_from_slice(data);
                    } else {
                        stdout.extend_from_slice(data);
                    }
                }
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
                _ => {}
            }
        }

        Ok(CmdOutput {
            code: code.unwrap_or(-1),
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }

    fn label(&self) -> &str {
        &self.label
    }
}
