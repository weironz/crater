//! 裸名字:`crater apply yq` 里的 `yq` 是什么。
//!
//! 最短的那条命令必须能用。`crater install yq` 之后,后续每一次收敛、预演、
//! 对账、退役都不该再重复一遍"蓝图在哪、机群在哪、参数是什么" —— 那三样
//! `install` 已经写进 `yq.app.yaml` 了,而那个文件就是**这次安装的正身**。
//!
//! 判据是 **`<source>` 是不是一个存在的文件**:
//!
//! - 是文件 → 蓝图/栈,原样走(这条路先于本模块,见 `main.rs`)
//! - 不是文件、而 `<name>.app.yaml` 在 → 从它取蓝图、机群、参数
//! - 两者都不在 → 交给包这条路:去仓库索引里找这个名字,拉下来再跑
//!
//! 第三条是 helm 那种用法(`crater repo add` 之后 `crater apply yq` 直接
//! 从远端拉)。它**不在本模块**实现 —— `pkg::install` 已经把"名字 → 索引 →
//! 拉包 → 契约对账 → 落 app 文件 → 出计划"整条走通了,本模块只负责判断
//! "本地有没有",没有就让路。
//!
//! 走远端那条时,**计划照印**:先看见会变什么,再收敛。这不是多余的一步 ——
//! 拉下来的字节是别人做的,而它下一步要改的是生产机。

use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::target::TargetOpts;

/// 一份蓝图,以及跑它需要的机群与参数。
#[derive(Debug)]
pub(crate) struct Resolved {
    pub(crate) blueprint: PathBuf,
    pub(crate) target: TargetOpts,
    pub(crate) sets: Vec<String>,
}

/// `<name>` → 本地那份任务。命令行给的**盖在** app 文件之上。
///
/// 返回 `None` 表示"这里没有这个名字的任务" —— 不是错误,而是让调用方去走
/// 远端那条路。
pub(crate) fn resolve(
    name: &str,
    cli: &TargetOpts,
    cli_sets: &[String],
) -> Result<Option<Resolved>> {
    resolve_in(Path::new("."), name, cli, cli_sets)
}

/// 同上,但在指定目录里找 —— 测试用得着,而且不必去动进程级的 cwd
/// (那是全局状态,并行跑的测试会互相踩)。
pub(crate) fn resolve_in(
    dir: &Path,
    name: &str,
    cli: &TargetOpts,
    cli_sets: &[String],
) -> Result<Option<Resolved>> {
    let app = dir.join(format!("{name}.app.yaml"));
    if !app.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&app)?;
    let def = crate::ui_app::parse_app(&app, &text)
        .map_err(|e| anyhow::anyhow!("{} 读不了:{e}", app.display()))?;

    // app 文件里的路径相对它自己所在的目录 —— 不是相对当前工作目录。
    let blueprint = dir.join(&def.blueprint);
    if !blueprint.is_file() {
        bail!(
            "{} 指着 `{}`,但那个文件不在。\n\
             \n\
             它可能被移动或删掉了。改 {} 里的 `blueprint:` 指到实际位置,\n\
             或者重装一次:`crater install {name}`。",
            app.display(),
            def.blueprint,
            app.display()
        );
    }

    // 合并规则:命令行**盖住** app 文件。app 文件记的是"这次安装是什么样",
    // 命令行是"这一次我要不一样" —— 后者更具体,所以它赢。
    let mut target = cli.clone();
    if target.inventory.is_none() && !def.inventory.is_empty() {
        target.inventory = Some(dir.join(&def.inventory));
    }
    if target.limit.is_none() && !def.limit.is_empty() {
        target.limit = Some(def.limit.join(","));
    }

    // `--set` 后来者胜(`plan::with_overrides` 用的是 `insert`),所以把命令行
    // 的追加在后面,就是让它覆盖。
    let mut sets: Vec<String> = def.params.iter().map(|(k, v)| format!("{k}={v}")).collect();
    sets.extend_from_slice(cli_sets);

    Ok(Some(Resolved {
        blueprint,
        target,
        sets,
    }))
}

/// 这个目录下已经有哪些任务(`*.app.yaml`)。
///
/// 用在两处:本地找不到时先说清"这里有什么",以及远端也找不到时把两边的
/// 落空并成一条消息 —— 只说"仓库里没有"会让人以为是仓库配错了,而实情
/// 可能只是名字打错、而正确的那个就在眼前。
pub(crate) fn apps_in(dir: &Path) -> Vec<String> {
    let mut here: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.strip_suffix(".app.yaml").map(str::to_string)
        })
        .collect();
    here.sort();
    here
}

/// `<source>` 是不是"该去包那条路解决"的东西,是的话给出规范化后的形式。
///
/// 认三种,都对应 helm 有的用法:
///
/// - `oci://reg/ns/yq:1.0` —— 显式协议头(helm 3.8+ 的写法)。前缀剥掉,
///   因为下游 `ImageStore` 收的是裸引用
/// - `reg/ns/yq:1.0` —— 裸 OCI 引用。判据是**带 `/`**:引用一定有仓库路径
/// - `yq` / `yq:4.44.3` —— 包名,去已配仓库的索引里查
///
/// **不认**的:现存路径(那是文件,先于本模块处理),以及名字部分带 `.` 的
/// ——`web.blueprint.yam`(手滑少个 l)当成包名去查,只会给出一个答非所问的
/// 错误,而真正的原因是文件名打错了。
///
/// 名字后面的 `:版本` 先摘掉再判:`yq:4.44.3` 的版本号里全是点,不摘就会被
/// 当成文件名扔掉。
pub(crate) fn remote_ref(source: &str) -> Option<String> {
    let s = source.strip_prefix("oci://").unwrap_or(source);
    if s.is_empty() || Path::new(s).exists() {
        return None;
    }
    // **长得像路径的一律不碰**,哪怕那个文件此刻不在。OCI 引用不会以 `./`
    // `../` `/` `~` 开头 —— 而一个打错的路径被拿去连 registry,报的会是
    // "仓库里没有 ./web.yaml",把人引向仓库配置,而真正的问题是路径写错了。
    if s.starts_with('.') || s.starts_with('/') || s.starts_with('~') {
        return None;
    }
    // 带仓库路径的就是 OCI 引用,直接给下游
    if s.contains('/') || s.contains('\\') {
        return Some(s.to_string());
    }
    // 剩下的当包名:摘掉 `:版本` 再看名字部分像不像文件名
    let name = s.split_once(':').map(|(a, _)| a).unwrap_or(s);
    (!name.is_empty() && !name.contains('.')).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    /// 只为拿一份**默认值真实**的 TargetOpts:手写 `Default` 会和 clap 上的
    /// `default_value` 分家,分家之后测试就测的不是真实默认。
    #[derive(clap::Parser)]
    struct W {
        #[command(flatten)]
        t: TargetOpts,
    }
    fn bare_cli() -> TargetOpts {
        W::parse_from(["x"]).t
    }

    fn fixture() -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("web.blueprint.yaml"), "blueprint: {}\n").unwrap();
        std::fs::write(d.path().join("inv-app.yaml"), "").unwrap();
        std::fs::write(
            d.path().join("web.app.yaml"),
            "app:\n  name: web\n  blueprint: web.blueprint.yaml\n  \
             inventory: inv-app.yaml\n  params:\n    port: 80\n",
        )
        .unwrap();
        d
    }

    /// 什么都不给时,机群与参数全从 app 文件来 —— 这正是 `crater apply yq`
    /// 能成立的原因。
    #[test]
    fn a_bare_name_picks_up_the_fleet_and_params_from_the_app_file() {
        let d = fixture();
        let r = resolve_in(d.path(), "web", &bare_cli(), &[])
            .unwrap()
            .expect("app 文件在,应该解出来");
        assert_eq!(r.blueprint, d.path().join("web.blueprint.yaml"));
        assert_eq!(r.target.inventory, Some(d.path().join("inv-app.yaml")));
        assert_eq!(r.sets, vec!["port=80".to_string()]);
    }

    /// 命令行必须盖过 app 文件:app 记的是"这次安装是什么样",命令行是
    /// "这一次我要不一样"。
    #[test]
    fn the_command_line_wins_over_the_app_file() {
        let d = fixture();
        let mut cli = bare_cli();
        cli.inventory = Some(PathBuf::from("inv-cli.yaml"));
        let r = resolve_in(d.path(), "web", &cli, &["port=8080".to_string()])
            .unwrap()
            .unwrap();
        assert_eq!(r.target.inventory, Some(PathBuf::from("inv-cli.yaml")));
        // 追加在后 = 覆盖(`with_overrides` 用 insert)
        assert_eq!(r.sets, vec!["port=80".to_string(), "port=8080".to_string()]);
    }

    /// app 文件里的相对路径是相对**它自己**,不是相对进程的 cwd —— 否则
    /// 从别的目录敲 `crater apply web` 会找不到蓝图。
    #[test]
    fn paths_in_an_app_file_are_relative_to_that_file() {
        let d = fixture();
        let r = resolve_in(d.path(), "web", &bare_cli(), &[])
            .unwrap()
            .unwrap();
        assert!(r.blueprint.is_absolute() || r.blueprint.starts_with(d.path()));
        assert!(r.blueprint.is_file());
    }

    /// 蓝图被删了要说清是哪个文件不在,并给出两条出路。
    #[test]
    fn a_dangling_blueprint_points_at_the_app_file() {
        let d = fixture();
        std::fs::remove_file(d.path().join("web.blueprint.yaml")).unwrap();
        let e = resolve_in(d.path(), "web", &bare_cli(), &[])
            .unwrap_err()
            .to_string();
        assert!(e.contains("web.blueprint.yaml"), "没说是哪个文件:{e}");
        assert!(e.contains("crater install web"), "没给出路:{e}");
    }

    /// 本地没有这个名字时**不报错** —— 让路给远端那条(去仓库索引里找)。
    /// 早年这里是直接 bail 的,那样 `crater apply yq` 就永远够不到 registry。
    #[test]
    fn an_unknown_name_yields_instead_of_failing() {
        let d = fixture();
        let r = resolve_in(d.path(), "nope", &bare_cli(), &[]).unwrap();
        assert!(r.is_none(), "本地没有就该让路,不该解出东西来");
    }

    #[test]
    fn apps_in_lists_what_is_here() {
        let d = fixture();
        assert_eq!(apps_in(d.path()), vec!["web".to_string()]);
    }

    /// 包名、裸引用、`oci://` 三种都该走包那条路 —— helm 有的用法我们都要有。
    #[test]
    fn names_and_references_both_route_to_the_package_path() {
        // 包名(去索引里查)
        assert_eq!(remote_ref("yq").as_deref(), Some("yq"));
        assert_eq!(remote_ref("k8s-ha").as_deref(), Some("k8s-ha"));
        // 名字带版本:`:4.44.3` 里全是点,摘掉版本再判才不会被当成文件名
        assert_eq!(remote_ref("yq:4.44.3").as_deref(), Some("yq:4.44.3"));
        // 裸 OCI 引用
        assert_eq!(
            remote_ref("registry-1.docker.io/ns/yq:1.0").as_deref(),
            Some("registry-1.docker.io/ns/yq:1.0")
        );
        // `oci://` 前缀剥掉 —— 下游 ImageStore 收的是裸引用
        assert_eq!(
            remote_ref("oci://registry-1.docker.io/ns/yq:1.0").as_deref(),
            Some("registry-1.docker.io/ns/yq:1.0")
        );
    }

    /// 手滑打错的文件名不该被当成包名 —— 那样报的错会答非所问。
    #[test]
    fn a_misspelled_filename_is_not_a_package_name() {
        assert_eq!(remote_ref("web.blueprint.yam"), None);
        assert_eq!(remote_ref(""), None);
        assert_eq!(remote_ref("oci://"), None);
        // 长得像路径的一律不碰 —— 哪怕文件此刻不在。打错的路径被拿去连
        // registry,报的会是"仓库里没有 ./web.yaml",把人引向仓库配置。
        assert_eq!(remote_ref("./web.yaml"), None);
        assert_eq!(remote_ref("../a/b.yaml"), None);
        assert_eq!(remote_ref("/etc/x.blueprint.yaml"), None);
        assert_eq!(remote_ref("~/blueprints/web.yaml"), None);
    }
}
