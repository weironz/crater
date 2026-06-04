//! OS family abstraction. M1 targets Debian/Ubuntu and RHEL families.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OsFamily {
    Debian,
    Rhel,
    #[default]
    Unknown,
}

impl OsFamily {
    pub fn as_str(&self) -> &'static str {
        match self {
            OsFamily::Debian => "debian",
            OsFamily::Rhel => "rhel",
            OsFamily::Unknown => "unknown",
        }
    }

    pub fn from_name(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "debian" | "ubuntu" => OsFamily::Debian,
            "rhel" | "centos" | "rocky" | "almalinux" | "fedora" => OsFamily::Rhel,
            _ => OsFamily::Unknown,
        }
    }

    /// Keys this family will match in a component's `by_os` package map,
    /// in priority order.
    pub fn match_keys(&self) -> &'static [&'static str] {
        match self {
            OsFamily::Debian => &["debian", "ubuntu"],
            OsFamily::Rhel => &["rhel", "centos", "rocky", "almalinux", "fedora"],
            OsFamily::Unknown => &[],
        }
    }

    pub fn install_cmd(&self, packages: &[String]) -> String {
        let pkgs = packages.join(" ");
        match self {
            OsFamily::Debian => format!(
                "apt-get update -y && DEBIAN_FRONTEND=noninteractive apt-get install -y {pkgs}"
            ),
            OsFamily::Rhel => format!("dnf install -y {pkgs} || yum install -y {pkgs}"),
            OsFamily::Unknown => format!("echo 'unknown OS family: cannot install {pkgs}'; exit 1"),
        }
    }
}

/// Detect a target's OS family by reading `/etc/os-release` through an executor
/// (works over SSH against a remote node).
pub async fn detect_via(exec: &dyn crate::executor::Executor) -> OsFamily {
    match exec.run("cat /etc/os-release").await {
        Ok(o) if o.ok() => family_from_os_release(&o.stdout),
        _ => OsFamily::Unknown,
    }
}

/// Detect the local machine's OS family from `/etc/os-release`.
pub fn detect_local() -> OsFamily {
    match std::fs::read_to_string("/etc/os-release") {
        Ok(c) => family_from_os_release(&c),
        Err(_) => OsFamily::Unknown,
    }
}

/// Parse `/etc/os-release` content (`ID=` / `ID_LIKE=`) into an [`OsFamily`].
pub fn family_from_os_release(content: &str) -> OsFamily {
    let mut id = String::new();
    let mut id_like = String::new();
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            id = unquote(v);
        } else if let Some(v) = line.strip_prefix("ID_LIKE=") {
            id_like = unquote(v);
        }
    }
    let hay = format!("{id} {id_like}").to_lowercase();
    if ["debian", "ubuntu"].iter().any(|k| hay.contains(k)) {
        OsFamily::Debian
    } else if ["rhel", "centos", "fedora", "rocky", "almalinux"]
        .iter()
        .any(|k| hay.contains(k))
    {
        OsFamily::Rhel
    } else {
        OsFamily::Unknown
    }
}

fn unquote(s: &str) -> String {
    s.trim().trim_matches('"').to_string()
}

/// Full OS identity (D-102): family for engine forks (apt vs dnf), plus the
/// DISTRO (`ID=`) and VERSION (`VERSION_ID=`) for the task `requires:` gate —
/// "debian-family" is not enough when an os_package closure was resolved
/// against ubuntu:24.04 or a vendor only certifies openEuler/Kylin.
#[derive(Debug, Clone, Default)]
pub struct OsInfo {
    pub family: OsFamily,
    /// `/etc/os-release` `ID` (lowercase): `ubuntu`, `debian`, `openeuler`,
    /// `kylin`, `rocky`, ... Empty when undetectable.
    pub distro: String,
    /// `/etc/os-release` `VERSION_ID`: `24.04`, `9.4`, `V10`, ... Empty when
    /// undetectable (some rolling distros omit it).
    pub version: String,
}

/// Detect a target's full OS identity through an executor (D-102).
pub async fn detect_info_via(exec: &dyn crate::executor::Executor) -> OsInfo {
    match exec.run("cat /etc/os-release").await {
        Ok(o) if o.ok() => info_from_os_release(&o.stdout),
        _ => OsInfo::default(),
    }
}

/// Parse `/etc/os-release` into the full [`OsInfo`].
pub fn info_from_os_release(content: &str) -> OsInfo {
    let mut info = OsInfo { family: family_from_os_release(content), ..Default::default() };
    for line in content.lines() {
        if let Some(v) = line.strip_prefix("ID=") {
            info.distro = unquote(v).to_lowercase();
        } else if let Some(v) = line.strip_prefix("VERSION_ID=") {
            info.version = unquote(v);
        }
    }
    info
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ubuntu() {
        let s = "ID=ubuntu\nID_LIKE=debian\n";
        assert_eq!(family_from_os_release(s), OsFamily::Debian);
    }

    #[test]
    fn detects_rocky() {
        let s = "ID=\"rocky\"\nID_LIKE=\"rhel centos fedora\"\n";
        assert_eq!(family_from_os_release(s), OsFamily::Rhel);
    }
}
