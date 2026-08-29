//! 包与容器类内建:`package` / `image_present`,过程性原语 `shell` / `wait`,
//! 以及 `health:` 段用的只读探针(`http` / `port_open` / `service_active` / `cmd`)。

use anyhow::Result;

use crate::builtins::file::run_ok;
use crate::eval::{ResolvedArgs, Yaml};
use crate::verbs::*;

/// `package` —— OS 包。`packages:` 按 family(debian/rhel)分叉:
/// 引擎只懂"怎么装",**装什么是数据**(D-017)。
pub struct Package;

impl ResourceType for Package {
    fn name(&self) -> &'static str {
        "package"
    }
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let pkgs = packages_for(ctx, args)?;
        if pkgs.is_empty() {
            return Ok(Observed::default());
        }
        let mut missing = Vec::new();
        for p in &pkgs {
            let (code, _) = ctx.probe(&format!(
                "dpkg -s {p} >/dev/null 2>&1 || rpm -q {p} >/dev/null 2>&1",
                p = sh(p)
            ))?;
            if code != 0 {
                missing.push(p.clone());
            }
        }
        // `declared` 让退役能判断"是不是全都已经卸干净了"。
        // 不能改用 present=false 表达 —— 那个布尔在 diff 里另有含义
        // (「没有适配本机 family 的包名」),混用会让安装路径失灵。
        Ok(Observed::present([
            ("missing", missing.join(",")),
            ("declared", pkgs.len().to_string()),
        ]))
    }
    fn diff(&self, input: &DiffInput) -> Change {
        if !input.observed.present {
            return Change::Unknown("未针对本机 family 声明包名".into());
        }
        let missing = input.observed.get("missing").unwrap_or_default();
        if missing.is_empty() {
            Change::Ok
        } else {
            Change::Update(vec![FieldDiff::set("install", missing)])
        }
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        let pkgs = packages_for(ctx, args)?;
        if pkgs.is_empty() {
            anyhow::bail!("package:没有适配本机 family 的包名");
        }
        // 索引比仓库旧时,apt 会去抓已被归档取代的 .deb 版本,得到 404 而不是
        // "包不存在"。这类失败**刷新索引就能好**,而且极其常见(任何开机久了
        // 或从旧快照恢复的机器都会碰上)。
        //
        // 刷新不做在前面:那会给每次 apply 都加上几十秒的固定成本,而绝大多数
        // 运行里包本就装好、根本不会走到这里。失败后重试一次是更划算的交换。
        let (code, out) = ctx.run(&pkg_cmd(true, &pkgs))?;
        if code == 0 {
            return Ok(Outcome::Changed);
        }
        run_ok(ctx, refresh_cmd())?;
        let (code2, out2) = ctx.run(&pkg_cmd(true, &pkgs))?;
        if code2 != 0 {
            anyhow::bail!(
                "装包失败(刷新索引后重试仍失败,exit {code2}):{}\n\
                 首次失败输出:\n{}\n重试输出:\n{}",
                pkgs.join(" "),
                out.trim(),
                out2.trim()
            );
        }
        Ok(Outcome::Changed)
    }
    /// 全都不在了就没什么可卸的 —— 否则重跑 destroy 会永远显示"还有活干"。
    fn destroy_change(&self, observed: &Observed) -> Change {
        if !observed.present {
            return Change::Ok;
        }
        let declared: usize = observed.get("declared").and_then(|s| s.parse().ok()).unwrap_or(0);
        let missing = observed.get("missing").unwrap_or_default();
        let missing_n = if missing.is_empty() { 0 } else { missing.split(',').count() };
        if declared > 0 && missing_n >= declared {
            Change::Ok
        } else {
            Change::Destroy
        }
    }
    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        let pkgs = packages_for(ctx, args)?;
        if pkgs.is_empty() {
            return Ok(Outcome::Ok);
        }
        run_ok(ctx, &pkg_cmd(false, &pkgs))?;
        Ok(Outcome::Changed)
    }
}

/// 生成**目标机上**要跑的包管理命令。`install=true` 装,否则卸。
/// 刷新包索引。`|| true` 收口:索引里有个别源不可达是常态,不该因此让
/// 整条安装路径失败 —— 真正的判据是紧随其后的安装成不成功。
fn refresh_cmd() -> &'static str {
    "if command -v apt-get >/dev/null 2>&1; then apt-get -o DPkg::Lock::Timeout=300 update -qq || true; \
     elif command -v dnf >/dev/null 2>&1; then dnf makecache -q || true; \
     else yum makecache -q || true; fi"
}

fn pkg_cmd(install: bool, pkgs: &[String]) -> String {
    let list = pkgs.iter().map(|p| sh(p)).collect::<Vec<_>>().join(" ");
    let (apt_op, rpm_op) = if install {
        ("install -y", "install -y")
    } else {
        ("purge -y", "remove -y")
    };
    // `DPkg::Lock::Timeout` 让 apt **等锁**而不是当场失败。
    //
    // Ubuntu 默认开着 unattended-upgrades,它随时可能持有 dpkg 锁 ——
    // 真机上销毁 5 台就撞上了一台。这类冲突纯属时机问题、几十秒后自然消失,
    // 让整次部署为它失败是不划算的。apt 自己就支持等,不必我们写重试。
    format!(
        "if command -v apt-get >/dev/null 2>&1; then \
           DEBIAN_FRONTEND=noninteractive apt-get -o DPkg::Lock::Timeout=300 {apt_op} {list}; \
         elif command -v dnf >/dev/null 2>&1; then dnf {rpm_op} {list}; \
         else yum {rpm_op} {list}; fi"
    )
}

/// 按目标 family 取包名列表。`packages: {debian: [...], rhel: [...]}`。
fn packages_for(ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Vec<String>> {
    let Some(Yaml::Mapping(by_family)) = args.get("packages") else {
        return Ok(Vec::new());
    };
    let (_, family) = ctx.probe(
        "if command -v apt-get >/dev/null 2>&1; then echo debian; \
         elif command -v dnf >/dev/null 2>&1 || command -v yum >/dev/null 2>&1; then echo rhel; \
         else echo unknown; fi",
    )?;
    let key = Yaml::String(family.trim().to_string());
    Ok(match by_family.get(&key) {
        Some(Yaml::Sequence(items)) => items.iter().map(crate::eval::scalar_to_string).collect(),
        _ => Vec::new(),
    })
}

/// `image_present` —— 容器镜像在目标运行时里在不在。
pub struct ImagePresent;

impl ResourceType for ImagePresent {
    fn name(&self) -> &'static str {
        "image_present"
    }

    fn retire_note(&self) -> Option<&'static str> {
        Some("镜像可能被别的东西用着,不擅自删")
    }
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let runtime = arg_str_opt(args, "runtime").unwrap_or("docker");
        let (code, out) = ctx.probe(&format!("{runtime} images 2>/dev/null | tail -n +2 | wc -l"))?;
        if code != 0 {
            return Ok(Observed::default()); // 运行时都没有 —— 说不清
        }
        Ok(Observed::present([("count", out.trim().to_string())]))
    }
    fn diff(&self, input: &DiffInput) -> Change {
        if !input.observed.present {
            return Change::Unknown("目标上没有可用的容器运行时".into());
        }
        // 镜像清单来自 materials,与实际 tag 的比对要等闭包解析 —— 如实说明而非猜。
        Change::Unknown("镜像清单需在执行期与 OCI 闭包比对".into())
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        let _ = (ctx, args);
        anyhow::bail!("image_present:镜像导入需 OCI 闭包支持(尚未接入新管线)")
    }
    fn destroy(&self, _c: &dyn Ctx, _a: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        // 不删镜像:别的负载可能正用着,而且重新拉取代价高。
        Ok(Outcome::Ok)
    }
}

/// `wait` —— 等端口 / 路径达到某状态。**只读**:它改变不了任何东西。
pub struct Wait;

impl ResourceType for Wait {
    fn name(&self) -> &'static str {
        "wait"
    }

    fn retire_note(&self) -> Option<&'static str> {
        Some("等待没有逆操作")
    }
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let (code, _) = ctx.probe(&wait_probe(args))?;
        Ok(Observed::present([("satisfied", (code == 0).to_string())]))
    }
    fn diff(&self, input: &DiffInput) -> Change {
        let want_positive =
            !matches!(arg_str_opt(input.args, "state"), Some("stopped") | Some("absent"));
        let satisfied = input.observed.get("satisfied") == Some("true");
        if satisfied == want_positive {
            Change::Ok
        } else {
            Change::Update(vec![FieldDiff::set("wait", "条件尚未满足")])
        }
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        let timeout = args.get("timeout").and_then(Yaml::as_u64).unwrap_or(30);
        let delay = args.get("delay").and_then(Yaml::as_u64).unwrap_or(0);
        let want_positive = !matches!(arg_str_opt(args, "state"), Some("stopped") | Some("absent"));
        let probe = wait_probe(args);
        let neg = if want_positive { "" } else { "! " };
        let cmd = format!(
            "sleep {delay}; for _ in $(seq 1 {timeout}); do if {neg}{{ {probe} ; }} >/dev/null 2>&1; \
             then exit 0; fi; sleep 1; done; echo 'wait timed out after {timeout}s' >&2; exit 1"
        );
        run_ok(ctx, &cmd)?;
        // 等待成功不算"改变了什么" —— 它是只读的。
        Ok(Outcome::Ok)
    }
    fn destroy(&self, _c: &dyn Ctx, _a: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        Ok(Outcome::Ok)
    }
}

fn wait_probe(args: &ResolvedArgs) -> String {
    if let Some(path) = arg_str_opt(args, "path") {
        return format!("test -e {}", sh(path));
    }
    let port = args.get("port").map(crate::eval::scalar_to_string).unwrap_or_default();
    let host = arg_str_opt(args, "host").unwrap_or("127.0.0.1");
    // nc → bash /dev/tcp,覆盖主流与 busybox。
    format!(
        "if command -v nc >/dev/null 2>&1; then nc -z {host} {port}; \
         else timeout 2 bash -c 'echo > /dev/tcp/{host}/{port}'; fi"
    )
}

/// `shell` —— 逃生舱。有 `check:` 就有幂等,没有就在 plan 里显示 `?`
/// 并计入模型化欠债:**接住,不羞辱,但可见**。
pub struct Shell;

impl ResourceType for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        // `creates:` 是 `check:` 的常见特例("这个路径在就算做过了")。
        // 它一直登记在类型卡上,却从没被实现 —— 于是写了 creates 的 shell
        // 仍被判为"说不清",连带整步被跳过。文档承诺过的字段必须真的管用。
        if let Some(path) = arg_str_opt(args, "creates") {
            let (code, _) = ctx.probe(&format!("test -e {}", sh(path)))?;
            if code == 0 {
                return Ok(Observed::present([("creates", "exists".into())]));
            }
            // 路径不在:若还写了 check,继续按 check 判;否则就是"没做过"。
            if arg_str_opt(args, "check").is_none() {
                return Ok(Observed::absent());
            }
        }
        let Some(check) = arg_str_opt(args, "check") else {
            return Ok(Observed::default());
        };
        // `check:` 与 `cmd:` 共享同一套 env —— 否则 `KUBECONFIG` 只对其中一个生效,
        // 探针会因为"看不到集群"而永远判定"没做过"。
        let (code, _) = ctx.probe(&with_env(args, check))?;
        Ok(if code == 0 {
            Observed::present([("check", "satisfied".into())])
        } else {
            Observed::absent()
        })
    }
    fn diff(&self, input: &DiffInput) -> Change {
        let probe = arg_str_opt(input.args, "check").or_else(|| arg_str_opt(input.args, "creates"));
        match probe {
            None => Change::Unknown("裸 shell 没有 `check:` 或 `creates:`,无法预演".into()),
            Some(_) if input.observed.present => Change::Ok,
            Some(_) => Change::Create(vec![FieldDiff::set(
                "cmd",
                truncate(arg_str_opt(input.args, "cmd").unwrap_or_default()),
            )]),
        }
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        let cmd = arg_str(args, "cmd")?;
        let full = match arg_str_opt(args, "chdir") {
            Some(d) => format!("cd {} && {cmd}", sh(d)),
            None => cmd.to_string(),
        };
        run_ok(ctx, &with_env(args, &full))?;
        Ok(Outcome::Changed)
    }
    fn destroy(&self, _c: &dyn Ctx, _a: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        // shell 没有逆操作 —— 想要退役行为就写进 blueprint 的 procedure。
        Ok(Outcome::Warn)
    }
}

/// 给命令加上 `env:` 前缀。
///
/// 早期版本接受 `env:` 却**从未使用**它 —— 于是 `KUBECONFIG=…` 被静默丢弃,
/// blueprint 里每个 kubectl 都会失败。与 `on:` 同一类 bug:声明了却不生效。
fn with_env(args: &ResolvedArgs, cmd: &str) -> String {
    let Some(Yaml::Mapping(env)) = args.get("env") else {
        return cmd.to_string();
    };
    if env.is_empty() {
        return cmd.to_string();
    }
    let prefix: Vec<String> = env
        .iter()
        .map(|(k, v)| {
            format!(
                "{}={}",
                crate::eval::scalar_to_string(k),
                sh(&crate::eval::scalar_to_string(v))
            )
        })
        .collect();
    format!("{} {cmd}", prefix.join(" "))
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= 60 {
        return s.to_string();
    }
    format!("{}…", s.chars().take(59).collect::<String>())
}

// ---------------------------------------------------------------- 健康探针
//
// `health:` 段用的类型。全部只读:apply 就是"再探一次",destroy 无意义。

macro_rules! probe_type {
    ($name:ident, $key:literal, $build:expr) => {
        pub struct $name;
        impl ResourceType for $name {
            fn name(&self) -> &'static str {
                $key
            }
            fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
                let build: fn(&ResolvedArgs) -> String = $build;
                let (code, out) = ctx.probe(&build(args))?;
                Ok(Observed::present([
                    ("healthy", (code == 0).to_string()),
                    ("detail", out.trim().to_string()),
                ]))
            }
            fn diff(&self, input: &DiffInput) -> Change {
                if input.observed.get("healthy") == Some("true") {
                    Change::Ok
                } else {
                    Change::Update(vec![FieldDiff::set(
                        "health",
                        input.observed.get("detail").unwrap_or("探测失败"),
                    )])
                }
            }
            fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
                // 探针改变不了世界:只能再看一眼,不健康就如实失败。
                let observed = self.observe(ctx, args)?;
                if observed.get("healthy") == Some("true") {
                    Ok(Outcome::Ok)
                } else {
                    anyhow::bail!(
                        "健康探针 `{}` 未通过:{}",
                        $key,
                        observed.get("detail").unwrap_or("(无输出)")
                    )
                }
            }
            fn destroy(&self, _c: &dyn Ctx, _a: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
                Ok(Outcome::Ok)
            }
        }
    };
}

probe_type!(Http, "http", |a| {
    let url = arg_str_opt(a, "url").unwrap_or_default();
    let want = a.get("status").and_then(Yaml::as_u64).unwrap_or(200);
    format!(
        "code=$(curl -s -o /dev/null -w '%{{http_code}}' {}); echo \"$code\"; test \"$code\" = '{want}'",
        sh(url)
    )
});

probe_type!(PortOpen, "port_open", |a| {
    let port = a.get("port").map(crate::eval::scalar_to_string).unwrap_or_default();
    let host = arg_str_opt(a, "host").unwrap_or("127.0.0.1");
    format!(
        "if command -v nc >/dev/null 2>&1; then nc -z {host} {port}; \
         else timeout 2 bash -c 'echo > /dev/tcp/{host}/{port}'; fi"
    )
});

probe_type!(ServiceActive, "service_active", |a| {
    format!("systemctl is-active --quiet {}", sh(arg_str_opt(a, "name").unwrap_or_default()))
});

probe_type!(CmdProbe, "cmd", |a| { render_cmd(a) });

/// 把 `cmd` 渲染成一条可执行命令行。
///
/// 两种形态:
/// - `run: "自由字符串"` —— 只读探针位的便利写法(过 shell,可用管道);
/// - `argv: [...]` + `flags: [...]` —— **动作位的正规写法**:每个 token 独立引用,
///   直达 execve 语义,注入与引号事故根治;条件是 flag 条目的属性(见 ir::Flag)。
///
/// 关键:`flags` 里 `when` 为假的条目**根本不出现在命令行上** —— 作者不必写
/// `${cond ? "--x" : ""}` 这种空串占位,也就没有把逻辑塞进字符串的动机。
/// 条件求值发生在**解析后、渲染前**,由调用方在 resolve_args 阶段完成;
/// 到这里时 args 里的 flags 已是**筛选过**的最终列表。
pub(crate) fn render_cmd(a: &ResolvedArgs) -> String {
    if let Some(run) = arg_str_opt(a, "run") {
        return run.to_string();
    }
    let mut parts: Vec<String> = match a.get("argv") {
        Some(Yaml::Sequence(items)) => items.iter().map(|v| sh(&crate::eval::scalar_to_string(v))).collect(),
        _ => Vec::new(),
    };
    // flags 已在求值期按 when 筛选;这里只负责拼接与转义。
    if let Some(Yaml::Sequence(flags)) = a.get("flags") {
        for f in flags {
            let Some(m) = f.as_mapping() else { continue };
            if let Some(name) = m.get(Yaml::from("name")).and_then(Yaml::as_str) {
                parts.push(sh(name));
            }
            if let Some(v) = m.get(Yaml::from("value")) {
                parts.push(sh(&crate::eval::scalar_to_string(v)));
            }
        }
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::FakeCtx;

    fn args(pairs: &[(&str, Yaml)]) -> ResolvedArgs {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }
    fn diff<T: ResourceType>(t: &T, a: &ResolvedArgs, o: &Observed) -> Change {
        t.diff(&DiffInput { args: a, observed: o, upstream_changed: false })
    }

    #[test]
    fn package_picks_the_list_for_the_targets_family() {
        // 引擎只懂"怎么装";"装什么"是数据(D-017)。
        let ctx = FakeCtx::new().on("echo debian", 0, "debian\n").on("dpkg -s", 1, "");
        let a = args(&[(
            "packages",
            serde_yaml::from_str("{debian: [socat, conntrack], rhel: [socat]}").unwrap(),
        )]);
        let obs = Package.observe(&ctx, &a).unwrap();
        assert_eq!(obs.get("missing"), Some("socat,conntrack"));
        assert_eq!(diff(&Package, &a, &obs).fields()[0].to_string(), "install: socat,conntrack");
    }

    #[test]
    fn package_all_installed_is_a_noop() {
        let ctx = FakeCtx::new().on("echo debian", 0, "debian\n").on("dpkg -s", 0, "");
        let a = args(&[("packages", serde_yaml::from_str("{debian: [socat]}").unwrap())]);
        let obs = Package.observe(&ctx, &a).unwrap();
        assert_eq!(diff(&Package, &a, &obs), Change::Ok);
    }

    #[test]
    fn package_without_a_list_for_this_family_says_so() {
        let ctx = FakeCtx::new().on("echo debian", 0, "debian\n");
        let a = args(&[("packages", serde_yaml::from_str("{rhel: [socat]}").unwrap())]);
        let obs = Package.observe(&ctx, &a).unwrap();
        assert!(matches!(diff(&Package, &a, &obs), Change::Unknown(_)));
    }

    #[test]
    fn shell_with_a_check_becomes_idempotent() {
        let a = args(&[("cmd", Yaml::from("kubeadm init")), ("check", Yaml::from("test -f /x"))]);
        assert_eq!(
            diff(&Shell, &a, &Observed::present([("check", "satisfied".into())])),
            Change::Ok
        );
        assert!(matches!(diff(&Shell, &a, &Observed::absent()), Change::Create(_)));
    }

    #[test]
    fn a_bare_shell_is_unknown_and_its_command_is_truncated_in_the_plan() {
        let long = "x".repeat(200);
        let a = args(&[("cmd", Yaml::from(long.as_str()))]);
        assert!(matches!(diff(&Shell, &a, &Observed::default()), Change::Unknown(_)));
        assert!(truncate(&long).chars().count() <= 60);
    }

    #[test]
    fn shell_env_reaches_both_the_command_and_its_check() {
        // 曾经的真 bug:`env:` 被接受但从不使用,blueprint 里每个 kubectl 都会失败。
        let ctx = FakeCtx::new().on("", 0, "");
        let mut a = args(&[
            ("cmd", Yaml::from("kubectl get nodes")),
            ("check", Yaml::from("kubectl get ds/x")),
        ]);
        a.insert(
            "env".into(),
            serde_yaml::from_str("{KUBECONFIG: /etc/kubernetes/admin.conf}").unwrap(),
        );
        Shell.observe(&ctx, &a).unwrap();
        Shell.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        assert_eq!(cmds.len(), 2);
        for c in &cmds {
            assert!(
                c.starts_with("KUBECONFIG='/etc/kubernetes/admin.conf' kubectl"),
                "探针与命令都要带上 env:{c}"
            );
        }
    }

    #[test]
    fn shell_without_env_is_left_untouched() {
        let ctx = FakeCtx::new().on("", 0, "");
        let a = args(&[("cmd", Yaml::from("echo hi"))]);
        Shell.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        assert_eq!(ctx.calls()[0].text(), "echo hi");
    }

    #[test]
    fn shell_destroy_warns_because_there_is_no_inverse() {
        let ctx = FakeCtx::new();
        assert_eq!(
            Shell.destroy(&ctx, &args(&[]), &Observed::present([])).unwrap(),
            Outcome::Warn
        );
    }

    #[test]
    fn wait_is_read_only_even_when_it_succeeds() {
        let ctx = FakeCtx::new().on("", 0, "");
        let a = args(&[("port", Yaml::from(9000))]);
        let obs = Wait.observe(&ctx, &a).unwrap();
        assert_eq!(diff(&Wait, &a, &obs), Change::Ok);
        // 即便执行,也报 ok 而不是 changed:它没改变任何东西。
        assert_eq!(Wait.apply(&ctx, &a, &Change::Ok).unwrap(), Outcome::Ok);
    }

    #[test]
    fn wait_stopped_inverts_the_condition() {
        let a = args(&[("port", Yaml::from(9000)), ("state", Yaml::from("stopped"))]);
        let obs = Observed::present([("satisfied", "true".into())]);
        assert!(!diff(&Wait, &a, &obs).is_noop(), "端口开着但期望关着 → 要等");
    }

    #[test]
    fn wait_probe_falls_back_from_nc_to_dev_tcp() {
        let a = args(&[("port", Yaml::from(9000))]);
        let probe = wait_probe(&a);
        assert!(probe.contains("nc -z") && probe.contains("/dev/tcp"), "{probe}");
    }

    #[test]
    fn health_probes_fail_loudly_rather_than_reporting_ok() {
        let ctx = FakeCtx::new().on("systemctl is-active", 1, "");
        let a = args(&[("name", Yaml::from("rustfs"))]);
        let obs = ServiceActive.observe(&ctx, &a).unwrap();
        assert!(!diff(&ServiceActive, &a, &obs).is_noop());
        assert!(ServiceActive.apply(&ctx, &a, &Change::Ok).is_err());
    }

    #[test]
    fn http_probe_compares_the_status_code() {
        let a = args(&[("url", Yaml::from("http://x/health")), ("status", Yaml::from(200))]);
        let ctx = FakeCtx::new().on("curl", 0, "200\n");
        let obs = Http.observe(&ctx, &a).unwrap();
        assert_eq!(diff(&Http, &a, &obs), Change::Ok);
        assert!(ctx.calls()[0].text().contains("'200'"), "{:?}", ctx.calls());
    }

    #[test]
    fn structured_cmd_renders_argv_and_flags_with_each_token_quoted() {
        // 每个 token 独立转义 —— 带空格的值不会把命令拆散,注入无从下手。
        let a: ResolvedArgs = serde_yaml::from_str(
            "argv: [kubeadm, init]\nflags:\n  - {name: --pod-network-cidr, value: 10.244.0.0/16}\n  - {name: --upload-certs}\n",
        )
        .unwrap();
        assert_eq!(
            render_cmd(&a),
            "'kubeadm' 'init' '--pod-network-cidr' '10.244.0.0/16' '--upload-certs'"
        );
    }

    #[test]
    fn a_flag_filtered_out_upstream_simply_is_not_there() {
        // 这是"没有地方写三元"的另一半:条件为假的 flag **根本不出现**,
        // 作者不需要用空字符串占位,也就没有把逻辑塞进字符串的动机。
        let a: ResolvedArgs =
            serde_yaml::from_str("argv: [kubeadm, init]\nflags: [{name: --pod-network-cidr, value: x}]\n")
                .unwrap();
        let rendered = render_cmd(&a);
        assert!(!rendered.contains("upload-certs"), "{rendered}");
        assert!(!rendered.contains("''"), "不该留下空串占位:{rendered}");
    }

    #[test]
    fn a_value_with_spaces_survives_as_one_token() {
        let a: ResolvedArgs =
            serde_yaml::from_str("argv: [echo]\nflags: [{name: --msg, value: \"hello world\"}]\n")
                .unwrap();
        assert_eq!(render_cmd(&a), "'echo' '--msg' 'hello world'");
    }

    #[test]
    fn the_free_form_run_shorthand_still_works_for_probes() {
        // 只读探针位仍可写自由字符串(要管道):限权针对的是**值里的逻辑**,
        // 不是禁止一切 shell。
        let a: ResolvedArgs = serde_yaml::from_str("run: \"kubectl get nodes | wc -l\"\n").unwrap();
        assert_eq!(render_cmd(&a), "kubectl get nodes | wc -l");
    }

    #[test]
    fn image_present_refuses_to_pretend_without_a_closure() {
        let ctx = FakeCtx::new();
        let a = args(&[("material", Yaml::from("img-apiserver"))]);
        assert!(ImagePresent.apply(&ctx, &a, &Change::Ok).is_err());
    }
}
