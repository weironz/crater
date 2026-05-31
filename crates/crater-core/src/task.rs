//! Task model (D-037): `crater apply <action>` — a declarative set of states to
//! reach on targets. A *task* = ordered `actions` + materials + targeting; an
//! *action* = one primitive call (`place`/`run_cmd`/…).
//!
//! Strictly under D-036: a task is pure DATA. Every action carries only a
//! primitive + its params + **closed-enum** switches (`when_os`/`when_offline`)
//! + declarative `needs`. There is NO `when:` expression, NO `loop:`, NO
//! computation — all control flow (filtering, ordering, looping) is done by the
//! Rust engine in [`crate::engine::plan_from_task`].

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::component::{Action, Material, RegisterSpec};
use crate::engine::Phase;

/// A task file: `crater apply <name>.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskFile {
    pub name: String,
    /// Target group name, or `all` (default). A declarative label the engine
    /// resolves against the inventory — NOT an expression (D-036).
    #[serde(default = "default_hosts")]
    pub hosts: String,
    /// Static data, exposed to templates as `{{ key }}` (pure substitution).
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// Offline material closure (D-034) — what `crater build` packs.
    #[serde(default)]
    pub materials: Vec<Material>,
    /// Facts to capture on each host after its actions run, exposed to later
    /// host groups as `hostvars.<host>.<name>` (D-030, now in the task model).
    #[serde(default)]
    pub register: Vec<RegisterSpec>,
    /// Ordered actions. Dependencies via `needs`; the engine topo-sorts.
    #[serde(default)]
    pub actions: Vec<ActionStep>,
    /// Handlers (D-037-b): actions run once at the end, only if a `changed`
    /// action `notify`d them (by `id`). Deduped, in notify order.
    #[serde(default)]
    pub handlers: Vec<ActionStep>,
}

fn default_hosts() -> String {
    "all".into()
}

/// One action in a task: a primitive (flattened: `action: place` + its params)
/// plus declarative metadata. None of these fields require *execution* to
/// understand — they are read statically by the engine (D-036).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionStep {
    /// Stable id for `needs` references. Defaults to `action<index>`.
    #[serde(default)]
    pub id: Option<String>,
    /// Ids this step depends on; the engine topologically orders (NOT YAML).
    #[serde(default)]
    pub needs: Vec<String>,
    /// `install` (default) / `verify` / `preflight` — affects idempotency report.
    #[serde(default)]
    pub phase: Phase,
    /// Closed-enum condition: run only on these OS families (empty = all).
    #[serde(default)]
    pub when_os: Vec<String>,
    /// Closed-enum condition: run only offline (`true`) / only online (`false`).
    #[serde(default)]
    pub when_offline: Option<bool>,
    /// Engine retry count (data). Runtime behavior lands in D-037-b. `0` = none.
    #[serde(default)]
    pub retries: u32,
    /// Continue past a failure even after retries (D-037-b): the step reports
    /// `warn` instead of aborting.
    #[serde(default)]
    pub ignore_errors: bool,
    /// Handler ids to trigger when this step reports `changed` (D-037-b).
    #[serde(default)]
    pub notify: Vec<String>,
    /// The primitive + its params, flattened so the YAML stays flat:
    /// `{ id, action: place, dest: …, needs: [..] }`.
    #[serde(flatten)]
    pub action: Action,
}

impl TaskFile {
    pub fn from_yaml_file(path: &Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }
}

/// Cheap probe: does this YAML file look like a task (a top-level `actions:`)?
/// Used by `crater apply <file>` to route task vs. legacy spec without forcing a
/// flag. Returns false on parse failure (caller falls back to spec).
pub fn is_task_file(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
        .map(|v| v.get("actions").is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_task_with_flattened_action() {
        // The risk point: `#[serde(flatten)]` over an internally-tagged enum.
        let yaml = r#"
name: install-yq
vars: { version: "4.53.2" }
materials:
  - { name: yq-bin, kind: binary, url_tmpl: "https://x/{{version}}/yq" }
actions:
  - id: place_yq
    action: place
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"
  - id: verify
    action: run_cmd
    cmd: "yq --version"
    phase: verify
    needs: [place_yq]
"#;
        let t: TaskFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(t.name, "install-yq");
        assert_eq!(t.hosts, "all");
        assert_eq!(t.actions.len(), 2);
        assert_eq!(t.actions[0].id.as_deref(), Some("place_yq"));
        assert!(matches!(t.actions[0].action, Action::Place { .. }));
        assert_eq!(t.actions[1].phase, Phase::Verify);
        assert_eq!(t.actions[1].needs, vec!["place_yq".to_string()]);
    }
}
