//! Declarative component descriptor (`components/<name>/component.yaml`).
//!
//! A component is a list of action primitives across three phases:
//! `preflight`, `install`, `verify`. Built-in and third-party components
//! share this exact schema — drop a directory under `components/` to add one.
//!
//! Both [`Check`] and [`Action`] are **internally tagged** (`check:` / `action:`).
//! serde_yaml 0.9 represents *externally* tagged enums with `!Tag` YAML tags,
//! which we avoid so the YAML stays plain `key: value`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ComponentDescriptor {
    pub name: String,
    #[serde(default)]
    pub version_default: Option<String>,
    #[serde(default)]
    pub supported_os: Vec<String>,
    /// Other components that must be deployed before this one (M3 DAG).
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub preflight: Vec<Check>,
    #[serde(default)]
    pub install: Vec<Action>,
    #[serde(default)]
    pub verify: Vec<Action>,
    /// Reserved for offline artifact manifest (M2). Opaque for now.
    #[serde(default)]
    pub offline: Option<serde_yaml::Value>,
}

impl ComponentDescriptor {
    pub fn from_yaml_file(path: &std::path::Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }
}

/// Preflight checks — internally tagged by the `check:` key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "check", rename_all = "snake_case")]
pub enum Check {
    PortFree { port: u16 },
    KernelMin { version: String },
    DiskFree { path: String, min_gb: u64 },
}

/// Action primitives — internally tagged by the `action:` key.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    /// OS-family-keyed package lists under `packages:`.
    PkgInstall {
        packages: BTreeMap<String, Vec<String>>,
    },
    Download {
        /// `{{version}}` is substituted at plan time.
        url_tmpl: String,
        #[serde(default)]
        sha256: Option<String>,
        #[serde(default)]
        dest: Option<PathBuf>,
    },
    Extract {
        to: PathBuf,
        #[serde(default)]
        from: Option<PathBuf>,
        /// tar --strip-components value.
        #[serde(default)]
        strip: u32,
    },
    RenderTemplate {
        src: String,
        dst: PathBuf,
    },
    WriteFile {
        dst: PathBuf,
        content: String,
    },
    SystemdUnit {
        name: String,
        #[serde(default)]
        enable: bool,
        #[serde(default)]
        start: bool,
    },
    RunCmd {
        cmd: String,
    },
    /// Reserved for offline image loading (M2+).
    LoadImage {
        reference: String,
    },
}
