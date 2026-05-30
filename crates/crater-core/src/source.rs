//! Artifact source: China-friendly mirror rewrite + control-plane HTTP fetch.
//!
//! The mirror rewrite keeps online deploys working from inside China. The
//! fetch is used by `crater build` (M2) to pull artifacts onto the online
//! control machine before packing them into an offline bundle.

#[derive(Debug, Clone)]
pub struct MirrorRule {
    pub from: String,
    pub to: String,
}

/// Online source with China-friendly mirror rewrite rules.
#[derive(Debug, Clone)]
pub struct OnlineSource {
    pub mirrors: Vec<MirrorRule>,
}

impl OnlineSource {
    pub fn with_default_mirrors() -> Self {
        Self {
            mirrors: vec![
                MirrorRule {
                    from: "https://github.com".into(),
                    to: "https://ghproxy.net/https://github.com".into(),
                },
                MirrorRule {
                    from: "registry.k8s.io".into(),
                    to: "registry.aliyuncs.com/google_containers".into(),
                },
            ],
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

/// Fetch a URL into memory on the control machine (used by `crater build`).
/// Follows redirects; uses rustls (no OpenSSL).
pub async fn fetch(url: &str) -> crate::Result<Vec<u8>> {
    let client = reqwest::Client::builder().user_agent("crater/0.1").build()?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("GET {url} -> HTTP {}", resp.status());
    }
    let bytes = resp.bytes().await?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_github() {
        let s = OnlineSource::with_default_mirrors();
        let out = s.rewrite("https://github.com/foo/bar/releases/x.tgz");
        assert!(out.contains("/https://github.com/foo/bar"));
    }

    #[test]
    fn passthrough_unmatched() {
        let s = OnlineSource::with_default_mirrors();
        let url = "https://download.docker.com/linux/static/x.tgz";
        assert_eq!(s.rewrite(url), url);
    }
}
