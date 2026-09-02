//! `service` —— systemd 单元的运行态与开机自启。
//!
//! 这是**"handler 被删掉"这条裁定的兑现点**(rustfs 试金石裁定 B):
//! 上游资源(二进制 / 配置 / unit)本轮有变更时,`diff` 直接判定需要重启 ——
//! 作者不写 `notify:`,也就不会因为忘了写而漏重启。

use anyhow::Result;

use crate::builtins::file::run_ok;
use crate::eval::{ResolvedArgs, Yaml};
use crate::verbs::*;

pub struct Service;

const SEP: &str = "\u{1}";

impl ResourceType for Service {
    fn name(&self) -> &'static str {
        "service"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let name = arg_str(args, "name")?;
        // is-active / is-enabled 都以非零退出表达"否",所以用 `|| true` 收口,
        // 再靠输出文本判断 —— 否则会把"服务停着"误当成"探测失败"。
        let cmd = format!(
            "{{ systemctl is-active {n} || true; }} | head -1; printf '{SEP}'; \
             {{ systemctl is-enabled {n} 2>/dev/null || true; }} | head -1; printf '{SEP}'; \
             systemctl list-unit-files {n} --no-legend --no-pager 2>/dev/null | head -1",
            n = sh(name)
        );
        let (_, out) = ctx.probe(&cmd)?;
        let parts: Vec<&str> = out.split(SEP).collect();
        let active = parts.first().map(|s| s.trim()).unwrap_or_default();
        let enabled = parts.get(1).map(|s| s.trim()).unwrap_or_default();
        let unit_line = parts.get(2).map(|s| s.trim()).unwrap_or_default();

        // unit 不存在 ⇒ 资源不存在(而不是"存在但停着")。
        //
        // **判据不能用 is-active**:unit 文件已被删除时它照样回 "inactive"
        // (退出码 4,但输出非空),于是"不存在"会被读成"存在但停着",
        // 紧接着 destroy 去 stop 一个不存在的 unit,得到 exit 5 而失败。
        // 真机上重跑 destroy 正是这样炸的 —— 退役因此不幂等。
        // 权威答案是 is-enabled 的 `not-found`,辅以 list-unit-files 为空。
        if unit_line.is_empty() && (enabled == "not-found" || enabled.is_empty()) {
            return Ok(Observed::absent());
        }
        Ok(Observed::present([
            ("active", active.to_string()),
            ("enabled", enabled.to_string()),
        ]))
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let want_state = arg_str_opt(input.args, "state").unwrap_or("started");
        let want_enabled = arg_bool(input.args, "enabled");
        let obs = input.observed;

        if !obs.present {
            // unit 尚未落地(通常是同一轮里 systemd_unit/copy 刚创建),
            // 说"将启动"而不是报错 —— plan 是对**这一轮结束后**的预测。
            let mut f = vec![FieldDiff::set("state", want_state)];
            if let Some(e) = want_enabled {
                f.push(FieldDiff::set("enabled", e.to_string()));
            }
            return Change::Create(f);
        }

        let is_active = obs.get("active") == Some("active");
        let is_enabled = obs.get("enabled") == Some("enabled");
        let mut fields = Vec::new();

        match want_state {
            "started" => {
                if !is_active {
                    fields.push(FieldDiff::change(
                        "state",
                        obs.get("active").unwrap_or("?"),
                        "active",
                    ));
                } else if input.upstream_changed {
                    // ← 取代 handler/notify
                    fields.push(FieldDiff::change("state", "active", "restarted(上游已变)"));
                }
            }
            "stopped" => {
                if is_active {
                    fields.push(FieldDiff::change("state", "active", "inactive"));
                }
            }
            // 显式 restarted 是"每次都重启"的意思,永远不是 noop。
            "restarted" => fields.push(FieldDiff::set("state", "restarted")),
            other => return Change::Unknown(format!("未知 state `{other}`")),
        }
        if let Some(want) = want_enabled {
            if want != is_enabled {
                fields.push(FieldDiff::change(
                    "enabled",
                    is_enabled.to_string(),
                    want.to_string(),
                ));
            }
        }
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, change: &Change) -> Result<Outcome> {
        let name = arg_str(args, "name")?;
        let want_state = arg_str_opt(args, "state").unwrap_or("started");
        // 上游改过 unit 文件时必须先 daemon-reload,否则 systemd 还认旧的。
        run_ok(ctx, "systemctl daemon-reload")?;

        let restart_needed = change
            .fields()
            .iter()
            .any(|f| f.to.as_deref().is_some_and(|t| t.starts_with("restarted")));
        let verb = match (want_state, restart_needed) {
            ("stopped", _) => "stop",
            (_, true) | ("restarted", _) => "restart",
            _ => "start",
        };
        run_ok(ctx, &format!("systemctl {verb} {}", sh(name)))?;

        if let Some(enabled) = arg_bool(args, "enabled") {
            let verb = if enabled { "enable" } else { "disable" };
            run_ok(ctx, &format!("systemctl {verb} {}", sh(name)))?;
        }
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        // 停 + 禁用;unit 文件本身由它自己那条资源的 destroy 删(各归各的)。
        //
        // 容忍 exit 5(unit 不存在):observe 与 destroy 之间 unit 可能已被别的
        // 东西移走(比如同一轮里包被卸了)。那不是失败,是"已经没了"。
        let name = arg_str(args, "name")?;
        for verb in ["stop", "disable"] {
            let (code, out) = ctx.run(&format!("systemctl {verb} {}", sh(name)))?;
            if code != 0 && code != 5 {
                anyhow::bail!("systemctl {verb} {name} 失败(exit {code}):{}", out.trim());
            }
        }
        Ok(Outcome::Changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::eval::Yaml;

    fn args(state: &str, enabled: Option<bool>) -> ResolvedArgs {
        let mut a = ResolvedArgs::new();
        a.insert("name".into(), Yaml::from("rustfs"));
        a.insert("state".into(), Yaml::from(state));
        if let Some(e) = enabled {
            a.insert("enabled".into(), Yaml::from(e));
        }
        a
    }
    fn running() -> Observed {
        Observed::present([("active", "active".into()), ("enabled", "enabled".into())])
    }
    fn diff_of(a: &ResolvedArgs, o: &Observed, upstream: bool) -> Change {
        Service.diff(&DiffInput {
            args: a,
            observed: o,
            upstream_changed: upstream,
        })
    }

    #[test]
    fn a_running_enabled_service_needs_nothing() {
        assert_eq!(
            diff_of(&args("started", Some(true)), &running(), false),
            Change::Ok
        );
    }

    #[test]
    fn upstream_change_triggers_a_restart_without_any_handler() {
        // 这条就是"IR 里没有 handler/notify"的兑现:作者什么都没写,
        // 二进制/配置一变,服务自己要求重启。
        let c = diff_of(&args("started", Some(true)), &running(), true);
        assert!(matches!(c, Change::Update(_)), "{c:?}");
        assert!(
            c.fields()[0].to_string().contains("restarted"),
            "{:?}",
            c.fields()
        );
    }

    #[test]
    fn stopped_service_is_started() {
        let obs = Observed::present([("active", "inactive".into()), ("enabled", "enabled".into())]);
        let c = diff_of(&args("started", Some(true)), &obs, false);
        assert_eq!(c.fields()[0].to_string(), "state: inactive → active");
    }

    #[test]
    fn enabling_alone_is_enough_to_plan_a_change() {
        let obs = Observed::present([("active", "active".into()), ("enabled", "disabled".into())]);
        let c = diff_of(&args("started", Some(true)), &obs, false);
        assert_eq!(c.fields().len(), 1);
        assert_eq!(c.fields()[0].to_string(), "enabled: false → true");
    }

    #[test]
    fn explicit_restarted_is_never_a_noop() {
        // 与 started 不同:作者写 restarted 就是要求每次都重启。
        assert!(!diff_of(&args("restarted", None), &running(), false).is_noop());
    }

    #[test]
    fn unknown_state_is_surfaced_not_silently_ignored() {
        assert!(matches!(
            diff_of(&args("bounced", None), &running(), false),
            Change::Unknown(_)
        ));
    }

    #[test]
    fn observe_reads_active_and_enabled_in_one_probe() {
        let ctx = FakeCtx::new().on(
            "systemctl is-active",
            0,
            &format!("active{SEP}enabled{SEP}rustfs.service enabled"),
        );
        let obs = Service.observe(&ctx, &args("started", None)).unwrap();
        assert_eq!(obs.get("active"), Some("active"));
        assert_eq!(obs.get("enabled"), Some("enabled"));
        assert_eq!(ctx.calls().len(), 1);
        assert!(ctx.writes().is_empty());
    }

    #[test]
    fn a_missing_unit_reads_as_absent_not_as_stopped() {
        let ctx = FakeCtx::new().on("systemctl is-active", 0, &format!("{SEP}{SEP}"));
        assert!(
            !Service
                .observe(&ctx, &args("started", None))
                .unwrap()
                .present
        );
    }

    #[test]
    fn apply_reloads_before_acting_and_restarts_when_upstream_moved() {
        let ctx = FakeCtx::new().on("systemctl", 0, "");
        let change = diff_of(&args("started", Some(true)), &running(), true);
        Service
            .apply(&ctx, &args("started", Some(true)), &change)
            .unwrap();
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        assert_eq!(cmds[0], "systemctl daemon-reload", "{cmds:?}");
        assert!(
            cmds.iter().any(|c| c.starts_with("systemctl restart")),
            "{cmds:?}"
        );
        assert!(
            cmds.iter().any(|c| c.starts_with("systemctl enable")),
            "{cmds:?}"
        );
    }

    #[test]
    fn destroy_stops_and_disables_but_leaves_the_unit_file_alone() {
        let ctx = FakeCtx::new().on("systemctl", 0, "");
        Service
            .destroy(&ctx, &args("started", None), &running())
            .unwrap();
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(cmds.iter().any(|c| c.starts_with("systemctl stop")));
        assert!(cmds.iter().any(|c| c.starts_with("systemctl disable")));
        assert!(
            !cmds.iter().any(|c| c.contains("rm ")),
            "unit 文件归它自己那条资源管"
        );
    }
}

/// `systemd_unit` —— unit 文件本身(rustfs 试金石裁定 C)。
///
/// 旧写法是 `copy` 一坨内联 INI 文本:不可校验(拼错字段要等 systemd 报)、
/// 不可字段级 diff(整文件 hash)、跨发行版不可移植。类型化之后,plan 能说出
/// "只有 ExecStart 变了"。
pub struct SystemdUnit;

const UNIT_DIR: &str = "/etc/systemd/system";

impl ResourceType for SystemdUnit {
    fn name(&self) -> &'static str {
        "systemd_unit"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let path = unit_path(args)?;
        let (code, out) = ctx.probe(&format!("cat {}", sh(&path)))?;
        if code != 0 {
            return Ok(Observed::absent());
        }
        // unit 内容来自物料时,直接做内容寻址比对:目标上实际内容的摘要 vs
        // 物料的摘要。此前这里只能报"说不清",而那正是 verify 唯一给不出
        // 绿灯的原因 —— 能算出来的东西不该被当成算不出来。
        if let Some(name) = arg_str_opt(args, "from_material") {
            let actual = crate::builtins::copy::sha256_hex(&out);
            let mut fields = std::collections::BTreeMap::new();
            fields.insert("sha256".to_string(), actual);
            if let Some(want) = ctx.material_digest(name)? {
                fields.insert("want_sha256".to_string(), want);
            }
            return Ok(Observed {
                present: true,
                fields,
            });
        }
        // 只解析我们会写的那几项 —— 别人手改的其它字段不该被我们判成"漂移"。
        let mut fields = std::collections::BTreeMap::new();
        for line in out.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let k = k.trim();
            if [
                "ExecStart",
                "Restart",
                "EnvironmentFile",
                "Description",
                "After",
                "WantedBy",
            ]
            .contains(&k)
            {
                fields.insert(k.to_lowercase(), v.trim().to_string());
            }
        }
        Ok(Observed {
            present: true,
            fields,
        })
    }

    fn diff(&self, input: &DiffInput) -> Change {
        if arg_str_opt(input.args, "from_material").is_some() {
            if !input.observed.present {
                return Change::Create(vec![FieldDiff::set("unit", "(来自物料)")]);
            }
            return match input.observed.get("want_sha256") {
                // 拿得到物料摘要就正经比对内容 —— 不再是一句"说不清"。
                Some(want) if input.observed.get("sha256") == Some(want) => Change::Ok,
                Some(want) => Change::Update(vec![FieldDiff::change(
                    "unit",
                    &input.observed.get("sha256").unwrap_or("?")
                        [..12.min(input.observed.get("sha256").unwrap_or("?").len())],
                    &want[..12.min(want.len())],
                )]),
                // 远端物料且未声明 sha256 —— 此刻确实算不出来,如实说。
                None if input.upstream_changed => {
                    Change::Update(vec![FieldDiff::set("unit", "(物料已变)")])
                }
                None => Change::Unknown(
                    "物料未声明 `sha256:` 且内容在远端 —— 无法在 plan 期比对".into(),
                ),
            };
        }
        let desired = rendered_fields(input.args);
        if !input.observed.present {
            return Change::Create(
                desired
                    .iter()
                    .map(|(k, v)| FieldDiff::set(k, v.clone()))
                    .collect(),
            );
        }
        let fields: Vec<FieldDiff> = desired
            .iter()
            .filter_map(|(k, want)| {
                let have = input.observed.get(&k.to_lowercase())?;
                (have != want).then(|| FieldDiff::change(k, have, want.clone()))
            })
            .collect();
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _c: &Change) -> Result<Outcome> {
        let path = unit_path(args)?;
        if let Some(m) = arg_str_opt(args, "from_material") {
            ctx.place_material(m, &path)?;
        } else {
            ctx.write_file(&path, &render_unit(args))?;
        }
        // dropin 各自独立成文件,便于覆盖上游 unit 而不改它。
        for dropin in list_of(args, "dropins") {
            let dir = format!("{UNIT_DIR}/{}.service.d", arg_str(args, "name")?);
            run_ok(ctx, &format!("mkdir -p {}", sh(&dir)))?;
            ctx.place_material(&dropin, &format!("{dir}/{dropin}.conf"))?;
        }
        run_ok(ctx, "systemctl daemon-reload")?;
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        let path = unit_path(args)?;
        run_ok(ctx, &format!("rm -f {}", sh(&path)))?;
        run_ok(ctx, "systemctl daemon-reload")?;
        Ok(Outcome::Changed)
    }
}

fn unit_path(args: &ResolvedArgs) -> Result<String> {
    let name = arg_str(args, "name")?;
    Ok(if name.contains('.') {
        format!("{UNIT_DIR}/{name}")
    } else {
        format!("{UNIT_DIR}/{name}.service")
    })
}

/// 会被写进 unit、且会参与 diff 的字段(有序,渲染与比对共用一份)。
fn rendered_fields(args: &ResolvedArgs) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    let mut put = |k: &'static str, v: Option<String>| {
        if let Some(v) = v.filter(|s| !s.is_empty()) {
            out.push((k, v));
        }
    };
    put(
        "Description",
        arg_str_opt(args, "description").map(str::to_string),
    );
    put("After", non_empty(list_of(args, "after").join(" ")));
    put(
        "ExecStart",
        arg_str_opt(args, "exec_start").map(str::to_string),
    );
    put(
        "EnvironmentFile",
        arg_str_opt(args, "environment_file").map(str::to_string),
    );
    put("Restart", arg_str_opt(args, "restart").map(str::to_string));
    put(
        "WantedBy",
        Some(
            arg_str_opt(args, "wanted_by")
                .unwrap_or("multi-user.target")
                .to_string(),
        ),
    );
    out
}

fn non_empty(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

fn render_unit(args: &ResolvedArgs) -> String {
    let f = rendered_fields(args);
    let get = |key: &str| f.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
    let mut s = String::from("[Unit]\n");
    for k in ["Description", "After"] {
        if let Some(v) = get(k) {
            s.push_str(&format!("{k}={v}\n"));
        }
    }
    s.push_str("\n[Service]\n");
    for k in ["EnvironmentFile", "ExecStart", "Restart"] {
        if let Some(v) = get(k) {
            // EnvironmentFile 前缀 `-` 表示"文件不存在也不报错",与旧库写法一致。
            let v = if k == "EnvironmentFile" && !v.starts_with('-') {
                format!("-{v}")
            } else {
                v
            };
            s.push_str(&format!("{k}={v}\n"));
        }
    }
    if let Some(Yaml::Mapping(limits)) = args.get("limits") {
        for (k, v) in limits {
            s.push_str(&format!(
                "Limit{}={}\n",
                crate::eval::scalar_to_string(k),
                crate::eval::scalar_to_string(v)
            ));
        }
    }
    s.push_str("\n[Install]\n");
    if let Some(v) = get("WantedBy") {
        s.push_str(&format!("WantedBy={v}\n"));
    }
    s
}

fn list_of(args: &ResolvedArgs, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Yaml::Sequence(items)) => items.iter().map(crate::eval::scalar_to_string).collect(),
        Some(Yaml::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::ctx::FakeCtx;

    fn args() -> ResolvedArgs {
        let mut a = ResolvedArgs::new();
        a.insert("name".into(), Yaml::from("rustfs"));
        a.insert("description".into(), Yaml::from("RustFS object storage"));
        a.insert(
            "exec_start".into(),
            Yaml::from("/usr/local/bin/rustfs $VOL"),
        );
        a.insert("environment_file".into(), Yaml::from("/etc/default/rustfs"));
        a.insert("restart".into(), Yaml::from("on-failure"));
        a.insert(
            "after".into(),
            serde_yaml::from_str("[network-online.target]").unwrap(),
        );
        a.insert(
            "limits".into(),
            serde_yaml::from_str("{NOFILE: 1048576}").unwrap(),
        );
        a
    }

    #[test]
    fn renders_a_valid_three_section_unit() {
        let text = render_unit(&args());
        assert!(text.starts_with("[Unit]\n"), "{text}");
        assert!(
            text.contains("\n[Service]\n") && text.contains("\n[Install]\n"),
            "{text}"
        );
        assert!(
            text.contains("ExecStart=/usr/local/bin/rustfs $VOL"),
            "{text}"
        );
        assert!(text.contains("LimitNOFILE=1048576"), "{text}");
        // `-` 前缀:环境文件缺失不该让服务起不来
        assert!(
            text.contains("EnvironmentFile=-/etc/default/rustfs"),
            "{text}"
        );
        assert!(
            text.contains("WantedBy=multi-user.target"),
            "默认 WantedBy:{text}"
        );
    }

    #[test]
    fn diff_pinpoints_the_single_changed_directive() {
        // 这就是类型化相对"copy 一坨 INI 文本"的收益:能说出**哪一行**变了。
        let obs = Observed {
            present: true,
            fields: [
                (
                    "execstart".to_string(),
                    "/usr/local/bin/rustfs OLD".to_string(),
                ),
                ("restart".to_string(), "on-failure".to_string()),
                (
                    "description".to_string(),
                    "RustFS object storage".to_string(),
                ),
                (
                    "environmentfile".to_string(),
                    "/etc/default/rustfs".to_string(),
                ),
                ("after".to_string(), "network-online.target".to_string()),
                ("wantedby".to_string(), "multi-user.target".to_string()),
            ]
            .into(),
        };
        let c = SystemdUnit.diff(&DiffInput {
            args: &args(),
            observed: &obs,
            upstream_changed: false,
        });
        assert_eq!(c.fields().len(), 1, "{:?}", c.fields());
        assert!(
            c.fields()[0].to_string().starts_with("ExecStart:"),
            "{:?}",
            c.fields()
        );
    }

    #[test]
    fn an_identical_unit_is_a_noop() {
        let mut fields = std::collections::BTreeMap::new();
        for (k, v) in rendered_fields(&args()) {
            fields.insert(k.to_lowercase(), v);
        }
        let obs = Observed {
            present: true,
            fields,
        };
        assert_eq!(
            SystemdUnit.diff(&DiffInput {
                args: &args(),
                observed: &obs,
                upstream_changed: false
            }),
            Change::Ok
        );
    }

    #[test]
    fn unrelated_hand_edited_directives_are_not_reported_as_drift() {
        // 目标机上别人加的 `MemoryMax=` 不归我们管 —— 只比对我们写的那几项。
        let ctx = FakeCtx::new().on(
            "cat",
            0,
            "[Service]\nExecStart=/usr/local/bin/rustfs $VOL\nMemoryMax=4G\n",
        );
        let obs = SystemdUnit.observe(&ctx, &args()).unwrap();
        assert_eq!(obs.get("execstart"), Some("/usr/local/bin/rustfs $VOL"));
        assert!(
            obs.fields.keys().all(|k| k != "memorymax"),
            "{:?}",
            obs.fields
        );
    }

    #[test]
    fn a_unit_from_a_material_is_compared_by_content_not_declared_unknown() {
        // 这曾是 verify 唯一给不出绿灯的原因:能算出来的东西被当成算不出来。
        let body = "[Unit]\nDescription=demo\n";
        let mut a = ResolvedArgs::new();
        a.insert("name".into(), Yaml::from("demo"));
        a.insert("from_material".into(), Yaml::from("unit-demo"));

        let same = Observed {
            present: true,
            fields: [
                (
                    "sha256".to_string(),
                    crate::builtins::copy::sha256_hex(body),
                ),
                (
                    "want_sha256".to_string(),
                    crate::builtins::copy::sha256_hex(body),
                ),
            ]
            .into(),
        };
        assert_eq!(
            SystemdUnit.diff(&DiffInput {
                args: &a,
                observed: &same,
                upstream_changed: false
            }),
            Change::Ok,
            "内容一致就该是 ✓,不是 ?"
        );

        let drifted = Observed {
            present: true,
            fields: [
                (
                    "sha256".to_string(),
                    crate::builtins::copy::sha256_hex("changed by hand"),
                ),
                (
                    "want_sha256".to_string(),
                    crate::builtins::copy::sha256_hex(body),
                ),
            ]
            .into(),
        };
        let c = SystemdUnit.diff(&DiffInput {
            args: &a,
            observed: &drifted,
            upstream_changed: false,
        });
        assert!(matches!(c, Change::Update(_)), "{c:?}");
    }

    #[test]
    fn a_remote_material_without_a_declared_digest_still_says_it_cannot_tell() {
        // 诚实的边界:算不出来的时候不许假装算得出来。
        let mut a = ResolvedArgs::new();
        a.insert("name".into(), Yaml::from("demo"));
        a.insert("from_material".into(), Yaml::from("unit-demo"));
        let obs = Observed {
            present: true,
            fields: [("sha256".to_string(), "abc".to_string())].into(),
        };
        assert!(matches!(
            SystemdUnit.diff(&DiffInput {
                args: &a,
                observed: &obs,
                upstream_changed: false
            }),
            Change::Unknown(_)
        ));
    }

    #[test]
    fn observing_a_material_backed_unit_hashes_what_is_actually_there() {
        let body = "[Unit]\nDescription=demo\n";
        let ctx = FakeCtx::new().on("cat", 0, body);
        let mut a = ResolvedArgs::new();
        a.insert("name".into(), Yaml::from("demo"));
        a.insert("from_material".into(), Yaml::from("unit-demo"));
        let obs = SystemdUnit.observe(&ctx, &a).unwrap();
        assert_eq!(
            obs.get("sha256"),
            Some(crate::builtins::copy::sha256_hex(body).as_str())
        );
        assert!(ctx.writes().is_empty());
    }

    #[test]
    fn writing_a_unit_always_reloads_the_daemon() {
        let ctx = FakeCtx::new().on("systemctl", 0, "");
        SystemdUnit
            .apply(&ctx, &args(), &Change::Create(vec![]))
            .unwrap();
        let calls: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(
            calls.iter().any(|c| c.contains("rustfs.service")),
            "{calls:?}"
        );
        assert!(
            calls.iter().any(|c| c == "systemctl daemon-reload"),
            "不 reload 的话 systemd 还认旧的:{calls:?}"
        );
    }

    #[test]
    fn a_name_with_an_explicit_suffix_is_respected() {
        let mut a = args();
        a.insert("name".into(), Yaml::from("containerd.socket"));
        assert_eq!(
            unit_path(&a).unwrap(),
            "/etc/systemd/system/containerd.socket"
        );
    }
}

#[cfg(test)]
mod absence_tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::eval::Yaml;

    fn args(name: &str) -> ResolvedArgs {
        [("name".to_string(), Yaml::from(name))]
            .into_iter()
            .collect()
    }

    #[test]
    fn a_deleted_unit_is_absent_even_though_is_active_says_inactive() {
        // 真机实证:unit 文件删掉后 `systemctl is-active` 仍回 "inactive"(非空)。
        // 早先的判据 `unit_line.is_empty() && active.is_empty()` 因此判不出
        // "不存在",destroy 会去 stop 一个不存在的 unit 拿到 exit 5 —— 退役不幂等。
        let ctx = FakeCtx::new().on(
            "systemctl is-active",
            0,
            &format!("inactive{SEP}not-found{SEP}"),
        );
        let obs = Service.observe(&ctx, &args("keepalived")).unwrap();
        assert!(!obs.present, "unit 已删除却被判为存在:{obs:?}");
    }

    #[test]
    fn a_stopped_but_installed_service_is_still_present() {
        // 反向保险:装着但停着,必须仍是"存在" —— 否则 `state: started`
        // 永远不会去启动它。
        let ctx = FakeCtx::new().on(
            "systemctl is-active",
            0,
            &format!("inactive{SEP}enabled{SEP}nginx.service enabled"),
        );
        let obs = Service.observe(&ctx, &args("nginx")).unwrap();
        assert!(obs.present, "{obs:?}");
        assert_eq!(obs.get("active"), Some("inactive"));
    }

    #[test]
    fn destroying_a_vanished_unit_is_not_a_failure() {
        // observe 与 destroy 之间 unit 可能已被别的东西移走(同一轮里包被卸了)。
        // exit 5 = unit 不存在,那不是失败,是"已经没了"。
        // FakeCtx 对未注册命令回 exit 1,所以 disable 也要显式注册成 5。
        let ctx = FakeCtx::new()
            .on("systemctl stop", 5, "Unit keepalived.service not loaded.")
            .on(
                "systemctl disable",
                5,
                "Unit keepalived.service does not exist.",
            );
        let obs = Observed::present([("active", "inactive".into()), ("enabled", "".into())]);
        assert_eq!(
            Service.destroy(&ctx, &args("keepalived"), &obs).unwrap(),
            Outcome::Changed
        );
    }

    #[test]
    fn a_real_stop_failure_still_fails() {
        // 别把 exit 5 的宽容扩大成"什么错都咽下去"。
        let ctx = FakeCtx::new().on("systemctl stop", 1, "Job for x.service failed");
        let obs = Observed::present([("active", "active".into()), ("enabled", "".into())]);
        assert!(Service.destroy(&ctx, &args("x"), &obs).is_err());
    }
}
