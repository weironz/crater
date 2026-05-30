//! `crater.yaml` — the top-level declarative spec.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CraterSpec {
    #[serde(default)]
    pub inventory: Inventory,
    #[serde(default)]
    pub components: Vec<ComponentRef>,
    /// Reserved for offline mode (M2). Unused in M1.
    #[serde(default)]
    pub offline: bool,
    /// Reserved for AI config (M4/M5). Unused in M1.
    #[serde(default)]
    pub ai: Option<AiConfig>,
}

impl CraterSpec {
    pub fn from_yaml_file(path: &Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Inventory {
    #[serde(default)]
    pub hosts: Vec<Host>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Host {
    pub name: String,
    pub address: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// SSH password (M1). Key-based auth + secret store come later.
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}

fn default_user() -> String {
    "root".into()
}
fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentRef {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub params: BTreeMap<String, serde_yaml::Value>,
}

/// Reserved for M4/M5. Present so specs don't need rewriting later.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AiConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub endpoint: Option<String>,
}
