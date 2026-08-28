//! Procedure 执行器 —— 让"舞"真的能跳。
//!
//! 资源声明的是**期望态**;procedure 声明的是**从 A 态到 B 态怎么安全地走**
//! (kubeadm init → 传 token → 其余逐台 join、drain → 换二进制 → uncordon)。
//! 这是 D-106「状态/过程分离」的执行侧兑现。
//!
//! 与 [`plan`](crate::plan) 最根本的区别:**procedure 是机群级的**。
//! 一支舞的步骤分布在不同主机上,还要把 fact 从一台传到另一台 —— 所以它不能
//! 塞进"逐台独立跑"的循环里,需要能拿到**任意成员**执行上下文的 [`Targets`]。

use std::collections::BTreeMap;

use anyhow::{bail, Result};

use crate::eval::{Scope, Yaml};
use crate::fleet::Fleet;
use crate::ir::{Blueprint, Procedure, Step};
use crate::verbs::{Change, Ctx, DiffInput, Observed, Outcome};

/// 执行一支舞所需的全部目标接入能力。
///
/// 之所以不是"一个 Ctx",是因为 procedure 会在**多台机器之间**走:
/// 首台 init、其余 join,还要把 token 从前者带给后者。
pub trait Targets {
    fn fleet(&self) -> &Fleet;
    /// 取某个成员的执行上下文。
    fn ctx(&self, member: &str) -> Result<&dyn Ctx>;
    /// 该成员的求值作用域(已含它自己的 `substrate.*` 事实)。
    fn scope(&self, member: &str) -> Result<Scope>;
}

/// 一支舞的执行记录。
#[derive(Debug, Clone, Default)]
pub struct ProcReport {
    /// (步骤 id, 成员, 结果)
    pub steps: Vec<(String, String, Outcome)>,
    /// 沿途导出的跨主机 fact。
    pub facts: BTreeMap<String, String>,
    /// 被 `on:` 排除、因而整步没跑的步骤(诊断用:一支舞"什么都没做"往往是选错了组)。
    pub skipped: Vec<String>,
}

impl ProcReport {
    pub fn changed(&self) -> usize {
        self.steps.iter().filter(|(_, _, o)| *o == Outcome::Changed).count()
    }
    pub fn ok(&self) -> usize {
        self.steps.iter().filter(|(_, _, o)| *o == Outcome::Ok).count()
    }
    pub fn summary(&self) -> String {
        let mut s = format!("changed={} ok={}", self.changed(), self.ok());
        if !self.skipped.is_empty() {
            s.push_str(&format!(" skipped={}", self.skipped.len()));
        }
        s
    }
}

/// 跑一支具名的舞。
///
/// `args` 是调用方传入的过程参数(如 `upgrade` 的 `to: 1.37.0`),
/// 它们以 `params.*` 的身份进入每一步的求值作用域。
pub fn run(
    bp: &Blueprint,
    name: &str,
    targets: &dyn Targets,
    args: &BTreeMap<String, Yaml>,
) -> Result<ProcReport> {
    let proc = bp
        .procedures
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("blueprint `{}` 没有名为 `{name}` 的 procedure", bp.name))?;
    check_params(proc, args)?;

    let mut report = ProcReport::default();
    // 步骤**严格按序**:舞的每一拍都依赖前一拍的结果,这里不做重排也不做并发。
    for step in &proc.steps {
        run_step(proc, step, targets, args, &mut report)?;
    }
    Ok(report)
}

fn check_params(proc: &Procedure, args: &BTreeMap<String, Yaml>) -> Result<()> {
    for (name, spec) in &proc.params {
        match args.get(name) {
            Some(v) => spec
                .ty
                .check(v)
                .map_err(|e| anyhow::anyhow!("procedure `{}` 参数 `{name}`:{e}", proc.name))?,
            None if spec.default.is_some() || !spec.required => {}
            None => bail!("procedure `{}` 缺少必填参数 `{name}`", proc.name),
        }
    }
    Ok(())
}

fn run_step(
    proc: &Procedure,
    step: &Step,
    targets: &dyn Targets,
    args: &BTreeMap<String, Yaml>,
    report: &mut ProcReport,
) -> Result<()> {
    let fleet = targets.fleet();
    let where_ = |e: &str| format!("procedure `{}` 步骤 `{}`:{e}", proc.name, step.id);

    // 谁参与这一步。`on:` 是机群层判定,`where` 子句要用各自的单机事实。
    //
    // 这里与**资源**的处理刻意不同:资源引用了不存在的组是错误(某台机器会因此
    // 缺状态,属于静默少装);而一支舞覆盖多种拓扑是常态 —— 单 master 时
    // `rest(controlplane)` 与 `role.worker` 本就该为空,不该让整支舞失败。
    // 代价是拼错的组名不再致命,所以它必须**响亮地留痕**(见 skipped)。
    let mut members = Vec::new();
    let mut unknown_role: Option<String> = None;
    for m in &fleet.members {
        let scope = step_scope(targets, &m.name, proc, args, report)?;
        match fleet.matches(&step.on, &m.name, &scope) {
            Ok(true) => members.push(m.name.clone()),
            Ok(false) => {}
            Err(crate::fleet::SelectError::UnknownRole { .. }) if unknown_role.is_none() => {
                unknown_role = Some(
                    fleet
                        .matches(&step.on, &m.name, &scope)
                        .unwrap_err()
                        .to_string(),
                );
            }
            Err(crate::fleet::SelectError::UnknownRole { .. }) => {}
            Err(e) => bail!(where_(&format!("`on: {}` {e}", step.on))),
        }
    }
    if members.is_empty() {
        // 一支舞"什么都没做"最常见的两个原因:拓扑本就没这一层,或者组名拼错了。
        // 两者在引擎眼里一样,所以把线索一并写进留痕。
        let mut note = format!("{} (on: {})", step.id, step.on);
        if let Some(why) = unknown_role {
            note.push_str(&format!(" —— {why}"));
        }
        report.skipped.push(note);
        return Ok(());
    }

    // 导出 fact 的步骤必须只选中一台 —— 否则同名 fact 会被多台各写一份,
    // 消费方拿到哪一份将取决于执行顺序。宁可拒绝,不做"最后一个赢"。
    if !step.exports.is_empty() && members.len() > 1 {
        bail!(where_(&format!(
            "`exports:` 选中了 {} 台({}),但一个 fact 只能有一个来源 —— \
             用 `first(...)` 把它收敛到一台",
            members.len(),
            members.join(", ")
        )));
    }

    let rt = crate::builtins::get(&step.ty)
        .ok_or_else(|| anyhow::anyhow!(where_(&format!("类型 `{}` 没有实现", step.ty))))?;

    // `throttle` 的语义是"同时最多几台"。当前执行是**串行**的,所以任何 throttle
    // 都天然被满足(护 etcd 那种"必须逐台"的约束不会被违反);它真正开始起作用
    // 要等并发调度落地(P1)。
    for member in &members {
        let base = step_scope(targets, member, proc, args, report)?;

        if let Some(w) = &step.when {
            if !base.eval_bool(w).map_err(|e| anyhow::anyhow!(where_(&e)))? {
                continue;
            }
        }

        let items: Vec<Option<Yaml>> = match &step.each {
            None => vec![None],
            Some(each) => base
                .expand_each(each)
                .map_err(|e| anyhow::anyhow!(where_(&e)))?
                .into_iter()
                .map(Some)
                .collect(),
        };

        for item in items {
            let scope = match item {
                Some(v) => base.with_item(v),
                None => base.clone(),
            };
            let resolved = scope
                .resolve_args(&step.args)
                .map_err(|e| anyhow::anyhow!(where_(&e)))?;
            let ctx = targets.ctx(member)?;

            // 与资源同一套五动词:先 observe 再 diff —— 有 `check:` 的步骤因此天然幂等,
            // 重跑一支舞不会把已经做过的事再做一遍。
            let observed = rt
                .observe(ctx, &resolved)
                .map_err(|e| anyhow::anyhow!(where_(&e.to_string())))?;
            let change = rt.diff(&DiffInput {
                args: &resolved,
                observed: &observed,
                upstream_changed: false,
            });

            let outcome = match &change {
                Change::Ok => Outcome::Ok,
                _ => attempt(rt, ctx, &resolved, &change, step)
                    .map_err(|e| anyhow::anyhow!(where_(&format!("在 {member} 上:{e}"))))?,
            };
            report.steps.push((step.id.clone(), member.clone(), outcome));
        }

        // 导出 fact:在**产出它的那一步之后、那台机器上**取值。
        for (fact, cmd) in &step.exports {
            let ctx = targets.ctx(member)?;
            let (code, out) = ctx.probe(cmd)?;
            if code != 0 {
                bail!(where_(&format!(
                    "导出 fact `{fact}` 失败(exit {code}):{cmd}\n{}",
                    out.trim()
                )));
            }
            report.facts.insert(fact.clone(), out.trim().to_string());
        }
    }
    Ok(())
}

/// 一步在某台机器上的求值作用域:该台事实 ⊕ 过程参数 ⊕ 已导出的 fact。
fn step_scope(
    targets: &dyn Targets,
    member: &str,
    proc: &Procedure,
    args: &BTreeMap<String, Yaml>,
    report: &ProcReport,
) -> Result<Scope> {
    let mut scope = targets.scope(member)?;
    // 过程参数以 params.* 出现,默认值兜底。
    for (name, spec) in &proc.params {
        if let Some(v) = args.get(name).cloned().or_else(|| spec.default.clone()) {
            scope.params.insert(name.clone(), v);
        }
    }
    for (k, v) in args {
        scope.params.insert(k.clone(), v.clone());
    }
    scope.facts = report
        .facts
        .iter()
        .map(|(k, v)| (k.clone(), Yaml::String(v.clone())))
        .collect();
    Ok(scope)
}

/// 执行一步,处理 `retries` 与 `ignore_errors`。
fn attempt(
    rt: &dyn crate::verbs::ResourceType,
    ctx: &dyn Ctx,
    args: &crate::eval::ResolvedArgs,
    change: &Change,
    step: &Step,
) -> Result<Outcome> {
    let mut last: Option<anyhow::Error> = None;
    for _ in 0..=step.strategy.retries {
        match rt.apply(ctx, args, change) {
            Ok(o) => return Ok(o),
            Err(e) => last = Some(e),
        }
    }
    let err = last.unwrap_or_else(|| anyhow::anyhow!("未知失败"));
    if step.strategy.ignore_errors {
        // 声明了容错就继续,但**必须留痕** —— 静默吞掉失败是运维工具的大忌。
        eprintln!("  warn {}: {err}(ignore_errors)", step.id);
        return Ok(Outcome::Warn);
    }
    Err(err)
}

// ---------------------------------------------------------------- 自定义类型(L2)

/// 按 blueprint 自定义类型的 `observe:` 探针读现实。
///
/// 引擎不必懂 kubeadm(D-017):作者用一条只读命令 + 输出映射补齐 observe,
/// 五动词的其余四个由 procedure 实现。
pub fn observe_custom(def: &crate::ir::TypeDef, ctx: &dyn Ctx, scope: &Scope) -> Result<Observed> {
    let cmd = crate::expr::Template::parse(&def.observe.cmd)
        .map_err(|e| anyhow::anyhow!("type `{}` observe.cmd:{e}", def.name))?;
    let rendered = scope
        .resolve(&crate::ir::Value::Tmpl(cmd))
        .map(|v| crate::eval::scalar_to_string(&v))
        .map_err(|e| anyhow::anyhow!("type `{}` observe.cmd:{e}", def.name))?;

    let (code, out) = ctx.probe(&rendered)?;
    if def.observe.parse.is_empty() {
        // 没给映射就按退出码判定存在与否 —— 最常见的 `test -f …` 形态。
        return Ok(if code == 0 { Observed::present([]) } else { Observed::absent() });
    }
    // 有映射:输出里出现哪个标记,就置哪个字段。全都没出现 ⇒ 不存在。
    let mut fields = BTreeMap::new();
    for (field, marker) in &def.observe.parse {
        if out.contains(marker.as_str()) {
            fields.insert(field.clone(), marker.clone());
        }
    }
    Ok(if fields.is_empty() {
        Observed::absent()
    } else {
        Observed { present: true, fields }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::fleet::Member;
    use crate::parse::blueprint_from_str;

    /// 一个机群的假目标:每台一份 FakeCtx,可分别设定应答。
    struct FakeTargets {
        fleet: Fleet,
        ctxs: BTreeMap<String, FakeCtx>,
    }

    impl FakeTargets {
        fn new(members: Vec<(&str, Vec<&str>)>) -> Self {
            let fleet = Fleet::new(
                members.iter().map(|(n, roles)| Member::new(*n, roles)).collect(),
            );
            let ctxs = members
                .iter()
                .map(|(n, _)| (n.to_string(), FakeCtx::new().on("", 0, "")))
                .collect();
            FakeTargets { fleet, ctxs }
        }
        fn with_ctx(mut self, member: &str, ctx: FakeCtx) -> Self {
            self.ctxs.insert(member.to_string(), ctx);
            self
        }
        fn calls_on(&self, member: &str) -> Vec<String> {
            self.ctxs[member].calls().iter().map(|c| c.text().to_string()).collect()
        }
    }

    impl Targets for FakeTargets {
        fn fleet(&self) -> &Fleet {
            &self.fleet
        }
        fn ctx(&self, member: &str) -> Result<&dyn Ctx> {
            self.ctxs
                .get(member)
                .map(|c| c as &dyn Ctx)
                .ok_or_else(|| anyhow::anyhow!("no ctx for {member}"))
        }
        fn scope(&self, member: &str) -> Result<Scope> {
            Ok(Scope {
                fleet: Some(self.fleet.clone()),
                host: Some(member.to_string()),
                ..Default::default()
            })
        }
    }

    /// k8s-ha 试金石的骨架:首台 init 并导出 token,其余逐台 join。
    const K8S: &str = r#"
name: k8s
types:
  - name: cluster_member
    observe: { cmd: "test -f /etc/kubernetes/kubelet.conf && echo joined || echo absent",
               parse: { joined: joined } }
    apply: bootstrap
procedures:
  bootstrap:
    steps:
      - shell:
          cmd: "kubeadm init"
          check: "test -f /etc/kubernetes/admin.conf"
        on: first(role.controlplane)
        exports:
          join: "kubeadm token create --print-join-command"
      - shell:
          cmd: "${facts.join} --control-plane"
          check: "test -f /etc/kubernetes/kubelet.conf"
        on: rest(role.controlplane)
        strategy: { throttle: 1 }
      - shell:
          cmd: "${facts.join}"
          check: "test -f /etc/kubernetes/kubelet.conf"
        on: role.worker
resources:
  - cluster_member: { role: control-plane }
"#;

    fn ha_targets() -> FakeTargets {
        // 三 master + 一 worker;所有 check 都失败(=还没装),命令都成功。
        let blank = || {
            FakeCtx::new()
                .on("test -f", 1, "")
                .on("kubeadm token create", 0, "kubeadm join 10.0.0.1:6443 --token abc\n")
                .on("", 0, "")
        };
        FakeTargets::new(vec![
            ("n11", vec!["controlplane"]),
            ("n12", vec!["controlplane"]),
            ("n13", vec!["controlplane"]),
            ("w01", vec!["worker"]),
        ])
        .with_ctx("n11", blank())
        .with_ctx("n12", blank())
        .with_ctx("n13", blank())
        .with_ctx("w01", blank())
    }

    #[test]
    fn the_dance_runs_init_once_then_joins_the_rest() {
        let bp = blueprint_from_str(K8S).unwrap();
        let t = ha_targets();
        let r = run(&bp, "bootstrap", &t, &BTreeMap::new()).unwrap();

        // init 只在首台
        assert!(t.calls_on("n11").iter().any(|c| c == "kubeadm init"));
        for m in ["n12", "n13", "w01"] {
            assert!(!t.calls_on(m).iter().any(|c| c == "kubeadm init"), "{m} 不该 init");
        }
        // 其余 master 走 control-plane join,worker 走普通 join
        assert!(t.calls_on("n12").iter().any(|c| c.contains("--control-plane")));
        assert!(t.calls_on("w01").iter().any(|c| c.starts_with("kubeadm join")));
        assert!(!t.calls_on("w01").iter().any(|c| c.contains("--control-plane")));
        assert_eq!(r.changed(), 4, "四台各动一次:{:?}", r.steps);
    }

    #[test]
    fn a_fact_exported_on_one_host_reaches_the_others() {
        // 这是"状态/过程分离"能成立的关键:token 从首台流到其余台。
        let bp = blueprint_from_str(K8S).unwrap();
        let t = ha_targets();
        let r = run(&bp, "bootstrap", &t, &BTreeMap::new()).unwrap();
        assert_eq!(
            r.facts.get("join").map(String::as_str),
            Some("kubeadm join 10.0.0.1:6443 --token abc")
        );
        // 消费方拿到的是**渲染后**的真实命令,不是字面量
        assert!(
            t.calls_on("n12")
                .iter()
                .any(|c| c.starts_with("kubeadm join 10.0.0.1:6443 --token abc")),
            "{:?}",
            t.calls_on("n12")
        );
    }

    #[test]
    fn steps_run_in_declaration_order_across_the_fleet() {
        // 舞的每一拍依赖前一拍:join 绝不能早于 init(否则 token 还不存在)。
        let bp = blueprint_from_str(K8S).unwrap();
        let t = ha_targets();
        let r = run(&bp, "bootstrap", &t, &BTreeMap::new()).unwrap();
        let order: Vec<&str> = r.steps.iter().map(|(_, m, _)| m.as_str()).collect();
        assert_eq!(order, vec!["n11", "n12", "n13", "w01"]);
    }

    #[test]
    fn an_already_joined_node_is_skipped_making_the_dance_idempotent() {
        // 有 `check:` 的步骤走的是同一套 observe→diff —— 重跑不会重来一遍。
        let bp = blueprint_from_str(K8S).unwrap();
        let done = || {
            FakeCtx::new()
                .on("test -f", 0, "") // 已经装好了
                .on("kubeadm token create", 0, "kubeadm join x\n")
                .on("", 0, "")
        };
        let t = FakeTargets::new(vec![("n11", vec!["controlplane"]), ("n12", vec!["controlplane"])])
            .with_ctx("n11", done())
            .with_ctx("n12", done());
        let r = run(&bp, "bootstrap", &t, &BTreeMap::new()).unwrap();
        assert_eq!(r.changed(), 0, "全都已就位:{:?}", r.steps);
        assert_eq!(r.ok(), 2);
        assert!(!t.calls_on("n11").iter().any(|c| c == "kubeadm init"), "不该重跑 init");
    }

    #[test]
    fn an_empty_selection_is_recorded_not_silently_ignored() {
        // 单 master:`rest(controlplane)` 为空是正常的,但要留痕 ——
        // 一支舞"什么都没做"最常见的原因就是组选错了。
        let bp = blueprint_from_str(K8S).unwrap();
        let t = FakeTargets::new(vec![("n11", vec!["controlplane"])]).with_ctx(
            "n11",
            FakeCtx::new().on("test -f", 1, "").on("", 0, "ok\n"),
        );
        let r = run(&bp, "bootstrap", &t, &BTreeMap::new()).unwrap();
        assert_eq!(r.skipped.len(), 2, "rest(cp) 与 role.worker 都为空:{:?}", r.skipped);
        assert!(r.skipped[0].contains("rest(role.controlplane)"), "{:?}", r.skipped);
        // 组根本不存在时要把线索写进留痕 —— 否则拼错组名就成了静默失败。
        assert!(
            r.skipped[1].contains("worker") && r.skipped[1].contains("已知的组"),
            "{:?}",
            r.skipped
        );
    }

    #[test]
    fn exporting_from_a_multi_host_step_is_refused() {
        // 同名 fact 被多台各写一份,消费方拿到哪一份取决于执行顺序 —— 不做"最后一个赢"。
        let bp = blueprint_from_str(
            r#"
name: t
procedures:
  boot:
    steps:
      - shell: { cmd: "true", check: "false" }
        on: role.controlplane
        exports: { token: "echo x" }
"#,
        )
        .unwrap();
        let t = FakeTargets::new(vec![
            ("n11", vec!["controlplane"]),
            ("n12", vec!["controlplane"]),
        ]);
        let err = run(&bp, "boot", &t, &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("一个 fact 只能有一个来源"), "{err}");
        assert!(err.contains("first("), "要给出改法:{err}");
    }

    #[test]
    fn procedure_params_are_typed_and_reach_the_commands() {
        let bp = blueprint_from_str(
            r#"
name: t
procedures:
  upgrade:
    params:
      to: { type: version }
    steps:
      - shell: { cmd: "kubeadm upgrade apply v${params.to}", check: "false" }
        on: all
"#,
        )
        .unwrap();
        // 默认假目标对一切命令返回 0,连 `check:` 也会"成功" → diff 恒为 Ok、
        // apply 永不触发。这里显式让 check 失败,表示"还没升级"。
        let t = FakeTargets::new(vec![("n11", vec![])])
            .with_ctx("n11", FakeCtx::new().on("false", 1, "").on("", 0, ""));
        let args = BTreeMap::from([("to".to_string(), Yaml::from("1.37.0"))]);
        run(&bp, "upgrade", &t, &args).unwrap();
        assert!(
            t.calls_on("n11").iter().any(|c| c == "kubeadm upgrade apply v1.37.0"),
            "{:?}",
            t.calls_on("n11")
        );

        // 类型不符要在动手之前拒绝
        let bad = BTreeMap::from([("to".to_string(), Yaml::from("not-a-version"))]);
        assert!(run(&bp, "upgrade", &t, &bad).is_err());
    }

    #[test]
    fn a_missing_fact_producer_fails_with_the_step_named() {
        let bp = blueprint_from_str(
            r#"
name: t
procedures:
  boot:
    steps:
      - shell: { cmd: "${facts.ghost} now", check: "false" }
        on: all
"#,
        )
        .unwrap();
        let t = FakeTargets::new(vec![("n11", vec![])]);
        let err = run(&bp, "boot", &t, &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("procedure `boot`"), "要指到是哪支舞哪一步:{err}");
    }

    #[test]
    fn ignore_errors_keeps_going_but_reports_warn() {
        let bp = blueprint_from_str(
            r#"
name: t
procedures:
  boot:
    steps:
      - shell: { cmd: "false", check: "false" }
        on: all
        strategy: { ignore_errors: true }
      - shell: { cmd: "echo after", check: "false" }
        on: all
"#,
        )
        .unwrap();
        let t = FakeTargets::new(vec![("n11", vec![])])
            .with_ctx("n11", FakeCtx::new().on("false", 1, "boom").on("", 0, ""));
        let r = run(&bp, "boot", &t, &BTreeMap::new()).unwrap();
        assert!(r.steps.iter().any(|(_, _, o)| *o == Outcome::Warn));
        assert!(
            t.calls_on("n11").iter().any(|c| c == "echo after"),
            "后续步骤应继续:{:?}",
            t.calls_on("n11")
        );
    }

    #[test]
    fn a_failing_step_without_ignore_errors_aborts_the_dance() {
        let bp = blueprint_from_str(
            r#"
name: t
procedures:
  boot:
    steps:
      - shell: { cmd: "false", check: "false" }
        on: all
      - shell: { cmd: "echo after", check: "false" }
        on: all
"#,
        )
        .unwrap();
        let t = FakeTargets::new(vec![("n11", vec![])])
            .with_ctx("n11", FakeCtx::new().on("false", 1, "boom").on("", 0, ""));
        assert!(run(&bp, "boot", &t, &BTreeMap::new()).is_err());
        assert!(
            !t.calls_on("n11").iter().any(|c| c == "echo after"),
            "失败后不该继续跳"
        );
    }

    #[test]
    fn an_unknown_procedure_name_is_a_clear_error() {
        let bp = blueprint_from_str(K8S).unwrap();
        let t = ha_targets();
        let err = run(&bp, "nope", &t, &BTreeMap::new()).unwrap_err().to_string();
        assert!(err.contains("没有名为 `nope`"), "{err}");
    }

    // ---- 自定义类型的 observe ----

    #[test]
    fn custom_observe_maps_output_markers_to_fields() {
        let bp = blueprint_from_str(K8S).unwrap();
        let def = bp.custom_type("cluster_member").unwrap();
        let joined = FakeCtx::new().on("test -f", 0, "joined\n");
        let obs = observe_custom(def, &joined, &Scope::default()).unwrap();
        assert!(obs.present);
        assert_eq!(obs.get("joined"), Some("joined"));

        let absent = FakeCtx::new().on("test -f", 0, "absent\n");
        assert!(!observe_custom(def, &absent, &Scope::default()).unwrap().present);
    }

    #[test]
    fn custom_observe_without_a_parse_map_falls_back_to_the_exit_code() {
        let bp = blueprint_from_str(
            r#"
name: t
types:
  - name: thing
    observe: { cmd: "test -f /marker" }
    apply: boot
procedures:
  boot:
    steps: []
"#,
        )
        .unwrap();
        let def = bp.custom_type("thing").unwrap();
        assert!(
            observe_custom(def, &FakeCtx::new().on("test", 0, ""), &Scope::default())
                .unwrap()
                .present
        );
        assert!(!observe_custom(def, &FakeCtx::new(), &Scope::default()).unwrap().present);
    }

    #[test]
    fn custom_observe_is_read_only() {
        let bp = blueprint_from_str(K8S).unwrap();
        let def = bp.custom_type("cluster_member").unwrap();
        let ctx = FakeCtx::new().on("test -f", 0, "joined\n");
        observe_custom(def, &ctx, &Scope::default()).unwrap();
        assert!(ctx.writes().is_empty(), "{:?}", ctx.writes());
    }
}
