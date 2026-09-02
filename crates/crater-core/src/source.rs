//! Artifact source: China-friendly mirror rewrite + control-plane HTTP fetch.
//!
//! The mirror rewrite keeps online deploys working from inside China. The
//! fetch is used by `crater build` (M2) to pull artifacts onto the online
//! control machine before packing them into an offline bundle.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MirrorRule {
    pub from: String,
    pub to: String,
}

/// Mirror configuration — pure data, loaded from `mirrors.default.yaml` (baked
/// in) or an external override. The engine names no product here; it only
/// parses what the data says.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct MirrorConfig {
    #[serde(default)]
    pub registry_rewrites: Vec<MirrorRule>,
    #[serde(default)]
    pub github_mirrors: Vec<String>,
}

/// Built-in default mirror data. Knowledge lives in the YAML, not in code.
const DEFAULT_MIRRORS_YAML: &str = include_str!("mirrors.default.yaml");

impl MirrorConfig {
    /// Load mirror config, preferring an external override over the baked-in
    /// default: `$CRATER_MIRRORS`, then `./mirrors.yaml`, then the default.
    pub fn load() -> Self {
        if let Ok(path) = std::env::var("CRATER_MIRRORS") {
            if let Some(cfg) = Self::try_file(std::path::Path::new(&path)) {
                return cfg;
            }
        }
        if let Some(cfg) = Self::try_file(std::path::Path::new("mirrors.yaml")) {
            return cfg;
        }
        serde_yaml::from_str(DEFAULT_MIRRORS_YAML).unwrap_or_default()
    }

    fn try_file(path: &std::path::Path) -> Option<Self> {
        let text = std::fs::read_to_string(path).ok()?;
        serde_yaml::from_str(&text).ok()
    }
}

/// Online source with China-friendly mirror rewrite rules.
#[derive(Debug, Clone)]
pub struct OnlineSource {
    pub mirrors: Vec<MirrorRule>,
}

impl OnlineSource {
    pub fn with_default_mirrors() -> Self {
        Self {
            mirrors: MirrorConfig::load().registry_rewrites,
        }
    }

    pub fn none() -> Self {
        Self { mirrors: vec![] }
    }

    /// Rewrite a URL through the first matching mirror rule.
    pub fn rewrite(&self, url: &str) -> String {
        for m in &self.mirrors {
            if url.contains(&m.from) {
                return url.replacen(&m.from, &m.to, 1);
            }
        }
        url.to_string()
    }
}

/// Build the ordered list of URLs to try for a given source URL: the original
/// first (works when the control machine has direct access — common for the
/// online build host), then CN GitHub mirrors (from data) if it's a github URL.
pub fn fetch_candidates(url: &str) -> Vec<String> {
    fetch_candidates_with(url, &MirrorConfig::load().github_mirrors)
}

/// Same as [`fetch_candidates`] but with an explicit mirror list (testable,
/// no file IO). The mirror prefixes come from data, not code.
pub fn fetch_candidates_with(url: &str, github_mirrors: &[String]) -> Vec<String> {
    let mut out = vec![url.to_string()];
    if url.contains("https://github.com") {
        for m in github_mirrors {
            out.push(format!("{m}{url}"));
        }
    }
    out
}

/// 连不上就别等了。10 秒握不上手的 registry/镜像站,再等也握不上。
pub(crate) const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// **空闲**超时,不是总时长。
///
/// 这个区分是这条改动的全部要点:crater 要拉几百 MB 的离线闭包,给总时长
/// 设上限等于给"多大的包能拉"设上限 —— 一条 1MB/s 的内网线上,300MB 要
/// 五分钟,而那是完全正常的一次拉取。
///
/// `read_timeout` 掐的是**卡住**:60 秒一个字节都没来才算。慢但在动的传输
/// 不受影响。将来有人想"把超时调小一点"时,请先读完这段 —— 换成
/// `.timeout()` 会让大包在慢网上必失败,而症状是"随机断在中途"。
pub(crate) const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

fn client() -> crate::Result<reqwest::Client> {
    client_with(CONNECT_TIMEOUT, READ_TIMEOUT)
}

fn client_with(
    connect: std::time::Duration,
    read: std::time::Duration,
) -> crate::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("crater/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::limited(10))
        .connect_timeout(connect)
        .read_timeout(read)
        .build()?)
}

/// Fetch a single URL into memory. Follows redirects; uses rustls (no OpenSSL).
pub async fn fetch(url: &str) -> crate::Result<Vec<u8>> {
    let c = client()?;
    let resp = c.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("GET {url} -> HTTP {}", resp.status());
    }
    Ok(resp.bytes().await?.to_vec())
}

/// Fetch with fallback across [`fetch_candidates`]. Returns the bytes and the
/// URL that actually worked, or an error aggregating all attempts.
pub async fn fetch_best(url: &str) -> crate::Result<(Vec<u8>, String)> {
    let mut errors = Vec::new();
    for cand in fetch_candidates(url) {
        match fetch(&cand).await {
            Ok(bytes) => return Ok((bytes, cand)),
            Err(e) => errors.push(format!("  {cand}: {e}")),
        }
    }
    anyhow::bail!("all sources failed for {url}:\n{}", errors.join("\n"))
}

#[cfg(test)]
mod timeout_tests {
    use super::*;
    use std::time::Duration;

    /// **接受连接、然后什么都不回**的服务器 —— 黑洞。
    ///
    /// 这正是不可达 registry 的真实形态里最难查的一种:TCP 握上了,所以
    /// "连不上"的报错永远不会出现;而 HTTP 响应一个字节都不来。没有读超时
    /// 的客户端会在这里**永远**等下去。
    fn black_hole() -> (String, std::thread::JoinHandle<()>) {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            // 收下连接,握住不放,什么都不写。
            if let Ok((sock, _)) = l.accept() {
                std::thread::sleep(Duration::from_secs(30));
                drop(sock);
            }
        });
        (format!("http://{addr}/x"), h)
    }

    /// 没有这条,`crater pull` 撞上黑洞就是无声挂死。
    #[tokio::test]
    async fn a_black_hole_server_times_out_instead_of_hanging_forever() {
        let (url, _h) = black_hole();
        let c = client_with(Duration::from_millis(500), Duration::from_millis(500)).unwrap();

        let began = std::time::Instant::now();
        let r = c.get(&url).send().await;
        let took = began.elapsed();

        assert!(r.is_err(), "黑洞应该超时报错,却拿到了响应");
        // 上限放宽到 5 秒:CI 上慢一点没关系,要卡住的是"永远"这种量级。
        assert!(took < Duration::from_secs(5), "等了 {took:?} —— 超时没生效");
    }

    // 连接超时(`CONNECT_TIMEOUT`)**没有**对应的测试,这是有意的。
    //
    // 试过:连 TEST-NET-1(192.0.2.1,RFC 5737 保证不路由到真实主机)本该在
    // 300ms 的 connect_timeout 上失败,实测却是 5.0 秒 —— 因为开发/CI 的
    // 沙箱网络**拦截全部出站 TCP 并一律接受**,裸 socket 连 192.0.2.1:9 都
    // 会"连上"。连接阶段根本不会卡住,于是这条超时永远不触发。
    //
    // 写一条在这种环境下测不到东西的测试,只会得到一个测别人的绿灯。上面
    // 那条黑洞测试才是真实场景(握手成功、响应不来),而它测的正是不可达
    // registry 让 crater 挂死的那条路径。

    /// 默认值必须是**空闲**超时的量级,不是"总共只能跑这么久"。
    ///
    /// 有人把它改成 `.timeout()` 或者调到几秒时,这条会红并指向那段注释:
    /// 拉几百 MB 的闭包是正常操作。
    #[test]
    fn the_default_read_timeout_is_generous_enough_for_a_big_closure() {
        assert!(
            READ_TIMEOUT >= Duration::from_secs(30),
            "读超时是**空闲**超时,压到 {READ_TIMEOUT:?} 会误杀慢网上的大包"
        );
        assert!(CONNECT_TIMEOUT <= Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_registry() {
        let s = OnlineSource::with_default_mirrors();
        let out = s.rewrite("docker.io/library/busybox:latest");
        assert!(out.starts_with("docker.m.daocloud.io"));
    }

    #[test]
    fn passthrough_unmatched() {
        let s = OnlineSource::with_default_mirrors();
        let url = "https://download.docker.com/linux/static/x.tgz";
        assert_eq!(s.rewrite(url), url);
    }

    #[test]
    fn default_mirror_data_parses() {
        // The baked-in default is valid data and carries the CN mirrors.
        let cfg: MirrorConfig = serde_yaml::from_str(DEFAULT_MIRRORS_YAML).unwrap();
        assert!(!cfg.registry_rewrites.is_empty());
        assert!(cfg.github_mirrors.iter().any(|m| m.contains("ghfast.top")));
    }

    #[test]
    fn github_fetch_candidates_include_mirrors() {
        let mirrors = vec!["https://ghfast.top/".to_string()];
        let c = fetch_candidates_with("https://github.com/o/r/releases/x.tgz", &mirrors);
        assert_eq!(c[0], "https://github.com/o/r/releases/x.tgz"); // direct first
        assert_eq!(c.len(), 2); // plus the one mirror
        assert!(c.iter().any(|u| u.contains("ghfast.top")));
    }

    #[test]
    fn nongithub_has_single_candidate() {
        let mirrors = vec!["https://ghfast.top/".to_string()];
        let c = fetch_candidates_with("https://download.docker.com/x.tgz", &mirrors);
        assert_eq!(c.len(), 1);
    }
}
