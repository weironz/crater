//! 试金石回归测试:两块 docs/research/ir-example-*.md 的真实 blueprint 必须
//! **解析通过且 lint 零 error**。IR 改动一旦让它们不成立,这里立刻红。

use crater_ir::{lint, parse};

fn load(name: &str) -> crater_ir::Blueprint {
    let path = format!("{}/tests/fixtures/{name}.yaml", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("读 {path}: {e}"));
    parse::blueprint_from_str(&text).unwrap_or_else(|e| panic!("解析 {name} 失败: {e}"))
}

fn assert_clean(name: &str, bp: &crater_ir::Blueprint) {
    let diags = lint::lint(bp);
    let errs = lint::errors(&diags);
    assert!(
        errs.is_empty(),
        "{name} lint 有 error:\n{}",
        errs.iter()
            .map(|d| d.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn rustfs_parses_and_lints_clean() {
    let bp = load("rustfs");
    assert_clean("rustfs", &bp);

    assert_eq!(bp.name, "rustfs");
    assert_eq!(bp.resources.len(), 6);
    assert_eq!(bp.preflight.len(), 2);
    assert_eq!(bp.health.len(), 2);
    // 双 arch 物料同名,靠 `when:` 区分成两个 flavor —— 闭包 = f(values)。
    assert_eq!(bp.materials.len(), 2);
    assert!(bp
        .materials
        .iter()
        .all(|m| m.name == "rustfs-bin" && m.when.is_some()));
    // secret 参数被标记 → 孪生/日志/API 打码的依据。
    assert!(bp.params["secret_key"].secret);
    assert!(!bp.params["access_key"].secret);
    // 语义类型:port/version/list 都保住了,不再被压成字符串。
    assert_eq!(bp.params["port"].ty, crater_ir::schema::ParamType::Port);
    assert!(matches!(
        bp.params["data_dirs"].ty,
        crater_ir::schema::ParamType::List(_)
    ));
}

#[test]
fn rustfs_has_no_ceremony_fields() {
    let bp = load("rustfs");
    // 旧模型每步都要 id/needs;新模型自动生成 id,且默认按声明顺序建边。
    assert!(bp.resources.iter().all(|r| r.deps.is_empty()));
    // 同类型多条自动编号,互不撞名。
    let ids: Vec<&str> = bp.resources.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids[0], "file");
    assert_eq!(ids[1], "file1");
    let uniq: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(uniq.len(), ids.len(), "自动 id 撞名: {ids:?}");
}

#[test]
fn k8s_parses_and_lints_clean() {
    let bp = load("k8s-ha");
    assert_clean("k8s-ha", &bp);
    assert_eq!(bp.requires.arch, vec!["amd64"]);
    assert_eq!(bp.requires.os[0].distro, "ubuntu");
}

#[test]
fn k8s_selectors_express_first_and_rest() {
    use crater_ir::selector::Selector;
    let bp = load("k8s-ha");
    let boot = &bp.procedures["bootstrap"];

    // 旧模型只能"全组跑 + check 守卫跳过首台";这里直说 first / rest。
    let init = boot
        .steps
        .iter()
        .find(|s| s.exports.contains_key("join"))
        .unwrap();
    assert!(matches!(init.on, Selector::First(_)));
    let cp_join = boot
        .steps
        .iter()
        .find(|s| matches!(s.on, Selector::Rest(_)))
        .expect("应有 rest(role.controlplane) 的 join 步");
    assert_eq!(
        cp_join.strategy.throttle,
        Some(1),
        "cp join 必须逐台护 etcd"
    );
    assert_eq!(init.on.roles(), vec!["controlplane"]);
}

#[test]
fn k8s_facts_are_declared_next_to_their_producer() {
    let bp = load("k8s-ha");
    let boot = &bp.procedures["bootstrap"];
    let init = boot.steps.iter().find(|s| !s.exports.is_empty()).unwrap();
    // 旧模型在文件顶部 register: 声明、300 行外消费;现在就近声明。
    assert_eq!(
        init.exports.keys().collect::<Vec<_>>(),
        vec!["certkey", "join"]
    );
    // 产销平衡由 lint 保证(见 lint 单测),这里确认消费方确实引用了它们。
    let consumer = boot
        .steps
        .iter()
        .find(|s| matches!(s.on, crater_ir::selector::Selector::Rest(_)))
        .unwrap();
    let cmd = format!("{:?}", consumer.args.get("cmd").unwrap());
    assert!(
        cmd.contains("facts.join") && cmd.contains("facts.certkey"),
        "{cmd}"
    );
}

#[test]
fn k8s_custom_type_carries_the_dance() {
    let bp = load("k8s-ha");
    let t = bp.custom_type("cluster_member").expect("自定义类型");
    // 状态在资源(用户写 `cluster_member: {role: ...}`),舞封装在 procedure 里。
    assert_eq!(t.apply, "bootstrap");
    assert_eq!(t.destroy.as_deref(), Some("reset"));
    assert!(
        t.observe.cmd.contains("kubelet.conf"),
        "observe 必须是只读探针"
    );
    // 用户面对的是名词:资源列表里出现两条 cluster_member,没有 init/join 字样。
    let members: Vec<_> = bp
        .resources
        .iter()
        .filter(|r| r.ty == "cluster_member")
        .collect();
    assert_eq!(members.len(), 2);
}

#[test]
fn touchstones_are_dramatically_shorter_than_the_old_form() {
    // 与 library/ 里的对照物比行数 —— 这是 DSL 重做的核心指标(见 D-106)。
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf();
    for (new, old, floor) in [
        ("rustfs", "library/rustfs/rustfs.yaml", 2.0),
        ("k8s-ha", "library/k8s/k8s-ha.yaml", 2.0),
    ] {
        let old_path = repo.join(old);
        let Ok(old_text) = std::fs::read_to_string(&old_path) else {
            continue; // 老库被删掉后本断言自然退休
        };
        let new_text = std::fs::read_to_string(format!(
            "{}/tests/fixtures/{new}.yaml",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap();
        let (o, n) = (old_text.lines().count(), new_text.lines().count());
        assert!(
            (o as f64) / (n as f64) >= floor,
            "{new}: 新写法 {n} 行 vs 旧 {o} 行,压缩比不足 {floor}×"
        );
    }
}
