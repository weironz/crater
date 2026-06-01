//! CPU architecture abstraction (D-048). A target property detected per host
//! (`uname -m`) and a material-selection axis: a `kind: file` material may
//! declare per-arch variants, and `place` picks the one matching the target.
//!
//! Naming: we canonicalize on the OCI `platform.architecture` spelling
//! (`amd64`/`arm64`), accepting the `uname -m` spellings (`x86_64`/`aarch64`)
//! as aliases so task authors may write either.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Arch {
    /// x86-64 (uname `x86_64`).
    #[serde(alias = "x86_64", alias = "x86-64")]
    Amd64,
    /// AArch64 (uname `aarch64`).
    #[serde(alias = "aarch64")]
    Arm64,
    Unknown,
}

impl Arch {
    /// OCI `platform.architecture` spelling.
    pub fn as_str(&self) -> &'static str {
        match self {
            Arch::Amd64 => "amd64",
            Arch::Arm64 => "arm64",
            Arch::Unknown => "unknown",
        }
    }

    /// Normalize a `uname -m` token (or an OCI arch name) into an [`Arch`].
    pub fn from_uname(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "x86_64" | "x86-64" | "amd64" => Arch::Amd64,
            "aarch64" | "arm64" => Arch::Arm64,
            _ => Arch::Unknown,
        }
    }
}

/// Detect a target's CPU arch via `uname -m` through an executor (works over
/// SSH against a remote node).
pub async fn detect_via(exec: &dyn crate::executor::Executor) -> Arch {
    match exec.run("uname -m").await {
        Ok(o) if o.ok() => Arch::from_uname(&o.stdout),
        _ => Arch::Unknown,
    }
}

/// The control machine's own arch (compile-time `std::env::consts::ARCH`).
/// Used as the dry-run preview arch when no target connection is made.
pub fn detect_local() -> Arch {
    Arch::from_uname(std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_uname_spellings() {
        assert_eq!(Arch::from_uname("x86_64"), Arch::Amd64);
        assert_eq!(Arch::from_uname("aarch64\n"), Arch::Arm64);
        assert_eq!(Arch::from_uname("arm64"), Arch::Arm64);
        assert_eq!(Arch::from_uname("riscv64"), Arch::Unknown);
    }

    #[test]
    fn yaml_accepts_canonical_and_alias() {
        let a: Arch = serde_yaml::from_str("amd64").unwrap();
        assert_eq!(a, Arch::Amd64);
        let b: Arch = serde_yaml::from_str("x86_64").unwrap();
        assert_eq!(b, Arch::Amd64);
        let c: Arch = serde_yaml::from_str("aarch64").unwrap();
        assert_eq!(c, Arch::Arm64);
    }
}
