//! `file` —— 路径的期望态(目录 / 存在 / 不存在)+ 权限属主。
//!
//! 五动词齐全的最小样本:一次 `stat` 就把现实全拿到,于是 plan 能说出
//! "会创建" / "只改权限" / "已就位"三种确定结论,而不是 ansible 那样"跑了才知道"。

use anyhow::Result;

use crate::eval::ResolvedArgs;
use crate::verbs::*;

pub struct File;

/// stat 输出的分隔符:路径里可能有空格,但不会有这个。
const SEP: &str = "\u{1}";

impl ResourceType for File {
    fn name(&self) -> &'static str {
        "file"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let path = arg_str(args, "path")?;
        // 一趟 stat 拿全类型/权限/属主 —— 少一次往返就是少一次 SSH 延迟。
        let cmd = format!("stat -c '%F{SEP}%a{SEP}%U{SEP}%G' {}", sh(path));
        let (code, out) = ctx.probe(&cmd)?;
        if code != 0 {
            return Ok(Observed::absent());
        }
        let f: Vec<&str> = out.trim().split(SEP).collect();
        Ok(Observed::present([
            ("kind", f.first().copied().unwrap_or_default().to_string()),
            ("mode", f.get(1).copied().unwrap_or_default().to_string()),
            ("owner", f.get(2).copied().unwrap_or_default().to_string()),
            ("group", f.get(3).copied().unwrap_or_default().to_string()),
        ]))
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let state = arg_str_opt(input.args, "state").unwrap_or("directory");
        let obs = input.observed;

        if state == "absent" {
            return if obs.present { Change::Destroy } else { Change::Ok };
        }
        if !obs.present {
            let mut fields = vec![FieldDiff::set("state", state)];
            for key in ["mode", "owner", "group"] {
                if let Some(v) = arg_str_opt(input.args, key) {
                    fields.push(FieldDiff::set(key, v));
                }
            }
            return Change::Create(fields);
        }

        let mut fields = Vec::new();
        // 类型不符(要目录却是文件)→ 也是 Update,由 apply 重建。
        let want_dir = state == "directory";
        let is_dir = obs.get("kind").is_some_and(|k| k.contains("directory"));
        if want_dir != is_dir {
            fields.push(FieldDiff::change(
                "state",
                obs.get("kind").unwrap_or("?"),
                state,
            ));
        }
        for key in ["mode", "owner", "group"] {
            if let (Some(want), Some(have)) = (arg_str_opt(input.args, key), obs.get(key)) {
                // mode 比较去掉前导 0:`"0750"` 与 stat 的 `750` 是同一件事。
                let (w, h) = (want.trim_start_matches('0'), have.trim_start_matches('0'));
                if w != h {
                    fields.push(FieldDiff::change(key, have, want));
                }
            }
        }
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _change: &Change) -> Result<Outcome> {
        let path = arg_str(args, "path")?;
        let state = arg_str_opt(args, "state").unwrap_or("directory");
        // `state: absent` 直接删,**不借道 destroy()**。destroy 对 absent 资源的
        // 语义是"退役时无事可做"(期望本就是不存在,别把别人的东西删了)——
        // 那对退役是对的,对 apply 是灾难:此前 Destroy 变更被路由过去,
        // 删除静默变成空操作还报 ok,真机上 nginx 的 sites-enabled/default
        // 就是这么在 plan 里"将删除"、执行后依然活着的。
        if state == "absent" {
            run_ok(ctx, &format!("rm -rf {}", sh(path)))?;
            return Ok(Outcome::Changed);
        }
        let cmd = match state {
            "directory" => format!("mkdir -p {}", sh(path)),
            "touch" => format!("touch {}", sh(path)),
            other => anyhow::bail!("未知 state `{other}`"),
        };
        run_ok(ctx, &cmd)?;
        for (key, cmd) in [("mode", "chmod"), ("owner", "chown"), ("group", "chgrp")] {
            if let Some(v) = arg_str_opt(args, key) {
                run_ok(ctx, &format!("{cmd} {} {}", sh(v), sh(path)))?;
            }
        }
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        // 已经不在了就别再发 `rm -rf` —— 退役一台从没装过的机器应当安静通过,
        // 而不是刷一屏无意义的删除命令(甚至因为路径不存在而报错)。
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        if arg_str_opt(args, "state") == Some("absent") {
            // 期望本就是"不存在" —— 退役时无事可做(别把别人的东西删了)。
            return Ok(Outcome::Ok);
        }
        let path = arg_str(args, "path")?;

        // **挂载点不归 file 管。**
        //
        // 一份蓝图里常见 `mount: /srv/data` 紧跟 `file: /srv/data`(后者只是
        // 给挂载点设属主和权限)。逆序退役时 file 排在 mount 之前,于是
        // `rm -rf` 打在一个活着的挂载点上 —— 轻则失败(Device busy,真机上
        // 就是这么炸的),重则在挂载已被卸掉时**删光底层目录里的真实数据**。
        //
        // 目录本身归 mount 的退役处理(卸载 + 清 fstab),这里让开。
        let (mp, _) = ctx.probe(&format!("mountpoint -q {}", sh(path)))?;
        if mp == 0 {
            return Ok(Outcome::Warn);
        }
        run_ok(ctx, &format!("rm -rf {}", sh(path)))?;
        Ok(Outcome::Changed)
    }
}

/// 跑一条写命令,非零退出就带上输出报错(静默失败是运维工具的大忌)。
pub(crate) fn run_ok(ctx: &dyn Ctx, cmd: &str) -> Result<String> {
    let (code, out) = ctx.run(cmd)?;
    if code != 0 {
        anyhow::bail!("命令失败(exit {code}):{cmd}\n{}", out.trim());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::eval::Yaml;

    fn args(pairs: &[(&str, &str)]) -> ResolvedArgs {
        pairs.iter().map(|(k, v)| (k.to_string(), Yaml::from(*v))).collect()
    }

    fn diff_of(t: &File, a: &ResolvedArgs, o: &Observed) -> Change {
        t.diff(&DiffInput { args: a, observed: o, upstream_changed: false })
    }

    #[test]
    fn absent_path_plans_a_create_listing_every_field() {
        let a = args(&[("path", "/data"), ("state", "directory"), ("mode", "0750")]);
        let c = diff_of(&File, &a, &Observed::absent());
        assert!(matches!(c, Change::Create(_)));
        let rendered: Vec<String> = c.fields().iter().map(|f| f.to_string()).collect();
        assert!(rendered.iter().any(|s| s.contains("mode: 0750")), "{rendered:?}");
    }

    #[test]
    fn matching_reality_plans_nothing() {
        let a = args(&[("path", "/data"), ("state", "directory"), ("mode", "0750")]);
        let obs = Observed::present([
            ("kind", "directory".into()),
            ("mode", "750".into()),
            ("owner", "root".into()),
            ("group", "root".into()),
        ]);
        assert_eq!(diff_of(&File, &a, &obs), Change::Ok, "幂等:第二次跑必须是 Ok");
    }

    #[test]
    fn only_the_wrong_field_shows_up_in_the_plan() {
        let a = args(&[("path", "/data"), ("state", "directory"), ("mode", "0700")]);
        let obs = Observed::present([("kind", "directory".into()), ("mode", "750".into())]);
        let c = diff_of(&File, &a, &obs);
        assert_eq!(c.fields().len(), 1, "只该报 mode 一项:{c:?}");
        assert_eq!(c.fields()[0].to_string(), "mode: 750 → 0700");
    }

    #[test]
    fn state_absent_inverts_the_whole_contract() {
        let a = args(&[("path", "/tmp/x"), ("state", "absent")]);
        assert_eq!(diff_of(&File, &a, &Observed::absent()), Change::Ok);
        assert_eq!(
            diff_of(&File, &a, &Observed::present([("kind", "regular file".into())])),
            Change::Destroy
        );
        // 退役时不该去删一个"本来就该不存在"的路径。
        let ctx = FakeCtx::new();
        assert_eq!(File.destroy(&ctx, &a, &Observed::absent()).unwrap(), Outcome::Ok);
        assert!(ctx.writes().is_empty());
    }

    #[test]
    fn destroying_a_path_that_is_already_gone_is_silent() {
        // 曾经的真 bug:destroy 不看 observed,对不存在的路径也发 `rm -rf`。
        let ctx = FakeCtx::new();
        let a = args(&[("path", "/data"), ("state", "directory")]);
        assert_eq!(File.destroy(&ctx, &a, &Observed::absent()).unwrap(), Outcome::Ok);
        assert!(ctx.calls().is_empty(), "{:?}", ctx.calls());
    }

    #[test]
    fn observe_issues_exactly_one_readonly_probe() {
        // observe 只读是 plan 可信的前提 —— 用记录式假目标把它钉住。
        let ctx = FakeCtx::new().on("stat -c", 0, &format!("directory{SEP}750{SEP}root{SEP}root"));
        let obs = File.observe(&ctx, &args(&[("path", "/data")])).unwrap();
        assert!(obs.present);
        assert_eq!(obs.get("mode"), Some("750"));
        assert_eq!(ctx.calls().len(), 1, "多一次往返就是多一次 SSH 延迟");
        assert!(ctx.writes().is_empty(), "observe 期间不许有任何写:{:?}", ctx.writes());
    }

    #[test]
    fn observe_treats_a_failing_stat_as_absent() {
        let ctx = FakeCtx::new(); // 未注册 → 退出码 1
        assert!(!File.observe(&ctx, &args(&[("path", "/nope")])).unwrap().present);
    }

    #[test]
    fn apply_creates_then_sets_permissions() {
        let ctx = FakeCtx::new().on("", 0, "");
        let a = args(&[("path", "/data"), ("state", "directory"), ("mode", "0750")]);
        assert_eq!(File.apply(&ctx, &a, &Change::Create(vec![])).unwrap(), Outcome::Changed);
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(cmds[0].starts_with("mkdir -p"), "{cmds:?}");
        assert!(cmds.iter().any(|c| c.starts_with("chmod '0750'")), "{cmds:?}");
    }

    #[test]
    fn a_failing_command_is_reported_not_swallowed() {
        let ctx = FakeCtx::new().on("mkdir", 1, "Permission denied");
        let a = args(&[("path", "/data"), ("state", "directory")]);
        let err = File.apply(&ctx, &a, &Change::Create(vec![])).unwrap_err().to_string();
        assert!(err.contains("Permission denied"), "{err}");
    }
}

#[cfg(test)]
mod mountpoint_guard_tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::eval::Yaml;

    fn args(path: &str) -> ResolvedArgs {
        [("path".to_string(), Yaml::from(path)), ("state".to_string(), Yaml::from("directory"))]
            .into_iter()
            .collect()
    }

    #[test]
    fn destroying_a_mountpoint_is_refused_not_attempted() {
        // 蓝图里 `mount: /srv/data` 后面常跟 `file: /srv/data`(设属主权限)。
        // 逆序退役时 file 排在 mount 前面 —— `rm -rf` 打在活着的挂载点上,
        // 轻则 Device busy(真机上就是这么炸的),重则在挂载已卸时**删光
        // 底层目录里的真实数据**。目录归 mount 的退役管,这里必须让开。
        let ctx = FakeCtx::new().on("mountpoint -q", 0, "");
        let out = File.destroy(&ctx, &args("/srv/data"), &Observed::present([])).unwrap();
        assert_eq!(out, Outcome::Warn);
        assert!(
            !ctx.calls().iter().any(|c| c.text().starts_with("rm -rf")),
            "对挂载点发了 rm:{:?}",
            ctx.calls()
        );
    }

    #[test]
    fn destroying_a_plain_directory_still_removes_it() {
        // 别把守卫扩大成"什么都不敢删"。
        let ctx = FakeCtx::new().on("mountpoint -q", 1, "").on("rm -rf", 0, "");
        let out = File.destroy(&ctx, &args("/opt/plain"), &Observed::present([])).unwrap();
        assert_eq!(out, Outcome::Changed);
        assert!(ctx.calls().iter().any(|c| c.text().starts_with("rm -rf '/opt/plain'")));
    }
}

#[cfg(test)]
mod destroy_change_tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::eval::Yaml;

    #[test]
    fn an_apply_of_a_destroy_change_actually_removes_the_path() {
        // plan 判了 Destroy,apply 就必须真删。此前这里把 Observed::default()
        // (present=false)传给 destroy,触发它"不在就不动"的闸 ——
        // 删除静默变空操作还报 ok。
        let ctx = FakeCtx::new().on("mountpoint -q", 1, "").on("rm -rf", 0, "");
        let args: ResolvedArgs = [
            ("path".to_string(), Yaml::from("/etc/nginx/sites-enabled/default")),
            ("state".to_string(), Yaml::from("absent")),
        ]
        .into_iter()
        .collect();
        let out = File.apply(&ctx, &args, &Change::Destroy).unwrap();
        assert_eq!(out, Outcome::Changed);
        assert!(
            ctx.calls().iter().any(|c| c.text().starts_with("rm -rf '/etc/nginx/sites-enabled/default'")),
            "没有发出删除:{:?}",
            ctx.calls()
        );
    }
}
