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

    /// Write directly to the local filesystem — bypassing the trait's
    /// shell-base64 default, which would blow `MAX_ARG_STRLEN` on large blobs
    /// (e.g. a 13 MB binary placed during a local `apply`).
    async fn write_file(&self, path: &str, content: &[u8]) -> crate::Result<()> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)
            .map_err(|e| anyhow::anyhow!("write_file {path} failed: {e}"))?;
        Ok(())
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

/// SSH authentication method. Password or a private-key file (with optional
/// passphrase) — the latter is the norm for fleets that disable password auth.
pub enum SshAuth {
    Password(String),
    Key {
        path: std::path::PathBuf,
        passphrase: Option<String>,
    },
}

impl SshExecutor {
    /// Connect with a password (kept for call-site stability).
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
    ) -> crate::Result<Self> {
        Self::connect_auth(host, port, user, &SshAuth::Password(password.to_string())).await
    }

    /// Connect with an explicit auth method (password or key).
    pub async fn connect_auth(
        host: &str,
        port: u16,
        user: &str,
        auth: &SshAuth,
    ) -> crate::Result<Self> {
        let config = Arc::new(client::Config::default());
        let mut handle = client::connect(config, (host, port), ClientHandler)
            .await
            .map_err(|e| anyhow::anyhow!("ssh connect {host}:{port} failed: {e}"))?;
        // russh 0.45: authenticate_* return Result<bool>.
        let authed = match auth {
            SshAuth::Password(pw) => handle
                .authenticate_password(user, pw)
                .await
                .map_err(|e| anyhow::anyhow!("ssh auth error for {user}@{host}: {e}"))?,
            SshAuth::Key { path, passphrase } => {
                let key = russh::keys::load_secret_key(path, passphrase.as_deref())
                    .map_err(|e| anyhow::anyhow!("load ssh key {}: {e}", path.display()))?;
                handle
                    .authenticate_publickey(user, Arc::new(key))
                    .await
                    .map_err(|e| anyhow::anyhow!("ssh key auth error for {user}@{host}: {e}"))?
            }
        };
        if !authed {
            anyhow::bail!("ssh authentication failed for {user}@{host} (check password/key)");
        }
        Ok(Self {
            handle,
            label: format!("{user}@{host}:{port}"),
        })
    }
}

#[async_trait]
impl Executor for SshExecutor {
    /// Stream the blob to the target over a SINGLE channel: `cat > path` with the
    /// raw bytes on stdin. No base64 (stdin is binary-safe — it's not a shell
    /// argument, so MAX_ARG_STRLEN doesn't apply) and no per-chunk round trips.
    ///
    /// The old writer appended base64 in 60 KB chunks, one SSH `exec` per chunk —
    /// ~12k sequential round trips for a 500 MB blob, latency-bound at ~1 MB/s
    /// (≈100× slower than the link, network idle). russh streams a single channel
    /// with SSH-level windowing, so this runs at line rate.
    async fn write_file(&self, path: &str, content: &[u8]) -> crate::Result<()> {
        let mut channel = self.handle.channel_open_session().await?;
        let cmd = format!("mkdir -p \"$(dirname '{path}')\" && cat > '{path}'");
        channel.exec(true, cmd.as_str()).await?;
        channel
            .data(content)
            .await
            .map_err(|e| anyhow::anyhow!("stream to {path} failed: {e}"))?;
        channel.eof().await?;

        let mut code: Option<i32> = None;
        let mut stderr: Vec<u8> = Vec::new();
        while let Some(msg) = channel.wait().await {
            match msg {
                ChannelMsg::ExtendedData { ref data, ext } if ext == 1 => {
                    stderr.extend_from_slice(data)
                }
                ChannelMsg::ExitStatus { exit_status } => code = Some(exit_status as i32),
                _ => {}
            }
        }
        let code = code.unwrap_or(-1);
        if code != 0 {
            anyhow::bail!(
                "write_file {path} failed (code {code}): {}",
                String::from_utf8_lossy(&stderr).trim()
            );
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
