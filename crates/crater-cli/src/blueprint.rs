//! 新 IR 管线的命令行入口:`crater plan` 遇到 **blueprint 格式**的文件时走这里
//! (旧版 task 文件仍走 `apply.rs`,两条管线并存到迁移完成)。
//!
//! 当前只支持**本机**目标 —— SSH / 自举 agent 的 `Ctx` 实现在执行层(P1)。
//! 但"plan 零写入"这条纪律与目标是谁无关,本机就能把它演示清楚。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::{oops, say};
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

/// `crater destroy -f blueprint.yaml [--yes]` —— 退役。
///
/// **默认只预演**。这是破坏性命令唯一负责任的默认值:`plan`/`apply` 的分工
/// 在这里塌成一条命令,所以安全阀必须在命令自己身上 —— 不加 `--yes` 就只
/// 打印会拆掉什么,一个字节都不动。
///
/// 蓝图里没有 `teardown:` 段:退役由五动词逆序推导(ir-draft §4)。
pub async fn destroy_blueprint(
    path: &Path,
    target: &TargetOpts,
    sets: &[String],
    yes: bool,
) -> Result<()> {
    destroy_lensed(path, target, sets, yes, &Lens::default()).await?;
    if !yes {
        // 只在**直接调用**时收尾;栈驱动时由栈统一说一次,免得每份蓝图重复一遍。
        say!("以上为**预演**,一个字节都没动。确认无误后加 `--yes` 执行。");
    }
    Ok(())
}

pub(crate) async fn destroy_lensed(
    path: &Path,
    target: &TargetOpts,
    sets: &[String],
    yes: bool,
    lens: &Lens,
) -> Result<()> {
    let store = FileStore::default_location();
    let bp = load(path)?;
    let mut overrides = lens.params.clone();
    overrides.extend(parse_sets(sets)?);
    let all_hosts = target.hosts()?;
    let hosts = target.exec_hosts()?;
    let fleet = build_fleet(&all_hosts, target.declared_groups()).remap(&lens.groups);
    // 退役**不**校验机群契约:契约问的是"够不够装",而拆东西不需要够 ——
    // 一个只剩一台 master 的残破集群,恰恰是最需要拆掉的那种。
    let (_closure_dir, blobs) = open_closure(target)?;

    let mut failures = 0usize;
    let mut removed_any = false;

    // 先跳退役的舞,再拆资源 —— 与 apply **相反**,而且必须相反:
    // apply 时资源要先就位(kubeadm 得先有 containerd);退役时若先卸掉
    // containerd/kubelet,etcd 里那个成员就成了永远清不掉的孤儿。
    if yes {
        let targets =
            connect_fleet(&bp, &hosts, &fleet, &overrides, &base_dir(path), target.parallel, &blobs)
                .await?;
        let dances = {
            let first = fleet.members.first().map(|m| m.name.clone());
            match first {
                Some(m) => plan::destroy_dances(&bp, &targets.scope(&m)?, targets.ctx(&m)?)?,
                None => Vec::new(),
            }
        };
        for name in &dances {
            say!("── procedure {name}(退役)──");
            match procedure::run(&bp, name, &targets, &BTreeMap::new()) {
                Ok(r) => {
                    print_proc_report(&r);
                    removed_any = true;
                }
                Err(e) => {
                    failures += 1;
                    oops!("退役过程 {name} 失败 —— {e}\n");
                }
            }
        }
        if failures > 0 {
            bail!("退役过程失败,已中止 —— 资源未拆除(避免留下半退役状态)");
        }
    }

    for host in &hosts {
        let transport = build_transport(host).await?;
        say!("── {} ──", host_label(host));
        crate::events::emit(serde_json::json!({
            "e": "host_start", "host": host.name, "label": host_label(host),
        }));
        let facts = crater_ir::facts::Facts::new(transport.as_ref())
            .gather_all()
            .with_context(|| format!("{}:采集目标事实", host_label(host)))?;
        let mut scope = plan::with_overrides(plan::scope_from_defaults(&bp), &overrides);
        scope.substrate = facts;
        scope.fleet = Some(fleet.clone());
        scope.identify(&fleet_name(host), &host.roles);
        let ctx = MaterialCtx::new(transport, &bp, scope.clone(), blobs.clone(), base_dir(path));

        let plan = plan::plan_destroy(&bp, &scope, &ctx)
            .with_context(|| format!("{}:求退役计划", host_label(host)))?;
        for item in &plan.items {
            crate::events::emit(serde_json::json!({
                "e": "plan_item", "host": host.name,
                "id": item.id, "change": change_kind(&item.change),
            }));
        }
        print_destroy_plan(&bp, &plan, yes);

        if !yes {
            continue;
        }
        if !plan.has_changes() {
            say!("没有东西可拆,跳过。\n");
            crate::events::emit(serde_json::json!({
                "e": "host_done", "host": host.name, "result": "noop",
            }));
            continue;
        }
        removed_any = true;
        match plan::destroy(&bp, &scope, &ctx) {
            Ok(report) => {
                // 退役步骤事后逐条发:destroy 单台通常秒级,粒度仍到资源。
                for (id, oc) in &report.steps {
                    use crater_ir::verbs::Outcome as O;
                    crate::events::emit(serde_json::json!({
                        "e": "step", "host": host.name, "id": id,
                        "outcome": match oc { O::Ok => "ok", O::Changed => "changed", O::Warn => "warn" },
                    }));
                }
                print_report(&report);
                crate::events::emit(serde_json::json!({
                    "e": "host_done", "host": host.name, "result": "ok",
                }));
            }
            Err(e) => {
                failures += 1;
                oops!("退役失败 —— {e}");
                crate::events::emit(serde_json::json!({
                    "e": "host_done", "host": host.name, "result": "failed",
                    "detail": format!("{e:#}"),
                }));
            }
        }
        // 部署记录跟着资源一起走。留着它,下次 verify 会拿一份已经不存在的
        // 部署去核对现实,报出一堆"漂移"。
        let record_id = DeploymentRecord::make_id(&bp.name, &fleet_name(host));
        if let Err(e) = store.remove(&record_id) {
            oops!("(部署记录清理失败,不影响本次退役:{e})");
        }
    }

    if !yes {
        return Ok(());
    }
    if failures > 0 {
        bail!("{failures}/{} 台目标退役失败", hosts.len());
    }
    if !removed_any {
        say!("所有目标本就是干净的。");
    }
    Ok(())
}

fn print_destroy_plan(bp: &Blueprint, p: &Plan, will_execute: bool) {
    let mode = if will_execute { "退役(随后执行)" } else { "退役预演(零写入)" };
    say!("blueprint {} —— {mode}\n", bp.name);
    for item in &p.items {
        // 退役计划里 `-` 是"将删除",`✓` 是"本就不在" —— 后者不是成功,
        // 是"没什么可做",措辞要分得开。
        let note = match &item.change {
            Change::Destroy => "将删除",
            Change::Ok => "不在(无需处理)",
            Change::Unknown(w) if w.starts_with("保留:") => "保留",
            Change::Unknown(_) => "说不清",
            _ => "?",
        };
        say!("  {} {:<44} {note}", item.change.sigil(), item.label());
        if let Change::Unknown(why) = &item.change {
            say!("       {}", why.trim_start_matches("保留:"));
        }
    }
    say!("\n退役:{}", p.summary());
    if !p.has_changes() {
        say!("目标上没有本蓝图的任何资源。");
    }
    say!();
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

/// `crater verify -f blueprint.yaml [--json <path>]` —— **只读**核对:现实还符合期望吗?
///
/// 与 plan 的差别只在解读:plan 回答"要做什么",verify 回答"部署过的东西还对不对"。
/// 两者共用同一个 observe —— 所以不可能出现"plan 说要改、verify 说没事"。
/// `--json` 给核对结果一份机器可读输出 —— UI 的对账供血管道。
pub async fn verify_blueprint_json(
    path: &Path,
    target: &TargetOpts,
    sets: &[String],
    json_out: Option<&Path>,
) -> Result<()> {
    VERIFY_JSON.with(|v| *v.borrow_mut() = Some(Vec::new()));
    let r = run_on_targets(path, target, sets, Mode::Verify, &Lens::default()).await;
    let entries = VERIFY_JSON.with(|v| v.borrow_mut().take()).unwrap_or_default();
    if let Some(out) = json_out {
        let doc = serde_json::json!({
            "blueprint": path.display().to_string(),
            "inventory": target.inventory.as_ref().map(|p| p.display().to_string()),
            "ts": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0),
            "hosts": entries,
        });
        std::fs::write(out, serde_json::to_string_pretty(&doc)?)?;
    }
    r
}

thread_local! {
    /// Verify 模式的结构化收集槽。线程局部而非改十几处签名:
    /// run_on_targets 的调用链很深,verify 又只是三种模式之一。
    static VERIFY_JSON: std::cell::RefCell<Option<Vec<serde_json::Value>>> =
        const { std::cell::RefCell::new(None) };
}

fn verify_collect(entry: serde_json::Value) {
    VERIFY_JSON.with(|v| {
        if let Some(list) = v.borrow_mut().as_mut() {
            list.push(entry);
        }
    });
}

/// 一台主机的核对结论 → JSON(供 --json 与 UI 的对账看板)。
fn verdict_json(
    host: &str,
    record_id: &str,
    v: &crater_ir::state::DriftVerdict,
    prev: Option<&DeploymentRecord>,
) -> serde_json::Value {
    use crater_ir::state::DriftVerdict as V;
    let (verdict, drifted, unknown) = match v {
        V::NeverDeployed => ("never", vec![], 0usize),
        V::InSync => ("in_sync", vec![], 0),
        V::Drifted(d) => ("drifted", d.clone(), 0),
        V::Indeterminate { drifted, unknown } => ("indeterminate", drifted.clone(), *unknown),
    };
    serde_json::json!({
        "host": host,
        "record_id": record_id,
        "verdict": verdict,
        "drifted": drifted.iter().map(|d| serde_json::json!({
            "id": d.id, "detail": d.detail, "known": d.known,
        })).collect::<Vec<_>>(),
        "unknown": unknown,
        "applied_at": prev.map(|p| p.applied_at),
        "blueprint_sha256": prev.and_then(|p| p.blueprint_sha256.clone()),
    })
}

/// 期望态文件的 sha256(读不到返回 None —— 指纹缺失按 Unknown 处理,不误报)。
fn file_sha(p: &Path) -> Option<String> {
    std::fs::read(p).ok().map(|b| crater_core::bundle::sha256_hex(&b))
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
    // 机群契约按**整份 inventory** 成立,循环只走 `--limit` 选中的那些 ——
    // 限定的是"这次动谁",不是"机群变小了"(见 target::apply_limit)。
    let all_hosts = target.hosts()?;
    let hosts = target.exec_hosts()?;

    // 组名重映射:蓝图写 `role.controlplane`,inventory 叫 `k8s_masters`,
    // 栈把两个词接上 —— 蓝图本身一字不改。
    let fleet = build_fleet(&all_hosts, target.declared_groups()).remap(&lens.groups);
    enforce_contract(&bp, &fleet)?;
    // 闭包在**连机器之前**装载并校验:字节坏了要在这里知道,不是推到一半。
    // `_closure_dir` 必须活到本函数结束 —— blob 就在那个临时目录里。
    let (_closure_dir, blobs) = open_closure(target)?;

    let mut failures = 0usize;
    // 自定义类型(L2)的弥合是机群级的舞:逐台 converge 只记下"需要跳哪支",
    // 收齐去重后在机群层跑**一次** —— 在循环里跑会把同一支舞跳 N 遍。
    let mut dances: std::collections::BTreeSet<String> = Default::default();
    // linear 策略走另一条执行路径:资源在外层、机器在内层。
    if target.strategy == crate::target::Strategy::Linear && mode == Mode::Apply {
        return run_linear(&bp, &hosts, &fleet, &overrides, path, target, &blobs, &store).await;
    }

    // 前缀列宽按本轮全体主机定,各行正文才对得齐。
    crate::out::fleet(&hosts.iter().map(|h| h.name.clone()).collect::<Vec<_>>());
    // 模式与蓝图名**只报一次** —— 每台重复一遍,五台就是五遍同样的话。
    say!(
        "blueprint {} —— {},{} 台目标",
        bp.name,
        match mode {
            Mode::Plan => "零写入预演",
            Mode::Apply => "计划随后执行",
            Mode::Verify => "只读核对",
        },
        hosts.len()
    );
    // 机群汇总:每台一行,跑完一起给 —— 逐台的 `执行:...` 散在几百行中间,
    // 五台机器跑完根本拼不出全貌。
    // 每台一行**固定列**的计数器(ansible PLAY RECAP 的形状)。
    // 原先各分支各塞一句话("无变更" / "changed=1 ok=0" / "计划 +1 ~0"),
    // 列对不齐、也没法横向比较 —— 汇总的全部价值就在能横着扫。
    #[derive(Default, Clone)]
    struct Tally {
        ok: usize,
        changed: usize,
        warn: usize,
        failed: usize,
        drifted: usize,
    }
    let mut recap: Vec<(String, Tally)> = Vec::new();

    // 资源 × 主机的矩阵。逐行输出回答"这台机器怎么了",矩阵回答"这个资源
    // 在哪几台上出了状况" —— 后者正是 ansible 的 task-major 视图擅长的事,
    // 而 crater 的执行是逐台的,所以把它做成**跑完之后的一次汇总**,
    // 不必为了输出形状去改执行模型。
    //
    // 顺带补上了 skipped 的信息:某台没有某个资源(选择器/when 没选中它)时
    // 格子是 `·` —— 此前那种情况在输出里直接消失,分不清"跳过"和"没这条"。
    let mut mx_rows: Vec<String> = Vec::new();               // 资源 id,首现顺序
    let mut mx_label: BTreeMap<String, String> = BTreeMap::new();
    let mut mx: BTreeMap<String, BTreeMap<String, char>> = BTreeMap::new();
    let note = |rows: &mut Vec<String>,
                    lab: &mut BTreeMap<String, String>,
                    cells: &mut BTreeMap<String, BTreeMap<String, char>>,
                    host: &str,
                    id: &str,
                    label: &str,
                    c: char| {
        if !lab.contains_key(id) {
            rows.push(id.to_string());
            lab.insert(id.to_string(), label.to_string());
        }
        cells.entry(id.to_string()).or_default().insert(host.to_string(), c);
    };
    let batches = crate::target::batches(&hosts, target.serial.as_deref())?;
    let n_batches = batches.len();
    'outer: for (bi, batch) in batches.iter().enumerate() {
    if n_batches > 1 {
        crate::out::leave();
        say!(
            "── 批次 {}/{}({})──",
            bi + 1,
            n_batches,
            batch.iter().map(|h| h.name.as_str()).collect::<Vec<_>>().join(", ")
        );
    }
    let batch_fail_before = failures;
    for host in batch {
        let transport = build_transport(host).await?;
        crate::out::enter(&host.name);
        // `@local` 是本机哨兵值,直接渲染出来是 `root@@local:22`,像个 bug。
        if host.is_local() {
            say!("本机执行");
        } else {
            say!("连接 {}@{}:{}", host.user, host.address, host.port);
        }
        crate::events::emit(serde_json::json!({
            "e": "host_start", "host": host.name, "label": host_label(host),
        }));

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
        // 三个动词共用:audit 语境下 ok = 同步、非 ok = 漂移候选;
        // converge 语境下则是"待执行预告",之后被 step 事件逐条定案。
        for item in &plan.items {
            crate::events::emit(serde_json::json!({
                "e": "plan_item", "host": host.name,
                "id": item.id, "change": change_kind(&item.change),
            }));
        }

        // 记录 id 用**机群名**(inventory 的 name),不用展示标签:
        // 标签会因为地址/端口变化而改,记录会因此对不上。
        let record_id = DeploymentRecord::make_id(&bp.name, &fleet_name(host));
        let previous = store.load(&record_id).unwrap_or(None);

        if mode == Mode::Verify {
            // 只读路径:不打印"将创建"之类的动作语,只回答"还对不对"。
            let verdict = state::assess(&plan, previous.as_ref());
            let vj = verdict_json(&fleet_name(host), &record_id, &verdict, previous.as_ref());
            crate::events::emit(serde_json::json!({
                "e": "verify", "host": host.name, "report": vj.clone(),
            }));
            verify_collect(vj);
            // 核对本身就是一次"上次什么时候看过"的证据 —— 回写 verified_at,
            // UI 的"多久没核对"才有数据;资源快照保持 apply 时的孪生,不动。
            if let Some(mut prev) = previous.clone() {
                prev.verified_at = Some(std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0));
                let _ = store.save(&prev);
            }
            let failed = report_drift(&verdict, previous.as_ref());
            for item in &plan.items {
                note(&mut mx_rows, &mut mx_label, &mut mx, &host.name, &item.id, &item.label(), '✓');
            }
            let n_drift = match &verdict {
                crater_ir::state::DriftVerdict::Drifted(d) => d.len(),
                crater_ir::state::DriftVerdict::Indeterminate { drifted, .. } => drifted.len(),
                _ => 0,
            };
            if let crater_ir::state::DriftVerdict::Drifted(d) = &verdict {
                for it in d {
                    if let Some(row) = mx.get_mut(&it.id) {
                        row.insert(host.name.clone(), '✗');
                    }
                }
            }
            recap.push((
                host.name.clone(),
                Tally {
                    // 逐资源计数,而不是"这台漂没漂" —— 一台漂了 1 项和漂了 15 项
                    // 是两回事,汇总要能一眼看出轻重。
                    ok: plan.items.len().saturating_sub(n_drift),
                    drifted: n_drift,
                    failed: usize::from(failed && n_drift == 0), // 连不上之类
                    ..Default::default()
                },
            ));
            crate::events::emit(serde_json::json!({
                "e": "host_done", "host": host.name,
                "result": if failed { "drifted" } else { "ok" },
            }));
            if failed {
                failures += 1;
            }
            continue;
        }

        print_plan(&bp, &plan, converge);
        report_closure(&bp, &scope);

        if converge {
            if !plan.has_changes() {
                say!("跳过执行(无变更)");
                // 未变更的机器**也要进汇总** —— 汇总缺了它们,就无法回答
                // "这次到底覆盖了几台",而那正是多机部署后第一个要确认的事。
                for item in &plan.items {
                    note(&mut mx_rows, &mut mx_label, &mut mx, &host.name,
                         &item.id, &item.label(), '✓');
                }
                // 已是期望态 ≠ 什么都没发生:这些资源都被观察过、判定为符合。
                // 报 ok=0 会让"全都对"和"一个都没查"看起来一样。
                recap.push((
                    host.name.clone(),
                    Tally {
                        ok: plan.items.iter()
                            .filter(|i| matches!(i.change, crater_ir::verbs::Change::Ok))
                            .count(),
                        ..Default::default()
                    },
                ));
                crate::events::emit(serde_json::json!({
                    "e": "host_done", "host": host.name, "result": "noop",
                }));
                continue;
            }
            let step_host = host.name.clone();
            let on_step = move |id: &str, oc: crater_ir::verbs::Outcome| {
                use crater_ir::verbs::Outcome as O;
                crate::events::emit(serde_json::json!({
                    "e": "step", "host": step_host, "id": id,
                    "outcome": match oc { O::Ok => "ok", O::Changed => "changed", O::Warn => "warn" },
                }));
            };
            match plan::converge_with(&bp, &scope, &ctx, &on_step) {
                Ok(report) => {
                    print_report(&report);
                    let by_id: BTreeMap<&str, String> =
                        plan.items.iter().map(|i| (i.id.as_str(), i.label())).collect();
                    for (id, oc) in &report.steps {
                        use crater_ir::verbs::Outcome as O;
                        let c = match oc { O::Ok => '✓', O::Changed => '~', O::Warn => '!' };
                        let lab = by_id.get(id.as_str()).cloned().unwrap_or_else(|| id.clone());
                        note(&mut mx_rows, &mut mx_label, &mut mx, &host.name, id, &lab, c);
                    }
                    recap.push((
                        host.name.clone(),
                        Tally {
                            ok: report.ok(),
                            changed: report.changed(),
                            warn: report.steps.iter()
                                .filter(|(_, o)| *o == crater_ir::verbs::Outcome::Warn)
                                .count(),
                            ..Default::default()
                        },
                    ));
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
                            // 期望态指纹:OutOfDate 检测的地基 —— 当前文件 hash ≠
                            // 记录 hash 时,UI 能判"期望态已改、尚未收敛"。
                            rec.blueprint_sha256 = file_sha(path);
                            rec.inventory_sha256 =
                                target.inventory.as_deref().and_then(file_sha);
                            if let Some(prev) = &previous {
                                rec.applied_at = prev.applied_at; // 首次部署时间不该被刷新
                            }
                            if let Err(e) = store.save(&rec) {
                                oops!("(记账失败,不影响本次部署:{e})");
                            }
                            // 自定义类型此刻本就还没弥合(舞在逐台循环之后才跳),
                            // 不该当成"收敛失败"吓人;其余项才是真的没达成。
                            let stuck = after
                                .changing()
                                .filter(|i| bp.custom_type(&i.ty).is_none())
                                .count();
                            if stuck > 0 {
                                say!("注意:收敛后仍有 {stuck} 项未达期望态 —— 见上方 plan");
                            }
                        }
                        Err(e) => oops!("(收敛后复观察失败:{e})"),
                    }
                }
                Err(e) => {
                    failures += 1;
                    for item in &plan.items {
                        note(&mut mx_rows, &mut mx_label, &mut mx, &host.name,
                             &item.id, &item.label(), '✗');
                    }
                    recap.push((
                        host.name.clone(),
                        Tally { failed: 1, ..Default::default() },
                    ));
                    oops!("执行失败 —— {e}");
                    crate::events::emit(serde_json::json!({
                        "e": "host_done", "host": host.name, "result": "failed",
                        "detail": format!("{e:#}"),
                    }));
                    continue;
                }
            }
            crate::events::emit(serde_json::json!({
                "e": "host_done", "host": host.name, "result": "ok",
            }));
        } else {
            for item in &plan.items {
                note(&mut mx_rows, &mut mx_label, &mut mx, &host.name,
                     &item.id, &item.label(), item.change.sigil());
            }
            // plan 不执行,但"有几项已符合"是它算出来的结论,该报。
            recap.push((
                host.name.clone(),
                Tally {
                    ok: plan.items.iter()
                        .filter(|i| matches!(i.change, crater_ir::verbs::Change::Ok))
                        .count(),
                    changed: plan.items.iter()
                        .filter(|i| !matches!(i.change, crater_ir::verbs::Change::Ok))
                        .count(),
                    ..Default::default()
                },
            ));
            crate::events::emit(serde_json::json!({
                "e": "host_done", "host": host.name, "result": "planned",
            }));
        }
    }
    // 这一批出事就停 —— 滚动的意义正在于此:出事时还剩大半个机群是好的。
    // 继续推下去,等于把一个已知会失败的变更铺满全场。
    if failures > batch_fail_before && bi + 1 < n_batches {
        crate::out::leave();
        oops!(
            "批次 {}/{} 有 {} 处失败 —— 停止滚动,剩余 {} 批未执行",
            bi + 1,
            n_batches,
            failures - batch_fail_before,
            n_batches - bi - 1
        );
        break 'outer;
    }
    }
    // 机群汇总。逐台的 `执行:...` 散在几百行中间,五台跑完拼不出全貌 ——
    // 这一段就是 ansible 的 PLAY RECAP 干的事。
    crate::out::leave();

    // ── 矩阵 ──
    let host_names: Vec<String> = hosts.iter().map(|h| h.name.clone()).collect();
    if host_names.len() > 1 && !mx_rows.is_empty() {
        print_matrix(&host_names, &mx_rows, &mx_label, &mx);
    }

    if recap.len() > 1 {
        say!("── 汇总 ──");
        let w = recap.iter().map(|(n, _)| n.chars().count()).max().unwrap_or(0).max(8);
        for (name, t) in &recap {
            // 固定列、恒定出现(哪怕是 0)—— 只在非零时才印的计数器,
            // 会让"这次没有失败"和"这个字段忘了统计"看起来一模一样。
            say!(
                "  {:<w$} : ok={:<4} changed={:<4} warn={:<4} failed={:<4}{}",
                name,
                t.ok,
                t.changed,
                t.warn,
                t.failed,
                if mode == Mode::Verify { format!(" drifted={}", t.drifted) } else { String::new() },
                w = w
            );
        }
        say!();
    }
    // 逐台收敛之后再跳机群级的舞(顺序不能反:舞往往依赖资源已就位)。
    if converge && !dances.is_empty() && failures == 0 {
        let targets = connect_fleet(&bp, &hosts, &fleet, &overrides, &base_dir(path), target.parallel, &blobs).await?;
        for name in &dances {
            say!("── procedure {name} ──");
            crate::events::emit(serde_json::json!({ "e": "proc_start", "name": name }));
            match procedure::run(&bp, name, &targets, &BTreeMap::new()) {
                Ok(r) => {
                    print_proc_report(&r);
                    crate::events::emit(serde_json::json!({
                        "e": "proc_done", "name": name, "result": "ok",
                    }));
                }
                Err(e) => {
                    failures += 1;
                    oops!("procedure {name} 失败 —— {e}\n");
                    crate::events::emit(serde_json::json!({
                        "e": "proc_done", "name": name, "result": "failed", "detail": format!("{e:#}"),
                    }));
                }
            }
        }
    }

    crate::events::emit(serde_json::json!({
        "e": "done", "failures": failures, "hosts": hosts.len(),
    }));
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
/// 只读探针用的连接。与部署走同一条建连路径 —— 免得"facts 能连、apply 连不上"
/// 这种最误导人的差异。
pub(crate) async fn probe_ctx(host: &Host) -> Result<Box<dyn Ctx>> {
    build_transport(host).await
}

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
    // 一行一物料,**带来源**。
    //
    // 曾经压成"闭包 1 项:yq-bin"只报名字 —— 但清单的价值恰恰在来源:
    // `${substrate.arch}` 选出了哪个变体、摘要有没有,全靠这一列看出来。
    // 名字谁都能从蓝图里读到,来源才是运行期才定的东西。
    for l in lines {
        say!("闭包{l}");
    }
    if broken > 0 {
        say!("闭包 {broken}/{} 项无法解析", items.len());
    }
}

/// task-major 执行:**一个资源在全机群跑完,再下一个**(ansible 的 linear)。
///
/// 与默认的逐台执行是同一套语义、不同的顺序 —— 逐项收敛的判断
/// (Unknown 要重新观察、上游动过要复查预测)只有 `converge_item` 一份实现,
/// 两条路径共用它,否则最微妙的地方会分家。
///
/// 为什么值得单独一条路径:滚动升级问的是"这一步在所有机器上都成了吗"。
/// 逐台执行要等最后一台跑完,才会发现第三步早在第二台上就炸了 —— 而那时
/// 第一台已经被改完了。
async fn run_linear(
    bp: &Blueprint,
    hosts: &[Host],
    fleet: &Fleet,
    overrides: &[(String, Yaml)],
    path: &Path,
    target: &TargetOpts,
    blobs: &BlobMap,
    store: &FileStore,
) -> Result<()> {
    use crater_ir::verbs::Outcome as O;

    crate::out::fleet(&hosts.iter().map(|h| h.name.clone()).collect::<Vec<_>>());
    let batches = crate::target::batches(hosts, target.serial.as_deref())?;
    say!(
        "blueprint {} —— 逐资源执行(linear),{} 台目标{}",
        bp.name,
        hosts.len(),
        if batches.len() > 1 { format!(",分 {} 批", batches.len()) } else { String::new() }
    );

    let n_batches = batches.len();
    let mut all_order: Vec<String> = Vec::new();
    let mut all_labels: BTreeMap<String, String> = BTreeMap::new();
    let mut all_cells: BTreeMap<String, BTreeMap<String, char>> = BTreeMap::new();
    let mut failures = 0usize;
    let mut dances: BTreeSet<String> = Default::default();

    'batches: for (bi, hosts) in batches.iter().enumerate() {
    if n_batches > 1 {
        crate::out::leave();
        say!(
            "── 批次 {}/{}({})──",
            bi + 1,
            n_batches,
            hosts.iter().map(|h| h.name.as_str()).collect::<Vec<_>>().join(", ")
        );
    }
    let batch_fail_before = failures;
    // 只连**这一批** —— 滚动的前提是没轮到的机器一根手指都不碰。
    let targets =
        connect_fleet(bp, hosts, fleet, overrides, &base_dir(path), target.parallel, blobs).await?;

    // 每台各求各的计划:资源集合可以不同(选择器 / when / each)。
    let mut plans: BTreeMap<String, plan::Plan> = BTreeMap::new();
    for host in hosts {
        let name = fleet_name(host);
        let sc = targets.scope(&name)?;
        let p = plan::plan(bp, &sc, targets.ctx(&name)?)
            .with_context(|| format!("{}:对 {} 求计划", host_label(host), path.display()))?;
        plans.insert(name, p);
    }

    // 资源顺序取**各台计划的并集**,按首现次序 —— 蓝图里的书写顺序即依赖顺序,
    // 而某台可能没有其中几项(选择器没选中),不能拿任意一台的计划当全集。
    let mut order: Vec<String> = Vec::new();
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    for host in hosts {
        for item in &plans[&fleet_name(host)].items {
            if !labels.contains_key(&item.id) {
                order.push(item.id.clone());
                labels.insert(item.id.clone(), item.label());
            }
        }
    }

    let mut upstream: BTreeMap<String, bool> = hosts.iter().map(|h| (fleet_name(h), false)).collect();
    // 某台失败后就把它摘出去,后续资源不再对它下手 —— 与 ansible 一致:
    // 在一台半坏的机器上继续往下做,只会把故障现场搅得更难查。
    let mut down: BTreeSet<String> = BTreeSet::new();
    let mut outcomes: BTreeMap<String, BTreeMap<String, char>> = BTreeMap::new();

    for id in &order {
        let label = &labels[id];
        let mut row: BTreeMap<String, char> = BTreeMap::new();
        let mut n_changed = 0usize;
        let mut n_ok = 0usize;
        let mut n_fail = 0usize;
        for host in hosts {
            let name = fleet_name(host);
            if down.contains(&name) {
                row.insert(name, '✗');
                continue;
            }
            let Some(item) = plans[&name].items.iter().find(|i| &i.id == id) else {
                // 这台没有这一项 —— 选择器没选中它。等价于 ansible 的 skipping。
                row.insert(name, '·');
                continue;
            };
            crate::out::enter(&name);
            if let Some(def) = bp.custom_type(&item.ty) {
                if !matches!(item.change, crater_ir::verbs::Change::Ok) {
                    dances.insert(def.apply.clone());
                }
                say!("  warn    {label}(自定义类型,交给机群级 procedure)");
                row.insert(name, '!');
                continue;
            }
            match plan::converge_item(bp, targets.ctx(&name)?, item, upstream[&name]) {
                Ok((oc, changed)) => {
                    if changed {
                        upstream.insert(name.clone(), true);
                    }
                    let (tag, c) = match oc {
                        O::Ok => ("ok     ", '✓'),
                        O::Changed => ("changed", '~'),
                        O::Warn => ("warn   ", '!'),
                    };
                    match oc {
                        O::Changed => n_changed += 1,
                        O::Ok => n_ok += 1,
                        O::Warn => {}
                    }
                    say!("  {tag} {label}");
                    row.insert(name, c);
                }
                Err(e) => {
                    n_fail += 1;
                    failures += 1;
                    oops!("failed  {label} —— {e}");
                    row.insert(name.clone(), '✗');
                    down.insert(name);
                }
            }
        }
        crate::out::leave();
        // **每个资源做完立刻给全机群的结论** —— 这正是逐台执行给不了的那句话。
        say!(
            "→ {label}:changed={n_changed} ok={n_ok} failed={n_fail}{}",
            if down.is_empty() { String::new() } else { format!(",已摘除 {} 台", down.len()) }
        );
        outcomes.insert(id.clone(), row);
        if !down.is_empty() && down.len() == hosts.len() {
            oops!("全部目标已失败,中止");
            break;
        }
    }
    say!();

    // 记账:收敛后**重新观察**,记的是现实不是意图(与逐台路径同一条纪律)。
    for host in hosts {
        let name = fleet_name(host);
        if down.contains(&name) {
            continue;
        }
        let sc = targets.scope(&name)?;
        if let Ok(after) = plan::plan(bp, &sc, targets.ctx(&name)?) {
            let mut rec = DeploymentRecord::from_plan(&bp.name, bp.version.as_deref(), &name, &after);
            rec.blueprint_sha256 = file_sha(path);
            rec.inventory_sha256 = target.inventory.as_deref().and_then(file_sha);
            if let Ok(Some(prev)) = store.load(&rec.id) {
                rec.applied_at = prev.applied_at;
            }
            if let Err(e) = store.save(&rec) {
                oops!("(记账失败,不影响本次部署:{e})");
            }
        }
    }

    // 机群级的舞:**每批跑完就跳自己这一批的**(顺序不变:资源先就位再跳舞)。
    //
    // 分批时这一步只握着本批的连接 —— 如果某支舞要靠别批机器的 exports,
    // 它会在这里明确失败,而不是拿到半个机群的事实悄悄算错。
    if !dances.is_empty() && failures == batch_fail_before {
        for name in &dances {
            say!("── procedure {name} ──");
            match procedure::run(bp, name, &targets, &BTreeMap::new()) {
                Ok(r) => print_proc_report(&r),
                Err(e) => {
                    failures += 1;
                    oops!("procedure {name} 失败 —— {e}");
                }
            }
        }
        dances.clear();
    }

    // 本批结果并入总表(矩阵与退出码是全局的)。
    for id in &order {
        if !all_labels.contains_key(id) {
            all_order.push(id.clone());
            all_labels.insert(id.clone(), labels[id].clone());
        }
        all_cells.entry(id.clone()).or_default().extend(
            outcomes.get(id).cloned().unwrap_or_default(),
        );
    }

    if failures > batch_fail_before && bi + 1 < n_batches {
        crate::out::leave();
        oops!(
            "批次 {}/{} 有 {} 处失败 —— 停止滚动,剩余 {} 批未执行",
            bi + 1,
            n_batches,
            failures - batch_fail_before,
            n_batches - bi - 1
        );
        break 'batches;
    }
    }

    let names: Vec<String> = batches.iter().flatten().map(fleet_name).collect();
    print_matrix(&names, &all_order, &all_labels, &all_cells);
    if failures > 0 {
        bail!("{failures} 处执行失败");
    }
    Ok(())
}

/// 资源 × 主机矩阵:横着看一个资源在全机群的落点。
///
/// 逐行输出是 host-major(这台机器怎么了),矩阵补上 task-major 的那一半
/// (这个资源在哪几台上出了状况)—— 两个问题都常问,而 crater 的执行是
/// 逐台的,所以把后者做成跑完之后的汇总,不必为输出形状改执行模型。
fn print_matrix(
    hosts: &[String],
    rows: &[String],
    labels: &BTreeMap<String, String>,
    cells: &BTreeMap<String, BTreeMap<String, char>>,
) {
    const LABEL_MAX: usize = 38;
    // 列宽 = 主机名宽(至少 3,给记号留位置)。
    let colw = hosts.iter().map(|h| h.chars().count()).max().unwrap_or(3).max(3);
    let labw = rows
        .iter()
        .map(|id| labels.get(id).map(|l| l.chars().count()).unwrap_or(0))
        .max()
        .unwrap_or(0)
        .min(LABEL_MAX);
    // 太宽就没法读了 —— 与其吐一屏错位的字符,不如换一种说法。
    let too_wide = labw + 2 + hosts.len() * (colw + 1) > 160;

    say!("── 矩阵 ──");
    if too_wide {
        // 宽机群降级:只列**有状况**的资源,并把涉及的主机名直接写出来。
        // 一百台的网格没人看得懂,而"哪几台不一样"永远是那个真问题。
        let mut any = false;
        for id in rows {
            let row = &cells[id];
            let odd: Vec<&String> = hosts
                .iter()
                .filter(|h| !matches!(row.get(*h), Some('✓') | None))
                .collect();
            if odd.is_empty() {
                continue;
            }
            any = true;
            let names: Vec<String> = odd
                .iter()
                .map(|h| format!("{}{}", row.get(h.as_str()).copied().unwrap_or('·'), h))
                .collect();
            say!("  {:<labw$}  {}", elide_label(labels, id, LABEL_MAX), names.join(" "), labw = labw);
        }
        if !any {
            say!("  ({} 台全部符合期望态)", hosts.len());
        }
        say!("  ({} 台超出网格宽度,只列有状况的资源)", hosts.len());
        say!();
        return;
    }
    // 表头
    let head: String = hosts.iter().map(|h| format!("{h:>colw$} ")).collect();
    say!("  {:<labw$}  {}", "", head.trim_end(), labw = labw);
    for id in rows {
        let row = &cells[id];
        let line: String = hosts
            .iter()
            // `·` = 这台没有这一项(选择器/when 没选中)—— 与"跳过"是同一件事,
            // 而此前它在输出里完全不可见。
            .map(|h| format!("{:>colw$} ", row.get(h).copied().unwrap_or('·')))
            .collect();
        say!("  {:<labw$}  {}", elide_label(labels, id, LABEL_MAX), line.trim_end(), labw = labw);
    }
    say!();
}

/// 标签太长就从中间省 —— 掐头会丢类型名,掐尾会丢是哪个文件,两头都要留。
fn elide_label(labels: &BTreeMap<String, String>, id: &str, max: usize) -> String {
    let l = labels.get(id).cloned().unwrap_or_else(|| id.to_string());
    if l.chars().count() <= max {
        return l;
    }
    let keep = max.saturating_sub(1) / 2;
    let head: String = l.chars().take(keep).collect();
    let tail: String = l.chars().skip(l.chars().count() - keep).collect();
    format!("{head}…{tail}")
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
    fn note(&self, msg: &str) {
        // 立刻刷出去:舞的每一步都可能是几分钟,缓冲住就失去了意义。
        use std::io::Write;
        say!("{msg}");
        let _ = std::io::stdout().flush();
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
            // 舞里的 `ok` 意思是"check 已满足,这一步没跑"。措辞要说清这件事:
            // 一支升级的舞报出满屏 `ok`,操作者本该起疑而不是安心。
            Ok => "已满足 ",
            Changed => "changed",
            Warn => "warn   ",
        };
        say!("  {tag} {member:<10} {step}");
    }
    for note in &report.skipped {
        // 一支舞"什么都没做"最常见的原因就是组选错了 —— 必须看得见。
        say!("  skip    {note}");
    }
    if !report.facts.is_empty() {
        say!(
            "  跨主机 fact:{}",
            report.facts.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    say!("执行:{}", report.summary());

    // 把"被 check 跳过"单独点名。
    //
    // 这条提示是被一次真实事故换来的:升级的三步 check 写成了"新版二进制在不在",
    // 而前一步刚把二进制装上去 —— 三步全跳,报告一片 ok,集群一点没升。
    // `ok` 混在正常输出里没人会多看一眼;单拎出来问一句"这些真的本就该跳过吗",
    // 才有机会当场发现。
    let skipped: Vec<String> = {
        use crater_ir::verbs::Outcome::*;
        let mut names: Vec<String> = report
            .steps
            .iter()
            .filter(|(_, _, o)| matches!(o, Ok))
            .map(|(s, _, _)| s.clone())
            .collect();
        names.dedup();
        names
    };
    if !skipped.is_empty() {
        say!(
            "  其中 {} 步因 check 已满足而未执行:{} —— 若本次期望它们做事,\n\
             \x20 请检查 check 是否检验了**本步自己的效果**(而非前序步骤建立的前提)",
            skipped.len(),
            skipped.join(", ")
        );
    }
    say!();
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

    say!("procedure {proc_name} —— {} 台目标\n", targets.fleet.members.len());
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
            say!("未部署过 —— 没有记录可核对(先 `crater apply`)\n");
            false
        }
        DriftVerdict::InSync => {
            let when = previous
                .map(|p| format!(",上次部署于 {}", fmt_epoch(p.applied_at)))
                .unwrap_or_default();
            say!("✓ 现实符合期望{when}\n");
            false
        }
        DriftVerdict::Drifted(items) => {
            say!("✗ 检测到漂移({} 项):", items.len());
            for i in items {
                let tag = if i.known { "漂移" } else { "新声明" };
                say!("  {tag} {:<14} {}", i.id, i.detail);
            }
            say!();
            true
        }
        DriftVerdict::Indeterminate { drifted, unknown } => {
            if !drifted.is_empty() {
                say!("✗ 检测到漂移({} 项):", drifted.len());
                for i in drifted {
                    say!("  {:<14} {}", i.id, i.detail);
                }
            }
            // 有说不清的项就不能说"一切正常" —— 那是假的安心。
            say!("? {unknown} 项无法核对(模型化欠债)—— 不能断言一切正常\n");
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
    say!("离线闭包 {} —— {} 份物料已备好\n", path.display(), map.len());
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
                // address 默认取 inventory 的连接地址;host vars 里的 `ip`
                // 会覆盖它 —— 走跳板/隧道时,控制端连的地址对同伴毫无意义。
                Member::new(fleet_name(h), &roles)
                    .with_address(h.address.clone())
                    .with_vars(h.vars.clone())
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

pub(crate) fn load(path: &Path) -> Result<Blueprint> {
    // path-aware:根文件声明的 `parts:` 在这里被并回来,对所有命令一视同仁。
    let bp = parse::blueprint_from_path(path).map_err(|e| anyhow::anyhow!("{e}"))?;

    // plan 之前先 lint:静态就能发现的问题不该等到探测目标机才暴露。
    let diags = lint::lint(&bp);
    let errs = lint::errors(&diags);
    if !errs.is_empty() {
        for d in &errs {
            oops!("  {d}");
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
    let _ = (bp, will_execute); // 模式与蓝图名已在机群层报过一次
    for item in &p.items {
        let sigil = item.change.sigil();
        say!("  {sigil} {:<44} {}", item.label(), describe(&item.change));
        for f in item.change.fields() {
            say!("       {f}");
        }
    }
    say!("计划 {}", p.summary());
    if p.debt() > 0 {
        // "接住,不羞辱,但可见":说不清的项要显式计数,不能混在成功里。
        say!(
            "其中 {} 项 plan 说不清(模型化欠债)—— 这些项 apply 前后都无法预演",
            p.debt()
        );
    }
    if !p.has_changes() {
        say!("已是期望态,无需变更");
    }
}

fn print_report(r: &RunReport) {
    for (id, outcome) in &r.steps {
        use crater_ir::verbs::Outcome::*;
        let tag = match outcome {
            Ok => "ok     ",
            Changed => "changed",
            Warn => "warn   ",
        };
        say!("  {tag} {id}");
    }
    say!("执行 {}", r.summary());
}

/// 事件流用的动作词(与 `describe` 的人话对应,给机器的短形式)。
fn change_kind(c: &Change) -> &'static str {
    match c {
        Change::Ok => "ok",
        Change::Create(_) => "create",
        Change::Update(_) => "update",
        Change::Destroy => "destroy",
        Change::Unknown(_) => "unknown",
    }
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
        // 一次探针 + 写文件的两条(先清空、再写块)。
        assert_eq!(calls.len(), 3, "{calls:?}");
        assert!(calls[1].contains(": > '/etc/demo'"), "先截断:{}", calls[1]);
        assert!(calls[2].contains("base64 -d"), "再按块写:{}", calls[2]);
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
