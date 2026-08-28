//! `on:` selector 的端到端契约。
//!
//! 修复前它被**完全忽略**:写 `on: role.controlplane` 的资源会在每一台机器上跑。
//! 声明了却静默不生效,比不支持更危险 —— 这些测试守住它真的在过滤。

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

/// 三台本地目标:n11/n12 是 controlplane,w01 是 worker。
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

fn blueprint(dir: &Path, root: &Path) -> PathBuf {
    let p = dir.join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: sel-demo
resources:
  - file: {{ path: "{root}/all",  state: directory }}
  - file: {{ path: "{root}/cp",   state: directory }}
    on: role.controlplane
  - file: {{ path: "{root}/init", state: directory }}
    on: first(role.controlplane)
  - file: {{ path: "{root}/join", state: directory }}
    on: rest(role.controlplane)
  - file: {{ path: "{root}/wk",   state: directory }}
    on: role.worker
"#,
            root = root.display()
        ),
    )
    .unwrap();
    p
}

/// 每台目标那一段的计划条数(按 `── host ──` 分段统计计划行)。
fn per_host_counts(out: &str) -> Vec<(String, usize)> {
    let mut result = Vec::new();
    let mut current: Option<String> = None;
    let mut n = 0usize;
    for line in out.lines() {
        if let Some(rest) = line.trim().strip_prefix("── ") {
            if let Some(h) = current.take() {
                result.push((h, n));
            }
            current = Some(rest.trim_end_matches(" ──").to_string());
            n = 0;
        } else {
            let t = line.trim_start();
            if t.starts_with("+ ") || t.starts_with("✓ ") || t.starts_with("~ ") {
                n += 1;
            }
        }
    }
    if let Some(h) = current {
        result.push((h, n));
    }
    result
}

#[test]
fn each_host_only_plans_the_resources_selected_for_it() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let inv = inventory(d.path());
    let bp = blueprint(d.path(), &root);

    let o = crater(
        &home,
        &["plan", "-f", bp.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    );
    assert!(o.status.success(), "{}", stdout(&o));
    let counts = per_host_counts(&stdout(&o));

    // n11 = all + cp + init;n12 = all + cp + join;w01 = all + wk
    assert_eq!(counts.len(), 3, "{counts:?}");
    assert_eq!(counts[0].1, 3, "n11 应有 3 项:{counts:?}");
    assert_eq!(counts[1].1, 3, "n12 应有 3 项:{counts:?}");
    assert_eq!(counts[2].1, 2, "w01 只该有 2 项(all + wk):{counts:?}");
}

#[test]
fn first_and_rest_split_the_control_plane() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let inv = inventory(d.path());
    let bp = blueprint(d.path(), &root);

    let out = stdout(&crater(
        &home,
        &["plan", "-f", bp.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    ));
    let segments: Vec<&str> = out.split("── ").skip(1).collect();
    assert_eq!(segments.len(), 3);
    // 首台拿到 init,不该拿到 join;第二台反之。这是 HA 编排的地基。
    assert!(segments[0].contains("/init"), "n11 段:{}", segments[0]);
    assert!(!segments[0].contains("/join"), "n11 不该有 join:{}", segments[0]);
    assert!(segments[1].contains("/join"), "n12 段:{}", segments[1]);
    assert!(!segments[1].contains("/init"), "n12 不该有 init:{}", segments[1]);
    // worker 两个都不该有
    assert!(!segments[2].contains("/init") && !segments[2].contains("/join"));
}

#[test]
fn each_fleet_member_gets_its_own_deployment_record() {
    // 早期版本对本地目标一律显示"本机",三台会共用一条记录、互相覆盖。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let inv = inventory(d.path());
    let bp = blueprint(d.path(), &root);

    crater(
        &home,
        &["apply", "-f", bp.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    );
    let dir = home.join(".crater").join("state");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["sel-demo_n11.yaml", "sel-demo_n12.yaml", "sel-demo_w01.yaml"]
    );
}

#[test]
fn a_misspelled_group_fails_loudly_instead_of_silently_skipping() {
    // 拼错组名让整段资源悄悄不执行、而 plan 看起来一切正常 —— 最难查的一类故障。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let inv = inventory(d.path());
    let p = d.path().join("bad.yaml");
    std::fs::write(
        &p,
        format!(
            "name: bad\nresources:\n  - file: {{ path: \"{}/x\", state: directory }}\n    on: role.controlplna\n",
            d.path().join("target").display()
        ),
    )
    .unwrap();

    let o = crater(
        &home,
        &["plan", "-f", p.to_str().unwrap(), "-i", inv.to_str().unwrap()],
    );
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("controlplna"), "{err}");
    assert!(err.contains("controlplane"), "要列出已知的组:{err}");
}

#[test]
fn a_group_selector_without_an_inventory_points_at_the_fix() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let p = d.path().join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            "name: g\nresources:\n  - file: {{ path: \"{}/x\", state: directory }}\n    on: role.controlplane\n",
            d.path().join("target").display()
        ),
    )
    .unwrap();

    let o = crater(&home, &["plan", "-f", p.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("-i inventory.yaml"), "要给出下一步:{err}");
}

#[test]
fn selectors_actually_gate_execution_not_just_the_plan() {
    // plan 说不做,apply 就真的不能做 —— 否则 plan 的可信度是假的。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let inv = d.path().join("inv.yaml");
    std::fs::write(
        &inv,
        r#"
inventory:
  hosts:
    - { name: w01, address: "@local" }
  groups:
    worker: { hosts: [w01] }
"#,
    )
    .unwrap();
    let p = d.path().join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: gate
resources:
  - file: {{ path: "{root}/worker-only", state: directory }}
    on: role.worker
  - file: {{ path: "{root}/nobody", state: directory }}
    on: host.n99
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
    assert!(root.join("worker-only").is_dir(), "选中的该建");
    assert!(!root.join("nobody").exists(), "没选中的绝不能建");
}
