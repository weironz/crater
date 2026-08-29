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
        // **分块**,而不是把整份 base64 拼成一条命令。
        //
        // 早先的实现把 58MB 二进制编码成 78MB 字符串再 format! 进单条 shell 命令。
        // 真机上实测的后果:进程 100% 占满一个核、网络零流量、目标机上没有任何
        // 会话 —— SSH 传输层要把这个超大请求塞进它的缓冲区,缓冲区按小增量增长
        // 就是 O(n²) 的 memcpy(78MB / 32KB ≈ 2400 次重分配,约 94GB 拷贝)。
        // 远端 `sh -c` 还得再解析一遍这个 78MB 的单引号字符串。
        //
        // 块大小是往返次数与单请求大小之间的取舍:2 MiB 原始数据 ≈ 2.7 MiB 命令,
        // 一个 58MB 的二进制 29 次往返 —— 在跳板/隧道那种高 RTT 链路上仍然可接受,
        // 而任何一次请求都不会大到让传输层退化。
        const CHUNK: usize = 2 * 1024 * 1024;

        // 先建目录并清空目标 —— 后续块一律追加,所以第一步必须是截断。
        let head = format!("mkdir -p \"$(dirname '{path}')\" && : > '{path}'");
        let out = self.run(&head).await?;
        if !out.ok() {
            anyhow::bail!("write_file {path}:建目录/清空失败(code {}):{}", out.code, out.stderr.trim());
        }

        // base64 的字母表是 A-Za-z0-9+/= ,不含单引号,所以块可以直接放进
        // 单引号里,无需转义。
        for (i, part) in content.chunks(CHUNK).enumerate() {
            let b64 = base64::engine::general_purpose::STANDARD.encode(part);
            let cmd = format!("printf %s '{b64}' | base64 -d >> '{path}'");
            let out = self.run(&cmd).await?;
            if !out.ok() {
                anyhow::bail!(
                    "write_file {path}:第 {} 块(共 {} 块)写入失败(code {}):{}",
                    i + 1,
                    content.len().div_ceil(CHUNK),
                    out.code,
                    out.stderr.trim()
                );
            }
        }
        Ok(())
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

#[cfg(test)]
mod write_file_tests {
    use super::*;
    use std::sync::Mutex;

    /// 记录每条命令的假 Executor —— 用来断言"分块"这件事真的发生了。
    struct Recorder {
        cmds: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Executor for Recorder {
        async fn run(&self, cmd: &str) -> crate::Result<CmdOutput> {
            self.cmds.lock().unwrap().push(cmd.to_string());
            Ok(CmdOutput { code: 0, stdout: String::new(), stderr: String::new() })
        }
    }

    #[tokio::test]
    async fn a_large_file_is_written_in_bounded_chunks() {
        // 这条测试守的是一次真机事故:把 58MB 编码成 78MB 拼进单条命令,
        // 进程 100% 占满一个核、网络零流量。任何一条命令都不该大到那个地步。
        let r = Recorder { cmds: Mutex::new(Vec::new()) };
        let content = vec![0xABu8; 5 * 1024 * 1024]; // 5 MiB
        r.write_file("/opt/big.bin", &content).await.unwrap();

        let cmds = r.cmds.lock().unwrap().clone();
        assert!(cmds[0].contains(": > '/opt/big.bin'"), "第一条应清空目标:{}", &cmds[0][..60.min(cmds[0].len())]);
        assert_eq!(cmds.len(), 1 + 3, "5MiB / 2MiB → 3 块:{}", cmds.len());
        // 单条命令的上限:2MiB 原始 → 约 2.7MiB base64,留一倍余量。
        for c in &cmds {
            assert!(c.len() < 6 * 1024 * 1024, "有命令过大:{} 字节", c.len());
        }
    }

    #[tokio::test]
    async fn the_first_chunk_truncates_and_the_rest_append() {
        // 顺序反了会得到一个"内容是最后一块"的文件,而且大小看着还挺像。
        let r = Recorder { cmds: Mutex::new(Vec::new()) };
        r.write_file("/opt/x", &vec![1u8; 3 * 1024 * 1024]).await.unwrap();
        let cmds = r.cmds.lock().unwrap().clone();
        assert!(cmds[0].contains(": >"), "首条截断");
        for c in &cmds[1..] {
            assert!(c.contains(">> '/opt/x'"), "其余追加:{c}");
            assert!(!c.contains("> '/opt/x'") || c.contains(">> '/opt/x'"));
        }
    }

    #[tokio::test]
    async fn a_small_file_still_takes_one_chunk() {
        let r = Recorder { cmds: Mutex::new(Vec::new()) };
        r.write_file("/etc/small.conf", b"hello").await.unwrap();
        assert_eq!(r.cmds.lock().unwrap().len(), 2, "清空 + 一块");
    }

    #[tokio::test]
    async fn an_empty_file_is_created_and_left_empty() {
        // `chunks()` 对空切片不产出任何块 —— 清空那一步必须已经把文件建出来。
        let r = Recorder { cmds: Mutex::new(Vec::new()) };
        r.write_file("/etc/empty", b"").await.unwrap();
        let cmds = r.cmds.lock().unwrap().clone();
        assert_eq!(cmds.len(), 1);
        assert!(cmds[0].contains(": > '/etc/empty'"));
    }
}
