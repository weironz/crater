//! Artifact source abstraction. Keeps deploy logic identical across
//! online and offline modes — only the source implementation differs.
//!
//! M1 implements [`OnlineSource`] (mirror rewrite is done; HTTP fetch is a
//! TODO once `reqwest` is wired in). Offline source lands in M2.

use async_trait::async_trait;

#[async_trait]
pub trait ArtifactSource: Send + Sync {
    async fn fetch_file(&self, url: &str) -> crate::Result<Vec<u8>>;
}

#[derive(Debug, Clone)]
pub struct MirrorRule {
    pub from: String,
    pub to: String,
}

/// Online source with China-friendly mirror rewrite rules.
pub struct OnlineSource {
    pub mirrors: Vec<MirrorRule>,
}

impl OnlineSource {
    pub fn with_default_mirrors() -> Self {
        Self {
            mirrors: vec![
                MirrorRule {
                    from: "https://github.com".into(),
                    to: "https://ghproxy.com/https://github.com".into(),
                },
                MirrorRule {
                    from: "registry.k8s.io".into(),
                    to: "registry.aliyuncs.com/google_containers".into(),
                },
            ],
        }
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

#[async_trait]
impl ArtifactSource for OnlineSource {
    async fn fetch_file(&self, url: &str) -> crate::Result<Vec<u8>> {
        let _rewritten = self.rewrite(url);
        // TODO(M1): reqwest (rustls) + resume + sha256 verification.
        anyhow::bail!("OnlineSource::fetch_file not yet implemented (M1 TODO)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_github() {
        let s = OnlineSource::with_default_mirrors();
        let out = s.rewrite("https://github.com/foo/bar/releases/x.tgz");
        assert!(out.starts_with("https://ghproxy.com/https://github.com/foo/bar"));
    }
}
