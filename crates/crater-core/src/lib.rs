//! crater-core — the deterministic deployment engine.
//!
//! Layers:
//! - [`spec`]      : `crater.yaml` (inventory + components + reserved AI/offline fields)
//! - [`component`] : declarative component descriptor schema + loader
//! - [`dag`]       : component dependency ordering (M3)
//! - [`engine`]    : turns a descriptor into an ordered execution plan + runs it
//! - [`os`]        : OS family abstraction (Debian vs RHEL)
//! - [`source`]    : artifact source (online mirror rewrite + control-plane fetch)
//! - [`executor`]  : runs commands on a target (Local / SSH)
//! - [`bundle`]    : offline bundle build/deploy (M2)
//! - [`ai`]        : AI copilot, NL -> validated spec (M4)
//! - [`diagnose`]  : offline rule-based failure diagnosis (M5)

pub mod ai;
pub mod arch;
pub mod bundle;
pub mod component;
pub mod dag;
pub mod diagnose;
pub mod engine;
pub mod executor;
pub mod module;
pub mod os;
pub mod project;
pub mod source;
pub mod spec;
pub mod state;
pub mod store;
pub mod task;

/// Crate-wide result type.
pub use anyhow::Result;
