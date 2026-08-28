//! 路径与内容类的其余内建:`template` / `lineinfile` / `unarchive`。
//!
//! 共性:现实都能一次探针读清楚,于是 plan 说得出确定结论。

use anyhow::Result;

use crate::builtins::file::run_ok;
use crate::eval::ResolvedArgs;
use crate::verbs::*;

/// `template` —— 渲染一份物料模板到目标路径。
///
/// 与 `copy` 的差别只在**内容从哪来**:模板的字节在控制端渲染(带 inventory 上下文),
/// 落地后同样按内容寻址判幂等。
///
/// 关键在于**渲染发生在 observe 期间**:渲染结果的摘要是控制端事实,
/// 把它取到手,`diff` 就能正经比对而不是报 `?` —— 否则 verify 永远给不出绿灯。
pub struct Template;

impl ResourceType for Template {
    fn name(&self) -> &'static str {
        "template"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let mut obs = crate::builtins::copy::Copy.observe(ctx, &dest_as_copy(args)?)?;
        // 现在就渲染 —— 渲染是纯函数且只在控制端,observe 的"只读"纪律不受影响。
        if obs.present {
            if let Some(name) = arg_str_opt(args, "material") {
                if let Some(text) = ctx.render_material(name)? {
                    obs.fields
                        .insert("want_sha256".into(), crate::builtins::copy::sha256_hex(&text));
                }
            }
        }
        Ok(obs)
    }

    fn diff(&self, input: &DiffInput) -> Change {
        if !input.observed.present {
            return Change::Create(vec![FieldDiff::set("content", "(模板渲染)")]);
        }
        // 拿到渲染摘要就正经比对(与 copy 同一条内容寻址路径);
        // 拿不到才退回粗判据 —— 说不清就说不清,不假装。
        match input.observed.get("want_sha256") {
            Some(_) => crate::builtins::copy::Copy.diff(input),
            None if input.upstream_changed => {
                Change::Update(vec![FieldDiff::change("content", "(已存在)", "(重新渲染)")])
            }
            None => Change::Unknown("此上下文渲染不了模板,无法与现实比对".into()),
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, change: &Change) -> Result<Outcome> {
        // 渲染得出来就写渲染结果;渲染不了(如 LocalCtx)才退回原样落地。
        if let Some(name) = arg_str_opt(args, "material") {
            if let Some(text) = ctx.render_material(name)? {
                let dest = arg_str(args, "dest")?;
                ctx.write_file(dest, &text)?;
                if let Some(mode) = arg_str_opt(args, "mode") {
                    run_ok(ctx, &format!("chmod {} {}", mode, sh(dest)))?;
                }
                return Ok(Outcome::Changed);
            }
        }
        crate::builtins::copy::Copy.apply(ctx, &material_as_copy(args)?, change)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        crate::builtins::copy::Copy.destroy(ctx, &dest_as_copy(args)?, obs)
    }
}

fn dest_as_copy(args: &ResolvedArgs) -> Result<ResolvedArgs> {
    let mut a = ResolvedArgs::new();
    a.insert("dest".into(), args.get("dest").cloned().unwrap_or_default());
    if let Some(m) = args.get("mode") {
        a.insert("mode".into(), m.clone());
    }
    Ok(a)
}

fn material_as_copy(args: &ResolvedArgs) -> Result<ResolvedArgs> {
    let mut a = dest_as_copy(args)?;
    for key in ["material", "src"] {
        if let Some(v) = args.get(key) {
            a.insert(key.into(), v.clone());
        }
    }
    Ok(a)
}

/// `lineinfile` —— 确保某一行在文件里存在 / 不存在。
///
/// 幂等靠一次 grep 探针;`regexp` 存在时按它替换,否则按整行匹配。
pub struct LineInFile;

impl ResourceType for LineInFile {
    fn name(&self) -> &'static str {
        "lineinfile"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let path = arg_str(args, "path")?;
        let (exists, _) = ctx.probe(&format!("test -f {}", sh(path)))?;
        if exists != 0 {
            return Ok(Observed::absent());
        }
        let pattern = arg_str_opt(args, "regexp")
            .map(str::to_string)
            .unwrap_or_else(|| regex_escape(arg_str_opt(args, "line").unwrap_or_default()));
        let (hit, matched) = ctx.probe(&format!("grep -E {} {} | head -1", sh(&pattern), sh(path)))?;
        Ok(Observed::present([
            ("present", (hit == 0).to_string()),
            ("line", matched.trim().to_string()),
        ]))
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let want_present = arg_str_opt(input.args, "state").unwrap_or("present") == "present";
        let line = arg_str_opt(input.args, "line").unwrap_or_default();
        let create = arg_bool(input.args, "create").unwrap_or(false);

        if !input.observed.present {
            return if want_present && create {
                Change::Create(vec![FieldDiff::set("line", line)])
            } else if want_present {
                Change::Unknown("目标文件不存在,且未声明 `create: true`".into())
            } else {
                Change::Ok // 文件都没有,那行自然不存在
            };
        }
        let has = input.observed.get("present") == Some("true");
        let current = input.observed.get("line").unwrap_or_default();
        match (want_present, has) {
            (true, false) => Change::Update(vec![FieldDiff::set("line", line)]),
            (true, true) if current != line => {
                Change::Update(vec![FieldDiff::change("line", current, line)])
            }
            (true, true) => Change::Ok,
            (false, true) => Change::Update(vec![FieldDiff::change("line", current, "(删除)")]),
            (false, false) => Change::Ok,
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _change: &Change) -> Result<Outcome> {
        let path = arg_str(args, "path")?;
        let line = arg_str_opt(args, "line").unwrap_or_default();
        let want_present = arg_str_opt(args, "state").unwrap_or("present") == "present";
        let pattern = arg_str_opt(args, "regexp")
            .map(str::to_string)
            .unwrap_or_else(|| regex_escape(line));

        if !want_present {
            run_ok(ctx, &format!("sed -i -E {} {}", sh(&format!("/{}/d", pattern)), sh(path)))?;
            return Ok(Outcome::Changed);
        }
        if arg_bool(args, "create").unwrap_or(false) {
            run_ok(ctx, &format!("touch {}", sh(path)))?;
        }
        // 有匹配就替换,没有就追加 —— 一条命令覆盖两种情形,少一次往返。
        let cmd = format!(
            "if grep -qE {p} {f}; then sed -i -E {sub} {f}; else printf '%s\\n' {l} >> {f}; fi",
            p = sh(&pattern),
            f = sh(path),
            sub = sh(&format!("s|{}|{}|", pattern, line.replace('|', r"\|"))),
            l = sh(line)
        );
        run_ok(ctx, &cmd)?;
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        // 退役 = 把这一行拿掉,而不是删整个文件(那是别人的文件)。
        if !obs.present || obs.get("present") != Some("true") {
            return Ok(Outcome::Ok);
        }
        let mut a = args.clone();
        a.insert("state".into(), crate::eval::Yaml::from("absent"));
        self.apply(ctx, &a, &Change::Destroy)
    }
}

/// 传给 grep -E 的字面量转义。
fn regex_escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            if "\\^$.|?*+()[]{}".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

/// `unarchive` —— 把归档展开到目标目录。
///
/// 幂等靠 `creates:`(作者声明"展开后会有这个文件"):存在就跳过。
/// 这是**数据**,不是代码 —— 引擎不猜哪个文件代表展开成功。
pub struct Unarchive;

impl ResourceType for Unarchive {
    fn name(&self) -> &'static str {
        "unarchive"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let Some(creates) = arg_str_opt(args, "creates") else {
            // 没有 creates 就无从判定 —— 现实读不出来,如实报告。
            return Ok(Observed::default());
        };
        let (code, _) = ctx.probe(&format!("test -e {}", sh(creates)))?;
        Ok(if code == 0 {
            Observed::present([("creates", creates.to_string())])
        } else {
            Observed::absent()
        })
    }

    fn diff(&self, input: &DiffInput) -> Change {
        if arg_str_opt(input.args, "creates").is_none() {
            return Change::Unknown(
                "未声明 `creates:` —— 无法判断是否已展开(每次都会重跑)".into(),
            );
        }
        if input.observed.present {
            Change::Ok
        } else {
            Change::Create(vec![FieldDiff::set(
                "to",
                arg_str_opt(input.args, "to").unwrap_or("?"),
            )])
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _change: &Change) -> Result<Outcome> {
        let to = arg_str(args, "to")?;
        let strip = args.get("strip").and_then(|v| v.as_u64()).unwrap_or(0);
        run_ok(ctx, &format!("mkdir -p {}", sh(to)))?;

        // 归档要么已在目标上(`from:`),要么先把物料落到临时路径再解。
        let archive = match arg_str_opt(args, "from") {
            Some(p) => p.to_string(),
            None => {
                let name = arg_str(args, "material")?;
                let tmp = format!("/tmp/crater-unarchive-{name}");
                ctx.place_material(name, &tmp)?;
                tmp
            }
        };
        let strip_flag = if strip > 0 {
            format!(" --strip-components={strip}")
        } else {
            String::new()
        };
        run_ok(
            ctx,
            &format!("tar -xf {} -C {}{strip_flag}", sh(&archive), sh(to)),
        )?;
        if arg_str_opt(args, "from").is_none() {
            let _ = ctx.run(&format!("rm -f {}", sh(&archive)));
        }
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        // 只删 `creates:` 指的那个产物 —— `to:` 往往是 /usr/local 之类的共享目录,
        // 整个删掉会连累别人。宁可留下一点残余,也不越权。
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        let Some(creates) = arg_str_opt(args, "creates") else {
            return Ok(Outcome::Warn);
        };
        run_ok(ctx, &format!("rm -rf {}", sh(creates)))?;
        Ok(Outcome::Changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::eval::Yaml;

    fn args(pairs: &[(&str, &str)]) -> ResolvedArgs {
        pairs.iter().map(|(k, v)| (k.to_string(), Yaml::from(*v))).collect()
    }
    fn diff<T: ResourceType>(t: &T, a: &ResolvedArgs, o: &Observed, up: bool) -> Change {
        t.diff(&DiffInput { args: a, observed: o, upstream_changed: up })
    }

    // ---- lineinfile ----

    #[test]
    fn lineinfile_is_idempotent_when_the_line_already_matches() {
        let a = args(&[("path", "/etc/fstab"), ("line", "tmpfs /tmp tmpfs defaults 0 0")]);
        let obs = Observed::present([
            ("present", "true".into()),
            ("line", "tmpfs /tmp tmpfs defaults 0 0".into()),
        ]);
        assert_eq!(diff(&LineInFile, &a, &obs, false), Change::Ok);
    }

    #[test]
    fn lineinfile_replaces_a_drifted_line_and_shows_both_sides() {
        let a = args(&[("path", "/etc/x"), ("regexp", "^PORT="), ("line", "PORT=9000")]);
        let obs = Observed::present([("present", "true".into()), ("line", "PORT=80".into())]);
        let c = diff(&LineInFile, &a, &obs, false);
        assert_eq!(c.fields()[0].to_string(), "line: PORT=80 → PORT=9000");
    }

    #[test]
    fn lineinfile_without_create_on_a_missing_file_is_unknown_not_a_silent_success() {
        let a = args(&[("path", "/etc/nope"), ("line", "x")]);
        assert!(matches!(
            diff(&LineInFile, &a, &Observed::absent(), false),
            Change::Unknown(_)
        ));
    }

    #[test]
    fn lineinfile_absent_on_a_missing_file_is_already_satisfied() {
        let a = args(&[("path", "/etc/nope"), ("line", "x"), ("state", "absent")]);
        assert_eq!(diff(&LineInFile, &a, &Observed::absent(), false), Change::Ok);
    }

    #[test]
    fn lineinfile_escapes_regex_metacharacters_in_the_literal_form() {
        // 没写 regexp 时,line 是**字面量**:`a.b` 不该匹配 `axb`。
        assert_eq!(regex_escape("a.b*c"), r"a\.b\*c");
        let ctx = FakeCtx::new().on("test -f", 0, "").on("grep -E", 1, "");
        let a = args(&[("path", "/etc/x"), ("line", "a.b")]);
        LineInFile.observe(&ctx, &a).unwrap();
        let probe = ctx.calls()[1].text().to_string();
        assert!(probe.contains(r"a\.b"), "{probe}");
    }

    #[test]
    fn lineinfile_destroy_removes_the_line_not_the_file() {
        let ctx = FakeCtx::new().on("sed", 0, "");
        let a = args(&[("path", "/etc/fstab"), ("line", "x")]);
        let obs = Observed::present([("present", "true".into()), ("line", "x".into())]);
        LineInFile.destroy(&ctx, &a, &obs).unwrap();
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(cmds[0].starts_with("sed -i"), "{cmds:?}");
        assert!(!cmds.iter().any(|c| c.contains("rm ")), "别删别人的文件:{cmds:?}");
    }

    // ---- unarchive ----

    #[test]
    fn unarchive_uses_the_authors_creates_probe_for_idempotency() {
        let a = args(&[("material", "ctd"), ("to", "/usr/local"), ("creates", "/usr/local/bin/containerd")]);
        assert_eq!(
            diff(&Unarchive, &a, &Observed::present([("creates", "x".into())]), false),
            Change::Ok
        );
        assert!(matches!(
            diff(&Unarchive, &a, &Observed::absent(), false),
            Change::Create(_)
        ));
    }

    #[test]
    fn unarchive_without_creates_admits_it_cannot_tell() {
        // 引擎不猜"哪个文件代表展开成功" —— 那是产品知识,必须由作者声明。
        let a = args(&[("material", "x"), ("to", "/opt")]);
        let c = diff(&Unarchive, &a, &Observed::default(), false);
        assert!(matches!(c, Change::Unknown(_)), "{c:?}");
    }

    #[test]
    fn unarchive_places_the_material_then_extracts_then_cleans_up() {
        let ctx = FakeCtx::new().on("", 0, "");
        let a = args(&[("material", "ctd"), ("to", "/usr/local"), ("creates", "/usr/local/bin/containerd")]);
        Unarchive.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        let calls: Vec<String> = ctx.calls().iter().map(|c| format!("{c:?}")).collect();
        assert!(calls.iter().any(|c| c.contains("Place")), "{calls:?}");
        assert!(calls.iter().any(|c| c.contains("tar -xf")), "{calls:?}");
        assert!(calls.iter().any(|c| c.contains("rm -f")), "临时归档要清掉:{calls:?}");
    }

    #[test]
    fn unarchive_destroy_only_removes_what_it_created() {
        // `to:` 常是 /usr/local 这类共享目录 —— 整个删掉会连累别人。
        let ctx = FakeCtx::new().on("rm", 0, "");
        let a = args(&[("to", "/usr/local"), ("creates", "/usr/local/bin/containerd"), ("material", "x")]);
        Unarchive
            .destroy(&ctx, &a, &Observed::present([("creates", "x".into())]))
            .unwrap();
        let cmd = ctx.calls()[0].text().to_string();
        assert!(cmd.contains("/usr/local/bin/containerd"), "{cmd}");
        assert!(!cmd.ends_with("'/usr/local'"), "不该删整个 /usr/local:{cmd}");
    }

    #[test]
    fn unarchive_passes_strip_components_through() {
        let ctx = FakeCtx::new().on("", 0, "");
        let mut a = args(&[("from", "/tmp/x.tar"), ("to", "/opt")]);
        a.insert("strip".into(), Yaml::from(1));
        Unarchive.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        assert!(
            ctx.calls().iter().any(|c| c.text().contains("--strip-components=1")),
            "{:?}",
            ctx.calls()
        );
    }

    // ---- template ----

    #[test]
    fn template_admits_it_cannot_predict_rendered_content() {
        let a = args(&[("material", "cfg.j2"), ("dest", "/etc/app.conf")]);
        let obs = Observed::present([("sha256", "abc".into())]);
        assert!(matches!(diff(&Template, &a, &obs, false), Change::Unknown(_)));
        // 但上游变了就说得出来
        assert!(matches!(diff(&Template, &a, &obs, true), Change::Update(_)));
        assert!(matches!(
            diff(&Template, &a, &Observed::absent(), false),
            Change::Create(_)
        ));
    }
}
