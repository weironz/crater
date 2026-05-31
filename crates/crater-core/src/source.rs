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
