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
    /// Roles whose host group runs **one host at a time** (D-071). A role-group
    /// holding any of these gets `forks=1` — e.g. control-plane `kubeadm join`
    /// must be serial so additional masters don't race on etcd quorum.
    #[serde(default)]
    pub serial_roles: Vec<String>,
    /// Ordered actions. Dependencies via `needs`; the engine topo-sorts.
    #[serde(default)]
    pub actions: Vec<ActionStep>,
    /// Teardown actions (D-049): the product-specific cleanup/uninstall, run by
    /// `crater delete`. **Authored data, NOT auto-derived** — real software's
    /// cleanup (kubeadm reset, rm /var/lib/mysql, rm /var/lib/docker) targets
    /// runtime-generated state that the install steps never created, so it can't
    /// be inverted from `actions`. Empty = this task has NO delete capability
    /// (delete is opt-in, never forced). Same primitives/engine/idempotency.
    #[serde(default)]
    pub teardown: Vec<ActionStep>,
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
    /// Closed-enum condition (D-071): run only on hosts holding one of these
    /// inventory roles (empty = all). Drives asymmetric multi-node topologies —
    /// e.g. `kubeadm init` on `[bootstrap]`, `kubeadm join` on `[worker]`.
    #[serde(default)]
    pub when_role: Vec<String>,
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
  - { name: yq-bin, kind: file, url_tmpl: "https://x/{{version}}/yq" }
actions:
  - id: place_yq
    action: place
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"
  - id: verify
    action: shell
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

    #[test]
    fn action_names_are_ansible_module_names_only() {
        // D-070: canonical ansible-aligned names only — old crater spellings are
        // GONE (no aliases).
        use crate::component::Action;
        let canonical = [
            ("shell", "RunCmd"),
            ("package", "PkgInstall"),
            ("unarchive", "Extract"),
            ("template", "RenderTemplate"),
            ("role", "Module"),
        ];
        for (name, want) in canonical {
            let yaml = match want {
                "RunCmd" => format!("action: {name}\ncmd: \"true\""),
                "PkgInstall" => format!("action: {name}\npackages: {{ debian: [x] }}"),
                "Extract" => format!("action: {name}\nto: /opt"),
                "RenderTemplate" => format!("action: {name}\nsrc: a.j2\ndst: /etc/a"),
                "Module" => format!("action: {name}\nuses: lineinfile"),
                _ => unreachable!(),
            };
            let a: Action = serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("{name}: {e}"));
            let got = match a {
                Action::RunCmd { .. } => "RunCmd",
                Action::PkgInstall { .. } => "PkgInstall",
                Action::Extract { .. } => "Extract",
                Action::RenderTemplate { .. } => "RenderTemplate",
                Action::Module { .. } => "Module",
                _ => "other",
            };
            assert_eq!(got, want, "action: {name} should parse to {want}");
        }
        // Old names must now be rejected.
        for old in ["run_cmd", "command", "pkg_install", "extract", "render_template", "module", "write_file", "systemd_unit"] {
            let yaml = format!("action: {old}\nname: x");
            assert!(
                serde_yaml::from_str::<Action>(&yaml).is_err(),
                "old name `{old}` should no longer parse"
            );
        }
    }

    #[test]
    fn copy_takes_content_or_src() {
        // D-068/070: `copy` takes `content` or `src`; `write_file`/`dst` are gone.
        use crate::component::Action;
        let by_content: Action =
            serde_yaml::from_str("action: copy\ndest: /etc/x\ncontent: \"hi\"").unwrap();
        let by_src: Action =
            serde_yaml::from_str("action: copy\ndest: /etc/x\nsrc: files/x").unwrap();
        for a in [by_content, by_src] {
            match a {
                Action::Copy { dest, .. } => assert_eq!(dest.to_str(), Some("/etc/x")),
                _ => panic!("should parse to Copy"),
            }
        }
    }

    #[test]
    fn service_canonical_only() {
        // D-069/070: service uses state/enabled; systemd_unit/enable/start are gone.
        use crate::component::{Action, ServiceState};
        let modern: Action =
            serde_yaml::from_str("action: service\nname: foo\nstate: restarted\nenabled: false")
                .unwrap();
        match modern {
            Action::Service { state, enabled, .. } => {
                assert_eq!(state, Some(ServiceState::Restarted));
                assert_eq!(enabled, Some(false));
            }
            _ => panic!("service should parse to Service"),
        }
    }

    #[test]
    fn file_kind_canonical_only() {
        // D-065/070: `kind: file` is the only spelling; `binary` is gone.
        use crate::component::MaterialKind;
        let yaml = "name: t\nmaterials:\n  - name: a\n    kind: file\n    url_tmpl: \"https://x/a\"\nactions: []\n";
        let t: TaskFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(t.materials[0].kind, MaterialKind::File);
        assert!(
            serde_yaml::from_str::<TaskFile>(
                "name: t\nmaterials:\n  - name: a\n    kind: binary\n    url_tmpl: x\nactions: []\n"
            )
            .is_err(),
            "kind: binary should no longer parse"
        );
    }
}
