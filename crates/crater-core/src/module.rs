//! Data-defined roles (`roles/<name>.yaml`) — ansible's `role` (D-029). A role
//! is a reusable `check → act` template: zero Rust, dropped into `roles/` and
//! referenced from a task via `action: role` (the old `modules/` dir + `action:
//! module` spelling still works as a back-compat fallback).
//!
//! It lowers to the same [`crate::engine::Op::Shell`] (with an idempotency
//! `check`) that the built-in modules use, so it inherits the B1 contract —
//! `ok` when the check passes, `changed` when `act` runs.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ModuleDescriptor {
    /// Optional display name (defaults to the file stem).
    #[serde(default)]
    pub name: Option<String>,
    /// Declared parameter names — every one must be supplied via `with:`.
    #[serde(default)]
    pub params: Vec<String>,
    /// Idempotency probe: exits 0 ⇒ already in desired state ⇒ `act` skipped.
    #[serde(default)]
    pub check: Option<String>,
    /// The command that brings the target to the desired state.
    pub act: String,
}

impl ModuleDescriptor {
    pub fn from_yaml_file(path: &std::path::Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read module {}: {e}", path.display()))?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// Verify every declared param is present in `with` (clear error if not).
    pub fn check_params(
        &self,
        uses: &str,
        with: &BTreeMap<String, String>,
    ) -> crate::Result<()> {
        for p in &self.params {
            if !with.contains_key(p) {
                anyhow::bail!("module '{uses}' missing required param '{p}'");
            }
        }
        Ok(())
    }
}
