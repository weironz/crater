//! Inventory file (`-i inventory.yaml`): the deploy targets + named groups.
//! (Tasks carry their own recipe; this file is just *where* to run.)

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
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
    /// Global vars (D-082): lowest-precedence environment values, applied to every
    /// host. Override task `params` defaults at apply time (vip/subnet/…).
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub hosts: Vec<Host>,
    /// Named groups → members (kubekey/Ansible style, D-077). Each group lists
    /// member host names (`hosts:`) and/or nested group names (`groups:`),
    /// nestable. A host's *roles* are derived from group membership (transitive),
    /// so the inventory never repeats role labels per host — see [`Inventory::resolve`].
    #[serde(default)]
    pub groups: BTreeMap<String, Group>,
}

/// A named inventory group (D-077): member hosts and/or nested groups. Mirrors
/// kubekey's `spec.groups` — membership lives here, not on the host.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Group {
    /// Member host names (must match a `hosts[].name`).
    #[serde(default)]
    pub hosts: Vec<String>,
    /// Nested group names, expanded transitively.
    #[serde(default)]
    pub groups: Vec<String>,
    /// Group vars (D-082): apply to the group's members; override global vars.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
}

impl Inventory {
    /// Resolve a group name to its full set of member host names, expanding
    /// nested `groups:` transitively. Unknown group names resolve to empty.
    /// `seen` guards against cycles.
    pub fn group_hosts(&self, name: &str, seen: &mut BTreeSet<String>) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        if !seen.insert(name.to_string()) {
            return out; // already expanding this group → cycle, stop.
        }
        if let Some(g) = self.groups.get(name) {
            for h in &g.hosts {
                out.insert(h.clone());
            }
            for sub in &g.groups {
                out.extend(self.group_hosts(sub, seen));
            }
        }
        out
    }

    /// Derive each host's roles from group membership and populate `Host.roles`.
    /// A host gets role `G` iff it resolves into group `G` (directly or via a
    /// nested group), so a control-plane host also carries the parent
    /// `k8s_cluster` role automatically. Any inline `roles:` on the host are
    /// kept and unioned in (back-compat / `--host` paths), so both styles work.
    /// Call once right after loading the inventory.
    pub fn derive_roles(&mut self) {
        // group name → resolved member host names.
        let resolved: BTreeMap<String, BTreeSet<String>> = self
            .groups
            .keys()
            .map(|g| {
                let mut seen = BTreeSet::new();
                (g.clone(), self.group_hosts(g, &mut seen))
            })
            .collect();
        for host in &mut self.hosts {
            let mut roles: BTreeSet<String> = host.roles.iter().cloned().collect();
            for (g, members) in &resolved {
                if members.contains(&host.name) {
                    roles.insert(g.clone());
                }
            }
            host.roles = roles.into_iter().collect();
        }
    }

    /// Merge the three var levels into each `host.vars` (D-082), precedence
    /// **global < group < host**. A host's groups (its derived `roles`) are
    /// applied in sorted order, so two groups setting the same key resolve
    /// deterministically (host vars are the escape hatch). Run AFTER
    /// [`Inventory::derive_roles`] (it reads `host.roles`).
    pub fn resolve_host_vars(&mut self) {
        let global = self.vars.clone();
        let group_vars: BTreeMap<String, BTreeMap<String, String>> = self
            .groups
            .iter()
            .map(|(k, g)| (k.clone(), g.vars.clone()))
            .collect();
        for host in &mut self.hosts {
            let mut merged = global.clone();
            let mut groups: Vec<&String> = host.roles.iter().collect();
            groups.sort();
            for g in groups {
                if let Some(gv) = group_vars.get(g) {
                    merged.extend(gv.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            }
            merged.extend(host.vars.iter().map(|(k, v)| (k.clone(), v.clone())));
            host.vars = merged;
        }
    }

    /// Prepare a freshly-loaded inventory for use: derive roles from groups, then
    /// merge the three var levels into each host. Call once after parsing.
    pub fn resolve(&mut self) {
        self.derive_roles();
        self.resolve_host_vars();
    }
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
    ///
    /// 值可以写成 `${env:VAR}` —— 执行前才解析(见 target::resolve_secrets)。
    /// **inventory 常常要进 git,而 git 历史删不掉**:凡是会进版本库的清单,
    /// 口令都该走 `${env:}` 或 `password_file:`,或者干脆用 `key:` 绕开。
    #[serde(default)]
    pub password: Option<String>,
    /// 从文件读口令(文件本身不进版本库)。与 `password` 二选一。
    #[serde(default)]
    pub password_file: Option<PathBuf>,
    /// SSH private-key file path. Takes precedence over `password` when set.
    #[serde(default)]
    pub key: Option<PathBuf>,
    #[serde(default)]
    pub roles: Vec<String>,
    /// Host vars (D-082): highest-precedence environment values for this host.
    /// After [`Inventory::resolve_host_vars`] this holds the FULLY MERGED set
    /// (global ⊕ group ⊕ host) that apply overlays onto the task's vars.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
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
            password_file: None,
            key: None,
            roles: Vec::new(),
            vars: BTreeMap::new(),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// D-077: roles are derived from group membership (kubekey style), with
    /// nested groups propagating upward — no per-host `roles:` needed.
    #[test]
    fn derives_roles_from_nested_groups() {
        let yaml = r#"
inventory:
  hosts:
    - { name: n11, address: 192.168.73.11 }
    - { name: n12, address: 192.168.73.12 }
    - { name: w1,  address: 192.168.73.21 }
  groups:
    k8s_cluster:
      groups: [controlplane, worker]
    controlplane:
      hosts: [n11, n12]
    worker:
      hosts: [w1]
"#;
        let mut inv: CraterSpec = serde_yaml::from_str(yaml).unwrap();
        inv.inventory.derive_roles();
        let roles = |name: &str| {
            inv.inventory
                .hosts
                .iter()
                .find(|h| h.name == name)
                .unwrap()
                .roles
                .clone()
        };
        // A control-plane host carries its own group AND the parent aggregate.
        assert_eq!(roles("n11"), vec!["controlplane", "k8s_cluster"]);
        assert_eq!(roles("n12"), vec!["controlplane", "k8s_cluster"]);
        assert_eq!(roles("w1"), vec!["k8s_cluster", "worker"]);
    }

    /// D-082: three var levels merge with precedence global < group < host.
    #[test]
    fn resolve_host_vars_precedence() {
        let yaml = r#"
inventory:
  vars:
    vip: "10.0.0.1"
    pod_cidr: "10.244.0.0/16"
    common: "global"
  hosts:
    - name: n11
      address: 1.1.1.1
      vars: { common: "host" }
    - name: w1
      address: 2.2.2.2
  groups:
    controlplane:
      hosts: [n11]
      vars: { vip: "192.168.73.14", common: "group" }
    worker:
      hosts: [w1]
"#;
        let mut inv: CraterSpec = serde_yaml::from_str(yaml).unwrap();
        inv.inventory.resolve();
        let h = |n: &str| inv.inventory.hosts.iter().find(|h| h.name == n).unwrap();
        // n11: group overrides global vip; host overrides group `common`; global pod_cidr inherited.
        assert_eq!(h("n11").vars.get("vip").unwrap(), "192.168.73.14");
        assert_eq!(h("n11").vars.get("common").unwrap(), "host");
        assert_eq!(h("n11").vars.get("pod_cidr").unwrap(), "10.244.0.0/16");
        // w1: only global (worker group has no vars).
        assert_eq!(h("w1").vars.get("vip").unwrap(), "10.0.0.1");
        assert_eq!(h("w1").vars.get("common").unwrap(), "global");
    }

    /// Inline `roles:` on a host are kept and unioned with group-derived roles.
    #[test]
    fn inline_roles_union_with_groups() {
        let yaml = r#"
inventory:
  hosts:
    - { name: a, address: 10.0.0.1, roles: [extra] }
  groups:
    web:
      hosts: [a]
"#;
        let mut inv: CraterSpec = serde_yaml::from_str(yaml).unwrap();
        inv.inventory.derive_roles();
        assert_eq!(inv.inventory.hosts[0].roles, vec!["extra", "web"]);
    }
}
