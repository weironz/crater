//! `crater.yaml` — the top-level declarative spec.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::component::{Action, Check, ComponentDescriptor, RegisterSpec};

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

    // ---- Path B: inline recipe -------------------------------------------
    // When any of these phases is non-empty, the recipe lives right here in the
    // spec instead of in `components/<name>/component.yaml`. One spec file then
    // fully describes a deployment, and `components/` becomes an *optional*
    // reuse library rather than a requirement.
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub supported_os: Vec<String>,
    #[serde(default)]
    pub preflight: Vec<Check>,
    #[serde(default)]
    pub install: Vec<Action>,
    #[serde(default)]
    pub verify: Vec<Action>,
    #[serde(default)]
    pub register: Vec<RegisterSpec>,
}

impl ComponentRef {
    /// True if this ref carries its recipe inline (Path B) rather than
    /// referencing a `components/<name>/` descriptor.
    pub fn is_inline(&self) -> bool {
        !self.preflight.is_empty() || !self.install.is_empty() || !self.verify.is_empty()
    }

    /// Build a [`ComponentDescriptor`] from the inline recipe fields.
    pub fn to_inline_descriptor(&self) -> ComponentDescriptor {
        ComponentDescriptor {
            name: self.name.clone(),
            aliases: Vec::new(),
            version_default: self.version.clone(),
            supported_os: self.supported_os.clone(),
            requires: self.requires.clone(),
            preflight: self.preflight.clone(),
            install: self.install.clone(),
            verify: self.verify.clone(),
            register: self.register.clone(),
            offline: None,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ref_without_recipe_is_not_inline() {
        let yaml = "name: yq\nversion: \"4.53.2\"\n";
        let cref: ComponentRef = serde_yaml::from_str(yaml).unwrap();
        assert!(!cref.is_inline());
    }

    #[test]
    fn inline_recipe_is_detected_and_converts() {
        let yaml = r#"
name: yq
version: "4.53.2"
install:
  - action: download
    url_tmpl: "https://example.com/yq"
    dest: /usr/local/bin/yq
verify:
  - action: run_cmd
    cmd: "yq --version"
"#;
        let cref: ComponentRef = serde_yaml::from_str(yaml).unwrap();
        assert!(cref.is_inline());
        let desc = cref.to_inline_descriptor();
        assert_eq!(desc.name, "yq");
        assert_eq!(desc.version_default.as_deref(), Some("4.53.2"));
        assert_eq!(desc.install.len(), 1);
        assert_eq!(desc.verify.len(), 1);
    }
}
