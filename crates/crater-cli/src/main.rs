//! `crater` CLI —— 声明式的 agentless 部署引擎。
//!
//! 一条主线:**蓝图**声明期望态,五动词(observe / diff / apply / destroy /
//! upgrade)把它对到机群上。默认停在计划上,`--yes` 才动手。
//!
//! ```text
//! 写与看
//!   crater lint <蓝图>                 静态检查,不连机器
//!   crater plan -f <蓝图> -i <机群>    零写入预演
//!   crater inspect <蓝图>              输入契约:要给什么参数、要什么样的机群
//!   crater types / facts               26 类资源类型 / substrate.* 事实白名单
//!
//! 装与拆
//!   crater apply   -f <蓝图> -i <机群>  过闸执行(--closure 走离线闭包)
//!   crater install <包名|引用> -i <机群> 拉包 → 契约 → 对账 → plan 闸门
//!   crater verify / destroy             只读核对 / 退役
//!   crater procedure <名> -f <蓝图>     跑一支声明好的"舞"
//!
//! 打包与分发
//!   crater build -f <蓝图> -o <闭包>    烤离线闭包(--for arch= 选变体)
//!   crater pkg push/pull/ls/inspect     蓝图打成 OCI 制品
//!   crater repo add/update, crater search  索引文件:有哪些包
//!
//! 其它
//!   crater ui [--workspace D]           本地 Web 控制台
//!   crater doctor --host H              离线规则诊断
//!   crater run / cp                     临时命令 / 传文件
//! ```
//!
//! 旧 task 管线(顶层 `actions:`)已在 D-151 整块删除。

mod blob_source;
mod blueprint;
mod closure;
mod events;
mod facts_cmd;
mod fmt_cmd;
mod images;
mod inspect_bp;
mod lint;
mod material_ctx;
mod named;
mod out;
mod pkg;
mod repo;
mod schema_cmd;
mod stack_cmd;
mod target;
mod types_cmd;
mod ui;
mod ui_app;
mod ui_catalog;
mod ui_contract;
mod ui_edit;
mod ui_git;
mod ui_inventory;
mod ui_overview;
mod ui_run;
mod update;
mod version_req;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use clap::{Parser, Subcommand};
use tracing::info;

use target::TargetOpts;

use crate::blueprint::StackMode;
use crater_core::executor::{Executor, SshExecutor};
use crater_core::store::ImageStore;

/// 顶层命令清单,按用途分组。
///
/// clap 4 不支持给子命令分组(`{subcommands}` 只能平铺),27 条平铺出来没人
/// 读得下去 —— 所以这里手写,并用 `grouped_listing_covers_every_command`
/// 钉住:少一条、多一条、改个名,测试就红。手写清单会漂,**除非有东西盯着**。
const COMMAND_GROUPS: &str = "\
命令:

  写蓝图
    create       生成一份可改的起步文件(inventory 骨架)
    types        列出内置资源类型及其字段
    facts        列出 `substrate.*` 能用的事实
    schema       生成 JSON Schema —— 编辑器补全与内联校验
    lint         静态检查蓝图,不连机器
    fmt          把一个顶层小节拆成单独文件(可逆)
    inspect      看蓝图的输入契约:参数、需要的角色、物料

  部署
    plan         预演会变什么 —— 连机器跑只读探针,不执行
    apply        收敛 —— 把蓝图声明的状态推到机群
    verify       对账 —— 部署过的东西还是原来的样子吗
    destroy      退场 —— 移除蓝图声明的一切(默认只预览)
    procedure    跑蓝图声明的编排(跨主机的步骤)

  打包与分发
    pkg          把蓝图打成 OCI 制品:推、拉、看契约
    install      一键装:拉包 → 读契约 → 对账机群 → 落文件 → plan
    build        把蓝图烤成离线闭包文件
    save         把本地制品导出成 oci-archive 文件
    load         把 oci-archive 文件导入本地存储
    repo         包仓库 —— 一个索引文件的地址
    search       在已配仓库的索引里搜包(只查本地缓存)

  本地存储
    images       列出本地存储里的制品
    pull         从 registry 拉制品进本地存储
    push         把本地存储的制品推上 registry
    tag          给制品加一个别名
    rmi          删掉一个引用(blob 留给 gc 扫)
    gc           清扫没人引用的 blob 与过期构建指纹
    registry     registry 凭据

  运维与排查
    ui           浏览器看板:部署状态、历史、Verify/Heal
    run          在目标机上跑一条临时命令
    cp           往目标机拷一个文件(分块 base64,不需要 scp)
    doctor       按内置离线规则诊断失败日志

  crater 自己
    update       把 crater 换成最新版
";

/// kubectl 的排版:先说这是什么,再列命令,最后才是 usage 与全局选项。
/// `{subcommands}` 被刻意排除 —— 分组清单由 `COMMAND_GROUPS` 提供。
const TOP_TEMPLATE: &str = "\
{about-with-newline}
{after-help}
{usage-heading}
  crater <命令> [选项]

全局选项(所有命令都能用):
{options}
用 `crater <命令> --help` 看某条命令的详细说明与例子。";

#[derive(Parser)]
#[command(
    name = "crater",
    version,
    about = "crater —— 声明式远程执行引擎:一份蓝图,收敛整个机群",
    help_template = TOP_TEMPLATE,
    after_help = COMMAND_GROUPS,
    next_line_help = true,
    disable_help_subcommand = true
)]
struct Cli {
    /// 把终端输出**同时**写一份到这里,每行带时间戳。
    ///
    /// 部署跑十分钟、翻回去想知道"哪一步慢"时,没有落盘就只剩滚屏。
    #[arg(long, global = true, env = "CRATER_LOG_FILE", value_name = "FILE")]
    log_file: Option<PathBuf>,
    /// 终端也带上 HH:MM:SS。默认关 —— 单机短命令带时间戳是噪音。
    #[arg(long, global = true)]
    timestamps: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 收敛 —— 把蓝图声明的状态推到机群
    ///
    /// 读一份蓝图,连上目标机,把 `resources:` 里的每一条推到它声明的状态。
    /// 已经对的不动,要改的才改 —— 重复跑同一条命令是安全的。
    ///
    /// `<source>` 按这个顺序解,**先本地后远端**:
    ///
    ///   1. 一个存在的文件    → 蓝图或栈(按内容分辨,不看文件名)
    ///   2. `<名字>.app.yaml`  → 已装的任务:蓝图、机群、参数都从它来
    ///   3. `oci://reg/ns/x:1` → OCI 引用,直连 registry(不需要配任何仓库)
    ///      `reg/ns/x:1`         同上,协议头可省
    ///   4. `yq` / `yq:4.44.3` → 包名,去已配仓库的索引里查
    ///
    /// 版本位可以是**范围**,两条路都支持:`4.*`、`^4.44`、`~4.44.1`、
    /// `>=4.10, <4.44`。不写版本就是最新。范围解析靠 registry 的 `tags/list`
    /// —— 不需要索引,发现版本是 OCI 自带的能力。
    ///
    /// 第 3、4 条就是 helm 的两种用法:引用直连,或先 `repo add` 再用名字。
    /// 索引只为回答"**有哪些包**" —— OCI 规范里没有搜索,所以那件事必须靠
    /// 一个索引文件,而它能随闭包一起进 U 盘。
    ///
    /// 远端这两条都会**先印出计划再收敛**:拉下来的字节是别人做的,而下一步
    /// 要改的是生产机。
    ///
    /// 命令行给的 `-i` / `--set` **盖过** app 文件里记的。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  # 直连 OCI 引用 —— 不需要配任何仓库
  crater apply oci://ghcr.io/acme/yq:4.44.3 -i inventory.yaml

  # 版本范围:问 registry 有哪些版本,挑最高的合格者
  crater apply 'oci://ghcr.io/acme/yq:4.*' -i inventory.yaml
  crater apply 'yq:^4.44' -i inventory.yaml

  # 或者先订阅一个索引,之后用名字
  crater repo add lab https://example.com/index.yaml
  crater apply yq -i inventory.yaml

  # 装过之后,机群与参数都记在 yq.app.yaml 里,不必再重复
  crater apply yq

  # 直接给蓝图文件
  crater apply -f web.blueprint.yaml -i inventory.yaml
  crater apply -f web.blueprint.yaml --host 10.0.0.5

  # 不连机器,只打印静态计划
  crater apply -f web.blueprint.yaml --dry-run

  # 覆盖 apply 阶段的参数(盖过 app 文件里记的)
  crater apply yq --set vip=10.0.0.9
"
    )]
    Apply {
        /// 蓝图或栈文件 —— 与 `-f` 等价,写哪个都行
        source: Option<String>,
        /// 蓝图或栈文件(与位置参数等价)
        #[arg(short, long, value_name = "FILE")]
        file: Option<PathBuf>,
        #[command(flatten)]
        target: TargetOpts,
        /// 不连机器,只打印静态计划。要连机器的预演用 `crater plan`
        #[arg(long)]
        dry_run: bool,
        /// 覆盖一个 **apply 阶段**的参数(`stage: apply`,例如 vip/subnet):
        /// `--set vip=10.0.0.9`。可重复,优先级最高(高于 inventory 变量)。
        /// build 阶段的参数在这里会被**拒绝** —— 烤好的闭包是冻住的,要改就
        /// `crater build --set` 重烤(D-093)。哪个参数属于哪个阶段,
        /// `crater inspect` 会列出来。
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// 预演会变什么 —— 连机器跑只读探针,不执行
    ///
    /// terraform 式的变更预演:连上目标机,对每一条资源跑它的只读幂等探针,
    /// 然后报告
    ///
    ///   ✓ ok            已经是声明的样子,不会动
    ///   ~ would-change  会被改
    ///   ? unknown       **探不出来** —— 没有探针,不是"没问题"
    ///   - skip          preflight / verify 这类不参与收敛的步骤
    ///
    /// 什么都不执行。`?` 与 `✓` 是两件事:前者是我们不知道,后者是我们知道
    /// 它对 —— 把不知道报成没问题,是这类工具最容易骗人的地方。
    ///
    /// 不想连机器就用 `crater apply --dry-run`,它只打印静态计划。
    ///
    /// `<source>` 与 `apply` 同一套解法:文件 → `<名字>.app.yaml` → OCI 引用
    /// → 仓库索引。本地没装过时会把包拉下来、印出计划,**停在那里**。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  # 已装的任务
  crater plan yq

  # 仓库里的包 / 直连引用:拉下来看计划,不收敛
  crater plan yq -i inventory.yaml
  crater plan oci://ghcr.io/acme/yq:4.44.3 -i inventory.yaml

  # 一份蓝图文件
  crater plan -f web.blueprint.yaml -i inventory.yaml
  crater plan -f web.blueprint.yaml --host 10.0.0.5
"
    )]
    Plan {
        /// 蓝图或栈文件 —— 与 `-f` 等价
        source: Option<String>,
        /// 蓝图或栈文件(与位置参数等价)
        #[arg(short, long, value_name = "FILE")]
        file: Option<PathBuf>,
        #[command(flatten)]
        target: TargetOpts,
        /// 覆盖 apply 阶段的参数,与 `apply --set` 同一道闸门(D-093)
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// 浏览器看板:部署状态、历史、Verify/Heal
    ///
    /// 起一个本地 web 看板,看 crater 把什么放在了哪、以及历次部署的历史。
    /// Axum + htmx,htmx 是内嵌的 —— 气隙机器上照样打得开。
    ///
    /// **默认只绑 localhost。** 会写的动作(Verify / Heal)用当前目录的
    /// `./inventory.yaml`,没有就只读。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  # 本机打开
  crater ui

  # 换端口
  crater ui --port 8080
"
    )]
    Ui {
        #[arg(long, default_value = "127.0.0.1")]
        bind: String,
        #[arg(long, default_value_t = 8080)]
        port: u16,
        /// Access token (D-099): requests must present it (first visit
        /// `http://host:port/?token=<t>` sets a cookie). REQUIRED when binding
        /// beyond localhost — the UI can apply/delete deployments.
        #[arg(long)]
        token: Option<String>,
        /// 工作区目录:蓝图 / inventory / app 文件都从这里找,UI 的写入也
        /// **只能落在它之内**。不给则沿用进程当前目录。
        ///
        /// 强烈建议显式指定并与工具源码分开:UI 在哪个目录起,点一下就在改
        /// 那个目录 —— 从仓库里起 UI,编辑动作会直接落到随工具发行的文件上。
        #[arg(long, env = "CRATER_WORKSPACE", value_name = "DIR")]
        workspace: Option<PathBuf>,
    },
    /// 把蓝图烤成离线闭包文件
    ///
    /// 把蓝图声明的全部物料抓下来、连同蓝图本身封进一个文件。这个文件带去
    /// 断网机房,`crater apply -f <蓝图> --closure <文件>` 就能部署,全程
    /// 不碰网络。
    ///
    /// 默认烤**每一个**声明的变体(不同架构、不同发行版)—— 气隙场景下,
    /// 少烤一个变体等于到了现场才发现装不上。用得到哪个很确定时,`--for`
    /// 可以只烤那个。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  # 烤全部变体(气隙最稳)
  crater build -f k8s.blueprint.yaml -o k8s.closure.tar

  # 只烤用得到的那个
  crater build -f k8s.blueprint.yaml -o k8s.closure.tar --for arch=amd64

  # 到了断网那头
  crater apply -f k8s.blueprint.yaml --closure k8s.closure.tar -i inventory.yaml
"
    )]
    Build {
        /// 蓝图或栈文件
        #[arg(short, long, value_name = "FILE")]
        file: PathBuf,
        /// 闭包写到哪。不给就按蓝图名生成
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// 只烤匹配这个画像的变体,如 `--for arch=amd64`。可重复。
        /// 不给就烤**全部**声明的变体
        #[arg(long = "for", value_name = "KEY=VAL")]
        profile: Vec<String>,
        /// 覆盖一个 **build 阶段**的参数,如 `--set version=4.55.1`。可重复。
        /// 它盖掉参数的 `default`,所以一份源能烤出任意版本(D-089)
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// 把本地制品导出成 oci-archive 文件
    ///
    /// 像 `docker save`。导出来的文件可以用 U 盘搬到断网那头,再 `crater load`
    /// 进去。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater save registry.example.com/ns/k8s:1.31 -o k8s.oci
"
    )]
    Save {
        /// Reference in the local store (`crater images` to list).
        reference: String,
        /// Output file, e.g. yq.oci
        #[arg(short, long)]
        output: PathBuf,
    },
    /// 看蓝图的输入契约:参数、需要的角色、物料
    ///
    /// 拿到一份别人写的蓝图,第一个问题是"我得准备什么才能跑它"。这条命令
    /// 回答的正是这个:有哪些参数(默认值、是否必填、属于 build 还是 apply
    /// 阶段)、inventory 里要有哪些角色、要下载哪些物料(D-081)。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater inspect k8s.blueprint.yaml
"
    )]
    Inspect {
        /// 蓝图或栈文件
        source: String,
    },
    /// 把一个顶层小节拆成单独文件(可逆)
    ///
    /// 蓝图长到几百行时,把 `resources:` 或 `substrate:` 拆出去单独放。
    ///
    /// **机械且可逆**:合起来的结果与写在一个文件里完全等价 —— 这正是它与
    /// `include` 的区别,后者会引入求值顺序,拆分就不再是纯排版了。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  # 把 resources: 拆到 web.resources.yaml
  crater fmt web.blueprint.yaml --split resources

  # 全部收回根文件
  crater fmt web.blueprint.yaml --join
"
    )]
    Fmt {
        /// The blueprint's root file.
        file: PathBuf,
        /// Section to externalise into `<stem>.<section>.yaml`.
        #[arg(long)]
        split: Option<String>,
        /// Merge every externalised section back into the root file.
        #[arg(long)]
        join: bool,
    },
    /// 生成 JSON Schema —— 编辑器补全与内联校验
    ///
    /// 让编辑器认识蓝图:补全字段、悬停看说明、写错当场标红。
    ///
    /// 给 `-f` 会**为某一份蓝图特化**:那份蓝图自己的物料名与自定义类型也变成
    /// 补全项 —— 通用 schema 只知道内置类型,不知道你声明了什么。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater schema > crater.schema.json
  crater schema -f web.blueprint.yaml > web.schema.json
"
    )]
    Schema {
        /// Blueprint to self-specialise against.
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Output path (default `.crater/schema.json`).
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Print to stdout instead of writing a file.
        #[arg(long = "stdout")]
        to_stdout: bool,
    },
    /// 列出 `substrate.*` 能用的事实
    ///
    /// 不给目标就只列有哪些事实可写;给了 `-i`/`--host` 就**真去机器上探一遍**,
    /// 摆成 事实 × 主机 的表。
    ///
    /// `when:` 条件不成立的时候,要看的正是"那这台到底是什么" —— 猜不如探。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater facts                          # 有哪些事实可用
  crater facts -i inventory.yaml        # 这个机群实际是什么
"
    )]
    Facts {
        #[command(flatten)]
        target: TargetOpts,
    },
    /// 列出内置资源类型及其字段
    ///
    /// 回答"`systemd_unit` 有哪些字段、哪些必填"。
    ///
    /// 它渲染的是**同一份注册表** —— lint 的报错和 `crater schema` 生成的
    /// JSON Schema 也从这份表来,所以三者不可能互相矛盾。文档与实现分家才会
    /// 出现"文档说有这个字段但引擎不认"。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater types                 # 全部 26 种
  crater types systemd_unit    # 只看一种
"
    )]
    Types {
        /// A type name for the full field card; omit to list everything.
        name: Option<String>,
        /// Machine-readable output (for editors / schema generation).
        #[arg(long)]
        json: bool,
    },
    /// 静态检查蓝图,不连机器
    ///
    /// 零连接的静态检查(D-107)。它抓的是这样一类错:类型名/字段名/参数名拼错、
    /// CEL 表达式里用了作用域外的变量、物料没声明、跨主机事实不配对。
    ///
    /// 这些错在 Ansible 里要等到**连上机器、跑到那一行**才会暴露 —— 也就是说
    /// 在半个机群已经改过之后。这里在敲回车之前就报,还带拼写建议。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater lint web.blueprint.yaml
  crater lint library/*/*.blueprint.yaml
"
    )]
    Lint {
        /// Files or directories to scan (recursively). Default: current directory.
        #[arg(default_value = ".")]
        paths: Vec<PathBuf>,
        /// Treat warnings as failures too (for CI).
        #[arg(long)]
        strict: bool,
        /// Machine-readable output for CI / editor integration.
        #[arg(long)]
        json: bool,
        /// Report per-section line counts. Informational only — line count does
        /// not measure complexity, so this never warns and never fails.
        #[arg(long)]
        stats: bool,
    },
    /// 跑蓝图声明的编排(跨主机的步骤)
    ///
    /// `apply` 是逐台收敛资源;procedure 是**机群级**的:它的步骤跨主机,而且
    /// 步骤之间能传事实 —— 建集群、滚动升级这类"先在 A 上做完拿到 token,再拿
    /// 去 B 上用"的动作,靠它。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater procedure bootstrap -f k8s.blueprint.yaml -i inventory.yaml
"
    )]
    Procedure {
        /// Procedure name (see `crater inspect`).
        name: String,
        #[arg(short, long)]
        file: Option<PathBuf>,
        #[command(flatten)]
        target: TargetOpts,
        /// Procedure params + deploy-stage overrides: `--set to=1.37.0`.
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// 退场 —— 移除蓝图声明的一切(默认只预览)
    ///
    /// **默认什么都不动**:不给 `--yes` 就只打印会移除什么。栈按声明的**倒序**
    /// 退场。
    ///
    /// 蓝图里没有 `teardown:` 这一节 —— 退场是从五个动词推导出来的,倒着走一遍
    /// 声明顺序。这意味着不必为"怎么卸载"再写一份、也不会漏。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  # 先看会移除什么(默认)
  crater destroy yq

  # 确认后真的动手
  crater destroy yq --yes

  # 或者直接给蓝图
  crater destroy -f web.blueprint.yaml -i inventory.yaml --yes
"
    )]
    Destroy {
        /// Blueprint or stack file.
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Same, positionally.
        source: Option<String>,
        #[command(flatten)]
        target: TargetOpts,
        /// Actually remove. Without this the command is a read-only preview.
        #[arg(long)]
        yes: bool,
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// 对账 —— 部署过的东西还是原来的样子吗
    ///
    /// 拿现场与**记录下来的部署状态**比对,只读。回答的是 `plan` 答不了的
    /// 那个问题:没有记录的话,"从来没部署过"和"部署完被人改了"长得一模一样。
    ///
    /// 报告用三态,`?` 与 `✓` 不混:探不出来就说探不出来。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater verify yq                              # 已装的任务
  crater verify -f web.blueprint.yaml -i inventory.yaml
"
    )]
    Verify {
        /// Write a machine-readable verify report here (per-host verdicts).
        #[arg(long, value_name = "FILE")]
        json: Option<PathBuf>,
        /// Blueprint file (new IR pipeline).
        #[arg(short, long)]
        file: Option<PathBuf>,
        /// Or positionally.
        source: Option<String>,
        #[command(flatten)]
        target: TargetOpts,
        /// Deploy-stage param overrides (must match what was applied).
        #[arg(long = "set", value_name = "KEY=VAL")]
        set: Vec<String>,
    },
    /// 按内置离线规则诊断失败日志
    ///
    /// 把一份失败日志(或一台机器上收来的诊断信息)对着内置故障特征库过一遍,
    /// 匹配上就给出原因与可照做的修法。**全离线**:不联网、不调模型。
    ///
    /// 匹配不上时它明说"没有匹配的特征",不编一个原因出来。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  # 看一份本地日志
  crater doctor --file deploy.log

  # 去机器上收诊断信息
  crater doctor --host 10.0.0.5 --password ***
"
    )]
    Doctor {
        /// 分析这份本地日志/错误文件(完全离线,不 SSH)
        #[arg(long, value_name = "FILE")]
        file: Option<PathBuf>,
        /// 或者去这台机器上收诊断信息
        #[arg(long)]
        host: Option<String>,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
    },
    /// 在目标机上跑一条临时命令
    ///
    /// `ansible -m shell` 那种用法:不写蓝图,直接在一台机器上执行一条命令。
    ///
    /// **它不进对账**:临时命令改出来的东西,`verify` 不知道、`destroy` 收不
    /// 回来。要留下的改动请写进蓝图。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater run --host 10.0.0.5 --password *** -- systemctl status docker
"
    )]
    Run {
        #[arg(long)]
        host: String,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        #[arg(trailing_var_arg = true, required = true)]
        cmd: Vec<String>,
    },
    /// 往目标机拷一个文件
    ///
    /// 分块 base64 走 SSH —— 目标机上**不需要** scp/sftp,这也是 crater 敢说
    /// "目标机零安装"的一部分。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater cp --host 10.0.0.5 --password *** --src ./app.conf --dst /etc/app.conf --chmod 0644
"
    )]
    Cp {
        #[arg(long)]
        host: String,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value_t = 22)]
        port: u16,
        /// Local source file.
        #[arg(long)]
        src: PathBuf,
        /// Remote destination path.
        #[arg(long)]
        dst: String,
        /// chmod the remote file (e.g. 755) after upload.
        #[arg(long)]
        chmod: Option<String>,
    },
    /// 把 crater 自己换成最新版 —— 与 `scripts/install.sh` 同一套规矩
    /// (取 musl 静态包、核对 SHA256SUMS、原子替换)。
    ///
    /// **装在哪就换哪**(按 `current_exe` 定位),不猜 PATH:同时装了
    /// `~/.local/bin/crater` 与 `/usr/local/bin/crater` 时,换错一个的表现是
    /// "更新成功了但版本没变"。
    Update {
        /// 换到指定版本(如 `v0.2.0`);默认取最新 release。
        #[arg(long)]
        version: Option<String>,
        /// 只看有没有新版本,不替换。
        #[arg(long)]
        check: bool,
    },
    /// 列出本地存储里的制品
    ///
    /// 本地存储在 `~/.crater/store`。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater images
"
    )]
    Images,
    /// 从 registry 拉制品进本地存储
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater pull registry.example.com/ns/k8s:1.31
"
    )]
    Pull {
        /// e.g. docker.io/library/busybox:latest
        reference: String,
    },
    /// 把本地存储的制品推上 registry
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater push registry.example.com/ns/k8s:1.31
"
    )]
    Push { reference: String },
    /// 把 oci-archive 文件导入本地存储
    ///
    /// `crater save` 的反向操作。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater load k8s.oci
"
    )]
    Load {
        /// Path to the .oci archive.
        file: PathBuf,
        /// Tag to store it under (default: the archive's embedded ref.name, e.g.
        /// from `build -t`), e.g. 192.168.73.5:5000/yq:4.53.2
        #[arg(long = "as")]
        as_ref: Option<String>,
    },
    /// 给制品加一个别名
    ///
    /// 像 `docker tag`:同一份内容多一个引用,不复制字节。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater tag crater/k8s:1.31 registry.example.com/ns/k8s:1.31
"
    )]
    Tag {
        /// Existing reference in the local store.
        source: String,
        /// New reference to point at the same manifest (e.g. a registry address).
        target: String,
    },
    /// 删掉一个引用(blob 留给 gc 扫)
    ///
    /// 像 `docker rmi`,删的是**引用**不是字节。blob 是内容寻址的、可能被别的
    /// 制品共享,所以它们留到 `crater gc` 才扫 —— 立刻删字节会误伤共享者。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater rmi crater/k8s:1.31
"
    )]
    Rmi {
        /// Reference to remove, e.g. `crater/yq:4.40.5`.
        reference: String,
    },
    /// 清扫没人引用的 blob 与过期构建指纹
    ///
    /// 扫本地存储里已经没有任何引用指向的 blob,以及过期的构建指纹(D-097)。
    ///
    /// `--cache` 连下载缓存一起清。给 `--host`/`-i` 还会清**目标机上**的暂存
    /// blob(`/var/lib/crater/blobs`,D-095)—— 那份缓存下次 apply 会自己重建,
    /// 清掉只是慢一次,不会坏。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater gc                          # 只清本地存储
  crater gc --cache                  # 连下载缓存一起
  crater gc -i inventory.yaml        # 顺带清目标机上的暂存
"
    )]
    Gc {
        /// Also wipe the download cache (~/.crater/cache/{file,ospkg}).
        #[arg(long)]
        cache: bool,
        /// Report what would be freed, delete nothing.
        #[arg(long)]
        dry_run: bool,
        #[command(flatten)]
        target: TargetOpts,
    },
    /// 一键安装:拉包 → 读契约 → 对账机群 → 落任务文件 → plan(D-123)。
    ///
    /// **闸门一步不省** —— 默认停在计划上,`--yes` 才继续执行。"一键"省的是
    /// 找包、抄参数、比对组名那几步,不是"先看 diff 再动手"。
    Install {
        /// OCI 引用(如 reg/ns/rustfs:1.0),或本地已有的蓝图目录 / 文件。
        source: String,
        /// 任务名(默认取蓝图名)—— 决定落成哪个 `<名>.app.yaml`。
        #[arg(long)]
        name: Option<String>,
        /// 从哪个仓库找这个包(名字在多个仓库里都有时用)。
        #[arg(long)]
        repo: Option<String>,
        /// 参数覆盖,可给多次。
        #[arg(long = "set", value_name = "K=V")]
        set: Vec<String>,
        /// 过闸执行,不只是看计划。
        #[arg(long)]
        yes: bool,
        /// 连物料层一起拉 —— 离线现场才需要。
        #[arg(long)]
        full: bool,
        /// 换版本时,上一版目录里的本地改动"判不出"或"已漂移"也照旧升级。
        ///
        /// 旧目录一个字节都不删,只是那些改动**不会**跟到新版本。
        /// `--yes` 跨不过这道闸门:那句话的意思是"计划我看过了",
        /// 不是"我的改动随便丢"。
        #[arg(long)]
        force: bool,
        #[command(flatten)]
        target: TargetOpts,
    },
    /// 包仓库 —— 一个索引文件的地址。OCI 里没有搜索(`_catalog` 不在规范
    /// 里、Docker Hub 还禁用它),所以"有哪些包"靠索引文件回答,而它能随
    /// 闭包一起进 U 盘。
    Repo {
        #[command(subcommand)]
        cmd: RepoCmd,
    },
    /// 在已配仓库的索引里搜包。只查本地缓存,不连网 —— 要新的先 `repo update`。
    Search {
        /// 关键词(匹配包名与描述);留空列全部。
        #[arg(default_value = "")]
        query: String,
    },
    /// 蓝图包 —— 把一份蓝图打成 OCI 制品,推上去、拉下来、看契约(D-123)。
    ///
    /// 与 `crater images` 的分工:那条管旧 task 制品与普通镜像,这条只管
    /// 蓝图包(config 类型是 `vnd.crater.blueprint.config.v1+json`)。
    Pkg {
        #[command(subcommand)]
        cmd: PkgCmd,
    },
    /// Registry credentials.
    Registry {
        #[command(subcommand)]
        cmd: RegistryCmd,
    },
    /// 生成一份可改的起步文件
    ///
    /// 与其对着空文件发呆,不如先生成一份能跑的骨架再改。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  crater create inventory
"
    )]
    Create {
        #[command(subcommand)]
        what: CreateWhat,
    },
}

#[derive(Subcommand)]
enum PkgCmd {
    /// 组装并推到 registry。仓库名取蓝图 name,tag 取 version(同 Helm)。
    Push {
        /// 蓝图文件或它所在的目录(目录会连模板、静态文件一起打包)。
        path: PathBuf,
        /// 目标引用,如 registry.example.com/ns/rustfs:1.0
        reference: String,
        /// 把物料字节也烤进包(离线现场需要)。可给多次 = 多架构,
        /// 产出一个 image index,一个 tag 装下所有架构。
        #[arg(long, value_name = "ARCH")]
        arch: Vec<String>,
        /// 烘焙用的其它目标事实,`k=v`,与每个 `--arch` 合并。
        #[arg(long = "for", value_name = "K=V")]
        fors: Vec<String>,
    },
    /// 只组装进本地 store,不推 —— 想先看看包成什么样时用。
    Build {
        path: PathBuf,
        #[arg(short = 't', long = "tag")]
        reference: String,
        #[arg(long, value_name = "ARCH")]
        arch: Vec<String>,
        #[arg(long = "for", value_name = "K=V")]
        fors: Vec<String>,
    },
    /// 拉下来并摊回文件。默认瘦拉(物料层留在 registry,在线部署用不到)。
    Pull {
        reference: String,
        /// 摊到哪个目录(默认:当前目录下以包名新建)。
        #[arg(long)]
        into: Option<PathBuf>,
        /// 连物料层一起拉 —— 离线现场才需要。
        #[arg(long)]
        full: bool,
    },
    /// 看契约:要给什么参数、要什么样的机群。只拉 manifest + config,
    /// **一层都不下载** —— 几百字节就能回答"这东西要我给什么"。
    Inspect { reference: String },
    /// 远端有哪些版本(`/v2/<repo>/tags/list`,OCI 唯一的内容发现端点)。
    Tags { reference: String },
    /// 本地 store 里的蓝图包。
    Ls,
    /// 把包连同全部物料导成一个 tar —— U 盘搬去断网机房。
    ///
    /// 和索引一起放同一个目录,对面 `pkg load` + `repo add` 就能搜能装:
    ///
    ///   crater pkg save reg/ns/yq:4.44.3 -o /media/usb/yq.pkg.tar
    ///   crater pkg index --store -o /media/usb/index.yaml
    Save {
        /// 本地 store 里的引用(`crater pkg ls` 看有哪些)。
        reference: String,
        /// 输出文件,如 /media/usb/yq.pkg.tar
        #[arg(short, long)]
        output: PathBuf,
    },
    /// 把 `pkg save` 的 tar 收进本地 store —— 断网机上的第一步。
    Load {
        /// tar 的路径。
        file: PathBuf,
        /// 换一个引用存(默认用 tar 里带的那条)。
        #[arg(long = "as")]
        as_ref: Option<String>,
    },
    /// 生成索引文件 —— 别人 `repo add` 它,就能 `search` 和 `install`。
    ///
    /// 只读 manifest + config,一层都不下载。**没有"扫一个 registry"这种
    /// 来源** —— 那正是 OCI 问不出来的东西(`_catalog` 不在规范里)。
    Index {
        /// `oci://reg/ns/name`(不带 tag → 收全部版本)或带 tag 只收一版。
        sources: Vec<String>,
        /// 把本地 store 里的蓝图包也收进来。
        #[arg(long)]
        store: bool,
        #[arg(short = 'o', long, default_value = "index.yaml")]
        out: PathBuf,
        /// 并入已有索引而不是重写(增量发布)。
        #[arg(long)]
        merge: bool,
    },
}

#[derive(Subcommand)]
enum RepoCmd {
    /// 记下一个索引地址并同步一次。地址可以是 http(s)、本地路径或 U 盘路径。
    Add { name: String, url: String },
    /// 拉取索引(不给名字则全部)。
    Update { name: Option<String> },
    /// 已配的仓库及其同步状态。
    List,
    /// 移除一个仓库及其缓存。
    Remove { name: String },
}

#[derive(Subcommand)]
enum RegistryCmd {
    /// Store credentials for a registry (used by pull/push).
    /// 存一份 registry 凭据
    ///
    /// crater 也读 `~/.docker/config.json`(D-129)—— 已经 `docker login` 过
    /// 就不必再来一遍。这条是给"装了凭据助手、docker config 里没有明文"的
    /// 情况兜底的。
    #[command(
        verbatim_doc_comment,
        after_help = "\
用法示例:
  # 从标准输入读口令 —— 令牌不进命令行、不进 shell 历史
  gh auth token | crater registry login ghcr.io -u <用户名> --password-stdin

  # 已经 docker login 过的话,这一步根本不用做 —— crater 直接读
  #   ~/.docker/config.json 里的明文凭据(D-129)
"
    )]
    Login {
        /// Registry 主机,如 ghcr.io 或 registry.example.com:5000
        registry: String,
        #[arg(short, long)]
        username: String,
        /// 口令/令牌。**不建议** —— 它会留在 `ps` 的输出和 shell 历史里。
        /// 用 `--password-stdin`,或者什么都不给让它交互式问
        #[arg(short, long)]
        password: Option<String>,
        /// 从标准输入读口令(第一行)。CI 与管道用这个
        #[arg(long, conflicts_with = "password")]
        password_stdin: bool,
    },
}

#[derive(Subcommand)]
enum CreateWhat {
    /// Write a sample inventory.yaml (host list for `-i`) to edit.
    Inventory {
        /// Output path.
        #[arg(default_value = "inventory.yaml")]
        path: PathBuf,
        /// Overwrite if the file already exists.
        #[arg(long)]
        force: bool,
    },
}

/// Compact wall-clock timer (`HH:MM:SS`, UTC) — dependency-free, keeps log
/// lines short vs the default RFC3339 timestamp.
struct ClockTime;
impl tracing_subscriber::fmt::time::FormatTime for ClockTime {
    fn format_time(&self, w: &mut tracing_subscriber::fmt::format::Writer<'_>) -> std::fmt::Result {
        let s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        write!(
            w,
            "{:02}:{:02}:{:02}",
            (s / 3600) % 24,
            (s / 60) % 60,
            s % 60
        )
    }
}

fn log_level() -> tracing::Level {
    match std::env::var("CRATER_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .ok()
        .as_deref()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("trace") => tracing::Level::TRACE,
        Some("debug") => tracing::Level::DEBUG,
        Some("warn") => tracing::Level::WARN,
        Some("error") => tracing::Level::ERROR,
        _ => tracing::Level::INFO,
    }
}

/// 命令行给的 source 是不是**新 IR blueprint 文件**。是则走五动词管线,
/// 否则(旧 task / 镜像 ref / .oci / 命名 task)交回原管线 —— 两条管线并存到迁移完成。
fn blueprint_source(file: &Option<PathBuf>, source: &Option<String>) -> Option<PathBuf> {
    let candidate = file
        .clone()
        .or_else(|| source.as_ref().map(PathBuf::from))?;
    (candidate.is_file() && blueprint::is_blueprint_file(&candidate)).then_some(candidate)
}

/// `k8s-ha.blueprint.yaml` → `k8s-ha.closure.tar`;`platform.stack.yaml` → `platform.closure.tar`。
fn default_closure_path(file: &Path, kind_suffix: &str) -> PathBuf {
    let stem = file
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    PathBuf::from(format!(
        "{}.closure.tar",
        stem.trim_end_matches(kind_suffix)
    ))
}

/// 同上,但认的是**栈**。栈与蓝图靠形状分辨(`stack:` + `uses:`),不靠文件名。
fn stack_source(file: &Option<PathBuf>, source: &Option<String>) -> Option<PathBuf> {
    let candidate = file
        .clone()
        .or_else(|| source.as_ref().map(PathBuf::from))?;
    (candidate.is_file() && stack_cmd::is_stack_file(&candidate)).then_some(candidate)
}

/// 四个动词共用的一步:把 `<source>` 解成"跑哪份蓝图、在哪跑、带什么参数"。
///
/// 顺序是刻意的 —— **文件优先,再本地任务,最后远端仓库**。同名的 `yq` 文件
/// 和 `yq.app.yaml` 同时存在时,写了路径的那个人显然指的是文件;反过来猜会
/// 让"我明明指定了文件"变成一件要 debug 的事。
///
/// 返回 `None` 有两种含义,由调用方区分:`<source>` 根本不像名字(那是错误
/// 输入),或者它像名字但本地没有(那就该去仓库找)。用 `remote_name` 判。
fn source_of(
    file: &Option<PathBuf>,
    source: &Option<String>,
    target: &TargetOpts,
    sets: &[String],
) -> Result<Option<named::Resolved>> {
    if let Some(p) = blueprint_source(file, source) {
        return Ok(Some(named::Resolved {
            blueprint: p,
            target: target.clone(),
            sets: sets.to_vec(),
        }));
    }
    match source {
        // 只有裸名字才可能对应本地任务;引用不会有 `<引用>.app.yaml`。
        Some(s) if !s.contains('/') && !s.starts_with("oci://") => named::resolve(s, target, sets),
        _ => Ok(None),
    }
}

/// `<source>` 该不该拿去包那条路解决(包名走索引,引用直连 registry)。
fn remote_name(file: &Option<PathBuf>, source: &Option<String>) -> Option<String> {
    if file.is_some() {
        return None; // 给了 `-f` 就是要一份文件,不该悄悄跑去联网
    }
    source.as_deref().and_then(named::remote_ref)
}

/// 本地没有这个名字 → 去仓库把包拉下来跑(helm 那种用法)。
///
/// 整条路复用 `pkg::install`:它已经把"名字 → 索引 → 拉包 → 参数契约 →
/// 机群契约 → 落 app 文件 → 出计划"走通了,这里只决定**收不收敛**。
///
/// `converge=false`(`plan`,或 `apply --dry-run`)停在计划;`true` 时计划
/// 照印,然后执行 —— 拉下来的字节是别人做的,而下一步要改的是生产机,
/// 那一眼不能省。
async fn from_repo(name: &str, target: &TargetOpts, sets: &[String], converge: bool) -> Result<()> {
    // 引用是直连 registry,没有"本地找过了"这一步 —— 说了反而误导。
    if !name.contains('/') {
        say!("本地没有 {name}.app.yaml —— 去仓库找");
    }
    pkg::install(
        name,
        target,
        sets,
        None,
        None,
        pkg::InstallOpts {
            yes: converge,
            full: false,
            force: false,
        },
    )
    .await
}

#[tokio::main]
async fn main() -> Result<()> {
    // ANSI only on a real terminal — keeps redirected/piped output and the
    // agent's SSH-forwarded output free of escape codes.
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    events::init_from_env(); // CRATER_EVENTS=<path> → NDJSON 事件流(UI 供血)
    tracing_subscriber::fmt()
        .with_max_level(log_level())
        .with_timer(ClockTime)
        .with_target(false)
        .with_ansi(ansi)
        .with_writer(std::io::stdout)
        .init();

    let cli = Cli::parse();
    // 输出层要在任何命令跑起来之前定好 —— 晚一步,前面的行就没进日志。
    out::init(cli.log_file.as_deref(), cli.timestamps);
    match cli.cmd {
        Cmd::Apply {
            source,
            file,
            target,
            dry_run,
            set,
        } => {
            // 按文件形状分流:栈 → 逐蓝图;蓝图 → 五动词管线;都不是 → 说清楚。
            let probe = source.clone();
            if let Some(p) = stack_source(&file, &probe) {
                let m = if dry_run {
                    StackMode::Plan
                } else {
                    StackMode::Apply
                };
                return stack_cmd::run(&p, &target, &set, m).await;
            }
            if let Some(r) = source_of(&file, &probe, &target, &set)? {
                if dry_run {
                    return blueprint::plan_blueprint(&r.blueprint, &r.target, &r.sets).await;
                }
                return blueprint::apply_blueprint(&r.blueprint, &r.target, &r.sets).await;
            }
            if let Some(n) = remote_name(&file, &probe) {
                return from_repo(&n, &target, &set, !dry_run).await;
            }
            bail!(legacy_note("apply"))
        }
        Cmd::Plan {
            source,
            file,
            target,
            set,
        } => {
            // 与 apply 同一条分流,只是模式是 Plan。
            if let Some(p) = stack_source(&file, &source) {
                return stack_cmd::run(&p, &target, &set, StackMode::Plan).await;
            }
            match source_of(&file, &source, &target, &set)? {
                Some(r) => blueprint::plan_blueprint(&r.blueprint, &r.target, &r.sets).await,
                None => match remote_name(&file, &source) {
                    Some(n) => from_repo(&n, &target, &set, false).await,
                    None => bail!(legacy_note("plan")),
                },
            }
        }
        Cmd::Ui {
            bind,
            port,
            token,
            workspace,
        } => ui::serve(&bind, port, token, workspace).await,
        Cmd::Build {
            file,
            output,
            profile,
            set,
        } => {
            // 与 apply/plan 同一条按文件形状分派的路子。
            if stack_cmd::is_stack_file(&file) {
                let out = output.unwrap_or_else(|| default_closure_path(&file, ".stack"));
                return closure::build_stack(&file, &out, &profile, &set).await;
            }
            if blueprint::is_blueprint_file(&file) {
                let out = output.unwrap_or_else(|| default_closure_path(&file, ".blueprint"));
                return closure::build(&file, &out, &profile, &set).await;
            }
            bail!(legacy_note("build"))
        }
        Cmd::Inspect { source } => {
            let p = PathBuf::from(&source);
            if p.is_file() && (blueprint::is_blueprint_file(&p) || stack_cmd::is_stack_file(&p)) {
                return inspect_bp::run(&p);
            }
            bail!(legacy_note("inspect"))
        }
        Cmd::Save { reference, output } => {
            ImageStore::open()?.export_oci_archive(&reference, &output)?;
            info!("saved {reference} → {}", output.display());
            Ok(())
        }
        Cmd::Cp {
            host,
            user,
            password,
            port,
            src,
            dst,
            chmod,
        } => push_file(&host, &user, password, port, &src, &dst, chmod).await,
        Cmd::Update { version, check } => update::run(version, check).await,
        Cmd::Images => images::list_images().await,
        Cmd::Install {
            source,
            name,
            repo,
            set,
            yes,
            full,
            force,
            target,
        } => {
            let opts = pkg::InstallOpts { yes, full, force };
            pkg::install(
                &source,
                &target,
                &set,
                name.as_deref(),
                repo.as_deref(),
                opts,
            )
            .await
        }
        Cmd::Pkg { cmd } => match cmd {
            PkgCmd::Push {
                path,
                reference,
                arch,
                fors,
            } => pkg::push(&path, &reference, &arch, &fors).await,
            PkgCmd::Build {
                path,
                reference,
                arch,
                fors,
            } => pkg::build(&path, &reference, &arch, &fors).await,
            PkgCmd::Pull {
                reference,
                into,
                full,
            } => pkg::pull(&reference, into.as_deref(), full).await,
            PkgCmd::Inspect { reference } => pkg::inspect(&reference).await,
            PkgCmd::Tags { reference } => pkg::tags(&reference).await,
            PkgCmd::Ls => pkg::ls(),
            PkgCmd::Save { reference, output } => pkg::save(&reference, &output),
            PkgCmd::Load { file, as_ref } => pkg::load(&file, as_ref.as_deref()),
            PkgCmd::Index {
                sources,
                store,
                out,
                merge,
            } => repo::index(&sources, store, &out, merge).await,
        },
        Cmd::Repo { cmd } => match cmd {
            RepoCmd::Add { name, url } => repo::add(&name, &url).await,
            RepoCmd::Update { name } => repo::update(name.as_deref()).await,
            RepoCmd::List => repo::list(),
            RepoCmd::Remove { name } => repo::remove(&name),
        },
        Cmd::Search { query } => repo::search(&query),
        Cmd::Pull { reference } => images::pull_image(&reference).await,
        Cmd::Push { reference } => images::push_image(&reference).await,
        Cmd::Load { file, as_ref } => {
            let r = ImageStore::open()?.import_oci_archive(&file, as_ref.as_deref())?;
            info!("loaded {} → {r}", file.display());
            Ok(())
        }
        Cmd::Rmi { reference } => images::remove_image(&reference),
        Cmd::Gc {
            cache,
            dry_run,
            target,
        } => images::gc(cache, dry_run, target).await,
        Cmd::Tag { source, target } => {
            ImageStore::open()?.retag(&source, &target)?;
            info!("tagged {source} → {target}");
            Ok(())
        }
        Cmd::Registry { cmd } => match cmd {
            RegistryCmd::Login {
                registry,
                username,
                password,
                password_stdin,
            } => {
                let password = read_password(password, password_stdin)?;
                crater_core::store::save_login(&registry, &username, &password)?;
                info!("saved credentials for {registry}");
                Ok(())
            }
        },
        Cmd::Create { what } => match what {
            CreateWhat::Inventory { path, force } => target::create_inventory(&path, force),
        },
        Cmd::Fmt { file, split, join } => fmt_cmd::run(&file, split.as_deref(), join),
        Cmd::Facts { target } => facts_cmd::run(&target).await,
        Cmd::Types { name, json } => types_cmd::run(name.as_deref(), json),
        Cmd::Schema {
            file,
            output,
            to_stdout,
        } => schema_cmd::run(file.as_deref(), output.as_deref(), to_stdout),
        Cmd::Lint {
            paths,
            strict,
            json,
            stats,
        } => lint::run(&paths, strict, json, stats),
        Cmd::Procedure {
            name,
            file,
            target,
            set,
        } => match blueprint_source(&file, &None) {
            Some(p) => blueprint::run_procedure(&p, &name, &target, &set).await,
            None => anyhow::bail!("`crater procedure` 需要 `-f <blueprint.yaml>`"),
        },
        Cmd::Destroy {
            file,
            source,
            target,
            yes,
            set,
        } => {
            if let Some(p) = stack_source(&file, &source) {
                return stack_cmd::destroy(&p, &target, &set, yes).await;
            }
            match source_of(&file, &source, &target, &set)? {
                Some(r) => {
                    blueprint::destroy_blueprint(&r.blueprint, &r.target, &r.sets, yes).await
                }
                None => bail!(local_only("destroy", &source)),
            }
        }
        Cmd::Verify {
            json,
            file,
            source,
            target,
            set,
        } => {
            if let Some(p) = stack_source(&file, &source) {
                return stack_cmd::run(&p, &target, &set, StackMode::Verify).await;
            }
            match source_of(&file, &source, &target, &set)? {
                Some(r) => {
                    blueprint::verify_blueprint_json(
                        &r.blueprint,
                        &r.target,
                        &r.sets,
                        json.as_deref(),
                    )
                    .await
                }
                None => bail!(local_only("verify", &source)),
            }
        }
        Cmd::Doctor {
            file,
            host,
            user,
            password,
            port,
        } => doctor(file, host, &user, password, port).await,
        Cmd::Run {
            host,
            user,
            password,
            port,
            cmd,
        } => run_adhoc(&host, &user, password, port, &cmd.join(" ")).await,
    }
}

async fn run_adhoc(
    host: &str,
    user: &str,
    password: Option<String>,
    port: u16,
    cmd: &str,
) -> Result<()> {
    let pw = password
        .or_else(|| std::env::var("CRATER_SSH_PASSWORD").ok())
        .ok_or_else(|| anyhow!("--password (or CRATER_SSH_PASSWORD) required"))?;
    let exec = SshExecutor::connect(host, port, user, &pw).await?;
    let out = exec.run(cmd).await?;
    print!("{}", out.stdout);
    if !out.stderr.trim().is_empty() {
        eprintln!("--- stderr ---\n{}", out.stderr);
    }
    println!("--- exit {} ---", out.code);
    std::process::exit(if out.ok() { 0 } else { out.code });
}

async fn push_file(
    host: &str,
    user: &str,
    password: Option<String>,
    port: u16,
    src: &Path,
    dst: &str,
    chmod: Option<String>,
) -> Result<()> {
    let pw = password
        .or_else(|| std::env::var("CRATER_SSH_PASSWORD").ok())
        .ok_or_else(|| anyhow!("--password (or CRATER_SSH_PASSWORD) required"))?;
    let data = std::fs::read(src).map_err(|e| anyhow!("read {}: {e}", src.display()))?;
    println!(
        "Pushing {} ({} bytes) -> {user}@{host}:{dst} ...",
        src.display(),
        data.len()
    );
    let exec = SshExecutor::connect(host, port, user, &pw).await?;
    exec.write_file(dst, &data).await?;
    if let Some(mode) = chmod {
        let out = exec.run(&format!("chmod {mode} '{dst}'")).await?;
        if !out.ok() {
            anyhow::bail!("chmod {mode} {dst} failed: {}", out.stderr.trim());
        }
    }
    // Confirm via sha256 on the remote side.
    let out = exec
        .run(&format!("sha256sum '{dst}' | cut -d' ' -f1"))
        .await?;
    println!("remote sha256: {}", out.stdout.trim());
    println!("local  sha256: {}", crater_core::bundle::sha256_hex(&data));
    println!("Done.");
    Ok(())
}

/// `crater <name> [flags]` ≡ `crater apply <name> [flags]` (D-046): the bare
/// name routes to the named task `tasks/<name>.yaml`. The old component-spec
/// 语法错了就罢工 —— 那恰恰是最需要它的时候。
fn known_systemd_units(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.starts_with('.') && name != "target" {
                    stack.push(p);
                }
                continue;
            }
            if !blueprint::is_blueprint_file(&p) {
                continue;
            }
            let Ok(bp) = crater_ir::parse::blueprint_from_path(&p) else {
                continue;
            };
            for r in &bp.resources {
                if r.ty != "service" && r.ty != "systemd_unit" {
                    continue;
                }
                // 名字可能是 `${params.x}` 插值 —— 那种取不出字面量,跳过。
                if let Some(crater_ir::ir::Value::Lit(y)) = r.args.get("name") {
                    if let Some(n) = y.as_str() {
                        if !out.contains(&n.to_string()) {
                            out.push(n.to_string());
                        }
                    }
                }
            }
        }
    }
    out
}

async fn doctor(
    file: Option<PathBuf>,
    host: Option<String>,
    user: &str,
    password: Option<String>,
    port: u16,
) -> Result<()> {
    use crater_core::diagnose;

    // Gather the text to analyze: a local file, or collected from a host.
    let text = if let Some(f) = &file {
        std::fs::read_to_string(f).map_err(|e| anyhow!("read {}: {e}", f.display()))?
    } else if let Some(h) = &host {
        let pw = password
            .clone()
            .or_else(|| std::env::var("CRATER_SSH_PASSWORD").ok())
            .ok_or_else(|| anyhow!("--password required for --host"))?;
        let exec = SshExecutor::connect(h, port, user, &pw).await?;
        // Collect failure signals. Per-unit journals are derived from component
        // data (no hardcoded service names); the rest is product-agnostic.
        let mut probe = String::new();
        for unit in known_systemd_units(&PathBuf::from("tasks")) {
            probe.push_str(&format!(
                "echo '== journal: {unit} =='; journalctl -u {unit} --no-pager -n 50 2>/dev/null; "
            ));
        }
        probe.push_str(
            "echo '== recent errors =='; journalctl -p err --no-pager -n 100 2>/dev/null; \
             echo '== disk =='; df -h 2>/dev/null; \
             echo '== apt =='; tail -n 50 /var/log/apt/term.log 2>/dev/null",
        );
        exec.run(&probe).await?.stdout
    } else {
        return Err(anyhow!("provide --file <log> or --host <ip> to diagnose"));
    };

    let findings = diagnose::diagnose(&text);
    println!(
        "crater doctor — {} built-in rules, {} finding(s)\n",
        diagnose::rule_count(),
        findings.len()
    );
    if findings.is_empty() {
        println!("No known issue signatures matched.");
    } else {
        for (i, f) in findings.iter().enumerate() {
            println!("{}. [{}] {}", i + 1, f.category, f.cause);
            println!("   fix: {}\n", f.fix);
        }
    }

    // `--ai` 那段深度分析随旧管线一起删了(D-151):它调的是
    // `crater_core::ai`,而那个模块生成的是**旧 task YAML**。
    Ok(())
}

/// 旧 task 管线已删(D-151)—— 给撞上它的人一条能照做的出路。
///
/// 说清三件事:这条路没了、新的怎么写、去哪找例子。只说"不支持"会让人以为
/// 是自己写错了,然后去调参数 —— 而真相是这个形状的输入整个不再存在。
/// `verify` / `destroy` 只认本地。
///
/// 对一个**从没装过**的名字谈"漂移"或"退役"没有意义:漂移是拿现场比记录,
/// 退役是移除装过的东西 —— 两者都以"装过"为前提。为它去仓库拉一个包下来,
/// 拉到的也只是"包长什么样",不是"这台机器上有什么"。
fn local_only(cmd: &str, source: &Option<String>) -> String {
    let name = source.as_deref().unwrap_or("<名字>");
    let here = named::apps_in(Path::new("."));
    let mut m = format!("这里没有 {name}.app.yaml —— `crater {cmd}` 只认本地已装的任务。\n");
    if here.is_empty() {
        m.push_str("\n这个目录下还没有任何任务。\n");
    } else {
        m.push_str(&format!("\n这个目录下有:{}\n", here.join("、")));
    }
    m.push_str(&format!(
        "\n先装上再谈{}:\n  crater apply {name} -i inventory.yaml\n\n\
         或者直接给蓝图文件:`crater {cmd} -f <蓝图>`。",
        if cmd == "verify" { "对账" } else { "退役" }
    ));
    m
}

/// 口令从哪来:标准输入 → 命令行 → 交互式提问。
///
/// **命令行那条是最差的一条**,但删不掉(存量脚本在用),所以把它降级成
/// 三选一里最不推荐的:令牌写在 argv 里,同机器上任何人 `ps` 一下就看得见,
/// 而且它会留在 shell 历史里 —— 令牌被抄写的次数,就是它泄漏的机会次数
/// (与 D-129 里"crater 只读 docker 凭据、不让人再写一遍"同一条理由)。
/// 多行提示里**不要用 `\` 续行**:rustfmt 会把续行折成一行,而行首的缩进
/// 空格会留在字符串里,打印出来是错位的。这个坑在 `store::timeout_hint`
/// 踩过一次,写这个函数时又踩了一次 —— 所以两处都改成了整行字面量。
fn read_password(from_arg: Option<String>, from_stdin: bool) -> Result<String> {
    if from_stdin {
        let mut buf = String::new();
        std::io::BufRead::read_line(&mut std::io::stdin().lock(), &mut buf)?;
        let p = buf.trim_end_matches(['\n', '\r']).to_string();
        if p.is_empty() {
            bail!("`--password-stdin` 从标准输入读到的是空的");
        }
        return Ok(p);
    }
    if let Some(p) = from_arg {
        return Ok(p);
    }
    // 没有交互式提问。不回显的输入要引一个新依赖(rpassword),而真正的需求
    // 只是"令牌别进 argv" —— `--password-stdin` 已经满足了。为一个便利多背
    // 一个依赖不划算,尤其在这个二进制要静态链接、进气隙现场的场景下。
    bail!(
        "没给口令。用管道从标准输入给,令牌就不会留在 `ps` 和 shell 历史里:\n  echo <令牌> | crater registry login <registry> -u <用户名> --password-stdin"
    )
}

fn legacy_note(cmd: &str) -> String {
    format!(
        "`crater {cmd}` 现在只接受**蓝图**(`*.blueprint.yaml`)或**栈**\
         (`*.stack.yaml`)—— 旧 task 管线(顶层 `actions:`)已删除。\n\
         \n\
         迁移:一个 task 对应一份蓝图,`actions:` 里的每一步对应 `resources:`\n\
         里的一条声明(类型名基本同名)。`crater types` 列出全部 26 种类型\n\
         及其字段;例子在 `library/` 下,`library/_template/` 是最小骨架。"
    )
}

#[cfg(test)]
mod help_tests {
    use super::*;
    use clap::CommandFactory;

    /// 分组清单是**手写**的(clap 4 不支持子命令分组),所以必须有东西盯着
    /// 它:漏一条、多一条、改个名,这里就红。
    ///
    /// 没有这个测试,`COMMAND_GROUPS` 就是又一份"没人跑的文档" —— 而这个
    /// 仓库今天已经因为同一种病修过三次(README 的 404、`update` 的版本号、
    /// `ui` 那三段粘连的说明)。
    #[test]
    fn grouped_listing_covers_every_command() {
        let cmd = Cli::command();
        let real: std::collections::BTreeSet<String> = cmd
            .get_subcommands()
            .map(|c| c.get_name().to_string())
            .collect();

        // 清单里每行形如 `    <名字>   <一句话>`,取首个词。
        let listed: std::collections::BTreeSet<String> = COMMAND_GROUPS
            .lines()
            .filter(|l| l.starts_with("    ") && !l.starts_with("     "))
            .filter_map(|l| l.split_whitespace().next())
            .map(str::to_string)
            .collect();

        let missing: Vec<_> = real.difference(&listed).collect();
        let stale: Vec<_> = listed.difference(&real).collect();
        assert!(
            missing.is_empty() && stale.is_empty(),
            "分组清单与真实命令对不上。\n  清单里少了:{missing:?}\n  清单里多了(已不存在):{stale:?}"
        );
    }

    /// 每条命令都得有一句话说明 —— 空 about 在分组清单里是一行光秃秃的名字。
    #[test]
    fn every_command_has_a_one_line_summary() {
        let cmd = Cli::command();
        let naked: Vec<_> = cmd
            .get_subcommands()
            .filter(|c| c.get_about().is_none())
            .map(|c| c.get_name().to_string())
            .collect();
        assert!(naked.is_empty(), "这些命令没有一句话说明:{naked:?}");
    }

    /// help 里写的每一条例子,都真拿去解析一遍。
    ///
    /// 这次重写里我自己编错了三条:`fmt --extract`(真名 `--split`)、
    /// `cp` 写成位置参数(真的是 `--src/--dst`)、`load -f`(其实是位置参数)。
    /// 三条都是**看着对**的。靠人逐条核会漏,靠这个不会 —— 标志改了名而例子
    /// 没跟着改,这里立刻红。
    #[test]
    fn every_documented_example_actually_parses() {
        let cmd = Cli::command();
        let mut bad = Vec::new();
        for sub in cmd.get_subcommands() {
            let Some(help) = sub.get_after_help() else {
                continue;
            };
            for line in help.to_string().lines() {
                let line = line.trim();
                if !line.starts_with("crater ") {
                    continue;
                }
                // 例子里可能有 shell 重定向或行尾注释,那都不是 crater 的参数。
                let line = line.split(" > ").next().unwrap_or(line);
                let line = line.split(" #").next().unwrap_or(line).trim();
                let argv: Vec<&str> = line.split_whitespace().collect();
                if let Err(e) = Cli::command().try_get_matches_from(&argv) {
                    // 缺必填值之类的解析失败才算错;`--help`/`--version` 那种
                    // "正常提前退出"不算。
                    use clap::error::ErrorKind;
                    if !matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
                        bad.push(format!("{line}\n      → {}", e.kind()));
                    }
                }
            }
        }
        assert!(
            bad.is_empty(),
            "help 里这些例子解析不了:\n  {}",
            bad.join("\n  ")
        );
    }

    /// clap 的帮助模板出错是**运行时** panic,不是编译错 —— 没有这个测试,
    /// 写错一个 `{}` 占位符要等到用户敲 `--help` 才发现。
    #[test]
    fn help_renders_without_panicking() {
        let mut cmd = Cli::command();
        let rendered = cmd.render_help().to_string();
        assert!(rendered.contains("命令:"), "分组清单没渲染出来");
        assert!(rendered.contains("crater <命令>"), "usage 行没渲染出来");
    }
}
