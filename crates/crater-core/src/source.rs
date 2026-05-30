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
            mirrors: vec![MirrorRule {
                from: "registry.k8s.io".into(),
                to: "registry.aliyuncs.com/google_containers".into(),
            }],
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

/// GitHub mirror prefixes (CN-friendly) tried in order *after* a direct attempt.
/// These are public ghproxy-style reverse proxies; availability shifts over
/// time, so we keep several and fall through on failure.
pub const GITHUB_MIRRORS: &[&str] = &["https://ghfast.top/", "https://gh.api.99988866.xyz/"];

/// Build the ordered list of URLs to try for a given source URL: the original
/// first (works when the control machine has direct access — common for the
/// online build host), then CN GitHub mirrors if it's a github.com URL.
pub fn fetch_candidates(url: &str) -> Vec<String> {
    let mut out = vec![url.to_string()];
    if url.contains("https://github.com") {
        for m in GITHUB_MIRRORS {
            out.push(format!("{m}{url}"));
        }
    }
    out
}

fn client() -> crate::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("crater/0.1")
        .redirect(reqwest::redirect::Policy::limited(10))
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
mod tests {
    use super::*;

    #[test]
    fn rewrites_registry_k8s() {
        let s = OnlineSource::with_default_mirrors();
        let out = s.rewrite("registry.k8s.io/pause:3.9");
        assert!(out.starts_with("registry.aliyuncs.com/google_containers"));
    }

    #[test]
    fn passthrough_unmatched() {
        let s = OnlineSource::with_default_mirrors();
        let url = "https://download.docker.com/linux/static/x.tgz";
        assert_eq!(s.rewrite(url), url);
    }

    #[test]
    fn github_fetch_candidates_include_mirrors() {
        let c = fetch_candidates("https://github.com/o/r/releases/x.tgz");
        assert_eq!(c[0], "https://github.com/o/r/releases/x.tgz"); // direct first
        assert!(c.len() > 1); // plus mirrors
        assert!(c.iter().any(|u| u.contains("ghfast.top")));
    }

    #[test]
    fn nongithub_has_single_candidate() {
        let c = fetch_candidates("https://download.docker.com/x.tgz");
        assert_eq!(c.len(), 1);
    }
}
