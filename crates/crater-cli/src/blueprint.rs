//! 新 IR 管线的命令行入口:`crater plan` 遇到 **blueprint 格式**的文件时走这里
//! (旧版 task 文件仍走 `apply.rs`,两条管线并存到迁移完成)。
//!
//! 当前只支持**本机**目标 —— SSH / 自举 agent 的 `Ctx` 实现在执行层(P1)。
//! 但"plan 零写入"这条纪律与目标是谁无关,本机就能把它演示清楚。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use crater_core::executor::Executor;
use crater_core::spec::Host;
use crater_ir::ctx::LocalCtx;
use crater_ir::eval::Yaml;
use crater_ir::plan::{self, Plan, RunReport};
use crater_ir::procedure::{self, Targets};
use crater_ir::fleet::{Fleet, Member};
use crater_ir::state::{self, DeploymentRecord, DriftVerdict, FileStore, Store};
use crater_ir::verbs::{Change, Ctx};
use crater_ir::{lint, parse, Blueprint};

use crate::material_ctx::{BlobMap, MaterialCtx};
use crate::target::{connect_executor, TargetOpts};

/// 把 crater-core 的**异步** [`Executor`] 桥成 IR 的**同步** [`Ctx`]。
///
/// 为什么 `Ctx` 保持同步:它是四层实现共同的契约(Rust 内建 / blueprint types /
/// WASM 插件 / 协议桥),其中 WASM 侧天然是同步的;让整条 trait 染上 async 会
/// 把 `crater-ir` 拖进 tokio 依赖,而这个 crate 刻意保持最小依赖面。
///
/// 桥接只此一处,但要认两种线程:
/// - **runtime worker 线程**(逐台 plan/apply 的主路径):`block_in_place` 先把
///   线程让出去再阻塞,免得占着 worker 饿死调度器。要求多线程 runtime
///   (crater-cli 的 `#[tokio::main]` 即是)。
/// - **并发调度派生的普通线程**(`--parallel N` 时 `run_capped` 的作用域线程):
///   它们不在 runtime 上下文里,`block_in_place` 会直接 panic —— 那里必须用
///   构造时捕获的 Handle 直接 `block_on`。
///
/// 分派靠 `Handle::try_current()`:它成功当且仅当当前线程处在 runtime 上下文中。
struct RemoteCtx {
    exec: Box<dyn Executor>,
    /// 构造时(主线程上)捕获,供派生线程回到 runtime 上执行 SSH 往返。
    rt: tokio::runtime::Handle,
}

impl RemoteCtx {
    fn bridge<T>(&self, fut: impl std::future::Future<Output = T>) -> T {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.rt.block_on(fut))
        } else {
            // 非 runtime 线程:直接阻塞它就好,没有 worker 可饿死。
            self.rt.block_on(fut)
        }
    }

    fn exec_sync(&self, cmd: &str) -> Result<(i32, String)> {
        let out = self.bridge(self.exec.run(cmd))?;
        let mut text = out.stdout;
        if !out.stderr.is_empty() {
            text.push_str(&out.stderr);
        }
        Ok((out.code, text))
    }
}

impl Ctx for RemoteCtx {
    fn probe(&self, cmd: &str) -> anyhow::Result<(i32, String)> {
        self.exec_sync(cmd)
    }
    fn run(&self, cmd: &str) -> anyhow::Result<(i32, String)> {
        self.exec_sync(cmd)
    }
    fn write_file(&self, path: &str, content: &str) -> anyhow::Result<()> {
        self.bridge(self.exec.write_file(path, content.as_bytes()))
    }
    fn write_bytes(&self, path: &str, content: &[u8]) -> anyhow::Result<()> {
        // Executor 侧本就走分块 base64,二进制安全 —— 这里只是把它接出来。
        self.bridge(self.exec.write_file(path, content))
    }
    fn place_material(&self, name: &str, dest: &str) -> anyhow::Result<()> {
        // 物料解析由 `MaterialCtx` 包在外层完成;裸的传输层不该知道物料是什么。
        anyhow::bail!("物料 `{name}` → {dest}:未经 MaterialCtx 包装(内部错误)")
    }
}

/// 这个文件是不是新 IR 的 blueprint(而非旧 task / inventory / 别的 YAML)。
pub fn is_blueprint_file(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
        return false;
    };
    let Some(m) = v.as_mapping() else { return false };
    // 旧 task 的标志字段优先 —— 免得把待迁移文件误抓进新管线。
    if m.contains_key(serde_yaml::Value::from("actions"))
        || m.contains_key(serde_yaml::Value::from("plays"))
    {
        return false;
    }
    ["resources", "procedures", "types"]
        .iter()
        .any(|k| m.contains_key(serde_yaml::Value::from(*k)))
}

/// `crater plan -f blueprint.yaml [--host H] [--set k=v]` —— **零写入**预演。
pub async fn plan_blueprint(path: &Path, target: &TargetOpts, sets: &[String]) -> Result<()> {
    run_on_targets(path, target, sets, Mode::Plan, &Lens::default()).await
}

/// 栈透镜:栈加在一份蓝图上的两样东西 —— 组名重映射与作者侧参数。
///
/// 之所以是"透镜"而不是"改写蓝图":蓝图一字不改,只是**被这样看**。
/// 同一份蓝图在另一个栈里换一副透镜就换了组名,这正是它可复用的原因。
#[derive(Default, Clone)]
pub struct Lens {
    /// 蓝图组名 → inventory 组名。
    pub groups: std::collections::BTreeMap<String, String>,
    /// 作者侧参数:效力是**更强的默认值**,排在 CLI `--set` 之前,因而可被盖过。
    pub params: Vec<(String, Yaml)>,
}

/// `crater apply -f blueprint.yaml [--host H] [--set k=v]` —— 先预演再收敛,并记账。
pub async fn apply_blueprint(path: &Path, target: &TargetOpts, sets: &[String]) -> Result<()> {
    run_on_targets(path, target, sets, Mode::Apply, &Lens::default()).await
}

/// 栈驱动的入口:同一条执行路径,只是多戴一副透镜。
pub(crate) async fn run_lensed(
    path: &Path,
    target: &TargetOpts,
    sets: &[String],
    mode: StackMode,
    lens: &Lens,
) -> Result<()> {
    run_on_targets(path, target, sets, mode.into(), lens).await
}

/// 对外暴露的模式(栈模块用),与内部 `Mode` 一一对应。
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum StackMode {
    Plan,
    Apply,
    Verify,
}

impl From<StackMode> for Mode {
    fn from(m: StackMode) -> Mode {
        match m {
            StackMode::Plan => Mode::Plan,
            StackMode::Apply => Mode::Apply,
            StackMode::Verify => Mode::Verify,
        }
    }
}

/// `crater verify -f blueprint.yaml [--host H]` —— **只读**核对:现实还符合期望吗?
///
/// 与 plan 的差别只在解读:plan 回答"要做什么",verify 回答"部署过的东西还对不对"。
/// 两者共用同一个 observe —— 所以不可能出现"plan 说要改、verify 说没事"。
pub async fn verify_blueprint(path: &Path, target: &TargetOpts, sets: &[String]) -> Result<()> {
    run_on_targets(path, target, sets, Mode::Verify, &Lens::default()).await
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Plan,
    Apply,
    Verify,
}

/// 逐台目标跑 plan(可选再 converge)。多台之间串行 —— 并发调度是 P1 的事,
/// 现在先把"契约在真机上站得住"证明了。
async fn run_on_targets(
    path: &Path,
    target: &TargetOpts,
    sets: &[String],
    mode: Mode,
    lens: &Lens,
) -> Result<()> {
    let converge = mode == Mode::Apply;
    let store = FileStore::default_location();
    let bp = load(path)?;
    // 栈参数在前、CLI `--set` 在后 —— 后者赢。运行期的优先级层数因此没变,
    // 栈只是往"默认值"那一层里加了一笔。
    let mut overrides = lens.params.clone();
    overrides.extend(parse_sets(sets)?);
    let hosts = target.hosts()?;

    // 组名重映射:蓝图写 `role.controlplane`,inventory 叫 `k8s_masters`,
    // 栈把两个词接上 —— 蓝图本身一字不改。
    let fleet = build_fleet(&hosts, target.declared_groups()).remap(&lens.groups);
    enforce_contract(&bp, &fleet)?;
    // 闭包在**连机器之前**装载并校验:字节坏了要在这里知道,不是推到一半。
    // `_closure_dir` 必须活到本函数结束 —— blob 就在那个临时目录里。
    let (_closure_dir, blobs) = open_closure(target)?;

    let mut failures = 0usize;
    // 自定义类型(L2)的弥合是机群级的舞:逐台 converge 只记下"需要跳哪支",
    // 收齐去重后在机群层跑**一次** —— 在循环里跑会把同一支舞跳 N 遍。
    let mut dances: std::collections::BTreeSet<String> = Default::default();
    for host in &hosts {
        let transport = build_transport(host).await?;
        println!("── {} ──", host_label(host));

        // 先探目标侧事实(裁定 C):物料的多架构变体、`when:` 条件都靠它判定,
        // 所以必须在求计划之前拿到。白名单 + 一次性采全,之后求值零往返。
        let facts = crater_ir::facts::Facts::new(transport.as_ref())
            .gather_all()
            .with_context(|| format!("{}:采集目标事实", host_label(host)))?;
        let mut scope = plan::with_overrides(plan::scope_from_defaults(&bp), &overrides);
        scope.substrate = facts;
        scope.fleet = Some(fleet.clone());
        scope.identify(&fleet_name(host), &host.roles);

        // 再包上物料解析能力 —— 传输层不该知道"物料"是什么。
        let ctx = MaterialCtx::new(transport, &bp, scope.clone(), blobs.clone(), base_dir(path));

        // 审计语境不传播上游变更 —— verify 要回答"哪里漂了",不是"该重启什么"。
        let intent = if mode == Mode::Verify {
            plan::Intent::Audit
        } else {
            plan::Intent::Converge
        };
        let plan = plan::plan_with(&bp, &scope, &ctx, intent)
            .with_context(|| format!("{}:对 {} 求计划", host_label(host), path.display()))?;

        // 记录 id 用**机群名**(inventory 的 name),不用展示标签:
        // 标签会因为地址/端口变化而改,记录会因此对不上。
        let record_id = DeploymentRecord::make_id(&bp.name, &fleet_name(host));
        let previous = store.load(&record_id).unwrap_or(None);

        if mode == Mode::Verify {
            // 只读路径:不打印"将创建"之类的动作语,只回答"还对不对"。
            if report_drift(&state::assess(&plan, previous.as_ref()), previous.as_ref()) {
                failures += 1;
            }
            continue;
        }

        print_plan(&bp, &plan, converge);
        report_closure(&bp, &scope);

        if converge {
            if !plan.has_changes() {
                println!("无需变更,跳过执行。\n");
                continue;
            }
            match plan::converge(&bp, &scope, &ctx) {
                Ok(report) => {
                    print_report(&report);
                    dances.extend(report.procedures_needed.iter().cloned());
                    // 收敛后**重新观察一次**再记账:记录的是现实,不是意图。
                    match plan::plan(&bp, &scope, &ctx) {
                        Ok(after) => {
                            let mut rec = DeploymentRecord::from_plan(
                                &bp.name,
                                bp.version.as_deref(),
                                &fleet_name(host),
                                &after,
                            );
                            if let Some(prev) = &previous {
                                rec.applied_at = prev.applied_at; // 首次部署时间不该被刷新
                            }
                            if let Err(e) = store.save(&rec) {
                                eprintln!("(记账失败,不影响本次部署:{e})");
                            }
                            // 自定义类型此刻本就还没弥合(舞在逐台循环之后才跳),
                            // 不该当成"收敛失败"吓人;其余项才是真的没达成。
                            let stuck = after
                                .changing()
                                .filter(|i| bp.custom_type(&i.ty).is_none())
                                .count();
                            if stuck > 0 {
                                println!("注意:收敛后仍有 {stuck} 项未达期望态 —— 见上方 plan");
                            }
                        }
                        Err(e) => eprintln!("(收敛后复观察失败:{e})"),
                    }
                }
                Err(e) => {
                    failures += 1;
                    eprintln!("{}:执行失败 —— {e}\n", host_label(host));
                }
            }
        }
    }
    // 逐台收敛之后再跳机群级的舞(顺序不能反:舞往往依赖资源已就位)。
    if converge && !dances.is_empty() && failures == 0 {
        let targets = connect_fleet(&bp, &hosts, &fleet, &overrides, &base_dir(path), target.parallel, &blobs).await?;
        for name in &dances {
            println!("── procedure {name} ──");
            match procedure::run(&bp, name, &targets, &BTreeMap::new()) {
                Ok(r) => print_proc_report(&r),
                Err(e) => {
                    failures += 1;
                    eprintln!("procedure {name} 失败 —— {e}\n");
                }
            }
        }
    }

    if failures > 0 {
        // verify 的失败是"现实不符",不是"执行出错" —— 措辞不能混。
        bail!(
            "{failures}/{} 台目标{}",
            hosts.len(),
            if mode == Mode::Verify { "未通过核对" } else { "执行失败" }
        );
    }
    Ok(())
}

/// 只负责"把命令送到目标"的裸传输层(本机 / SSH),不懂物料。
async fn build_transport(host: &Host) -> Result<Box<dyn Ctx>> {
    if host.is_local() {
        return Ok(Box::new(LocalCtx));
    }
    let exec = connect_executor(host, true)
        .await
        .with_context(|| format!("连接 {}", host_label(host)))?;
    Ok(Box::new(RemoteCtx { exec, rt: tokio::runtime::Handle::current() }))
}

/// 报出这台机器实际会用到的闭包 —— air-gap 场景下这就是"要带走什么"的清单。
fn report_closure(bp: &Blueprint, scope: &plan::Scope) {
    let items = crater_ir::materials::closure(bp, scope);
    if items.is_empty() {
        return;
    }
    let mut lines = Vec::new();
    let mut broken = 0usize;
    for item in &items {
        match item {
            Ok(p) => lines.push(format!(
                "  {} ← {}{}",
                p.name,
                p.source,
                p.sha256.as_ref().map(|_| " (带摘要)").unwrap_or("")
            )),
            Err(e) => {
                broken += 1;
                lines.push(format!("  ✗ {e}"));
            }
        }
    }
    println!("闭包({} 项{}):", items.len(), if broken > 0 { format!(",{broken} 项无法解析") } else { String::new() });
    for l in lines {
        println!("{l}");
    }
    println!();
}

/// 机群级执行上下文 —— procedure 的舞要在多台之间走,所以得**同时**握住全部成员。
///
/// 与逐台 plan 的区别:那里一次只连一台;这里必须把整个机群的连接一起建起来,
/// 因为 `exports` 要把 fact 从首台带给其余台。
struct FleetTargets<'a> {
    fleet: Fleet,
    ctxs: BTreeMap<String, MaterialCtx<'a>>,
    scopes: BTreeMap<String, plan::Scope>,
    /// 机群级并发上限(`--parallel`)。步骤的 `throttle` 只能往下压。
    parallel: usize,
}

impl Targets for FleetTargets<'_> {
    fn fleet(&self) -> &Fleet {
        &self.fleet
    }
    fn ctx(&self, member: &str) -> Result<&dyn crater_ir::verbs::Ctx> {
        self.ctxs
            .get(member)
            .map(|c| c as &dyn crater_ir::verbs::Ctx)
            .ok_or_else(|| anyhow::anyhow!("没有 `{member}` 的连接"))
    }
    fn scope(&self, member: &str) -> Result<plan::Scope> {
        self.scopes
            .get(member)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("没有 `{member}` 的求值上下文"))
    }
    fn parallelism(&self) -> usize {
        self.parallel
    }
}

/// 把整个机群连起来 —— 舞开始之后才发现某台连不上,是最糟的失败时机。
async fn connect_fleet<'a>(
    bp: &'a Blueprint,
    hosts: &'a [Host],
    fleet: &Fleet,
    overrides: &[(String, Yaml)],
    base: &Path,
    parallel: usize,
    blobs: &BlobMap,
) -> Result<FleetTargets<'a>> {
    let mut transports = Vec::new();
    for host in hosts {
        transports.push((host, build_transport(host).await?));
    }
    let mut ctxs = BTreeMap::new();
    let mut scopes = BTreeMap::new();
    for (host, transport) in transports {
        let name = fleet_name(host);
        let facts = crater_ir::facts::Facts::new(transport.as_ref())
            .gather_all()
            .with_context(|| format!("{}:采集目标事实", host_label(host)))?;
        let mut scope = plan::with_overrides(plan::scope_from_defaults(bp), overrides);
        scope.substrate = facts;
        scope.fleet = Some(fleet.clone());
        scope.identify(&name, &host.roles);
        ctxs.insert(
            name.clone(),
            MaterialCtx::new(transport, bp, scope.clone(), blobs.clone(), base.to_path_buf()),
        );
        scopes.insert(name, scope);
    }
    Ok(FleetTargets { fleet: fleet.clone(), ctxs, scopes, parallel: parallel.max(1) })
}

fn print_proc_report(report: &procedure::ProcReport) {
    for (step, member, outcome) in &report.steps {
        use crater_ir::verbs::Outcome::*;
        let tag = match outcome {
            Ok => "ok     ",
            Changed => "changed",
            Warn => "warn   ",
        };
        println!("  {tag} {member:<10} {step}");
    }
    for note in &report.skipped {
        // 一支舞"什么都没做"最常见的原因就是组选错了 —— 必须看得见。
        println!("  skip    {note}");
    }
    if !report.facts.is_empty() {
        println!(
            "  跨主机 fact:{}",
            report.facts.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    println!("执行:{}\n", report.summary());
}

/// `crater procedure <name> -f blueprint.yaml [--set k=v]` —— 跳一支具名的舞。
pub async fn run_procedure(
    path: &Path,
    proc_name: &str,
    target: &TargetOpts,
    sets: &[String],
) -> Result<()> {
    let bp = load(path)?;
    if !bp.procedures.contains_key(proc_name) {
        let known: Vec<&str> = bp.procedures.keys().map(String::as_str).collect();
        bail!(
            "blueprint `{}` 没有 procedure `{proc_name}`{}",
            bp.name,
            if known.is_empty() {
                "(它没有定义任何 procedure)".to_string()
            } else {
                format!("(可用:{})", known.join(", "))
            }
        );
    }
    let overrides = parse_sets(sets)?;
    let hosts = target.hosts()?;
    let fleet = build_fleet(&hosts, target.declared_groups());
    enforce_contract(&bp, &fleet)?;
    let (_closure_dir, blobs) = open_closure(target)?;
    let targets = connect_fleet(&bp, &hosts, &fleet, &overrides, &base_dir(path), target.parallel, &blobs).await?;

    println!("procedure {proc_name} —— {} 台目标\n", targets.fleet.members.len());
    // `--set` 既是 deploy 期参数覆盖,也是过程参数(`--set to=1.37.0`)。
    let args: BTreeMap<String, Yaml> = overrides.into_iter().collect();
    let report = procedure::run(&bp, proc_name, &targets, &args)?;
    print_proc_report(&report);
    Ok(())
}

/// 打印漂移结论。返回 true 表示"需要引起注意"(供退出码使用)。
fn report_drift(verdict: &DriftVerdict, previous: Option<&DeploymentRecord>) -> bool {
    match verdict {
        DriftVerdict::NeverDeployed => {
            // 关键区分:没部署过 ≠ 漂了。混为一谈会让 verify 天天报警。
            println!("未部署过 —— 没有记录可核对(先 `crater apply`)\n");
            false
        }
        DriftVerdict::InSync => {
            let when = previous
                .map(|p| format!(",上次部署于 {}", fmt_epoch(p.applied_at)))
                .unwrap_or_default();
            println!("✓ 现实符合期望{when}\n");
            false
        }
        DriftVerdict::Drifted(items) => {
            println!("✗ 检测到漂移({} 项):", items.len());
            for i in items {
                let tag = if i.known { "漂移" } else { "新声明" };
                println!("  {tag} {:<14} {}", i.id, i.detail);
            }
            println!();
            true
        }
        DriftVerdict::Indeterminate { drifted, unknown } => {
            if !drifted.is_empty() {
                println!("✗ 检测到漂移({} 项):", drifted.len());
                for i in drifted {
                    println!("  {:<14} {}", i.id, i.detail);
                }
            }
            // 有说不清的项就不能说"一切正常" —— 那是假的安心。
            println!("? {unknown} 项无法核对(模型化欠债)—— 不能断言一切正常\n");
            true
        }
    }
}

/// unix 秒 → 人类可读(本地时区无关的相对描述,避免引入日期库)。
fn fmt_epoch(secs: u64) -> String {
    let ago = state::now().saturating_sub(secs);
    match ago {
        0..=59 => format!("{ago} 秒前"),
        60..=3599 => format!("{} 分钟前", ago / 60),
        3600..=86399 => format!("{} 小时前", ago / 3600),
        _ => format!("{} 天前", ago / 86400),
    }
}

/// 本地物料相对哪个目录解析 —— blueprint 文件自己的目录。
fn base_dir(path: &Path) -> PathBuf {
    path.parent().unwrap_or(Path::new(".")).to_path_buf()
}

/// 机群视角:`on:` / `first()` / `rest()` 的判定依据。
/// **顺序即 inventory 声明序**,所以 `first()` 每次跑都选中同一台。
/// 装载 `--closure`(没给就是空表 → 目标机自己联网取)。
///
/// 返回的 `TempDir` 必须被调用方**持有到部署结束** —— blob 就在里面,
/// 提前 drop 会让所有物料在推送时凭空消失。
fn open_closure(target: &TargetOpts) -> Result<(Option<tempfile::TempDir>, BlobMap)> {
    let Some(path) = &target.closure else {
        return Ok((None, BlobMap::new()));
    };
    let (dir, map) = crate::closure::load(path)?;
    println!("离线闭包 {} —— {} 份物料已备好\n", path.display(), map.len());
    Ok((Some(dir), map))
}

/// 机群契约在**一切之前**校验:没连机器、没跑 preflight、更没改任何东西。
///
/// 报全部不满足项而不是第一条 —— 修 inventory 的人应当一趟改完。
fn enforce_contract(bp: &crater_ir::ir::Blueprint, fleet: &Fleet) -> Result<()> {
    if let Err(errs) = fleet.check_contract(&bp.fleet) {
        let body = errs.iter().map(|e| format!("  · {e}")).collect::<Vec<_>>().join("\n");
        anyhow::bail!(
            "inventory 不满足蓝图 `{}` 的机群契约:\n{body}\n\n(契约写在蓝图的 `fleet.groups:`)",
            bp.name
        );
    }
    Ok(())
}

fn build_fleet(hosts: &[Host], declared: impl IntoIterator<Item = String>) -> Fleet {
    Fleet::new(
        hosts
            .iter()
            .map(|h| {
                let roles: Vec<&str> = h.roles.iter().map(String::as_str).collect();
                Member::new(fleet_name(h), &roles)
            })
            .collect(),
    )
    .with_declared_roles(declared)
}

/// 机群里的稳定标识:用 inventory 里的 `name`(而非地址)——
/// 同一台机器换了 IP 仍是同一个成员,`host.n11` 这样的 selector 才有意义。
fn fleet_name(h: &Host) -> String {
    h.name.clone()
}

/// 打印用的标签。**必须区分机群里的每一台** —— 早期版本对本地目标一律显示
/// "本机",三台机器就会共用同一个部署记录 id,互相覆盖。
fn host_label(h: &Host) -> String {
    match (h.is_local(), h.name.as_str()) {
        (true, "localhost") => "本机".into(),
        (true, name) => format!("{name}(本机)"),
        (false, name) => format!("{name}({}@{}:{})", h.user, h.address, h.port),
    }
}

fn load(path: &Path) -> Result<Blueprint> {
    // path-aware:根文件声明的 `parts:` 在这里被并回来,对所有命令一视同仁。
    let bp = parse::blueprint_from_path(path).map_err(|e| anyhow::anyhow!("{e}"))?;

    // plan 之前先 lint:静态就能发现的问题不该等到探测目标机才暴露。
    let diags = lint::lint(&bp);
    let errs = lint::errors(&diags);
    if !errs.is_empty() {
        for d in &errs {
            eprintln!("  {d}");
        }
        bail!("{} 有 {} 处 lint error,先修再 plan", path.display(), errs.len());
    }
    Ok(bp)
}

/// `--set k=v` → 参数覆盖。值按 YAML 解析,于是 `--set port=9443` 是整数、
/// `--set ha=true` 是布尔 —— 不会像旧模型那样一律变成字符串。
fn parse_sets(sets: &[String]) -> Result<Vec<(String, Yaml)>> {
    sets.iter()
        .map(|kv| {
            let (k, v) = kv
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("`--set {kv}` 应是 KEY=VALUE"))?;
            let parsed: Yaml = serde_yaml::from_str(v).unwrap_or_else(|_| Yaml::from(v));
            Ok((k.to_string(), parsed))
        })
        .collect()
}

fn print_plan(bp: &Blueprint, p: &Plan, will_execute: bool) {
    let mode = if will_execute { "计划(随后执行)" } else { "计划(零写入预演)" };
    println!("blueprint {} —— {mode}\n", bp.name);
    for item in &p.items {
        let sigil = item.change.sigil();
        println!("  {sigil} {:<44} {}", item.label(), describe(&item.change));
        for f in item.change.fields() {
            println!("       {f}");
        }
    }
    println!();
    println!("计划:{}", p.summary());
    if p.debt() > 0 {
        // "接住,不羞辱,但可见":说不清的项要显式计数,不能混在成功里。
        println!(
            "其中 {} 项 plan 说不清(模型化欠债)—— 这些项 apply 前后都无法预演",
            p.debt()
        );
    }
    if !p.has_changes() {
        println!("目标已处于期望态,无需变更。");
    }
    println!();
}

fn print_report(r: &RunReport) {
    for (id, outcome) in &r.steps {
        use crater_ir::verbs::Outcome::*;
        let tag = match outcome {
            Ok => "ok     ",
            Changed => "changed",
            Warn => "warn   ",
        };
        println!("  {tag} {id}");
    }
    println!("执行:{}\n", r.summary());
}

fn describe(c: &Change) -> String {
    match c {
        Change::Ok => "已是期望态".into(),
        Change::Create(_) => "将创建".into(),
        Change::Update(_) => "将修改".into(),
        Change::Destroy => "将删除".into(),
        Change::Unknown(why) => format!("说不清({why})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_values_keep_their_yaml_types() {
        let out = parse_sets(&["port=9443".into(), "ha=true".into(), "name=demo".into()]).unwrap();
        assert_eq!(out[0].1, Yaml::from(9443));
        assert_eq!(out[1].1, Yaml::from(true));
        assert_eq!(out[2].1, Yaml::from("demo"));
    }

    #[test]
    fn set_without_equals_is_rejected() {
        assert!(parse_sets(&["justakey".into()]).is_err());
    }
}


#[cfg(test)]
mod remote_ctx_tests {
    use super::*;
    use crater_core::executor::CmdOutput;
    use std::sync::{Arc, Mutex};

    /// 一个只记账的假 Executor —— 验证的是**桥接机制**,不是 SSH 本身。
    struct MockExec {
        seen: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl Executor for MockExec {
        async fn run(&self, cmd: &str) -> crater_core::Result<CmdOutput> {
            self.seen.lock().unwrap().push(cmd.to_string());
            Ok(CmdOutput { code: 0, stdout: format!("ran:{cmd}"), stderr: String::new() })
        }
        fn label(&self) -> &str {
            "mock"
        }
    }

    /// 必须是多线程 runtime:`block_in_place` 在单线程 runtime 上会 panic,
    /// 这条测试就是那个前提的守门人。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sync_ctx_bridges_onto_the_async_executor() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let ctx = RemoteCtx { exec: Box::new(MockExec { seen: seen.clone() }), rt: tokio::runtime::Handle::current() };

        let (code, out) = ctx.probe("stat -c '%a' /etc").unwrap();
        assert_eq!(code, 0);
        assert!(out.starts_with("ran:"), "{out}");
        ctx.write_file("/etc/demo", "body").unwrap();

        let calls = seen.lock().unwrap().clone();
        assert_eq!(calls.len(), 2, "{calls:?}");
        assert!(calls[1].contains("base64 -d"), "写文件走分块 base64:{}", calls[1]);
    }

    /// 并发调度会从**普通 std 线程**调用这座桥。
    ///
    /// 这不是理论风险:`block_in_place` 在非 runtime 线程上直接 panic,
    /// 于是 `--parallel N` 会在第一条 SSH 上炸掉。这条测试就是那个分支的守门人。
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_bridge_survives_being_called_from_a_plain_thread() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let ctx = RemoteCtx {
            exec: Box::new(MockExec { seen: seen.clone() }),
            rt: tokio::runtime::Handle::current(),
        };
        // 与 run_capped 派生的线程同类:不在 runtime 上下文里。
        std::thread::scope(|s| {
            for i in 0..4 {
                let ctx = &ctx;
                s.spawn(move || {
                    let (code, _) = ctx.probe(&format!("echo {i}")).unwrap();
                    assert_eq!(code, 0);
                });
            }
        });
        assert_eq!(seen.lock().unwrap().len(), 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn stderr_is_folded_into_the_output_not_dropped() {
        struct Noisy;
        #[async_trait::async_trait]
        impl Executor for Noisy {
            async fn run(&self, _cmd: &str) -> crater_core::Result<CmdOutput> {
                Ok(CmdOutput { code: 7, stdout: "out".into(), stderr: "boom".into() })
            }
            fn label(&self) -> &str {
                "noisy"
            }
        }
        let ctx = RemoteCtx { exec: Box::new(Noisy), rt: tokio::runtime::Handle::current() };
        let (code, text) = ctx.run("whatever").unwrap();
        assert_eq!(code, 7);
        // 失败诊断全靠 stderr —— 丢了它,run_ok 的报错就成了空壳。
        assert!(text.contains("boom"), "{text}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_bare_transport_refuses_to_resolve_materials() {
        // 分层纪律:传输层只管"把命令送到目标",不知道"物料"是什么。
        // 物料解析由 MaterialCtx 包在外层 —— 裸传输层被直接调用即是内部错误。
        let ctx = RemoteCtx { exec: Box::new(MockExec { seen: Default::default() }), rt: tokio::runtime::Handle::current() };
        let err = ctx
            .place_material("rustfs-bin", "/usr/local/bin/rustfs")
            .unwrap_err()
            .to_string();
        assert!(err.contains("MaterialCtx"), "{err}");
    }
}
