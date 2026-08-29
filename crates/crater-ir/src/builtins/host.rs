//! 主机基线类内建:`hostname` / `swap` / `kernel_modules` / `sysctl` / `user` / `group`。
//!
//! 这一批全是**试金石逼出来的**(k8s 裁定 E):旧写法里它们各占 2 个 shell 步骤
//! 加一条手写 `check:`(`swapoff -a` + 改 fstab、`modprobe` + `/etc/modules-load.d/`、
//! `sysctl --system` + 探 `ip_forward`)。类型化之后,幂等与退役都是免费的。

use anyhow::Result;

use crate::builtins::file::run_ok;
use crate::eval::{ResolvedArgs, Yaml};
use crate::verbs::*;

/// 内部分隔符:值里不可能出现,用它拼多项比空格安全。
const SEP: &str = "\u{1}";

/// `hostname` —— 目标主机名。kubeadm 之类按 OS hostname 认节点,所以它是状态不是命令。
pub struct Hostname;

impl ResourceType for Hostname {
    fn name(&self) -> &'static str {
        "hostname"
    }
    fn observe(&self, ctx: &dyn Ctx, _args: &ResolvedArgs) -> Result<Observed> {
        let (_, out) = ctx.probe("hostname")?;
        Ok(Observed::present([("name", out.trim().to_string())]))
    }
    fn diff(&self, input: &DiffInput) -> Change {
        let want = arg_str_opt(input.args, "name").unwrap_or_default();
        match input.observed.get("name") {
            Some(cur) if cur == want => Change::Ok,
            Some(cur) => Change::Update(vec![FieldDiff::change("name", cur, want)]),
            None => Change::Update(vec![FieldDiff::set("name", want)]),
        }
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        run_ok(ctx, &format!("hostnamectl set-hostname {}", sh(arg_str(args, "name")?)))?;
        Ok(Outcome::Changed)
    }
    fn destroy(&self, _ctx: &dyn Ctx, _args: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        // 改回什么?没有"原来的名字"可言 —— 退役时不动主机名是唯一安全的选择。
        Ok(Outcome::Ok)
    }
}

/// `swap` —— k8s 的老朋友:关掉,并且**重启后仍然关着**。
pub struct Swap;

impl ResourceType for Swap {
    fn name(&self) -> &'static str {
        "swap"
    }
    fn observe(&self, ctx: &dyn Ctx, _args: &ResolvedArgs) -> Result<Observed> {
        let (_, active) = ctx.probe("swapon --show --noheadings 2>/dev/null | head -1")?;
        let (fstab, _) = ctx.probe("grep -Eq '^[^#].*[[:space:]]swap[[:space:]]' /etc/fstab")?;
        Ok(Observed::present([
            ("active", (!active.trim().is_empty()).to_string()),
            // fstab 里还有未注释的 swap 行 ⇒ 重启会自己回来。
            ("persisted", (fstab == 0).to_string()),
        ]))
    }
    fn diff(&self, input: &DiffInput) -> Change {
        let want_off = arg_str_opt(input.args, "state").unwrap_or("disabled") == "disabled";
        let persist = arg_bool(input.args, "persist").unwrap_or(true);
        let active = input.observed.get("active") == Some("true");
        let in_fstab = input.observed.get("persisted") == Some("true");

        let mut fields = Vec::new();
        if want_off && active {
            fields.push(FieldDiff::change("active", "true", "false"));
        }
        if !want_off && !active {
            fields.push(FieldDiff::change("active", "false", "true"));
        }
        // 只关不改 fstab 是最常见的"重启后失效"陷阱 —— 类型化后它是可见的一项。
        if want_off && persist && in_fstab {
            fields.push(FieldDiff::change("fstab", "有 swap 条目", "注释掉"));
        }
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        let off = arg_str_opt(args, "state").unwrap_or("disabled") == "disabled";
        if off {
            run_ok(ctx, "swapoff -a")?;
            if arg_bool(args, "persist").unwrap_or(true) {
                run_ok(ctx, r"sed -ri '/\sswap\s/s/^([^#])/#\1/' /etc/fstab")?;
            }
        } else {
            run_ok(ctx, "swapon -a")?;
        }
        Ok(Outcome::Changed)
    }
    fn destroy(&self, _ctx: &dyn Ctx, _args: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        // 不擅自把 swap 打开:目标机原来什么样我们并不知道。
        Ok(Outcome::Ok)
    }
}

/// `kernel_modules` —— 加载内核模块,并可持久化到 `/etc/modules-load.d/`。
pub struct KernelModules;

const MODULES_CONF: &str = "/etc/modules-load.d/crater.conf";

impl ResourceType for KernelModules {
    fn name(&self) -> &'static str {
        "kernel_modules"
    }
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let mods = list_of(args, "load");
        let (_, out) = ctx.probe("lsmod | awk 'NR>1 {print $1}'")?;
        let loaded: Vec<&str> = out.lines().map(str::trim).collect();
        let missing: Vec<String> = mods
            .iter()
            .filter(|m| !loaded.contains(&m.as_str()))
            .cloned()
            .collect();
        let (persisted, _) = ctx.probe(&format!("test -f {}", sh(MODULES_CONF)))?;
        Ok(Observed::present([
            ("missing", missing.join(",")),
            ("persisted", (persisted == 0).to_string()),
        ]))
    }
    fn diff(&self, input: &DiffInput) -> Change {
        let missing = input.observed.get("missing").unwrap_or_default();
        let persist = arg_bool(input.args, "persist").unwrap_or(true);
        let persisted = input.observed.get("persisted") == Some("true");
        let mut fields = Vec::new();
        if !missing.is_empty() {
            fields.push(FieldDiff::set("load", missing));
        }
        if persist && !persisted {
            fields.push(FieldDiff::set("persist", MODULES_CONF));
        }
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        let mods = list_of(args, "load");
        for m in &mods {
            run_ok(ctx, &format!("modprobe {}", sh(m)))?;
        }
        if arg_bool(args, "persist").unwrap_or(true) {
            ctx.write_file(MODULES_CONF, &format!("{}\n", mods.join("\n")))?;
        }
        Ok(Outcome::Changed)
    }
    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        // 只撤掉持久化;不 rmmod —— 别的东西可能正用着这些模块。
        if arg_bool(args, "persist").unwrap_or(true) {
            run_ok(ctx, &format!("rm -f {}", sh(MODULES_CONF)))?;
            return Ok(Outcome::Changed);
        }
        Ok(Outcome::Ok)
    }
}

/// `sysctl` —— 内核参数。`set:` 直接给键值,或 `from_material:` 给一份配置文件。
pub struct Sysctl;

const SYSCTL_CONF: &str = "/etc/sysctl.d/99-crater.conf";

impl ResourceType for Sysctl {
    fn name(&self) -> &'static str {
        "sysctl"
    }
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let mut wrong = Vec::new();
        for (k, want) in pairs_of(args, "set") {
            let (code, out) = ctx.probe(&format!("sysctl -n {} 2>/dev/null", sh(&k)))?;
            // 探针失败 = 这个键在本机不存在(比如 br_netfilter 还没加载)。
            // 早期版本把 stderr 当成了"当前值",于是错误文本被塞进 diff 里刷屏。
            let current = if code == 0 { out.trim() } else { "(未设置)" };
            if current != want {
                wrong.push(format!("{k}: {current} → {want}"));
            }
        }
        // 用不可能出现在值里的分隔符,而不是空格 —— 值本身可以带空格。
        Ok(Observed::present([("wrong", wrong.join(SEP))]))
    }
    fn diff(&self, input: &DiffInput) -> Change {
        if arg_str_opt(input.args, "from_material").is_some() {
            // 文件内容要到执行期才知道 —— 不假装能预演。
            return if input.upstream_changed {
                Change::Update(vec![FieldDiff::set("from_material", "(重新应用)")])
            } else {
                Change::Unknown("`from_material` 的内容需在执行期才能与现实比对".into())
            };
        }
        let wrong = input.observed.get("wrong").unwrap_or_default();
        if wrong.is_empty() {
            Change::Ok
        } else {
            Change::Update(wrong.split(SEP).map(|w| FieldDiff::set("sysctl", w)).collect())
        }
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        if let Some(m) = arg_str_opt(args, "from_material") {
            ctx.place_material(m, SYSCTL_CONF)?;
        } else {
            let body: String = pairs_of(args, "set")
                .iter()
                .map(|(k, v)| format!("{k} = {v}\n"))
                .collect();
            ctx.write_file(SYSCTL_CONF, &body)?;
        }
        run_ok(ctx, "sysctl --system")?;
        Ok(Outcome::Changed)
    }
    fn destroy(&self, ctx: &dyn Ctx, _args: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        run_ok(ctx, &format!("rm -f {}", sh(SYSCTL_CONF)))?;
        // 不回滚运行中的值:重启即恢复,强行改回去可能打断正在跑的东西。
        Ok(Outcome::Changed)
    }
}

/// `user` —— 系统用户。
pub struct User;

impl ResourceType for User {
    fn name(&self) -> &'static str {
        "user"
    }
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let name = arg_str(args, "name")?;
        let (code, out) = ctx.probe(&format!(
            "getent passwd {} | cut -d: -f6,7 --output-delimiter='\u{1}'",
            sh(name)
        ))?;
        if code != 0 || out.trim().is_empty() {
            return Ok(Observed::absent());
        }
        let f: Vec<&str> = out.trim().split('\u{1}').collect();
        Ok(Observed::present([
            ("home", f.first().copied().unwrap_or_default().to_string()),
            ("shell", f.get(1).copied().unwrap_or_default().to_string()),
        ]))
    }
    fn diff(&self, input: &DiffInput) -> Change {
        presence_diff(input, &["home", "shell"])
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, change: &Change) -> Result<Outcome> {
        let name = arg_str(args, "name")?;
        if matches!(change, Change::Destroy) {
            return self.destroy(ctx, args, &Observed::present([]));
        }
        let mut flags = String::new();
        if arg_bool(args, "system").unwrap_or(false) {
            flags.push_str(" --system");
        }
        for (k, flag) in [("shell", "--shell"), ("home", "--home")] {
            if let Some(v) = arg_str_opt(args, k) {
                flags.push_str(&format!(" {flag} {}", sh(v)));
            }
        }
        let groups = list_of(args, "groups");
        if !groups.is_empty() {
            flags.push_str(&format!(" --groups {}", sh(&groups.join(","))));
        }
        let verb = if matches!(change, Change::Create(_)) { "useradd" } else { "usermod" };
        run_ok(ctx, &format!("{verb}{flags} {}", sh(name)))?;
        Ok(Outcome::Changed)
    }
    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        run_ok(ctx, &format!("userdel {}", sh(arg_str(args, "name")?)))?;
        Ok(Outcome::Changed)
    }
}

/// `group` —— 系统组。
pub struct Group;

impl ResourceType for Group {
    fn name(&self) -> &'static str {
        "group"
    }
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let (code, out) = ctx.probe(&format!("getent group {}", sh(arg_str(args, "name")?)))?;
        Ok(if code == 0 && !out.trim().is_empty() {
            Observed::present([])
        } else {
            Observed::absent()
        })
    }
    fn diff(&self, input: &DiffInput) -> Change {
        presence_diff(input, &[])
    }
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, change: &Change) -> Result<Outcome> {
        if matches!(change, Change::Destroy) {
            return self.destroy(ctx, args, &Observed::present([]));
        }
        let sys = if arg_bool(args, "system").unwrap_or(false) { " --system" } else { "" };
        run_ok(ctx, &format!("groupadd -f{sys} {}", sh(arg_str(args, "name")?)))?;
        Ok(Outcome::Changed)
    }
    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        run_ok(ctx, &format!("groupdel {}", sh(arg_str(args, "name")?)))?;
        Ok(Outcome::Changed)
    }
}

/// present/absent 型资源的共用 diff。
fn presence_diff(input: &DiffInput, compare: &[&str]) -> Change {
    let want_present = arg_str_opt(input.args, "state").unwrap_or("present") == "present";
    match (want_present, input.observed.present) {
        (true, false) => Change::Create(vec![FieldDiff::set(
            "name",
            arg_str_opt(input.args, "name").unwrap_or_default(),
        )]),
        (false, true) => Change::Destroy,
        (false, false) => Change::Ok,
        (true, true) => {
            let fields: Vec<FieldDiff> = compare
                .iter()
                .filter_map(|k| {
                    let want = arg_str_opt(input.args, k)?;
                    let have = input.observed.get(k)?;
                    (want != have).then(|| FieldDiff::change(k, have, want))
                })
                .collect();
            if fields.is_empty() {
                Change::Ok
            } else {
                Change::Update(fields)
            }
        }
    }
}

fn list_of(args: &ResolvedArgs, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Yaml::Sequence(items)) => items.iter().map(crate::eval::scalar_to_string).collect(),
        Some(Yaml::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

fn pairs_of(args: &ResolvedArgs, key: &str) -> Vec<(String, String)> {
    match args.get(key) {
        Some(Yaml::Mapping(m)) => m
            .iter()
            .map(|(k, v)| (crate::eval::scalar_to_string(k), crate::eval::scalar_to_string(v)))
            .collect(),
        _ => Vec::new(),
    }
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
    fn swap_catches_the_classic_reboot_trap() {
        // 只 `swapoff -a` 不改 fstab —— 重启后 swap 自己回来,k8s 再次拒绝启动。
        // 类型化之后这一项在 plan 里是**看得见**的。
        let a = args(&[("state", Yaml::from("disabled")), ("persist", Yaml::from(true))]);
        let obs = Observed::present([("active", "false".into()), ("persisted", "true".into())]);
        let c = diff(&Swap, &a, &obs);
        assert_eq!(c.fields()[0].to_string(), "fstab: 有 swap 条目 → 注释掉");
    }

    #[test]
    fn swap_already_off_and_persisted_is_a_noop() {
        let a = args(&[("state", Yaml::from("disabled"))]);
        let obs = Observed::present([("active", "false".into()), ("persisted", "false".into())]);
        assert_eq!(diff(&Swap, &a, &obs), Change::Ok);
    }

    #[test]
    fn swap_destroy_does_not_turn_swap_back_on() {
        // 目标机原来什么样我们并不知道 —— 擅自开回来是越权。
        let ctx = FakeCtx::new();
        assert_eq!(
            Swap.destroy(&ctx, &args(&[]), &Observed::present([])).unwrap(),
            Outcome::Ok
        );
        assert!(ctx.calls().is_empty());
    }

    #[test]
    fn kernel_modules_reports_only_the_missing_ones() {
        let ctx = FakeCtx::new()
            .on("lsmod", 0, "overlay\nbr_netfilter\next4\n")
            .on("test -f", 0, "");
        let a = args(&[(
            "load",
            serde_yaml::from_str("[overlay, br_netfilter, nf_conntrack]").unwrap(),
        )]);
        let obs = KernelModules.observe(&ctx, &a).unwrap();
        assert_eq!(obs.get("missing"), Some("nf_conntrack"));
        let c = diff(&KernelModules, &a, &obs);
        assert_eq!(c.fields()[0].to_string(), "load: nf_conntrack");
    }

    #[test]
    fn kernel_modules_destroy_does_not_rmmod() {
        // 别的东西可能正用着这些模块。
        let ctx = FakeCtx::new().on("rm", 0, "");
        KernelModules
            .destroy(&ctx, &args(&[]), &Observed::present([]))
            .unwrap();
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(!cmds.iter().any(|c| c.contains("rmmod")), "{cmds:?}");
        assert!(cmds[0].contains("modules-load.d"), "{cmds:?}");
    }

    #[test]
    fn sysctl_reports_each_wrong_key_with_both_values() {
        let ctx = FakeCtx::new()
            .on("sysctl -n 'net.ipv4.ip_forward'", 0, "0\n")
            .on("sysctl -n", 0, "1\n");
        let a = args(&[(
            "set",
            serde_yaml::from_str("{net.ipv4.ip_forward: 1, net.bridge.bridge-nf-call-iptables: 1}")
                .unwrap(),
        )]);
        let obs = Sysctl.observe(&ctx, &a).unwrap();
        assert_eq!(obs.get("wrong"), Some("net.ipv4.ip_forward: 0 → 1"));
        assert!(!diff(&Sysctl, &a, &obs).is_noop());
    }

    #[test]
    fn a_kernel_key_that_does_not_exist_yet_reads_as_unset_not_as_an_error_message() {
        // br_netfilter 尚未加载时 `sysctl -n` 会往 stderr 吐一段话。
        // 早期版本把它当成"当前值",于是 diff 里刷出一堆碎片。
        let ctx = FakeCtx::new().on("sysctl -n", 1, "cannot stat /proc/sys/...: No such file");
        let a = args(&[(
            "set",
            serde_yaml::from_str("{net.bridge.bridge-nf-call-iptables: 1}").unwrap(),
        )]);
        let obs = Sysctl.observe(&ctx, &a).unwrap();
        assert_eq!(obs.get("wrong"), Some("net.bridge.bridge-nf-call-iptables: (未设置) → 1"));
        let c = diff(&Sysctl, &a, &obs);
        assert_eq!(c.fields().len(), 1, "一个键就该是一项:{:?}", c.fields());
    }

    #[test]
    fn multiple_wrong_keys_stay_one_line_each() {
        let ctx = FakeCtx::new().on("sysctl -n", 0, "0\n");
        let a = args(&[(
            "set",
            serde_yaml::from_str("{a.b: 1, c.d: 1, e.f: 1}").unwrap(),
        )]);
        let obs = Sysctl.observe(&ctx, &a).unwrap();
        assert_eq!(diff(&Sysctl, &a, &obs).fields().len(), 3);
    }

    #[test]
    fn sysctl_all_correct_is_a_noop() {
        let ctx = FakeCtx::new().on("sysctl -n", 0, "1\n");
        let a = args(&[("set", serde_yaml::from_str("{net.ipv4.ip_forward: 1}").unwrap())]);
        let obs = Sysctl.observe(&ctx, &a).unwrap();
        assert_eq!(diff(&Sysctl, &a, &obs), Change::Ok);
    }

    #[test]
    fn hostname_compares_against_reality() {
        let a = args(&[("name", Yaml::from("n11"))]);
        assert_eq!(diff(&Hostname, &a, &Observed::present([("name", "n11".into())])), Change::Ok);
        let c = diff(&Hostname, &a, &Observed::present([("name", "ubuntu".into())]));
        assert_eq!(c.fields()[0].to_string(), "name: ubuntu → n11");
    }

    #[test]
    fn hostname_destroy_leaves_the_machine_alone() {
        let ctx = FakeCtx::new();
        Hostname.destroy(&ctx, &args(&[]), &Observed::present([])).unwrap();
        assert!(ctx.calls().is_empty(), "没有'原来的名字'可以改回去");
    }

    #[test]
    fn user_create_update_and_absent_are_all_expressible() {
        let a = args(&[("name", Yaml::from("app")), ("shell", Yaml::from("/bin/bash"))]);
        assert!(matches!(diff(&User, &a, &Observed::absent()), Change::Create(_)));
        assert_eq!(
            diff(&User, &a, &Observed::present([("shell", "/bin/bash".into())])),
            Change::Ok
        );
        let c = diff(&User, &a, &Observed::present([("shell", "/usr/sbin/nologin".into())]));
        assert_eq!(c.fields()[0].to_string(), "shell: /usr/sbin/nologin → /bin/bash");

        let absent = args(&[("name", Yaml::from("app")), ("state", Yaml::from("absent"))]);
        assert_eq!(diff(&User, &absent, &Observed::present([])), Change::Destroy);
        assert_eq!(diff(&User, &absent, &Observed::absent()), Change::Ok);
    }

    #[test]
    fn group_probes_getent_and_is_idempotent() {
        let ctx = FakeCtx::new().on("getent group", 0, "docker:x:999:\n");
        let a = args(&[("name", Yaml::from("docker"))]);
        let obs = Group.observe(&ctx, &a).unwrap();
        assert!(obs.present);
        assert_eq!(diff(&Group, &a, &obs), Change::Ok);
    }
}

// ============================================================ 时区与时钟

/// `timezone` —— 系统时区。
///
/// 时区只影响**显示**,不影响时间本身。但机群时区不一致会让"跨机对日志"变成
/// 苦差事:同一件事在三台机器上写着三个时刻,排查时先得在脑子里做减法。
/// 所以它值得是期望态,而不是一条 `timedatectl set-timezone`。
pub struct Timezone;

impl ResourceType for Timezone {
    fn name(&self) -> &'static str {
        "timezone"
    }

    fn observe(&self, ctx: &dyn Ctx, _args: &ResolvedArgs) -> Result<Observed> {
        let (code, out) = ctx.probe("timedatectl show -p Timezone --value")?;
        if code != 0 {
            return Ok(Observed::absent());
        }
        Ok(Observed::present([("name", out.trim().to_string())]))
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let want = arg_str_opt(input.args, "name").unwrap_or_default();
        match input.observed.get("name") {
            Some(cur) if cur == want => Change::Ok,
            Some(cur) => Change::Update(vec![FieldDiff::change("name", cur, want)]),
            None => Change::Update(vec![FieldDiff::set("name", want)]),
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        run_ok(ctx, &format!("timedatectl set-timezone {}", sh(arg_str(args, "name")?)))?;
        Ok(Outcome::Changed)
    }

    fn destroy(&self, _ctx: &dyn Ctx, _args: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        // 与 hostname 同理:没有"原来的时区"可以改回去。
        Ok(Outcome::Ok)
    }
}

/// systemd-timesyncd 的配置落点。用 drop-in 而不是改主配置 ——
/// 主配置属于发行版,发行版升级时会与我们的改动打架。
const TIMESYNC_DROPIN: &str = "/etc/systemd/timesyncd.conf.d/99-crater.conf";

/// `time_sync` —— 时钟同步。
///
/// **"启用了同步"和"已经同步上"是两件事**,这里分别观察 `NTP` 与
/// `NTPSynchronized`。只看前者会让一台刚开机、时钟还差几百毫秒的机器显示为绿灯
/// —— 而 etcd 的仲裁和证书有效期恰恰吃这几百毫秒。跨主机比时间戳的一切判据,
/// 精度下限都是它(见 library/selftest 的实测:未收敛的虚机偏差 124ms)。
pub struct TimeSync;

impl ResourceType for TimeSync {
    fn name(&self) -> &'static str {
        "time_sync"
    }

    fn observe(&self, ctx: &dyn Ctx, _args: &ResolvedArgs) -> Result<Observed> {
        let cmd = format!(
            "timedatectl show -p NTP --value; printf '{SEP}'; \
             timedatectl show -p NTPSynchronized --value; printf '{SEP}'; \
             cat {} 2>/dev/null | grep -i '^NTP=' | head -1 | cut -d= -f2-",
            sh(TIMESYNC_DROPIN)
        );
        let (_, out) = ctx.probe(&cmd)?;
        let p: Vec<&str> = out.split(SEP).collect();
        Ok(Observed::present([
            ("enabled", p.first().unwrap_or(&"").trim().to_string()),
            ("synced", p.get(1).unwrap_or(&"").trim().to_string()),
            ("servers", p.get(2).unwrap_or(&"").trim().to_string()),
        ]))
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let want_on = arg_bool(input.args, "enabled").unwrap_or(true);
        let want_servers = servers_of(input.args);
        let obs = input.observed;
        let is_on = obs.get("enabled") == Some("yes");
        let synced = obs.get("synced") == Some("yes");
        let have_servers = obs.get("servers").unwrap_or_default();

        let mut fields = Vec::new();
        if want_on != is_on {
            fields.push(FieldDiff::change("enabled", is_on.to_string(), want_on.to_string()));
        }
        if !want_servers.is_empty() && have_servers != want_servers {
            fields.push(FieldDiff::change(
                "servers",
                if have_servers.is_empty() { "(系统默认)" } else { have_servers },
                &want_servers,
            ));
        }
        // 已启用但**还没同步上**:这不是"配置不对",是"还没到位"。
        // 报出来而不是当绿灯 —— 否则跨主机计时的判据会建立在流沙上。
        if want_on && is_on && !synced && fields.is_empty() {
            return Change::Update(vec![FieldDiff::change("synced", "no", "yes(等待收敛)")]);
        }
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        let want_on = arg_bool(args, "enabled").unwrap_or(true);
        let servers = servers_of(args);

        if !servers.is_empty() {
            run_ok(ctx, &format!("mkdir -p \"$(dirname {})\"", sh(TIMESYNC_DROPIN)))?;
            ctx.write_file(TIMESYNC_DROPIN, &format!("[Time]\nNTP={servers}\n"))?;
        }
        run_ok(ctx, &format!("timedatectl set-ntp {}", if want_on { "true" } else { "false" }))?;
        if want_on {
            // 改了服务器就得重启守护进程,否则新配置要等下一次轮换才生效。
            // 刻意**不**校验退出码:有的系统用 chrony 而非 timesyncd,那里这条
            // 命令本就该失败,而 `set-ntp true` 已经把同步打开了。
            let _ = ctx.run("systemctl restart systemd-timesyncd 2>/dev/null || true")?;
        }

        if want_on && arg_bool(args, "wait").unwrap_or(false) {
            // 等**真的同步上**。不等的话,紧随其后的 kubeadm 可能拿着偏了几百毫秒
            // 的时钟去签证书 —— 那种失败发生在很久以后,且极难归因。
            let (code, _) = ctx.run(
                "for i in $(seq 1 60); do \
                   [ \"$(timedatectl show -p NTPSynchronized --value)\" = yes ] && exit 0; \
                   sleep 1; done; exit 1",
            )?;
            if code != 0 {
                // 软失败:同步不上通常是网络,不该让整个部署停摆,但必须留痕。
                return Ok(Outcome::Warn);
            }
        }
        Ok(Outcome::Changed)
    }

    fn destroy(&self, _ctx: &dyn Ctx, _args: &ResolvedArgs, _o: &Observed) -> Result<Outcome> {
        // 退役时不关时钟同步 —— 那对这台机器上**其它**东西是纯粹的伤害。
        Ok(Outcome::Ok)
    }
}

/// `servers:` 归一成 timesyncd 认的空格分隔串。
fn servers_of(args: &ResolvedArgs) -> String {
    match args.get("servers") {
        Some(crate::eval::Yaml::Sequence(items)) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect::<Vec<_>>()
            .join(" "),
        Some(one) => one.as_str().unwrap_or_default().to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod time_tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::eval::Yaml;

    fn args(pairs: &[(&str, &str)]) -> ResolvedArgs {
        pairs.iter().map(|(k, v)| (k.to_string(), Yaml::from(*v))).collect()
    }
    fn diff_of(t: &dyn ResourceType, a: &ResolvedArgs, o: &Observed) -> Change {
        t.diff(&DiffInput { args: a, observed: o, upstream_changed: false })
    }
    /// timedatectl 的三段应答:NTP / NTPSynchronized / drop-in 里的 servers
    fn ts(ntp: &str, synced: &str, servers: &str) -> FakeCtx {
        FakeCtx::new().on("timedatectl show -p NTP", 0, &format!("{ntp}{SEP}{synced}{SEP}{servers}"))
    }

    // ---------------------------------------------------------- timezone

    #[test]
    fn a_matching_timezone_is_a_noop() {
        let ctx = FakeCtx::new().on("timedatectl show -p Timezone", 0, "Asia/Shanghai\n");
        let a = args(&[("name", "Asia/Shanghai")]);
        let obs = Timezone.observe(&ctx, &a).unwrap();
        assert_eq!(diff_of(&Timezone, &a, &obs), Change::Ok);
    }

    #[test]
    fn a_different_timezone_shows_both_sides() {
        let ctx = FakeCtx::new().on("timedatectl show -p Timezone", 0, "UTC\n");
        let a = args(&[("name", "Asia/Shanghai")]);
        let obs = Timezone.observe(&ctx, &a).unwrap();
        match diff_of(&Timezone, &a, &obs) {
            Change::Update(f) => assert_eq!(f[0].to_string(), "name: UTC → Asia/Shanghai"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn observing_a_timezone_writes_nothing() {
        let ctx = FakeCtx::new().on("timedatectl show -p Timezone", 0, "UTC\n");
        Timezone.observe(&ctx, &args(&[("name", "UTC")])).unwrap();
        assert!(ctx.writes().is_empty(), "{:?}", ctx.writes());
    }

    #[test]
    fn destroying_a_timezone_changes_nothing() {
        // 没有"原来的时区"可以改回去 —— 与 hostname 同理。
        let ctx = FakeCtx::new();
        assert_eq!(Timezone.destroy(&ctx, &args(&[("name", "UTC")]), &Observed::absent()).unwrap(), Outcome::Ok);
        assert!(ctx.calls().is_empty());
    }

    // ---------------------------------------------------------- time_sync

    #[test]
    fn enabled_but_not_yet_synced_is_not_a_green_light() {
        // 这是本类型存在的**全部理由**:一台刚开机、时钟还差几百毫秒的机器,
        // 只看 NTP=yes 会显示绿灯 —— 而 etcd 仲裁与证书有效期恰恰吃这几百毫秒。
        let ctx = ts("yes", "no", "");
        let a = ResolvedArgs::new();
        let obs = TimeSync.observe(&ctx, &a).unwrap();
        match diff_of(&TimeSync, &a, &obs) {
            Change::Update(f) => assert!(f[0].to_string().contains("synced"), "{f:?}"),
            other => panic!("已启用但没同步上,不该是 {other:?}"),
        }
    }

    #[test]
    fn enabled_and_synced_is_a_noop() {
        let ctx = ts("yes", "yes", "");
        let a = ResolvedArgs::new();
        let obs = TimeSync.observe(&ctx, &a).unwrap();
        assert_eq!(diff_of(&TimeSync, &a, &obs), Change::Ok);
    }

    #[test]
    fn a_disabled_clock_is_reported_as_such() {
        let ctx = ts("no", "no", "");
        let a = ResolvedArgs::new();
        let obs = TimeSync.observe(&ctx, &a).unwrap();
        match diff_of(&TimeSync, &a, &obs) {
            Change::Update(f) => assert_eq!(f[0].to_string(), "enabled: false → true"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn changed_servers_are_detected() {
        let ctx = ts("yes", "yes", "ntp.ubuntu.com");
        let mut a = ResolvedArgs::new();
        a.insert("servers".into(), serde_yaml::from_str("[ntp.aliyun.com, cn.pool.ntp.org]").unwrap());
        let obs = TimeSync.observe(&ctx, &a).unwrap();
        match diff_of(&TimeSync, &a, &obs) {
            Change::Update(f) => assert_eq!(
                f[0].to_string(),
                "servers: ntp.ubuntu.com → ntp.aliyun.com cn.pool.ntp.org"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn matching_servers_are_a_noop_so_it_does_not_rewrite_every_run() {
        let ctx = ts("yes", "yes", "ntp.aliyun.com cn.pool.ntp.org");
        let mut a = ResolvedArgs::new();
        a.insert("servers".into(), serde_yaml::from_str("[ntp.aliyun.com, cn.pool.ntp.org]").unwrap());
        let obs = TimeSync.observe(&ctx, &a).unwrap();
        assert_eq!(diff_of(&TimeSync, &a, &obs), Change::Ok);
    }

    #[test]
    fn servers_go_into_a_dropin_not_the_distro_config() {
        // 改发行版的主配置会在系统升级时打架;drop-in 才是可维护的落点。
        let ctx = FakeCtx::new().on("", 0, "");
        let mut a = ResolvedArgs::new();
        a.insert("servers".into(), serde_yaml::from_str("[ntp.aliyun.com]").unwrap());
        TimeSync.apply(&ctx, &a, &Change::Update(vec![])).unwrap();
        let body = ctx.written_file(TIMESYNC_DROPIN).expect("应写 drop-in");
        assert_eq!(body, "[Time]\nNTP=ntp.aliyun.com\n");
        assert!(ctx.written_file("/etc/systemd/timesyncd.conf").is_none(), "不该动主配置");
    }

    #[test]
    fn waiting_for_convergence_that_never_comes_is_a_warn_not_a_hard_failure() {
        // 同步不上通常是网络,不该让整个部署停摆 —— 但必须留痕。
        let ctx = FakeCtx::new().on("timedatectl set-ntp", 0, "").on("seq 1 60", 1, "");
        let mut a = ResolvedArgs::new();
        a.insert("wait".into(), Yaml::from(true));
        assert_eq!(TimeSync.apply(&ctx, &a, &Change::Update(vec![])).unwrap(), Outcome::Warn);
    }

    #[test]
    fn destroying_time_sync_leaves_the_clock_alone() {
        // 关掉同步对这台机器上**其它**东西是纯粹的伤害。
        let ctx = FakeCtx::new();
        assert_eq!(TimeSync.destroy(&ctx, &ResolvedArgs::new(), &Observed::absent()).unwrap(), Outcome::Ok);
        assert!(ctx.calls().is_empty());
    }

    #[test]
    fn observing_time_sync_writes_nothing() {
        let ctx = ts("yes", "yes", "");
        TimeSync.observe(&ctx, &ResolvedArgs::new()).unwrap();
        assert!(ctx.writes().is_empty(), "{:?}", ctx.writes());
    }
}
