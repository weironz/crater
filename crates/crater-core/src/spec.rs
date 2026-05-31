//! Inventory file (`-i inventory.yaml`): the deploy targets + named groups.
//! (Tasks carry their own recipe; this file is just *where* to run.)

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Reserved address marking a host that runs on the control machine itself
/// (`crater apply <src>` with no `--host`/`-i`). [`Host::is_local`] detects it;
/// the pipeline then uses the local executor instead of SSH.
pub const LOCAL_ADDR: &str = "@local";

/// An inventory file: hosts + named groups. (Historically `crater.yaml`; kept
/// the name `CraterSpec` for call-site stability — it now holds only inventory.)
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CraterSpec {
    #[serde(default)]
    pub inventory: Inventory,
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
    /// Named groups → members (role names or other group names, nestable). Lets
    /// a task's `hosts:` target an aggregate like `cluster` (D-043). Purely
    /// declarative — the engine resolves it to a role set; no logic in YAML.
    #[serde(default)]
    pub groups: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Host {
    pub name: String,
    pub address: String,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// SSH password. Mutually optional with `key` — one of them is needed for
    /// remote hosts (local hosts need neither).
    #[serde(default)]
    pub password: Option<String>,
    /// SSH private-key file path. Takes precedence over `password` when set.
    #[serde(default)]
    pub key: Option<PathBuf>,
    #[serde(default)]
    pub roles: Vec<String>,
}

impl Host {
    /// A host that runs on the control machine itself (local execution).
    pub fn local() -> Self {
        Host {
            name: "localhost".into(),
            address: LOCAL_ADDR.into(),
            user: default_user(),
            port: 22,
            password: None,
            key: None,
            roles: Vec::new(),
        }
    }

    /// True if this host runs locally (no SSH) — see [`LOCAL_ADDR`].
    pub fn is_local(&self) -> bool {
        self.address == LOCAL_ADDR
    }
}

fn default_user() -> String {
    "root".into()
}
fn default_ssh_port() -> u16 {
    22
}
