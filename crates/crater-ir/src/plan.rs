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

/// **零写入**地问出"会发生什么"。
pub fn plan(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx) -> Result<Plan> {
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
            if !change.is_noop() {
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
        if !change.is_noop() && !matches!(change, Change::Unknown(_)) {
            upstream_changed = true;
        }
        items.push(PlanItem { id: u.id, ty: u.ty, args: u.args, observed, change });
    }
    Ok(Plan { items })
}

/// plan → 对非 noop 项 apply。幂等契约保证:立即重跑得到全 `ok`。
pub fn converge(bp: &Blueprint, scope: &Scope, ctx: &dyn Ctx) -> Result<RunReport> {
    let p = plan(bp, scope, ctx)?;
    let mut report = RunReport::default();
    for item in &p.items {
        match &item.change {
            Change::Ok => report.steps.push((item.id.clone(), Outcome::Ok)),
            Change::Unknown(_) => report.steps.push((item.id.clone(), Outcome::Warn)),
            // 自定义类型的弥合是机群级的舞 —— 记下来交给调用方,不在这里跑。
            _ if bp.custom_type(&item.ty).is_some() => {
                let def = bp.custom_type(&item.ty).expect("checked");
                report.procedures_needed.insert(def.apply.clone());
                report.steps.push((item.id.clone(), Outcome::Warn));
            }
            change => {
                let rt = resolve_type(bp, &item.ty)
                    .ok_or_else(|| anyhow::anyhow!("类型 `{}` 无实现", item.ty))?;
                let outcome = rt
                    .apply(ctx, &item.args, change)
                    .map_err(|e| anyhow::anyhow!("{}: {e}", item.id))?;
                report.steps.push((item.id.clone(), outcome));
            }
        }
    }
    Ok(report)
}

/// 退役:**逆序** destroy。没有 `teardown:` 段可写错、可与安装步骤不同步。
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
