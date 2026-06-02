//! Project = an ordered set of plays (D-083) — crater's **playbook** (ansible
//! `site.yml`). Each play runs one task on a host group, with optional `hosts`
//! and `vars` overrides; plays run top-to-bottom (a barrier between), so a large
//! delivery composes from reusable tasks/roles:
//!
//! ```yaml
//! name: platform
//! plays:
//!   - { name: 主机基线, source: host-init, hosts: all }
//!   - { name: K8s,      source: k8s-ha,    hosts: controlplane, vars: { vip: 10.0.0.9 } }
//!   - { name: 数据库,    source: mysql,     hosts: db }
//! ```
//!
//! Hierarchy: project (plays of tasks) → task (actions, incl. `action: role`) →
//! role (materials + actions). The project layer is pure orchestration — it
//! reuses the whole task/role/inventory/params pipeline unchanged.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Project {
    pub name: String,
    /// One-line summary (shown by `crater inspect`).
    #[serde(default)]
    pub description: Option<String>,
    /// Ordered plays. `apply` runs them top-to-bottom; `delete` bottom-to-top.
    #[serde(default)]
    pub plays: Vec<Play>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Play {
    /// Optional label for logs.
    #[serde(default)]
    pub name: Option<String>,
    /// The task to run: a named task (`tasks/<source>.yaml`) or a file path.
    pub source: String,
    /// Override the task's target group for this play (else the task's `hosts:`).
    #[serde(default)]
    pub hosts: Option<String>,
    /// Vars overlaid on the task for this play (override the task's param defaults).
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

impl Project {
    pub fn from_yaml_file(path: &Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }
}

/// Cheap probe: a project file has a top-level `plays:` (vs a task's `actions:`).
pub fn is_project_file(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
        .map(|v| v.get("plays").is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_project_with_plays() {
        let p: Project = serde_yaml::from_str(
            r#"
name: platform
description: demo
plays:
  - { name: base, source: host-init, hosts: all }
  - { source: k8s-ha, hosts: controlplane, vars: { vip: "10.0.0.9" } }
"#,
        )
        .unwrap();
        assert_eq!(p.plays.len(), 2);
        assert_eq!(p.plays[0].source, "host-init");
        assert_eq!(p.plays[1].hosts.as_deref(), Some("controlplane"));
        assert_eq!(p.plays[1].vars.get("vip").unwrap(), "10.0.0.9");
        assert!(p.plays[0].vars.is_empty());
    }
}
