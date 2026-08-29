//! 目标侧探测事实(k8s 试金石裁定 C)—— CEL 里 `substrate.*` 的来源。
//!
//! 三条纪律:
//! 1. **封闭白名单**:能探的就这几项,不做 ansible 式的全量 `setup`(那是一次
//!    几百个变量的往返,绝大多数没人用);
//! 2. **惰性 + 缓存**:一次连接内每项最多探一次;
//! 3. **只读**:全部走 [`Ctx::probe`],与 plan 的"零写入"承诺同源。
//!
//! 有了它,物料的多架构变体(`when: substrate.arch == 'arm64'`)才能在**目标机上**
//! 判定,而不是让作者在控制端猜。

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::eval::Yaml;
use crate::verbs::Ctx;

/// 一项可探测事实:名字 + 取值命令 + 归一化。
struct FactSpec {
    name: &'static str,
    cmd: &'static str,
    normalize: fn(&str) -> String,
}

const FACTS: &[FactSpec] = &[
    FactSpec { name: "arch", cmd: "uname -m", normalize: normalize_arch },
    FactSpec { name: "kernel", cmd: "uname -r", normalize: trim },
    FactSpec { name: "hostname", cmd: "hostname", normalize: trim },
    FactSpec {
        name: "distro",
        cmd: ". /etc/os-release 2>/dev/null && echo \"$ID\"",
        normalize: trim,
    },
    FactSpec {
        name: "version",
        cmd: ". /etc/os-release 2>/dev/null && echo \"$VERSION_ID\"",
        normalize: trim,
    },
    FactSpec {
        // deb / rpm —— 决定包管理器分叉的那一位。
        name: "family",
        cmd: "if command -v apt-get >/dev/null 2>&1; then echo debian; \
              elif command -v dnf >/dev/null 2>&1 || command -v yum >/dev/null 2>&1; then echo rhel; \
              else echo unknown; fi",
        normalize: trim,
    },
    FactSpec {
        name: "init",
        cmd: "if command -v systemctl >/dev/null 2>&1; then echo systemd; else echo none; fi",
        normalize: trim,
    },
    // 这台机器在机群里的**主地址**:按默认路由选出的那个,而不是 `hostname -I`
    // 的第一个 —— 后者在有 docker0/cni0 的机器上会给出网桥地址,拿它去配
    // apiserver 后端会得到一个别人连不上的集群。
    FactSpec {
        name: "ip",
        cmd: "ip -4 route get 1.1.1.1 2>/dev/null | grep -oE 'src [0-9.]+' | awk '{print $2}' | head -1",
        normalize: |s| s.trim().to_string(),
    },
    // 默认路由所在网卡。keepalived 的 VIP 必须绑在它上面,写死 eth0 在
    // ens18/enp0s3 这类命名下会直接失效。
    FactSpec {
        name: "iface",
        cmd: "ip -4 route show default 2>/dev/null | grep -oE 'dev [^ ]+' | awk '{print $2}' | head -1",
        normalize: |s| s.trim().to_string(),
    },
];

fn trim(s: &str) -> String {
    s.trim().to_string()
}

/// `uname -m` 的方言归一到 OCI 平台名 —— 物料变体条件写 `arm64` 就够,
/// 不必让每个作者都记住 `aarch64`/`x86_64` 这些别名。
fn normalize_arch(s: &str) -> String {
    match s.trim() {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        "armv7l" => "arm",
        other => other,
    }
    .to_string()
}

/// 惰性事实表:按需探测并缓存。
pub struct Facts<'a> {
    ctx: &'a dyn Ctx,
    cache: RefCell<BTreeMap<String, Yaml>>,
}

impl<'a> Facts<'a> {
    pub fn new(ctx: &'a dyn Ctx) -> Self {
        Facts { ctx, cache: RefCell::new(BTreeMap::new()) }
    }

    /// 白名单里的全部事实名。
    pub fn names() -> Vec<&'static str> {
        FACTS.iter().map(|f| f.name).collect()
    }

    /// 一次性把白名单探全 —— plan 之前调一次,之后 CEL 求值零往返。
    pub fn gather_all(&self) -> anyhow::Result<BTreeMap<String, Yaml>> {
        for spec in FACTS {
            self.get(spec.name)?;
        }
        Ok(self.cache.borrow().clone())
    }

    /// 取一项(已缓存则不再探)。未知名字返回 `None` —— 白名单之外一律不探,
    /// 免得 `substrate.` 变成一个可以夹带任意命令的口子。
    pub fn get(&self, name: &str) -> anyhow::Result<Option<Yaml>> {
        if let Some(v) = self.cache.borrow().get(name) {
            return Ok(Some(v.clone()));
        }
        let Some(spec) = FACTS.iter().find(|f| f.name == name) else {
            return Ok(None);
        };
        let (code, out) = self.ctx.probe(spec.cmd)?;
        // 探不到不是错误(容器里可能没有 /etc/os-release):记空串,
        // 让 `when:` 条件自然不成立,而不是整个 plan 崩掉。
        let value = if code == 0 { (spec.normalize)(&out) } else { String::new() };
        let y = Yaml::String(value);
        self.cache.borrow_mut().insert(name.to_string(), y.clone());
        Ok(Some(y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::{FakeCtx, LocalCtx};

    #[test]
    fn arch_aliases_normalize_to_oci_names() {
        assert_eq!(normalize_arch("x86_64"), "amd64");
        assert_eq!(normalize_arch("aarch64\n"), "arm64");
        assert_eq!(normalize_arch("riscv64"), "riscv64", "未知架构原样透传");
    }

    #[test]
    fn facts_are_probed_once_and_cached() {
        let ctx = FakeCtx::new().on("uname -m", 0, "aarch64\n");
        let facts = Facts::new(&ctx);
        assert_eq!(facts.get("arch").unwrap(), Some(Yaml::from("arm64")));
        assert_eq!(facts.get("arch").unwrap(), Some(Yaml::from("arm64")));
        assert_eq!(ctx.calls().len(), 1, "第二次取值不该再发往返");
    }

    #[test]
    fn gathering_is_read_only() {
        let ctx = FakeCtx::new().on("", 0, "x86_64");
        Facts::new(&ctx).gather_all().unwrap();
        assert!(ctx.writes().is_empty(), "事实采集期间发生了写:{:?}", ctx.writes());
    }

    #[test]
    fn names_outside_the_whitelist_are_never_probed() {
        // `substrate.` 不能变成夹带任意命令的口子。
        let ctx = FakeCtx::new();
        assert_eq!(Facts::new(&ctx).get("rm -rf /").unwrap(), None);
        assert!(ctx.calls().is_empty());
    }

    #[test]
    fn a_failing_probe_yields_an_empty_value_not_an_error() {
        // 容器里常常没有 /etc/os-release —— 条件自然不成立即可,不该整盘崩。
        let ctx = FakeCtx::new(); // 一切未注册 → 退出码 1
        assert_eq!(Facts::new(&ctx).get("distro").unwrap(), Some(Yaml::from("")));
    }

    #[test]
    fn real_host_reports_a_plausible_arch() {
        let facts = Facts::new(&LocalCtx);
        let arch = facts.get("arch").unwrap().unwrap();
        assert!(
            ["amd64", "arm64", "arm"].contains(&arch.as_str().unwrap()),
            "本机架构探测异常:{arch:?}"
        );
    }
}
