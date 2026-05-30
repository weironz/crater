//! crater-core — the deterministic deployment engine.
//!
//! Layers:
//! - [`spec`]      : `crater.yaml` (inventory + components + reserved AI/offline fields)
//! - [`component`] : declarative component descriptor schema + loader
//! - [`engine`]    : turns a descriptor into an ordered execution plan
//! - [`os`]        : OS family abstraction (Debian vs RHEL)
//! - [`source`]    : artifact source abstraction (Online now; Offline in M2)
//! - [`executor`]  : runs commands on a target (Local now; SSH next)

pub mod component;
pub mod engine;
pub mod executor;
pub mod os;
pub mod source;
pub mod spec;

/// Crate-wide result type.
pub use anyhow::Result;
