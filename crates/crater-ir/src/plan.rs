//! plan / converge / destroy —— **不是三个功能,是五动词契约的三个推论**。
//!
//! - `plan`    = ∀resource: observe + diff,**一次写入都不发生**;
//! - `converge`= plan 之后对非 noop 项调 apply;
//! - `destroy` = 逆序调用每个资源的 destroy(所以 IR 里没有 `teardown:`)。
//!
//! 旧模型里这三件事各写一遍(actions / teardown / plan 探针分头维护,还会不同步);
//! 这里它们共用同一个 observe,不可能不一致。

use anyhow::Result;

pub use crate::eval::Scope;
use crate::eval::{ResolvedArgs, Yaml};
use crate::ir::{Blueprint, ResourceDecl};
use crate::verbs::{Change, Ctx, DiffInput, Observed, Outcome, ResourceType};

/// plan 里的一行。
#[derive(Debug, Clone)]
pub struct PlanItem {
    /// 资源 id;`each` 展开后带下标(`file[1]`)。
    pub id: String,
    pub ty: String,
    pub args: ResolvedArgs,
    pub observed: Observed,
    pub change: Change,
}

impl PlanItem {
    /// 人类一眼能认出的标识:类型 + 它作用的**那个东西**。
    ///
    /// 自动 id(`file` / `file1` / `file2`)对机器够用,对人没用 ——
    /// 一屏 `+ file1` 看不出改的是哪个文件。这里挑该类型的"主键"参数补上。
    pub fn label(&self) -> String {
        const KEYS: &[&str] = &["path", "dest", "name", "to", "url", "run", "cmd"];
        match KEYS
            .iter()
            .find_map(|k| self.args.get(*k).and_then(|v| v.as_str()))
        {
            Some(v) => format!("{} {}", self.ty, elide(v)),
            None => self.id.clone(),
        }
    }
}

/// 长值居中省略:保留头尾(路径的辨识度在两端)。
fn elide(s: &str) -> String {
    const MAX: usize = 46;
    let n = s.chars().count();
    if n <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX / 2 - 1).collect();
    let tail: String = s.chars().skip(n - MAX / 2).collect();
    format!("{head}…{tail}")
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub items: Vec<PlanItem>,
}

impl Plan {
    pub fn changing(&self) -> impl Iterator<Item = &PlanItem> {
        self.items.iter().filter(|i| !i.change.is_noop())
    }
    pub fn has_changes(&self) -> bool {
        self.changing().next().is_some()
    }
    /// (create, update, destroy, ok, unknown)
    pub fn tally(&self) -> (usize, usize, usize, usize, usize) {
        let mut t = (0, 0, 0, 0, 0);
        for i in &self.items {
            match i.change {
                Change::Create(_) => t.0 += 1,
                Change::Update(_) => t.1 += 1,
                Change::Destroy => t.2 += 1,
                Change::Ok => t.3 += 1,
                Change::Unknown(_) => t.4 += 1,
            }
        }
        t
    }
    /// "模型化欠债":plan 说不清的项数。可见,但不阻断(ir-draft §4-4)。
    pub fn debt(&self) -> usize {
        self.tally().4
    }
    pub fn summary(&self) -> String {
        let (c, u, d, ok, unk) = self.tally();
        let mut s = format!("+{c} ~{u} -{d} ✓{ok}");
        if unk > 0 {
            s.push_str(&format!(" ?{unk}"));
        }
        s
    }
}

/// 一次执行的结果。
#[derive(Debug, Clone, Default)]
pub struct RunReport {
    pub steps: Vec<(String, Outcome)>,
    /// 自定义类型(L2)要跳的舞。**收敛不了它们** —— procedure 是机群级的,
    /// 不能在"逐台"循环里跑 N 遍。调用方收齐去重后在机群层跑一次。
    pub procedures_needed: std::collections::BTreeSet<String>,
}

impl RunReport {
    pub fn changed(&self) -> usize {
        self.steps.iter().filter(|(_, o)| *o == Outcome::Changed).count()
    }
    pub fn ok(&self) -> usize {
        self.steps.iter().filter(|(_, o)| *o == Outcome::Ok).count()
    }
    pub fn summary(&self) -> String {
        format!("changed={} ok={}", self.changed(), self.ok())
    }
}

/// 展开后的一条待办(资源 × each 项),`when` 已判定为真。
struct Unit {
    id: String,
    ty: String,
    args: ResolvedArgs,
}

/// 展开 `on` / `when` / `each`,把声明变成**这台机器上**的一串具体待办。
///
/// 三者正交,顺序也不能换:`on:` 决定"这台要不要参与"(机群层),`when:` 决定
/// "参与了要不要做"(单机条件),`each:` 决定"做几遍"。
fn expand(bp: &Blueprint, scope: &Scope) -> Result<Vec<Unit>> {
    let fleet = scope.fleet.clone().unwrap_or_default();
    let host = scope.host.clone().unwrap_or_default();
    let mut out = Vec::new();
    for r in &bp.resources {
        // `on:` 先判:不选中这台就连 plan 都不该出现它。
        if !fleet
            .matches(&r.on, &host, scope)
            .map_err(|e| anyhow::anyhow!("{} 的 `on: {}`:{e}", r.id, r.on))?
        {
            continue;
        }
        if let Some(w) = &r.when {
            if !scope.eval_bool(w).map_err(|e| anyhow::anyhow!("{}:{e}", r.id))? {
                continue; // 条件不成立 —— 连 plan 都不该出现它
            }
        }
        match &r.each {
            None => out.push(unit(r, scope, &r.id)?),
            Some(each) => {
                let items = scope.expand_each(each).map_err(|e| anyhow::anyhow!("{}:{e}", r.id))?;
                for (i, item) in items.into_iter().enumerate() {
                    let s = scope.with_item(item);
                    out.push(unit(r, &s, &format!("{}[{i}]", r.id))?);
                }
            }
        }
    }
    Ok(out)
}

fn unit(r: &ResourceDecl, scope: &Scope, id: &str) -> Result<Unit> {
    Ok(Unit {
        id: id.to_string(),
        ty: r.ty.clone(),
        args: scope
            .resolve_args(&r.args)
            .map_err(|e| anyhow::anyhow!("{id}:{e}"))?,
    })
}

/// 求计划的**语境** —— 同一份 observe,两种读法。
///
/// 这个区分是真机跑出来的:一处 unit 被手改,verify 却把后面 6 个物料型资源
/// 全染成漂移,归因被淹没。原因是 `upstream_changed` 这条保守规则 ——
/// 它服务于"宁可多重启一次,不要漏重启",那是**收敛**的取舍;
/// 而审计要回答的是"**哪里**漂了",传播只会制造假阳性。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Intent {
    /// 为了收敛:上游变更向下传播,该重启的一定重启。
    #[default]
    Converge,
    /// 为了审计(verify / drift):只报**自己**确实不符的项,不做因果传播。
    Audit,
}

/// **零写入**地问出"会发生什么"(收敛语境)。
pub fn plan(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx) -> Result<Plan> {
    plan_with(bp, scope, ctx, Intent::Converge)
}

/// 同上,但可指定语境。
pub fn plan_with(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx, intent: Intent) -> Result<Plan> {
    let units = expand(bp, scope)?;
    let mut items = Vec::new();
    // 保守的上游传播规则(rustfs 裁定 B):**本轮任一在先的资源会变**,
    // 就把"上游已变"传给后面的资源。精确到字段的传播留到 P1;
    // 保守的方向是"宁可多重启一次",而不是漏重启。
    let mut upstream_changed = false;

    for u in units {
        // L2:blueprint 自定义类型 —— 现实靠作者的探针读,动作是一支舞。
        if let Some(def) = bp.custom_type(&u.ty) {
            let (observed, change) = plan_custom(def, ctx, scope, &u.args)?;
            if intent == Intent::Converge && !change.is_noop() {
                upstream_changed = true;
            }
            items.push(PlanItem { id: u.id, ty: u.ty, args: u.args, observed, change });
            continue;
        }
        let Some(rt) = resolve_type(bp, &u.ty) else {
            items.push(PlanItem {
                id: u.id,
                ty: u.ty.clone(),
                args: u.args,
                observed: Observed::default(),
                change: Change::Unknown(format!("类型 `{}` 尚无五动词实现", u.ty)),
            });
            continue;
        };
        let observed = rt.observe(ctx, &u.args)?;
        let change = rt.diff(&DiffInput {
            args: &u.args,
            observed: &observed,
            upstream_changed,
        });
        // 审计语境下不传播:一处漂移不该把后面所有资源染红。
        if intent == Intent::Converge && !change.is_noop() && !matches!(change, Change::Unknown(_)) {
            upstream_changed = true;
        }
        items.push(PlanItem { id: u.id, ty: u.ty, args: u.args, observed, change });
    }
    Ok(Plan { items })
}

/// plan → 对非 noop 项 apply。幂等契约保证:立即重跑得到全 `ok`。
fn push_step(r: &mut RunReport, obs: &dyn Fn(&str, Outcome), id: &str, oc: Outcome) {
    obs(id, oc);
    r.steps.push((id.to_string(), oc));
}

pub fn converge(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx) -> Result<RunReport> {
    converge_with(bp, scope, ctx, &|_, _| {})
}

/// 每定案一步就回调一次的 `converge` —— 执行呈现(UI 矩阵)的供血口。
/// 回调拿到的是**已执行**的结果,不是预测;顺序即执行顺序。
pub fn converge_with(
    bp: &Blueprint,
    scope: &Scope,
    ctx: &dyn Ctx,
    on_step: &dyn Fn(&str, Outcome),
) -> Result<RunReport> {
    let p = plan(bp, scope, ctx)?;
    let mut report = RunReport::default();

    // 上游是否**真的**动过 —— 按执行结果算,不是按计划猜。
    //
    // plan 期也算过一次传导,但它只能基于预测,而预测可能是 `Unknown`
    // (典型:配置文件由同一轮里更早的 package 装出来,预测时还不存在)。
    // Unknown 不参与 plan 期传导,于是"配置改了却没重启服务"这种最经典的
    // 故障会静默发生:redis.conf 已是 bind 0.0.0.0,而进程仍监听 127.0.0.1。
    //
    // 这里按**实际结果**补一次传导:上游真动过、而某项在计划里是 Ok 的,
    // 就重新观察一次再判 —— 现实可能已经不是计划时那个样子了。
    let mut upstream_changed = false;

    for item in &p.items {
        let (outcome, changed) = converge_item(bp, ctx, item, upstream_changed)?;
        if changed {
            upstream_changed = true;
        }
        if let Some(def) = bp.custom_type(&item.ty) {
            if !matches!(item.change, Change::Ok) {
                report.procedures_needed.insert(def.apply.clone());
            }
        }
        push_step(&mut report, on_step, &item.id, outcome);
    }
    Ok(report)
}

/// 收敛**单个**资源项。返回 (结果, 这一项是否真的动过)。
///
/// 从 `converge_with` 的循环体里抽出来,是为了让调用方能按**别的顺序**驱动
/// 收敛 —— 比如 task-major:一个资源在全机群跑完,再下一个。逐项的语义
/// (Unknown 要重新观察、上游动过要复查预测)必须只有一份实现,否则两种
/// 顺序会在最微妙的地方分家。
///
/// `upstream_changed`:同一台机器上、这一项之前有没有东西真的改过。
pub fn converge_item(
    bp: &Blueprint,
    ctx: &dyn Ctx,
    item: &PlanItem,
    upstream_changed: bool,
) -> Result<(Outcome, bool)> {
    // 上游真动过之后,**这一项的预测就可能过时** —— 不限于"计划说 Ok"。
    //
    // 反例正是 redis:plan 时包还没装,service 因此被判为 Create;
    // 而包在安装时自己就把服务起起来了,`systemctl start` 对已在跑的服务
    // 是空操作 —— 于是刚被 lineinfile 改过的 bind 配置永远不生效,
    // 进程一直监听 127.0.0.1,而 crater 报的是成功。
    //
    // 代价是首次变更之后的每一项多一次探针。这个代价换的是"配置改了服务
    // 一定重启" —— 那是本工具最基本的承诺之一。
    let planned;
    let change = if upstream_changed && bp.custom_type(&item.ty).is_none() {
        let rt = resolve_type(bp, &item.ty)
            .ok_or_else(|| anyhow::anyhow!("类型 `{}` 无实现", item.ty))?;
        let fresh = rt.observe(ctx, &item.args)?;
        planned = rt.diff(&DiffInput {
            args: &item.args,
            observed: &fresh,
            upstream_changed: true,
        });
        &planned
    } else {
        &item.change
    };
    match change {
        Change::Ok => Ok((Outcome::Ok, false)),

        // 计划期说不清的项:**执行前重新观察一次**,再决定做不做。
        //
        // plan 是对"当下现实"的一次性预测,而 converge 走到这一项时现实
        // 往往已经变了 —— `lineinfile` 要改的 postgresql.conf,是同一轮里
        // 更早那条 `package` 装出来的。用陈旧的判断跳过它,等于让"同一次
        // apply 内部的先后依赖"永远不成立。
        //
        // 重新观察后仍说不清(裸 shell 这类),就**照跑**并记 warn ——
        // "接住,不羞辱,但可见"。此前是直接跳过,于是逃生舱形同虚设:
        // 没写 check 的 shell 在 apply 时一次都不会执行。
        Change::Unknown(_) if bp.custom_type(&item.ty).is_none() => {
            let rt = resolve_type(bp, &item.ty)
                .ok_or_else(|| anyhow::anyhow!("类型 `{}` 无实现", item.ty))?;
            let fresh = rt.observe(ctx, &item.args)?;
            let again = rt.diff(&DiffInput {
                args: &item.args,
                observed: &fresh,
                upstream_changed: false,
            });
            match again {
                Change::Ok => Ok((Outcome::Ok, true)),
                Change::Unknown(_) => {
                    // 「说不清」有两种,处置**相反**(D-135):
                    //
                    // - **什么都观察不到**(裸 shell 没写 `check:`)→ 照跑。
                    //   跑它就是它的全部语义,不跑等于这一步永远不执行。
                    // - **观察得到、只是比不了**(物料没声明 sha256,判不出
                    //   目标上那份是不是它)→ **不动**。动了就是猜,而这里
                    //   猜的代价是每次 apply 都重推一遍几百 MB 的物料。
                    //
                    // 判据用"这次观察到了什么"而不是类型名:前者是事实,
                    // 后者要维护一张会漏的名单。
                    let blind = !fresh.present && fresh.fields.is_empty();
                    if !blind {
                        return Ok((Outcome::Warn, false));
                    }
                    rt.apply(ctx, &item.args, &again)
                        .map_err(|e| anyhow::anyhow!("{}: {e}", item.id))?;
                    Ok((Outcome::Warn, true))
                }
                determinate => {
                    let outcome = rt
                        .apply(ctx, &item.args, &determinate)
                        .map_err(|e| anyhow::anyhow!("{}: {e}", item.id))?;
                    Ok((outcome, true))
                }
            }
        }
        Change::Unknown(_) => Ok((Outcome::Warn, false)),
        // 自定义类型的弥合是机群级的舞 —— 记下来交给调用方,不在这里跑。
        _ if bp.custom_type(&item.ty).is_some() => Ok((Outcome::Warn, false)),
        change => {
            let rt = resolve_type(bp, &item.ty)
                .ok_or_else(|| anyhow::anyhow!("类型 `{}` 无实现", item.ty))?;
            let outcome = rt
                .apply(ctx, &item.args, change)
                .map_err(|e| anyhow::anyhow!("{}: {e}", item.id))?;
            Ok((outcome, outcome == Outcome::Changed))
        }
    }
}

/// 退役:**逆序** destroy。没有 `teardown:` 段可写错、可与安装步骤不同步。
/// **退役计划** —— 零写入地回答"会拆掉什么"。
///
/// 与 `plan` 同源(同一个 observe),但语义反过来:存在的东西将被删除。
/// 破坏性动作必须先能被"看"—— 一个不能预演的 destroy,人只能靠勇气去按。
///
/// 顺序即声明序的**逆序**:后建的先拆(服务先停,配置后删,目录最后)。
pub fn plan_destroy(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx) -> Result<Plan> {
    plan_destroy_inner(bp, scope, ctx)
}

/// 本次退役需要跳的舞(按声明序去重)。
///
/// **顺序与 apply 相反,这不是对称的美学**:apply 是"资源就位 → 跳舞"
/// (kubeadm 得先有 containerd);destroy 必须"先跳舞退出集群 → 再拆资源",
/// 否则 containerd/kubelet 先被卸掉,etcd 里那个成员就成了永远清不掉的孤儿。
pub fn destroy_dances(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx) -> Result<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for u in expand(bp, scope)? {
        let Some(def) = bp.custom_type(&u.ty) else { continue };
        let Some(dance) = &def.destroy else { continue };
        if crate::procedure::observe_custom(def, ctx, scope)?.present && !out.contains(dance) {
            out.push(dance.clone());
        }
    }
    Ok(out)
}

fn plan_destroy_inner(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx) -> Result<Plan> {
    let mut units = expand(bp, scope)?;
    units.reverse();
    let mut items = Vec::new();
    for u in units {
        // L2 自定义类型:现实照样用作者的探针读,但**拆**要靠作者声明的舞。
        // 声明了 `destroy:` 就说得出"靠哪支舞退役";没声明才是真的说不清。
        if let Some(def) = bp.custom_type(&u.ty) {
            let observed = crate::procedure::observe_custom(def, ctx, scope)?;
            let change = match (&observed.present, &def.destroy) {
                (false, _) => Change::Ok,
                (true, Some(_)) => Change::Destroy,
                (true, None) => Change::Unknown(format!(
                    "自定义类型 `{}` 没声明 `destroy:`,引擎不知道怎么拆它",
                    u.ty
                )),
            };
            items.push(PlanItem { id: u.id, ty: u.ty, args: u.args, observed, change });
            continue;
        }
        let Some(rt) = resolve_type(bp, &u.ty) else {
            items.push(PlanItem {
                id: u.id,
                ty: u.ty.clone(),
                args: u.args,
                observed: Observed::default(),
                change: Change::Unknown(format!("类型 `{}` 尚无五动词实现", u.ty)),
            });
            continue;
        };
        let observed = rt.observe(ctx, &u.args)?;
        let change = match rt.retire_note() {
            // 刻意不退役的类型:**照实说保留**。计划承诺删除、apply 时悄悄跳过,
            // 比一开始就说清楚糟得多 —— 那会让人以为机器被清干净了。
            Some(why) if observed.present => Change::Unknown(format!("保留:{why}")),
            Some(_) => Change::Ok,
            // 其余交给类型自己判:`present` 服务的是收敛,退役要问的是
            // "还留着什么痕迹"。见 ResourceType::destroy_change。
            None => rt.destroy_change(&observed),
        };
        items.push(PlanItem { id: u.id, ty: u.ty, args: u.args, observed, change });
    }
    Ok(Plan { items })
}

pub fn destroy(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx) -> Result<RunReport> {
    let mut units = expand(bp, scope)?;
    units.reverse();
    let mut report = RunReport::default();
    for u in units {
        let Some(rt) = resolve_type(bp, &u.ty) else {
            report.steps.push((u.id, Outcome::Warn));
            continue;
        };
        let observed = rt.observe(ctx, &u.args)?;
        let outcome = rt
            .destroy(ctx, &u.args, &observed)
            .map_err(|e| anyhow::anyhow!("{}: {e}", u.id))?;
        report.steps.push((u.id, outcome));
    }
    Ok(report)
}

/// 类型解析:先内建(L1),再 blueprint 自定义(L2)。
/// L2 的五动词由 procedure 实现,执行器落在 P1 —— 现在诚实地返回 None,
/// plan 里显示 `?` 而不是假装成功。
fn resolve_type(bp: &Blueprint, name: &str) -> Option<&'static dyn ResourceType> {
    let _ = bp;
    crate::builtins::get(name)
}

/// 自定义类型(L2)的 observe + diff。
///
/// 引擎不懂 kubeadm(D-017):现实由作者声明的只读探针读出,
/// 弥合差异的**动作**则是一支舞 —— 由机群层执行,不在这里。
fn plan_custom(
    def: &crate::ir::TypeDef,
    ctx: &dyn Ctx,
    scope: &Scope,
    args: &ResolvedArgs,
) -> Result<(Observed, Change)> {
    let observed = crate::procedure::observe_custom(def, ctx, scope)?;
    let change = if observed.present {
        Change::Ok
    } else {
        Change::Create(vec![crate::verbs::FieldDiff::set(
            "via",
            format!("procedure {}", def.apply),
        )])
    };
    let _ = args;
    Ok((observed, change))
}

/// 便捷:把 params 的默认值装进 Scope(真实取值优先级见 ir-draft §4-1)。
pub fn scope_from_defaults(bp: &Blueprint) -> Scope {
    let params = bp
        .params
        .iter()
        .filter_map(|(k, p)| p.default.clone().map(|d| (k.clone(), d)))
        .collect();
    Scope { params, ..Default::default() }
}

/// 便捷:覆盖若干参数(`--set k=v`)。
pub fn with_overrides(mut scope: Scope, overrides: &[(String, Yaml)]) -> Scope {
    for (k, v) in overrides {
        scope.params.insert(k.clone(), v.clone());
    }
    scope
}
