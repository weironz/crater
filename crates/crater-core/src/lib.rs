//! crater-core —— 部署引擎的基础设施层。
//!
//! 这里**只放"与管线无关"的东西**:传输、制品、机群规格、事实。五动词的
//! 语义(蓝图 IR、plan/apply/verify)在 `crater-ir`,命令面在 `crater-cli`。
//!
//! - [`spec`]     :inventory 的形状(主机 / 组 / 三层 vars / 凭据)
//! - [`executor`] :在目标上跑命令(Local / SSH)
//! - [`store`]    :本地 OCI store 与 registry 客户端
//! - [`bundle`]   :离线闭包的制品格式(内容寻址的 blob + manifest)
//! - [`source`]   :按 URL 取字节(带镜像回退)
//! - [`state`]    :部署记账(`~/.crater/state`)
//! - [`arch`]     :CPU 架构归一到 OCI 平台名
//! - [`os`]       :发行版族抽象(debian / rhel)
//! - [`dag`]      :依赖定序
//! - [`diagnose`] :离线规则诊断
//! - [`zip`]      :控制端解包(目标机零依赖,见 D-103)
//!
//! 旧 task 管线的 `engine` / `task` / `component` / `project` / `module` /
//! `ai` 已在 D-151 整块删除。

pub mod arch;
pub mod bundle;
pub mod dag;
pub mod diagnose;
pub mod executor;
pub mod os;
pub mod source;
pub mod spec;
pub mod state;
pub mod store;
pub mod zip;

/// Crate-wide result type.
pub use anyhow::Result;
