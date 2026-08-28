//! procedure 执行器的端到端契约:**舞是机群级的**。
//!
//! 它与逐台 converge 的根本区别在于跨主机:首台产出的 fact 要能流到其余台。
//! 这是 D-106「状态/过程分离」能否成立的判据。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn crater(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crater"))
        .args(args)
        .env("HOME", home)
        .current_dir(workspace())
        .output()
        .expect("run crater")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn inventory(dir: &Path) -> PathBuf {
    let p = dir.join("inv.yaml");
    std::fs::write(
        &p,
        r#"
inventory:
  hosts:
    - { name: n11, address: "@local" }
    - { name: n12, address: "@local" }
    - { name: w01, address: "@local" }
  groups:
    controlplane: { hosts: [n11, n12] }
    worker: { hosts: [w01] }
"#,
    )
    .unwrap();
    p
}

/// 每台把自己名字写进不同文件 —— 这样"谁跑了哪一步"在磁盘上可验证。
fn dance(dir: &Path, root: &Path) -> PathBuf {
    let p = dir.join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: dance
procedures:
  bootstrap:
    steps:
      - shell:
          cmd: "mkdir -p {root} && echo THE-TOKEN > {root}/token"
          check: "test -f {root}/token"
        on: first(role.controlplane)
        exports: {{ join: "cat {root}/token" }}
      - shell:
          cmd: "echo '${{facts.join}}' > {root}/cp-join"
          check: "test -f {root}/cp-join"
        on: rest(role.controlplane)
        strategy: {{ throttle: 1 }}
      - shell:
          cmd: "echo '${{facts.join}}' > {root}/worker-join"
          check: "test -f {root}/worker-join"
        on: role.worker
resources:
  - file: {{ path: "{root}", state: directory }}
"#,
            root = root.display()
        ),
    )
    .unwrap();
    p
}

#[test]
fn a_fact_produced_on_the_first_host_reaches_the_others() {
    // 整个设计的判据:token 在 n11 产出,n12 与 w01 都要拿到**真实值**。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("state");
    let inv = inventory(d.path());
    let bp = dance(d.path(), &root);

    let o = crater(
        &home,
        &[
            "procedure",
            "bootstrap",
            "-f",
            bp.to_str().unwrap(),
            "-i",
            inv.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", stdout(&o));

    assert_eq!(std::fs::read_to_string(root.join("token")).unwrap().trim(), "THE-TOKEN");
    assert_eq!(std::fs::read_to_string(root.join("cp-join")).unwrap().trim(), "THE-TOKEN");
    assert_eq!(
        std::fs::read_to_string(root.join("worker-join")).unwrap().trim(),
        "THE-TOKEN",
        "worker 也要拿到跨主机 fact"
    );
    assert!(stdout(&o).contains("跨主机 fact:join"), "{}", stdout(&o));
}

#[test]
fn steps_target_only_the_hosts_their_selector_names() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("state");
    let inv = inventory(d.path());
    let bp = dance(d.path(), &root);

    let out = stdout(&crater(
        &home,
        &["procedure", "bootstrap", "-f", bp.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    ));
    // 第一步只在 n11;第二步只在 n12;第三步只在 w01
    // 只取步骤行,别把汇总行(`执行:changed=3 …`)也算进来
    let lines: Vec<&str> = out
        .lines()
        .filter(|l| l.trim_start().starts_with("changed "))
        .collect();
    assert_eq!(lines.len(), 3, "{out}");
    assert!(lines[0].contains("n11"), "{out}");
    assert!(lines[1].contains("n12"), "{out}");
    assert!(lines[2].contains("w01"), "{out}");
}

#[test]
fn rerunning_a_dance_is_idempotent() {
    // 步骤走的是同一套 observe→diff,有 `check:` 就不会重来一遍。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("state");
    let inv = inventory(d.path());
    let bp = dance(d.path(), &root);
    let args = [
        "procedure",
        "bootstrap",
        "-f",
        bp.to_str().unwrap(),
        "-i",
        inv.to_str().unwrap(),
    ];

    let first = stdout(&crater(&home, &args));
    assert!(first.contains("changed=3"), "{first}");
    let second = stdout(&crater(&home, &args));
    assert!(second.contains("changed=0 ok=3"), "重跑应全 ok:{second}");
}

#[test]
fn an_empty_topology_layer_is_skipped_visibly_not_silently() {
    // 单 master 无 worker:`rest(...)` 与 `role.worker` 本就该为空 —— 但要看得见。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("state");
    let inv = d.path().join("inv.yaml");
    std::fs::write(
        &inv,
        r#"
inventory:
  hosts:
    - { name: n11, address: "@local" }
  groups:
    controlplane: { hosts: [n11] }
"#,
    )
    .unwrap();
    let bp = dance(d.path(), &root);

    let o = crater(
        &home,
        &["procedure", "bootstrap", "-f", bp.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    );
    assert!(o.status.success(), "{}", stdout(&o));
    let out = stdout(&o);
    assert!(out.contains("skip"), "空选择要留痕:{out}");
    assert!(out.contains("skipped=2"), "{out}");
    assert!(!root.join("worker-join").exists(), "没有 worker 就不该有 worker 产物");
}

#[test]
fn a_custom_type_triggers_its_dance_from_apply_exactly_once() {
    // L2 数据模块层:用户写的是名词(`cluster_member`),舞被封装在类型里。
    // 关键是**一次** —— 在逐台循环里跑会把同一支舞跳 N 遍。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("state");
    let inv = inventory(d.path());
    let p = d.path().join("l2.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: l2
types:
  - name: cluster_member
    observe: {{ cmd: "test -f {root}/done && echo joined || echo absent",
               parse: {{ joined: joined }} }}
    apply: bootstrap
procedures:
  bootstrap:
    steps:
      - shell:
          cmd: "mkdir -p {root} && echo x >> {root}/ran && touch {root}/done"
          check: "test -f {root}/done"
        on: first(role.controlplane)
resources:
  - cluster_member: {{ role: control-plane }}
"#,
            root = root.display()
        ),
    )
    .unwrap();

    let o = crater(
        &home,
        &["apply", "-f", p.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    );
    assert!(o.status.success(), "{}", stdout(&o));
    let out = stdout(&o);
    assert!(out.contains("── procedure bootstrap ──"), "{out}");
    // 三台目标,但舞只跳一次
    let ran = std::fs::read_to_string(root.join("ran")).unwrap();
    assert_eq!(ran.lines().count(), 1, "舞被跳了 {} 遍", ran.lines().count());

    // 再 apply:探针说已在册 → 不再跳
    let again = stdout(&crater(
        &home,
        &["apply", "-f", p.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    ));
    assert!(!again.contains("── procedure"), "已就位不该再跳:{again}");
    assert_eq!(std::fs::read_to_string(root.join("ran")).unwrap().lines().count(), 1);
}

#[test]
fn plan_names_the_dance_a_custom_type_will_use() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("state");
    let inv = inventory(d.path());
    let p = d.path().join("l2.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: l2
types:
  - name: cluster_member
    observe: {{ cmd: "test -f {root}/done" }}
    apply: bootstrap
procedures:
  bootstrap:
    steps:
      - shell: {{ cmd: "true", check: "test -f {root}/done" }}
        on: all
resources:
  - cluster_member: {{ role: control-plane }}
"#,
            root = root.display()
        ),
    )
    .unwrap();

    let out = stdout(&crater(
        &home,
        &["plan", "-f", p.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    ));
    assert!(out.contains("via: procedure bootstrap"), "plan 要说清靠哪支舞:{out}");
    assert!(!out.contains("?1"), "自定义类型不该再计入模型化欠债:{out}");
}

#[test]
fn an_unknown_procedure_name_lists_the_available_ones() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("state");
    let inv = inventory(d.path());
    let bp = dance(d.path(), &root);

    let o = crater(
        &home,
        &["procedure", "nope", "-f", bp.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("bootstrap"), "要列出可用的:{err}");
}
