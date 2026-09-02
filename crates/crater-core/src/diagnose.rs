//! Offline diagnosis (M5) — `crater doctor`.
//!
//! Requirements §5.2: on the offline side, AI degrades to **pre-baked knowledge
//! first**. This module is that 固化产物: a deterministic rule set mapping observed
//! error signatures to a likely cause and a concrete fix — with **zero network
//! and zero model** required. When an AI endpoint *is* available (e.g. an
//! intranet model), the CLI can additionally ask it for deeper analysis, but the
//! rules always run first and stand alone.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub category: &'static str,
    pub cause: &'static str,
    pub fix: &'static str,
}

struct Rule {
    needles: &'static [&'static str],
    finding: Finding,
}

fn rules() -> &'static [Rule] {
    const R: &[Rule] = &[
        Rule {
            needles: &["unable to locate package", "has no installation candidate"],
            finding: Finding {
                category: "apt",
                cause: "包索引过期或未启用 universe 仓库",
                fix: "先 apt-get update;必要时 add-apt-repository universe;国内换镜像源(aliyun/清华)。crater install 已内置 apt-get update。",
            },
        },
        Rule {
            needles: &[
                "could not resolve host",
                "temporary failure resolving",
                "name or service not known",
            ],
            finding: Finding {
                category: "dns",
                cause: "DNS 解析失败(无网或 resolv.conf 缺失)",
                fix: "检查 /etc/resolv.conf 与网络;离线环境改用 crater build 制包 + crater deploy。",
            },
        },
        Rule {
            needles: &[
                "connection timed out",
                "failed to connect",
                "connection refused",
                "i/o timeout",
            ],
            finding: Finding {
                category: "network",
                cause: "外网连接超时/被拒(防火墙、代理或被墙)",
                fix: "确认目标可达;国内拉取走镜像源;无外网则用离线包部署。",
            },
        },
        Rule {
            needles: &["no space left on device"],
            finding: Finding {
                category: "disk",
                cause: "磁盘空间不足",
                fix: "清理大目录(/var/lib、日志等);扩容后重试(df -h 定位)。",
            },
        },
        Rule {
            needles: &["permission denied", "operation not permitted"],
            finding: Finding {
                category: "permission",
                cause: "权限不足(非 root / 缺 sudo / SELinux 限制)",
                fix: "以 root 运行;RHEL 系检查 SELinux(getenforce,必要时 setenforce 0 调试)。",
            },
        },
        Rule {
            needles: &["address already in use", "port is already allocated"],
            finding: Finding {
                category: "port",
                cause: "端口被占用",
                fix: "ss -ltnp | grep <port> 找占用进程;停冲突服务或换端口。",
            },
        },
        Rule {
            needles: &["x509", "certificate has expired", "certificate is not yet valid"],
            finding: Finding {
                category: "tls/time",
                cause: "TLS 证书问题,常见为系统时间不对",
                fix: "校时:timedatectl set-ntp true 或 date -s;企业 CA 则导入根证书。",
            },
        },
        Rule {
            needles: &["manifest unknown", "pull access denied", "error pulling image"],
            finding: Finding {
                category: "registry",
                cause: "镜像拉取失败(tag 不存在或 registry 不可达)",
                fix: "配置 registry 镜像(daemon.json registry-mirrors);离线用 load_image 预载镜像。",
            },
        },
        Rule {
            needles: &["exec format error"],
            finding: Finding {
                category: "arch",
                cause: "二进制架构与主机不符(amd64 vs arm64)",
                fix: "下载与目标 CPU 架构匹配的制品;crater 制包时按目标 arch 选择。",
            },
        },
        Rule {
            needles: &["cgroup", "failed to find cgroup"],
            finding: Finding {
                category: "cgroup",
                cause: "cgroup 驱动不匹配或 cgroup v2 问题",
                fix: "统一 cgroupdriver=systemd(各服务的 cgroup 驱动保持一致)。",
            },
        },
    ];
    R
}

/// Run all rules over `text` (typically collected logs/errors).
/// Deduplicates by (category, cause).
pub fn diagnose(text: &str) -> Vec<Finding> {
    let hay = text.to_lowercase();
    let mut out: Vec<Finding> = Vec::new();
    for rule in rules() {
        if rule.needles.iter().any(|n| hay.contains(&n.to_lowercase()))
            && !out
                .iter()
                .any(|f| f.category == rule.finding.category && f.cause == rule.finding.cause)
            {
                out.push(rule.finding.clone());
            }
    }
    out
}

pub fn rule_count() -> usize {
    rules().len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_apt_missing_package() {
        let f = diagnose("E: Unable to locate package docker.io");
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].category, "apt");
    }

    #[test]
    fn detects_network_and_disk_together() {
        let log = "curl: (7) Failed to connect ...\nwrite error: No space left on device";
        let f = diagnose(log);
        let cats: Vec<&str> = f.iter().map(|x| x.category).collect();
        assert!(cats.contains(&"network"));
        assert!(cats.contains(&"disk"));
    }

    #[test]
    fn clean_log_no_findings() {
        assert!(diagnose("Successfully installed everything. Done.").is_empty());
    }

    #[test]
    fn has_rules() {
        assert!(rule_count() >= 8);
    }
}
