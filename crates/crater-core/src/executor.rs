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

struct ClientHandler {
    host: String,
    port: u16,
}

#[async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    // russh 0.45: the key type lives at `russh::keys::key::PublicKey`.
    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // Pin host keys in ~/.crater/known_hosts, trust-on-first-use (D-094).
        Ok(verify_host_key(&self.host, self.port, server_public_key))
    }
}

/// Host-key policy (D-094), ansible-style `accept-new` by default:
///   - first connection → record the key in `~/.crater/known_hosts`, proceed;
///   - recorded & matching → proceed;
///   - recorded & DIFFERENT → refuse (possible MITM / reinstalled host);
///   - `CRATER_HOST_KEY_CHECKING=0|false|no|off` → skip entirely (ephemeral VMs).
/// crater keeps its own file (not ~/.ssh/known_hosts): it must never corrupt the
/// operator's OpenSSH state, and `$CRATER_HOME` keeps tests/CI hermetic.
fn verify_host_key(host: &str, port: u16, key: &russh::keys::key::PublicKey) -> bool {
    if let Ok(v) = std::env::var("CRATER_HOST_KEY_CHECKING") {
        if matches!(v.as_str(), "0" | "false" | "no" | "off") {
            return true;
        }
    }
    let path = crate::store::ImageStore::home().join("known_hosts");
    verify_host_key_at(host, port, key, &path)
}

/// Testable core of `verify_host_key` — same policy against an explicit file.
fn verify_host_key_at(
    host: &str,
    port: u16,
    key: &russh::keys::key::PublicKey,
    path: &std::path::Path,
) -> bool {
    use russh::keys::Error as KeyError;
    match russh::keys::check_known_hosts_path(host, port, key, path) {
        Ok(true) => true,
        // Unknown host (or no file yet) → TOFU: pin it and proceed.
        Ok(false) => match russh::keys::learn_known_hosts_path(host, port, key, path) {
            Ok(()) => {
                tracing::info!(
                    "ssh: 首次连接 {host}:{port},已钉其 host key(SHA256:{},记录于 {})",
                    key.fingerprint(),
                    path.display()
                );
                true
            }
            Err(e) => {
                // Couldn't persist the pin — still first-use trust, just warn.
                tracing::warn!(
                    "ssh: 无法记录 {host}:{port} 的 host key 到 {}:{e}(本次仍继续)",
                    path.display()
                );
                true
            }
        },
        Err(KeyError::KeyChanged { line }) => {
            tracing::error!(
                "ssh: {host}:{port} 的 HOST KEY 已变化!现指纹 SHA256:{},与 {} 第 {line} 行不符 \
                 —— 可能是中间人攻击,也可能主机重装过。确认无误后删除该行重连;\
                 临时跳过校验:CRATER_HOST_KEY_CHECKING=0",
                key.fingerprint(),
                path.display()
            );
            false
        }
        Err(e) => {
            tracing::error!("ssh: 读 known_hosts {} 失败:{e}(拒绝连接)", path.display());
            false
        }
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
        let handler = ClientHandler { host: host.to_string(), port };
        let mut handle = client::connect(config, (host, port), handler)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn gen_key() -> russh::keys::key::PublicKey {
        russh::keys::key::KeyPair::generate_ed25519()
            .expect("ed25519 keygen")
            .clone_public_key()
            .expect("public half")
    }

    /// D-094 TOFU round-trip: unknown host → pinned (file written) → match;
    /// a DIFFERENT key for the same host:port → refused (KeyChanged).
    #[test]
    fn host_key_tofu_pin_then_refuse_changed() {
        let dir = std::env::temp_dir().join(format!("crater-kh-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("known_hosts");

        let key = gen_key();
        // First use: unknown → learn + accept.
        assert!(verify_host_key_at("192.0.2.7", 22, &key, &path));
        assert!(path.is_file(), "first use must pin the key");
        // Second use, same key: recorded match → accept.
        assert!(verify_host_key_at("192.0.2.7", 22, &key, &path));
        // Same host:port presents a DIFFERENT key → refuse.
        let other = gen_key();
        assert!(!verify_host_key_at("192.0.2.7", 22, &other, &path));
        // A different port is a different endpoint → its own TOFU, accepted.
        assert!(verify_host_key_at("192.0.2.7", 2222, &other, &path));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
