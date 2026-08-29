//! `container` / `mount` / `cron` —— 三个"现实在别处"的类型。
//!
//! 共同的难点不在写,在**读**:容器的现实在 runtime 里,挂载的现实同时在
//! `/proc/mounts`(当下)和 `/etc/fstab`(重启后)两处,cron 的现实在一张
//! 纯文本表里且**没有天然主键**。observe 是强制的,所以这三处现实都必须被
//! 读清楚 —— 读不清就只能报 `?`,那等于把这三个类型做成写完就忘的 shell。

use anyhow::Result;

use crate::builtins::file::run_ok;
use crate::eval::{ResolvedArgs, Yaml};
use crate::verbs::*;

const SEP: &str = "\u{1}";

/// 取列表参数,元素一律转成字符串。
fn arg_list(args: &ResolvedArgs, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Yaml::Sequence(items)) => items.iter().map(yaml_to_string).collect(),
        Some(one) => vec![yaml_to_string(one)],
        None => Vec::new(),
    }
}

fn arg_map(args: &ResolvedArgs, key: &str) -> Vec<(String, String)> {
    match args.get(key) {
        Some(Yaml::Mapping(m)) => m
            .iter()
            .map(|(k, v)| (yaml_to_string(k), yaml_to_string(v)))
            .collect(),
        _ => Vec::new(),
    }
}

fn yaml_to_string(v: &Yaml) -> String {
    match v {
        Yaml::String(s) => s.clone(),
        other => serde_yaml::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

// ============================================================ container

/// `container` —— 目标运行时上的一个容器。
///
/// **幂等的判据是镜像 + 状态,不是"容器名存在"**:同名容器跑着旧镜像是最常见的
/// 现实,把它当 noop 会让"升级镜像"这条最普通的诉求悄悄失效。镜像变了就
/// 重建(docker 没有原地换镜像这回事)。
pub struct Container;

fn runtime_of(args: &ResolvedArgs) -> String {
    arg_str_opt(args, "runtime").unwrap_or("docker").to_string()
}

impl ResourceType for Container {
    fn name(&self) -> &'static str {
        "container"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let name = arg_str(args, "name")?;
        let rt = runtime_of(args);
        // 一次 inspect 读全:存在性 / 运行中 / 当前镜像 / 重启策略。
        // `{{.Image}}` 是解析后的 image ID,比不上作者写的 tag 可比 —— 用
        // `.Config.Image`(创建时的 ref),这才是"我要的镜像"的同一坐标系。
        let cmd = format!(
            "{rt} inspect {n} --format \
             '{{{{.State.Running}}}}{SEP}{{{{.Config.Image}}}}{SEP}{{{{.HostConfig.RestartPolicy.Name}}}}' \
             2>/dev/null",
            n = sh(name)
        );
        let (code, out) = ctx.probe(&cmd)?;
        if code != 0 || out.trim().is_empty() {
            return Ok(Observed::absent());
        }
        let p: Vec<&str> = out.trim().split(SEP).collect();
        Ok(Observed::present([
            ("running", p.first().unwrap_or(&"false").trim().to_string()),
            ("image", p.get(1).unwrap_or(&"").trim().to_string()),
            ("restart_policy", p.get(2).unwrap_or(&"").trim().to_string()),
        ]))
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let want_state = arg_str_opt(input.args, "state").unwrap_or("started");
        let want_image = arg_str_opt(input.args, "image");
        let obs = input.observed;

        if !obs.present {
            return match want_state {
                "absent" => Change::Ok,
                _ => Change::Create(vec![
                    FieldDiff::set("image", want_image.unwrap_or("(未声明 image)")),
                    FieldDiff::set("state", want_state),
                ]),
            };
        }
        if want_state == "absent" {
            return Change::Destroy;
        }

        let running = obs.get("running") == Some("true");
        let have_image = obs.get("image").unwrap_or_default();

        // 镜像变了 → 重建。docker 没有原地换镜像,所以这不是"更新一个字段",
        // 是"这个容器要被换掉";照实说,别让人以为是轻量改动。
        if let Some(want) = want_image {
            if !image_matches(have_image, want) {
                return Change::Update(vec![FieldDiff::change("image", have_image, want)]);
            }
        }
        // 上游变了(挂载进去的配置、镜像物料…)同样意味着重建 —— 与 service
        // 的 upstream_changed 是同一条裁定,不需要作者写 notify。
        if input.upstream_changed {
            return Change::Update(vec![FieldDiff::change("container", "(上游已变)", "重建")]);
        }

        let mut fields = Vec::new();
        match (want_state, running) {
            ("started", false) => fields.push(FieldDiff::change("state", "stopped", "running")),
            ("stopped", true) => fields.push(FieldDiff::change("state", "running", "stopped")),
            ("started", true) | ("stopped", false) => {}
            (other, _) => return Change::Unknown(format!("未知 state `{other}`")),
        }
        if let Some(want) = arg_str_opt(input.args, "restart_policy") {
            let have = obs.get("restart_policy").unwrap_or_default();
            if have != want {
                fields.push(FieldDiff::change("restart_policy", have, want));
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
        let rt = runtime_of(args);
        let want_state = arg_str_opt(args, "state").unwrap_or("started");

        // 只改运行状态(镜像没动)→ start/stop 就够,不必重建。
        let only_state = change.fields().iter().all(|f| f.field == "state");
        if only_state && !change.fields().is_empty() {
            let verb = if want_state == "started" { "start" } else { "stop" };
            run_ok(ctx, &format!("{rt} {verb} {}", sh(name)))?;
            return Ok(Outcome::Changed);
        }

        // 其余一律重建:先删旧的(可能不存在,所以 `|| true`),再按声明起新的。
        run_ok(ctx, &format!("{rt} rm -f {} >/dev/null 2>&1 || true", sh(name)))?;
        if want_state == "stopped" {
            run_ok(ctx, &format!("{rt} create {}", run_flags(args)?))?;
            return Ok(Outcome::Changed);
        }
        run_ok(ctx, &format!("{rt} run -d {}", run_flags(args)?))?;
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        let rt = runtime_of(args);
        run_ok(ctx, &format!("{rt} rm -f {}", sh(arg_str(args, "name")?)))?;
        Ok(Outcome::Changed)
    }
}

/// `nginx` 与 `nginx:latest` 是同一个镜像,`docker inspect` 回的是后者。
/// 不做这层归一,每次 plan 都会报一条不存在的镜像变更。
fn image_matches(have: &str, want: &str) -> bool {
    have == want || normalize_image(have) == normalize_image(want)
}

fn normalize_image(r: &str) -> String {
    if r.contains('@') {
        return r.to_string(); // digest 固定,不补 tag
    }
    // 冒号出现在最后一个 `/` 之后才是 tag(否则是 registry 端口)。
    let has_tag = match (r.rfind(':'), r.rfind('/')) {
        (Some(c), Some(s)) => c > s,
        (Some(_), None) => true,
        _ => false,
    };
    if has_tag { r.to_string() } else { format!("{r}:latest") }
}

/// 组装 `docker run` 的参数。顺序固定 —— apply 是幂等契约的一部分,
/// 同样的声明必须每次组出同样的命令。
fn run_flags(args: &ResolvedArgs) -> Result<String> {
    let mut out = vec![format!("--name {}", sh(arg_str(args, "name")?))];
    if let Some(p) = arg_str_opt(args, "restart_policy") {
        out.push(format!("--restart {}", sh(p)));
    }
    for p in arg_list(args, "ports") {
        out.push(format!("-p {}", sh(&p)));
    }
    for v in arg_list(args, "volumes") {
        out.push(format!("-v {}", sh(&v)));
    }
    for (k, v) in arg_map(args, "env") {
        out.push(format!("-e {}", sh(&format!("{k}={v}"))));
    }
    for extra in arg_list(args, "args") {
        out.push(extra); // 逃生舱:原样透传,不加引号(作者自己负责)
    }
    let image = arg_str_opt(args, "image")
        .ok_or_else(|| anyhow::anyhow!("container 缺少 `image:`(state 非 absent 时必需)"))?;
    out.push(sh(image));
    if let Some(cmd) = arg_str_opt(args, "command") {
        out.push(cmd.to_string());
    }
    Ok(out.join(" "))
}

// ============================================================ mount

/// `mount` —— 挂载点。
///
/// 现实分两处:`/proc/mounts` 是**当下**挂没挂,`/etc/fstab` 是**重启后**还挂不挂。
/// 只看前者会让"重启就丢"的挂载显示为绿灯 —— 那是最伤人的一种假绿灯,
/// 因为它要等到下一次重启才暴露。所以两处都读,`persist` 单独成一个字段。
pub struct Mount;

impl ResourceType for Mount {
    fn name(&self) -> &'static str {
        "mount"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let path = arg_str(args, "path")?;
        // loop 挂载的 SOURCE 是 `/dev/loop0`,而作者写的是镜像文件路径 ——
        // 不还原成后备文件就会永远判定"源变了",于是每次 apply 都要重挂一次
        // (而它已经挂着,mount 直接失败)。`losetup -O BACK-FILE` 给出真身。
        let cmd = format!(
            "s=$(findmnt -n -o SOURCE --target {p} 2>/dev/null | head -1); \
             case \"$s\" in /dev/loop*) b=$(losetup -nO BACK-FILE \"$s\" 2>/dev/null | sed 's/ (deleted)$//'); \
               [ -n \"$b\" ] && s=$b;; esac; \
             printf '%s' \"$s\"; printf '{SEP}'; \
             findmnt -n -o FSTYPE --target {p} 2>/dev/null | head -1; \
             printf '{SEP}'; \
             findmnt -n -o TARGET --target {p} 2>/dev/null | head -1; \
             printf '{SEP}'; \
             grep -E \"^[^#]*[[:space:]]{p_re}[[:space:]]\" /etc/fstab 2>/dev/null | head -1",
            p = sh(path),
            // fstab 的第二列是挂载点;这里只做最小转义,足够匹配普通路径。
            p_re = path.replace('.', r"\.")
        );
        let (_, out) = ctx.probe(&cmd)?;
        let p: Vec<&str> = out.split(SEP).collect();
        let src = p.first().map(|s| s.trim()).unwrap_or_default();
        let fstype = p.get(1).map(|s| s.trim()).unwrap_or_default();
        // findmnt --target 会回落到**父挂载点**(/data 没挂时回 /)。
        // 不比对 TARGET 就会把"没挂"读成"挂着 /" —— 一个静默的假绿灯。
        let target = p.get(2).map(|s| s.trim()).unwrap_or_default();
        let in_fstab = !p.get(3).map(|s| s.trim()).unwrap_or_default().is_empty();
        let mounted = !src.is_empty() && target == path;

        if !mounted && !in_fstab {
            return Ok(Observed::absent());
        }
        Ok(Observed::present([
            ("mounted", mounted.to_string()),
            ("src", if mounted { src.to_string() } else { String::new() }),
            ("fstype", if mounted { fstype.to_string() } else { String::new() }),
            ("persist", in_fstab.to_string()),
        ]))
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let want_state = arg_str_opt(input.args, "state").unwrap_or("mounted");
        let want_persist = arg_bool(input.args, "persist").unwrap_or(false);
        let obs = input.observed;

        if want_state == "absent" {
            return if obs.present { Change::Destroy } else { Change::Ok };
        }
        let mounted = obs.present && obs.get("mounted") == Some("true");
        let persisted = obs.present && obs.get("persist") == Some("true");

        if !obs.present {
            return match want_state {
                "unmounted" => Change::Ok,
                _ => Change::Create(vec![
                    FieldDiff::set("src", arg_str_opt(input.args, "src").unwrap_or("?")),
                    FieldDiff::set("state", "mounted"),
                ]),
            };
        }

        let mut fields = Vec::new();
        match (want_state, mounted) {
            ("mounted", false) => fields.push(FieldDiff::change("state", "unmounted", "mounted")),
            ("unmounted", true) => fields.push(FieldDiff::change("state", "mounted", "unmounted")),
            _ => {}
        }
        if mounted && want_state == "mounted" {
            // src / fstype 变了意味着要重挂,不是改一个字段 —— 照实说。
            if let Some(want) = arg_str_opt(input.args, "src") {
                let have = obs.get("src").unwrap_or_default();
                if !have.is_empty() && have != want {
                    fields.push(FieldDiff::change("src", have, want));
                }
            }
            if let Some(want) = arg_str_opt(input.args, "fstype") {
                let have = obs.get("fstype").unwrap_or_default();
                if !have.is_empty() && have != want {
                    fields.push(FieldDiff::change("fstype", have, want));
                }
            }
        }
        // 重启后还在不在,是独立于"当下挂没挂"的一件事。
        if want_persist != persisted {
            fields.push(FieldDiff::change(
                "persist",
                persisted.to_string(),
                want_persist.to_string(),
            ));
        }
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _change: &Change) -> Result<Outcome> {
        let path = arg_str(args, "path")?;
        let state = arg_str_opt(args, "state").unwrap_or("mounted");
        let persist = arg_bool(args, "persist").unwrap_or(false);

        if state == "unmounted" {
            run_ok(ctx, &format!("umount {} 2>/dev/null || true", sh(path)))?;
            return Ok(Outcome::Changed);
        }

        let src = arg_str(args, "src")?;
        let fstype = arg_str(args, "fstype")?;
        let opts = arg_str_opt(args, "opts").unwrap_or("defaults");

        // fstab 先写、再 `mount <path>`:让内核按 fstab 那一行去挂,
        // 于是"当下挂着的"和"重启后会挂的"必然是同一份参数 —— 两者不一致
        // 正是这个类型最容易制造的陷阱。
        if persist {
            let line = format!("{src}\t{path}\t{fstype}\t{opts}\t0 0");
            run_ok(
                ctx,
                &format!(
                    "sed -i {del} /etc/fstab && printf '%s\\n' {line} >> /etc/fstab",
                    del = sh(&format!("\\#[[:space:]]{path}[[:space:]]#d")),
                    line = sh(&line)
                ),
            )?;
        }
        run_ok(ctx, &format!("mkdir -p {}", sh(path)))?;
        let cmd = if persist {
            format!("mount {}", sh(path))
        } else {
            format!("mount -t {} -o {} {} {}", sh(fstype), sh(opts), sh(src), sh(path))
        };
        run_ok(ctx, &cmd)?;
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        let path = arg_str(args, "path")?;
        run_ok(ctx, &format!("umount {} 2>/dev/null || true", sh(path)))?;
        run_ok(
            ctx,
            &format!("sed -i {} /etc/fstab", sh(&format!("\\#[[:space:]]{path}[[:space:]]#d"))),
        )?;
        Ok(Outcome::Changed)
    }
}

// ============================================================ cron

/// `cron` —— 定时任务。
///
/// crontab 没有主键,所以幂等要靠**约定**:每条任务前面加一行
/// `# crater:<name>`,增删改都以它定位。这与 Ansible 的做法一致,
/// 且是唯一能在纯文本表里做到幂等的办法 —— 靠匹配命令行本身,
/// 改一次命令就会留下一条孤儿任务。
pub struct Cron;

fn cron_marker(name: &str) -> String {
    format!("# crater:{name}")
}

/// 目标用户的 crontab 读写命令对。
fn crontab_cmds(args: &ResolvedArgs) -> (String, String) {
    match arg_str_opt(args, "user") {
        Some(u) => (format!("crontab -l -u {}", sh(u)), format!("crontab -u {} -", sh(u))),
        None => ("crontab -l".into(), "crontab -".into()),
    }
}

impl ResourceType for Cron {
    fn name(&self) -> &'static str {
        "cron"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let name = arg_str(args, "name")?;
        let (list, _) = crontab_cmds(args);
        // 标记行的**下一行**就是任务本体。`-A 1` 拿到它,再取最后一行。
        let (_, out) = ctx.probe(&format!(
            "{list} 2>/dev/null | grep -A 1 -F {m} | tail -1",
            m = sh(&cron_marker(name))
        ))?;
        let line = out.trim();
        if line.is_empty() || line == cron_marker(name) {
            return Ok(Observed::absent());
        }
        // "schedule 命令"—— 前五段是时间,其余是命令(@reboot 之类只有一段)。
        let (schedule, job) = split_cron_line(line);
        Ok(Observed::present([("schedule", schedule), ("job", job)]))
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let want_state = arg_str_opt(input.args, "state").unwrap_or("present");
        let obs = input.observed;

        if want_state == "absent" {
            return if obs.present { Change::Destroy } else { Change::Ok };
        }
        let want_schedule = arg_str_opt(input.args, "schedule").unwrap_or("0 0 * * *");
        let want_job = arg_str_opt(input.args, "job").unwrap_or_default();

        if !obs.present {
            return Change::Create(vec![
                FieldDiff::set("schedule", want_schedule),
                FieldDiff::set("job", want_job),
            ]);
        }
        let mut fields = Vec::new();
        let have_s = obs.get("schedule").unwrap_or_default();
        let have_j = obs.get("job").unwrap_or_default();
        if have_s != want_schedule {
            fields.push(FieldDiff::change("schedule", have_s, want_schedule));
        }
        if have_j != want_job {
            fields.push(FieldDiff::change("job", have_j, want_job));
        }
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _change: &Change) -> Result<Outcome> {
        let name = arg_str(args, "name")?;
        if arg_str_opt(args, "state") == Some("absent") {
            return self.destroy(ctx, args, &Observed::present([]));
        }
        let schedule = arg_str_opt(args, "schedule").unwrap_or("0 0 * * *");
        let job = arg_str(args, "job")?;
        let (list, write) = crontab_cmds(args);
        let marker = cron_marker(name);

        // 先整段删掉旧的(标记行 + 它的下一行),再追加新的。
        // 一次写入完成,中途没有"任务已消失"的时间窗。
        let block = format!("{marker}\n{schedule} {job}");
        run_ok(
            ctx,
            &format!(
                "{{ {list} 2>/dev/null || true; }} | grep -v -F {m} | \
                 sed {del} | {{ cat; printf '%s\\n' {b}; }} | {write}",
                m = sh(&marker),
                // 删掉紧跟标记行的那一行:标记已被 grep -v 去掉,这里按内容再兜一次,
                // 防止历史遗留(有标记无任务 / 有任务无标记)残留半行。
                del = sh(&format!("\\#^[^#]*{}#d", escape_sed(job))),
                b = sh(&block)
            ),
        )?;
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        let name = arg_str(args, "name")?;
        let (list, write) = crontab_cmds(args);
        // 删标记行及其下一行(`$!N;/…/D` 是 sed 里"删掉匹配行和它下一行"的定式)。
        run_ok(
            ctx,
            &format!(
                "{{ {list} 2>/dev/null || true; }} | sed {expr} | {write}",
                expr = sh(&format!("/^{}$/,+1d", escape_sed(&cron_marker(name))))
            ),
        )?;
        Ok(Outcome::Changed)
    }
}

/// crontab 一行拆成"时间"和"命令"。`@reboot`/`@daily` 这类只占一段。
fn split_cron_line(line: &str) -> (String, String) {
    if line.starts_with('@') {
        let mut it = line.splitn(2, char::is_whitespace);
        let s = it.next().unwrap_or_default().to_string();
        return (s, it.next().unwrap_or_default().trim().to_string());
    }
    let mut fields = line.split_whitespace();
    let schedule: Vec<&str> = (0..5).filter_map(|_| fields.next()).collect();
    let rest = fields.collect::<Vec<_>>().join(" ");
    (schedule.join(" "), rest)
}

fn escape_sed(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '.' | '*' | '[' | ']' | '^' | '$' | '\\' | '/' | '#' => format!("\\{c}"),
            other => other.to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::FakeCtx;

    fn args(pairs: &[(&str, &str)]) -> ResolvedArgs {
        pairs.iter().map(|(k, v)| (k.to_string(), Yaml::from(*v))).collect()
    }

    fn diff_of(t: &dyn ResourceType, a: &ResolvedArgs, obs: &Observed) -> Change {
        t.diff(&DiffInput { args: a, observed: obs, upstream_changed: false })
    }

    // ------------------------------------------------------------ container

    #[test]
    fn a_container_running_an_older_image_is_not_a_noop() {
        // 同名容器跑着旧镜像是最常见的现实。按"名字在就算达标"判,
        // "升级镜像"这条最普通的诉求会悄悄失效。
        let ctx = FakeCtx::new().on("docker inspect", 0, &format!("true{SEP}nginx:1.24{SEP}no"));
        let a = args(&[("name", "web"), ("image", "nginx:1.25")]);
        let obs = Container.observe(&ctx, &a).unwrap();
        match diff_of(&Container, &a, &obs) {
            Change::Update(f) => assert_eq!(f[0].to_string(), "image: nginx:1.24 → nginx:1.25"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_untagged_image_matches_its_latest_tag() {
        // `docker inspect` 回 `nginx:latest`,作者写的是 `nginx`。
        // 不归一,每次 plan 都报一条不存在的镜像变更。
        let ctx = FakeCtx::new().on("docker inspect", 0, &format!("true{SEP}nginx:latest{SEP}no"));
        let a = args(&[("name", "web"), ("image", "nginx")]);
        let obs = Container.observe(&ctx, &a).unwrap();
        assert_eq!(diff_of(&Container, &a, &obs), Change::Ok);
    }

    #[test]
    fn a_registry_port_is_not_mistaken_for_a_tag() {
        assert_eq!(normalize_image("reg.io:5000/app"), "reg.io:5000/app:latest");
        assert_eq!(normalize_image("reg.io:5000/app:v1"), "reg.io:5000/app:v1");
        assert_eq!(normalize_image("app@sha256:abc"), "app@sha256:abc");
    }

    #[test]
    fn observing_a_container_writes_nothing() {
        let ctx = FakeCtx::new().on("docker inspect", 0, &format!("true{SEP}nginx{SEP}no"));
        Container.observe(&ctx, &args(&[("name", "web")])).unwrap();
        assert!(ctx.writes().is_empty(), "{:?}", ctx.writes());
    }

    #[test]
    fn a_stopped_container_is_started_not_recreated() {
        // 只差运行状态就 start —— 重建会丢掉容器内的可写层,代价完全不同。
        let ctx = FakeCtx::new().on("docker inspect", 0, &format!("false{SEP}nginx:1.25{SEP}no"));
        let a = args(&[("name", "web"), ("image", "nginx:1.25")]);
        let obs = Container.observe(&ctx, &a).unwrap();
        let change = diff_of(&Container, &a, &obs);
        let run = FakeCtx::new().on("", 0, "");
        Container.apply(&run, &a, &change).unwrap();
        let cmds: Vec<String> = run.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(cmds.iter().any(|c| c.contains("docker start")), "{cmds:?}");
        assert!(!cmds.iter().any(|c| c.contains("docker run")), "不该重建:{cmds:?}");
    }

    #[test]
    fn recreating_a_container_removes_the_old_one_first() {
        let ctx = FakeCtx::new().on("", 0, "");
        let a = args(&[("name", "web"), ("image", "nginx:1.25")]);
        Container
            .apply(&ctx, &a, &Change::Update(vec![FieldDiff::change("image", "a", "b")]))
            .unwrap();
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        let rm = cmds.iter().position(|c| c.contains("rm -f")).expect("应先删旧的");
        let run = cmds.iter().position(|c| c.contains("run -d")).expect("再起新的");
        assert!(rm < run, "{cmds:?}");
    }

    #[test]
    fn run_flags_are_assembled_in_a_stable_order() {
        // apply 是幂等契约的一部分:同样的声明必须每次组出同样的命令。
        let mut a = args(&[("name", "web"), ("image", "nginx"), ("restart_policy", "always")]);
        a.insert("ports".into(), serde_yaml::from_str("['80:80', '443:443']").unwrap());
        a.insert("env".into(), serde_yaml::from_str("{B: 2, A: 1}").unwrap());
        let once = run_flags(&a).unwrap();
        assert_eq!(once, run_flags(&a).unwrap());
        assert!(once.starts_with("--name 'web' --restart 'always' -p '80:80'"), "{once}");
        assert!(once.ends_with("'nginx'"), "镜像必须在参数之后:{once}");
    }

    #[test]
    fn a_container_without_an_image_fails_loudly_at_apply() {
        let ctx = FakeCtx::new().on("", 0, "");
        let err = Container
            .apply(&ctx, &args(&[("name", "web")]), &Change::Create(vec![]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("image"), "{err}");
    }

    #[test]
    fn destroying_an_absent_container_issues_no_command() {
        let ctx = FakeCtx::new();
        assert_eq!(
            Container.destroy(&ctx, &args(&[("name", "web")]), &Observed::absent()).unwrap(),
            Outcome::Ok
        );
        assert!(ctx.calls().is_empty());
    }

    // ------------------------------------------------------------ mount

    #[test]
    fn a_mount_that_would_not_survive_a_reboot_is_not_green() {
        // 最伤人的假绿灯:当下挂着,fstab 里没有,要等下次重启才暴露。
        let ctx = FakeCtx::new()
            .on("s=$(findmnt", 0, &format!("/dev/sdb1{SEP}ext4{SEP}/data{SEP}"));
        let mut a = args(&[("path", "/data"), ("src", "/dev/sdb1"), ("fstype", "ext4")]);
        a.insert("persist".into(), Yaml::from(true));
        let obs = Mount.observe(&ctx, &a).unwrap();
        match diff_of(&Mount, &a, &obs) {
            Change::Update(f) => {
                assert!(f.iter().any(|d| d.field == "persist"), "{f:?}");
            }
            other => panic!("挂着但不持久,不该是 {other:?}"),
        }
    }

    #[test]
    fn findmnt_falling_back_to_a_parent_mount_is_not_read_as_mounted() {
        // `findmnt --target /data` 在 /data 没挂时会回落到 `/`。
        // 不比对 TARGET 就会把"没挂"读成"挂着 /" —— 静默的假绿灯。
        let ctx = FakeCtx::new().on("s=$(findmnt", 0, &format!("/dev/sda1{SEP}ext4{SEP}/{SEP}"));
        let a = args(&[("path", "/data"), ("src", "/dev/sdb1"), ("fstype", "ext4")]);
        let obs = Mount.observe(&ctx, &a).unwrap();
        assert!(!obs.present, "回落到父挂载点被当成了已挂载:{obs:?}");
    }

    #[test]
    fn a_loop_mount_is_compared_by_its_backing_file_not_the_loop_device() {
        // findmnt 对 loop 挂载回报 /dev/loop0,而作者写的是镜像路径。
        // 不还原成后备文件就会永远判"源变了",每次 apply 都要重挂一次 ——
        // 而它已经挂着,mount 直接失败。探针里已用 losetup -O BACK-FILE 还原,
        // 所以这里的夹具给出的就是还原后的值。
        let ctx = FakeCtx::new().on(
            "s=$(findmnt",
            0,
            &format!("/var/lib/pgdisk.img{SEP}ext4{SEP}/srv/pgdata{SEP}/var/lib/pgdisk.img /srv/pgdata ext4"),
        );
        let mut a = args(&[("path", "/srv/pgdata"), ("src", "/var/lib/pgdisk.img"), ("fstype", "ext4")]);
        a.insert("persist".into(), Yaml::from(true));
        let obs = Mount.observe(&ctx, &a).unwrap();
        assert_eq!(diff_of(&Mount, &a, &obs), Change::Ok, "{obs:?}");
    }

    #[test]
    fn a_fully_satisfied_mount_is_a_noop() {
        let ctx = FakeCtx::new()
            .on("s=$(findmnt", 0, &format!("/dev/sdb1{SEP}ext4{SEP}/data{SEP}/dev/sdb1 /data ext4"));
        let mut a = args(&[("path", "/data"), ("src", "/dev/sdb1"), ("fstype", "ext4")]);
        a.insert("persist".into(), Yaml::from(true));
        let obs = Mount.observe(&ctx, &a).unwrap();
        assert_eq!(diff_of(&Mount, &a, &obs), Change::Ok, "{obs:?}");
    }

    #[test]
    fn a_persistent_mount_is_written_to_fstab_before_being_mounted() {
        // 顺序不是风格问题:先写 fstab 再 `mount <path>`,内核就按 fstab 那一行挂,
        // "当下挂着的"与"重启后会挂的"必然是同一份参数。
        let ctx = FakeCtx::new().on("", 0, "");
        let mut a = args(&[("path", "/data"), ("src", "/dev/sdb1"), ("fstype", "ext4")]);
        a.insert("persist".into(), Yaml::from(true));
        Mount.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        let fstab = cmds.iter().position(|c| c.contains("/etc/fstab")).expect("写 fstab");
        let mount = cmds.iter().rposition(|c| c.starts_with("mount ")).expect("挂载");
        assert!(fstab < mount, "{cmds:?}");
        assert!(cmds[mount] == "mount '/data'", "持久挂载应让内核读 fstab:{}", cmds[mount]);
    }

    #[test]
    fn a_transient_mount_passes_its_options_on_the_command_line() {
        let ctx = FakeCtx::new().on("", 0, "");
        let a = args(&[("path", "/mnt/x"), ("src", "//srv/s"), ("fstype", "cifs"), ("opts", "ro")]);
        Mount.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        let cmds: Vec<String> = ctx.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(cmds.iter().any(|c| c.contains("-t 'cifs' -o 'ro'")), "{cmds:?}");
        assert!(!cmds.iter().any(|c| c.contains("/etc/fstab")), "没要求持久就别动 fstab");
    }

    // ------------------------------------------------------------ cron

    #[test]
    fn a_cron_job_is_located_by_its_marker_not_by_its_command() {
        // 靠匹配命令行本身来定位,改一次命令就会留下一条孤儿任务。
        // 探针必须按标记检索 —— 这是幂等能成立的全部依据。
        let ctx = FakeCtx::new().on("crontab -l", 0, "");
        let a = args(&[("name", "backup"), ("job", "/usr/bin/backup")]);
        let obs = Cron.observe(&ctx, &a).unwrap();
        assert!(!obs.present, "标记检索无果就该视为不存在");
        let probe = ctx.calls()[0].text().to_string();
        assert!(probe.contains("# crater:backup") && probe.contains("grep -A 1"), "{probe}");
    }

    #[test]
    fn a_marker_with_no_job_after_it_is_absent_not_a_ghost_job() {
        // 历史遗留:标记行在文件末尾、后面没有任务。`grep -A 1 | tail -1`
        // 此时回的是标记行本身 —— 不识别就会把它当成一条命令是 "# crater:x" 的任务。
        let ctx = FakeCtx::new().on("crontab -l", 0, "# crater:backup\n");
        let obs = Cron.observe(&ctx, &args(&[("name", "backup"), ("job", "j")])).unwrap();
        assert!(!obs.present, "{obs:?}");
    }

    #[test]
    fn an_existing_job_with_a_changed_schedule_is_an_update() {
        // 夹具给的是 `grep -A 1 <标记> | tail -1` 的产物,即任务本体那一行。
        let ctx = FakeCtx::new().on("crontab -l", 0, "0 3 * * * /usr/bin/backup\n");
        let a = args(&[("name", "backup"), ("job", "/usr/bin/backup"), ("schedule", "0 5 * * *")]);
        let obs = Cron.observe(&ctx, &a).unwrap();
        match diff_of(&Cron, &a, &obs) {
            Change::Update(f) => assert_eq!(f[0].to_string(), "schedule: 0 3 * * * → 0 5 * * *"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_identical_job_is_a_noop() {
        let ctx = FakeCtx::new().on("crontab -l", 0, "0 3 * * * /usr/bin/backup\n");
        let a = args(&[("name", "backup"), ("job", "/usr/bin/backup"), ("schedule", "0 3 * * *")]);
        let obs = Cron.observe(&ctx, &a).unwrap();
        assert_eq!(diff_of(&Cron, &a, &obs), Change::Ok);
    }

    #[test]
    fn a_shorthand_schedule_keeps_the_whole_command() {
        // `@reboot` 只占一段;按"前五段是时间"硬拆会把命令切掉四个词。
        assert_eq!(
            split_cron_line("@reboot /usr/bin/x --a --b --c --d"),
            ("@reboot".into(), "/usr/bin/x --a --b --c --d".into())
        );
        assert_eq!(
            split_cron_line("0 3 * * * /usr/bin/x --flag"),
            ("0 3 * * *".into(), "/usr/bin/x --flag".into())
        );
    }

    #[test]
    fn writing_a_cron_job_happens_in_one_pass() {
        // 分两次(先删后加)会留下"任务已消失"的时间窗。
        let ctx = FakeCtx::new().on("", 0, "");
        let a = args(&[("name", "backup"), ("job", "/usr/bin/backup"), ("schedule", "0 3 * * *")]);
        Cron.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        assert_eq!(ctx.calls().len(), 1, "{:?}", ctx.calls());
        let cmd = ctx.calls()[0].text().to_string();
        assert!(cmd.contains("crontab -") && cmd.contains("# crater:backup"), "{cmd}");
    }

    #[test]
    fn a_per_user_crontab_is_addressed_with_u() {
        let a = args(&[("name", "x"), ("job", "y"), ("user", "deploy")]);
        let (list, write) = crontab_cmds(&a);
        assert_eq!(list, "crontab -l -u 'deploy'");
        assert_eq!(write, "crontab -u 'deploy' -");
    }

    #[test]
    fn destroying_an_absent_job_issues_no_command() {
        let ctx = FakeCtx::new();
        assert_eq!(
            Cron.destroy(&ctx, &args(&[("name", "x")]), &Observed::absent()).unwrap(),
            Outcome::Ok
        );
        assert!(ctx.calls().is_empty());
    }
}
