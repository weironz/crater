//! 五动词契约的端到端验证 —— 用**记录式假目标**(零网络)把三条核心主张钉死:
//!
//! 1. plan 期一条写命令都不发;
//! 2. 幂等:converge 之后重跑 plan 全 `✓`;
//! 3. handler 被删掉之后,上游变更**照样**触发服务重启;
//! 4. teardown 不用写:destroy 由契约逆序推导出来。

use crater_ir::ctx::FakeCtx;
use crater_ir::plan::{converge, destroy, plan, scope_from_defaults};
use crater_ir::{parse, Blueprint};

const BP: &str = r#"
name: demo-svc
params:
  port: { type: port, default: 9000 }
  data_dirs: { type: [string], default: ["/data/a", "/data/b"] }
resources:
  - file: { path: "${item}", state: directory, mode: "0750" }
    each: params.data_dirs
  - copy:
      dest: /etc/default/demo
      mode: "0600"
      content: "PORT=${params.port}\n"
  - service: { name: demo, state: started, enabled: true }
"#;

fn bp() -> Blueprint {
    parse::blueprint_from_str(BP).expect("解析 blueprint")
}

/// 一台"什么都还没有"的机器。
fn blank_host() -> FakeCtx {
    FakeCtx::new()
}

/// 一台已经装好、且完全符合期望的机器。
fn converged_host() -> FakeCtx {
    let sep = "\u{1}";
    // content 与 blueprint 里渲染出来的完全一致 → sha 必须对得上,
    // 所以这里让 copy 的探针返回真实内容的摘要。
    let sha = sha256_of("PORT=9000\n");
    FakeCtx::new()
        .on(
            "stat -c '%F",
            0,
            &format!("directory{sep}750{sep}root{sep}root"),
        )
        .on("sha256sum", 0, &format!("{sha}\n600\n"))
        .on(
            "systemctl is-active",
            0,
            &format!("active{sep}enabled{sep}demo.service enabled"),
        )
        // 写命令也要能成功 —— 这台机器是"真"的,不只是一组探针应答。
        .on("systemctl", 0, "")
        .on("rm -", 0, "")
        .on("chmod", 0, "")
        .on("mkdir", 0, "")
}

/// 与 copy.rs 内部实现一致的 sha256(测试独立算一遍,免得互相掩盖错误)。
fn sha256_of(s: &str) -> String {
    use std::process::Command;
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "printf '%s' {} | sha256sum | cut -d' ' -f1",
            shell_quote(s)
        ))
        .output()
        .expect("sha256sum");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[test]
fn plan_never_writes_to_the_target() {
    // 这是整个设计的地基:plan 可信,是因为 observe 只读。
    let ctx = blank_host();
    let p = plan(&bp(), &scope_from_defaults(&bp()), &ctx).unwrap();
    assert!(p.has_changes());
    assert!(
        ctx.writes().is_empty(),
        "plan 期发生了写操作:{:?}",
        ctx.writes()
    );
    assert!(!ctx.calls().is_empty(), "但确实探测过目标");
}

#[test]
fn a_blank_host_plans_everything_as_create() {
    let b = bp();
    let p = plan(&b, &scope_from_defaults(&b), &blank_host()).unwrap();
    // 2 个数据目录(each 展开)+ 1 个配置文件 + 1 个服务
    assert_eq!(
        p.items.len(),
        4,
        "{:#?}",
        p.items.iter().map(|i| &i.id).collect::<Vec<_>>()
    );
    assert_eq!(p.summary(), "+4 ~0 -0 ✓0");
    assert_eq!(p.debt(), 0, "全是可判定的资源,不该有欠债");
}

#[test]
fn each_expansion_gives_每项一个独立计划行() {
    let b = bp();
    let p = plan(&b, &scope_from_defaults(&b), &blank_host()).unwrap();
    let ids: Vec<&str> = p.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(&ids[..2], &["file[0]", "file[1]"]);
    // 每项的实参都被真正代入了(不是共用一份)。
    let paths: Vec<&str> = p.items[..2]
        .iter()
        .map(|i| i.args["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, vec!["/data/a", "/data/b"]);
}

#[test]
fn an_already_converged_host_plans_nothing() {
    // 幂等的正面表述:现实符合期望 → plan 是空的。
    let b = bp();
    let p = plan(&b, &scope_from_defaults(&b), &converged_host()).unwrap();
    assert_eq!(
        p.summary(),
        "+0 ~0 -0 ✓4",
        "{:#?}",
        p.items
            .iter()
            .map(|i| (&i.id, &i.change))
            .collect::<Vec<_>>()
    );
    assert!(!p.has_changes());
}

#[test]
fn converge_then_replan_is_idempotent() {
    let b = bp();
    let ctx = converged_host();
    let report = converge(&b, &scope_from_defaults(&b), &ctx).unwrap();
    assert_eq!(report.summary(), "changed=0 ok=4");
    ctx.reset_log();
    let p = plan(&b, &scope_from_defaults(&b), &ctx).unwrap();
    assert!(!p.has_changes(), "重跑仍应无变更");
}

#[test]
fn converge_on_a_blank_host_writes_and_reports_changed() {
    let ctx = FakeCtx::new().on("", 0, ""); // 一切命令成功,但探针返回空 → 视为不存在
    let b = bp();
    let report = converge(&b, &scope_from_defaults(&b), &ctx).unwrap();
    assert_eq!(report.changed(), 4);
    assert_eq!(
        ctx.written_file("/etc/default/demo").as_deref(),
        Some("PORT=9000\n")
    );
}

#[test]
fn upstream_change_restarts_the_service_with_no_handler_declared() {
    // blueprint 里没有一个 `notify:` —— 但配置变了,服务必须重启。
    let sep = "\u{1}";
    let ctx = FakeCtx::new()
        .on(
            "stat -c '%F",
            0,
            &format!("directory{sep}750{sep}root{sep}root"),
        )
        .on(
            "sha256sum",
            0,
            "0000000000000000000000000000000000000000000000000000000000000000\n600\n",
        )
        .on(
            "systemctl is-active",
            0,
            &format!("active{sep}enabled{sep}demo.service enabled"),
        );

    let b = bp();
    let p = plan(&b, &scope_from_defaults(&b), &ctx).unwrap();
    let svc = p.items.iter().find(|i| i.ty == "service").unwrap();
    assert!(!svc.change.is_noop(), "配置变了服务却不动:{:?}", svc.change);
    assert!(
        svc.change
            .fields()
            .iter()
            .any(|f| f.to_string().contains("restarted")),
        "{:?}",
        svc.change.fields()
    );
}

#[test]
fn a_stable_host_does_not_get_a_spurious_restart() {
    // 反向保险:上游没变时不能因为这条规则天天重启服务。
    let b = bp();
    let p = plan(&b, &scope_from_defaults(&b), &converged_host()).unwrap();
    let svc = p.items.iter().find(|i| i.ty == "service").unwrap();
    assert!(svc.change.is_noop(), "{:?}", svc.change);
}

#[test]
fn destroy_runs_in_reverse_declaration_order() {
    // blueprint 里没有 `teardown:` 段 —— 退役完全由五动词推出来。
    let ctx = converged_host();
    let b = bp();
    let report = destroy(&b, &scope_from_defaults(&b), &ctx).unwrap();
    let order: Vec<&str> = report.steps.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(order, vec!["service", "copy", "file[1]", "file[0]"]);
    assert_eq!(report.changed(), 4);
}

#[test]
fn destroying_an_already_clean_host_is_a_noop() {
    let ctx = blank_host();
    let b = bp();
    let report = destroy(&b, &scope_from_defaults(&b), &ctx).unwrap();
    assert_eq!(report.ok(), 4, "什么都不在了,就什么都别删");
    assert!(
        ctx.writes().is_empty(),
        "不该对不存在的资源发写命令:{:?}",
        ctx.writes()
    );
}

#[test]
fn param_overrides_flow_into_rendered_content() {
    use crater_ir::plan::with_overrides;
    let b = bp();
    let scope = with_overrides(
        scope_from_defaults(&b),
        &[("port".to_string(), serde_yaml::Value::from(9443))],
    );
    let ctx = FakeCtx::new().on("", 0, "");
    converge(&b, &scope, &ctx).unwrap();
    assert_eq!(
        ctx.written_file("/etc/default/demo").as_deref(),
        Some("PORT=9443\n")
    );
}

#[test]
fn a_custom_type_is_observed_by_its_declared_probe_not_marked_unknown() {
    // L2 数据模块层:引擎不懂 kubeadm,但作者给了只读探针 —— plan 因此说得出话,
    // 不再是一个 `?`。弥合动作是一支舞,由机群层执行。
    let b = parse::blueprint_from_str(CUSTOM).unwrap();

    let joined = FakeCtx::new().on("kubelet.conf", 0, "joined\n");
    let p = plan(&b, &scope_from_defaults(&b), &joined).unwrap();
    assert_eq!(
        p.summary(),
        "+0 ~0 -0 ✓1",
        "已在册就该是 ✓:{:?}",
        p.items[0].change
    );
    assert_eq!(p.debt(), 0, "自定义类型不再计入模型化欠债");

    let fresh = FakeCtx::new().on("kubelet.conf", 0, "absent\n");
    let p = plan(&b, &scope_from_defaults(&b), &fresh).unwrap();
    assert_eq!(p.summary(), "+1 ~0 -0 ✓0");
    // plan 要说清"靠哪支舞"达成
    assert!(
        p.items[0].change.fields()[0]
            .to_string()
            .contains("procedure bootstrap"),
        "{:?}",
        p.items[0].change.fields()
    );
}

#[test]
fn converging_a_custom_type_defers_to_the_fleet_level_dance() {
    // procedure 是机群级的:不能在"逐台"循环里跑 N 遍,所以 converge 只记下
    // "需要跳哪支舞",由调用方收齐去重后在机群层跑一次。
    let b = parse::blueprint_from_str(CUSTOM).unwrap();
    let ctx = FakeCtx::new().on("kubelet.conf", 0, "absent\n");
    let report = converge(&b, &scope_from_defaults(&b), &ctx).unwrap();
    assert!(
        report.procedures_needed.contains("bootstrap"),
        "{:?}",
        report.procedures_needed
    );
    // 不该在这一层偷偷执行任何舞的步骤
    assert!(
        !ctx.calls()
            .iter()
            .any(|c| c.text().contains("kubeadm init")),
        "{:?}",
        ctx.calls()
    );
}

const CUSTOM: &str = r#"
name: custom
types:
  - name: cluster_member
    observe: { cmd: "test -f /etc/kubernetes/kubelet.conf && echo joined || echo absent",
               parse: { joined: joined } }
    apply: bootstrap
procedures:
  bootstrap:
    steps:
      - shell: { cmd: "kubeadm init", check: "test -f /etc/kubernetes/admin.conf" }
        target: all
resources:
  - cluster_member: { role: control-plane }
"#;

#[test]
fn a_type_without_an_implementation_is_marked_unknown_not_faked() {
    // 一个既非内建、又没在 `types:` 里声明的类型:plan 必须诚实说"说不清",
    // 而不是假装成功。这些项计入"模型化欠债"。
    //
    // 内建登记表如今已全部有实现(见 builtins::pending 的断言),所以这条
    // 通路只能用一个引擎不认得的名字来走 —— lint 会拦下它,但 plan 本身
    // 必须在被绕过 lint 时依然诚实。
    let b = parse::blueprint_from_str("name: t\nresources:\n  - not_a_real_type: { name: web }\n")
        .unwrap();
    let p = plan(&b, &scope_from_defaults(&b), &blank_host()).unwrap();
    assert_eq!(p.debt(), 1);
    assert_eq!(p.summary(), "+0 ~0 -0 ✓0 ?1");
}

#[test]
fn conditional_resources_disappear_from_the_plan_entirely() {
    let b = parse::blueprint_from_str(
        r#"
name: t
params:
  ha: { type: bool, default: false }
resources:
  - file: { path: /always, state: directory }
  - file: { path: /only-ha, state: directory }
    when: params.ha
"#,
    )
    .unwrap();
    let p = plan(&b, &scope_from_defaults(&b), &blank_host()).unwrap();
    assert_eq!(p.items.len(), 1);
    assert_eq!(p.items[0].args["path"].as_str(), Some("/always"));
}

#[test]
fn plan_lines_name_the_thing_they_act_on() {
    // `+ file1` 看不出改的是哪个文件;标签要带上主键参数。
    let b = bp();
    let p = plan(&b, &scope_from_defaults(&b), &blank_host()).unwrap();
    let labels: Vec<String> = p.items.iter().map(|i| i.label()).collect();
    assert_eq!(labels[0], "file /data/a");
    assert_eq!(labels[1], "file /data/b");
    assert_eq!(labels[2], "copy /etc/default/demo");
    assert_eq!(labels[3], "service demo");
}

#[test]
fn very_long_paths_are_elided_in_the_middle() {
    // 路径的辨识度在两端 —— 掐头去尾会让两个不同的深路径看起来一样。
    let b = parse::blueprint_from_str(
        "name: t\nresources:\n  - file: { path: /very/long/prefix/that/goes/on/and/on/forever/and/ever/deep/final-name.conf, state: directory }\n",
    )
    .unwrap();
    let p = plan(&b, &scope_from_defaults(&b), &blank_host()).unwrap();
    let label = p.items[0].label();
    assert!(label.contains('…'), "{label}");
    assert!(label.starts_with("file /very/long"), "{label}");
    assert!(label.ends_with("final-name.conf"), "尾部必须保留:{label}");
}

#[test]
fn audit_does_not_let_one_drift_stain_everything_downstream() {
    // 真机跑出来的问题:一处 unit 被手改,verify 把后面 6 个物料型资源全染红,
    // 归因被淹没。收敛要传播(宁可多重启),审计不能传播(要指出**哪里**漂了)。
    use crater_ir::plan::{plan_with, Intent};

    let b = bp();
    // 配置内容不符(真漂移),其余一切正常。
    let sep = "\u{1}";
    let ctx = FakeCtx::new()
        .on(
            "stat -c '%F",
            0,
            &format!("directory{sep}750{sep}root{sep}root"),
        )
        .on(
            "sha256sum",
            0,
            "0000000000000000000000000000000000000000000000000000000000000000\n600\n",
        )
        .on(
            "systemctl is-active",
            0,
            &format!("active{sep}enabled{sep}demo.service enabled"),
        );

    let converge = plan_with(&b, &scope_from_defaults(&b), &ctx, Intent::Converge).unwrap();
    let audit = plan_with(&b, &scope_from_defaults(&b), &ctx, Intent::Audit).unwrap();

    // 收敛:配置变了 → 服务也要重启(两项)
    assert_eq!(converge.changing().count(), 2, "{:?}", converge.summary());
    // 审计:只有配置那一项真的不符,服务本身是好的
    assert_eq!(audit.changing().count(), 1, "{:?}", audit.summary());
    assert_eq!(audit.changing().next().unwrap().ty, "copy");
}

#[test]
fn audit_still_reports_genuinely_drifted_resources() {
    // 反向保险:别为了消除假阳性把真漂移也一并压掉。
    use crater_ir::plan::{plan_with, Intent};
    let b = bp();
    let audit = plan_with(&b, &scope_from_defaults(&b), &blank_host(), Intent::Audit).unwrap();
    assert_eq!(audit.changing().count(), 4, "空机器上四项都该报");
}

#[test]
fn a_destroy_plan_writes_nothing_and_lists_what_exists() {
    // 破坏性动作必须先能被"看"。一个不能预演的 destroy,人只能靠勇气去按。
    use crater_ir::plan::plan_destroy;
    let ctx = converged_host();
    let b = bp();
    let p = plan_destroy(&b, &scope_from_defaults(&b), &ctx).unwrap();
    assert!(
        ctx.writes().is_empty(),
        "退役计划期发生了写操作:{:?}",
        ctx.writes()
    );
    assert_eq!(p.summary(), "+0 ~0 -4 ✓0");
}

#[test]
fn a_destroy_plan_is_the_reverse_of_declaration_order() {
    // 后建的先拆:服务先停,配置后删,目录最后。
    use crater_ir::plan::plan_destroy;
    let b = bp();
    let p = plan_destroy(&b, &scope_from_defaults(&b), &converged_host()).unwrap();
    let order: Vec<&str> = p.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(order, vec!["service", "copy", "file[1]", "file[0]"]);
}

#[test]
fn a_destroy_plan_on_a_clean_host_shows_nothing_to_remove() {
    // 对不存在的资源报"将删除"会让人误以为还有残留。
    use crater_ir::plan::plan_destroy;
    let b = bp();
    let p = plan_destroy(&b, &scope_from_defaults(&b), &blank_host()).unwrap();
    assert_eq!(p.summary(), "+0 ~0 -0 ✓4");
    assert!(!p.has_changes());
}

#[test]
fn a_custom_type_without_a_destroy_dance_is_not_pretended_to_be_destroyable() {
    // 引擎不懂怎么拆 kubeadm。没声明 `destroy:` 时诚实报"说不清",
    // 而不是假装拆得掉。
    use crater_ir::plan::plan_destroy;
    let b = parse::blueprint_from_str(CUSTOM).unwrap();
    assert!(b.types[0].destroy.is_none(), "本夹具刻意不声明 destroy");
    let ctx = FakeCtx::new().on("kubelet.conf", 0, "joined\n");
    let p = plan_destroy(&b, &scope_from_defaults(&b), &ctx).unwrap();
    assert_eq!(p.debt(), 1, "{:?}", p.items[0].change);
}

#[test]
fn a_custom_type_with_a_destroy_dance_plans_as_a_real_removal() {
    // 声明了 `destroy:` 就说得出"靠哪支舞退役" —— 不再是一个 `?`。
    use crater_ir::plan::{destroy_dances, plan_destroy};
    let src = CUSTOM.replace(
        "apply: bootstrap",
        "apply: bootstrap\n    destroy: teardown",
    );
    let src = src.replace("procedures:\n  bootstrap:", "procedures:\n  teardown:\n    steps:\n      - shell: { cmd: \"kubeadm reset -f\", check: \"! test -f /etc/kubernetes/kubelet.conf\" }\n  bootstrap:");
    let b = parse::blueprint_from_str(&src).expect("夹具应能解析");

    let joined = FakeCtx::new().on("kubelet.conf", 0, "joined\n");
    let p = plan_destroy(&b, &scope_from_defaults(&b), &joined).unwrap();
    assert_eq!(p.summary(), "+0 ~0 -1 ✓0", "{:?}", p.items[0].change);
    assert_eq!(p.debt(), 0);
    assert_eq!(
        destroy_dances(&b, &scope_from_defaults(&b), &joined).unwrap(),
        vec!["teardown"]
    );

    // 已经不在册的机器不该再跳一次退役的舞。
    let gone = FakeCtx::new().on("kubelet.conf", 0, "absent\n");
    assert!(destroy_dances(&b, &scope_from_defaults(&b), &gone)
        .unwrap()
        .is_empty());
    assert_eq!(
        plan_destroy(&b, &scope_from_defaults(&b), &gone)
            .unwrap()
            .summary(),
        "+0 ~0 -0 ✓1"
    );
}
