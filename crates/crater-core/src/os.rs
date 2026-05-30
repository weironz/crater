//! OS family abstraction. M1 targets Debian/Ubuntu and RHEL families.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    Debian,
    Rhel,
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
