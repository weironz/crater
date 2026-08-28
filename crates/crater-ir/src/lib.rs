//! crater IR —— 七名词 + 五动词的类型化契约(D-106)。
//!
//! **IR 是唯一契约,语法只是前端**:YAML(今天)/ HCL / UI / MCP 都编译到这里,
//! plan / converge / drift / teardown / API 都只认这里。设计依据见
//! `docs/research/ir-draft.md`,两块试金石见 `ir-example-rustfs.md` / `ir-example-k8s.md`。
//!
//! 本 crate 只做**前端**(parse + lint)与**契约**(五动词 trait);执行、SSH、OCI 全在别处。

pub mod builtins;
pub mod ctx;
pub mod eval;
pub mod facts;
pub mod fleet;
pub mod expr;
pub mod ir;
pub mod jsonschema;
pub mod lint;
pub mod materials;
pub mod loc;
pub mod parse;
pub mod plan;
pub mod procedure;
pub mod schema;
pub mod selector;
pub mod state;
pub mod types;
pub mod verbs;

pub use ir::Blueprint;
pub use lint::{Diagnostic, Severity};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Parse(String),
    #[error("YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
}

impl Error {
    pub fn parse(msg: impl Into<String>) -> Self {
        Error::Parse(msg.into())
    }
}
