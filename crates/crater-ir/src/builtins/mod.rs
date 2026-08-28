//! L1 内建资源类型的**实现**(`types.rs` 只登记 lint 用的形状)。
//!
//! 这三个是骨架样本:`file`(状态最简)、`copy`(内容寻址幂等)、`service`
//! (承载"上游变更即重启"的裁定)。其余内建类型按同一模板补齐,四层实现
//! (Rust / blueprint types / WASM / 协议桥)履行同一个 trait。

pub mod copy;
pub mod file;
pub mod host;
pub mod paths;
pub mod pkg;
pub mod runtime;
pub mod service;

use crate::verbs::ResourceType;

/// 按类型名取实现。返回 `None` = 不是内建(可能是 blueprint 自定义类型)。
pub fn get(name: &str) -> Option<&'static dyn ResourceType> {
    match name {
        // 文件与内容
        "file" => Some(&file::File),
        "copy" => Some(&copy::Copy),
        "template" => Some(&paths::Template),
        "lineinfile" => Some(&paths::LineInFile),
        "unarchive" => Some(&paths::Unarchive),
        // 服务与主机基线
        "service" => Some(&service::Service),
        "systemd_unit" => Some(&service::SystemdUnit),
        "hostname" => Some(&host::Hostname),
        "swap" => Some(&host::Swap),
        "kernel_modules" => Some(&host::KernelModules),
        "sysctl" => Some(&host::Sysctl),
        "user" => Some(&host::User),
        "group" => Some(&host::Group),
        // 包与容器
        "package" => Some(&pkg::Package),
        "image_present" => Some(&pkg::ImagePresent),
        "container" => Some(&runtime::Container),
        // 主机层设施
        "mount" => Some(&runtime::Mount),
        "cron" => Some(&runtime::Cron),
        // 过程性原语
        "shell" => Some(&pkg::Shell),
        "wait" => Some(&pkg::Wait),
        // 健康探针
        "http" => Some(&pkg::Http),
        "port_open" => Some(&pkg::PortOpen),
        "service_active" => Some(&pkg::ServiceActive),
        "cmd" => Some(&pkg::CmdProbe),
        _ => None,
    }
}

/// 已实现五动词的类型名(与 `types.rs` 的登记表比对,得出"还欠多少")。
pub fn implemented() -> Vec<&'static str> {
    crate::types::BUILTINS
        .iter()
        .map(|t| t.name)
        .filter(|n| get(n).is_some())
        .collect()
}

/// 已登记但**还没有**五动词实现的类型 —— 差距要可见,不可含糊。
pub fn pending() -> Vec<&'static str> {
    crate::types::BUILTINS
        .iter()
        .map(|t| t.name)
        .filter(|n| get(n).is_none())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_implementation_is_registered_in_the_lint_catalog() {
        // 实现了却没登记 → lint 会把合法写法报成"未知类型"。
        for name in implemented() {
            assert!(crate::types::is_builtin(name), "`{name}` 未登记进 types.rs");
            assert_eq!(get(name).unwrap().name(), name);
        }
    }

    #[test]
    fn the_gap_between_catalog_and_implementation_is_explicit() {
        // 登记表是"lint 认得的写法",实现表是"真能跑的类型"。
        // 两者的差值必须**能被列出来**,而不是让用户 apply 时才撞上。
        let pending = pending();
        assert_eq!(
            implemented().len() + pending.len(),
            crate::types::BUILTINS.len()
        );
        // 登记表已全部有实现。这条断言是防倒退的:再登记新类型而不实现,
        // 它会立刻变红,逼人要么补实现、要么明确把它列进欠债。
        assert!(pending.is_empty(), "有登记未实现的类型:{pending:?}");
    }

    #[test]
    fn unknown_names_resolve_to_nothing() {
        assert!(get("cluster_member").is_none(), "自定义类型不该走内建表");
        assert!(get("nope").is_none());
    }
}
