//! Data-defined roles (`roles/<name>.yaml`) — ansible's `role` (D-029, D-080).
//!
//! A role is a **reusable bundle**, the unit that carries its own offline
//! closure (materials) plus a recipe (actions) + handlers + declared params —
//! everything except `hosts:` (which a task/play supplies). Referenced from a
//! task via `action: role`. At plan/build time [`crate::task::TaskFile::expand_roles`]
//! flattens role invocations into the task (actions spliced, materials hoisted),
//! so the rest of the pipeline (build closure, planner) is unchanged.
//!
//! Back-compat: the original thin form — a single `check → act` shell template
//! (e.g. `roles/lineinfile.yaml`) — is still supported (no `actions:`), lowering
//! to one [`crate::engine::Op::Shell`].

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::component::Material;
use crate::task::ActionStep;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModuleDescriptor {
    /// Optional display name (defaults to the file stem).
    #[serde(default)]
    pub name: Option<String>,
    /// Declared parameter names — every one must be supplied via `with:`.
    #[serde(default)]
    pub params: Vec<String>,
    /// The role's offline closure (D-080): binaries/images/os_packages/files it
    /// needs. Hoisted into the task's `materials:` at expansion.
    #[serde(default)]
    pub materials: Vec<Material>,
    /// Bundle recipe: the role's action steps. Non-empty ⇒ this is a full role
    /// (spliced into the task); empty ⇒ legacy thin form (uses `act`).
    #[serde(default)]
    pub actions: Vec<ActionStep>,
    /// Optional handlers, merged into the task's handlers at expansion.
    #[serde(default)]
    pub handlers: Vec<ActionStep>,
    /// Legacy thin form — idempotency probe: exits 0 ⇒ already satisfied ⇒ `act`
    /// skipped. Only used when `actions` is empty.
    #[serde(default)]
    pub check: Option<String>,
    /// Legacy thin form — the command bringing the target to desired state.
    /// Required only when `actions` is empty.
    #[serde(default)]
    pub act: Option<String>,
}

impl ModuleDescriptor {
    pub fn from_yaml_file(path: &std::path::Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read role {}: {e}", path.display()))?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// A full role bundle (has its own actions) vs the legacy single `act` form.
    pub fn is_bundle(&self) -> bool {
        !self.actions.is_empty()
    }

    /// Verify every declared param is present in `with` (clear error if not).
    pub fn check_params(
        &self,
        uses: &str,
        with: &BTreeMap<String, serde_yaml::Value>,
    ) -> crate::Result<()> {
        for p in &self.params {
            if !with.contains_key(p) {
                anyhow::bail!("role '{uses}' missing required param '{p}'");
            }
        }
        Ok(())
    }
}
