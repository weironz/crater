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
    target: role.controlplane
  - file: {{ path: "{root}/init", state: directory }}
    target: first(role.controlplane)
  - file: {{ path: "{root}/join", state: directory }}
    target: rest(role.controlplane)
  - file: {{ path: "{root}/wk",   state: directory }}
    target: role.worker
"#,
            root = root.display()
        ),
    )
    .unwrap();
    p
}

/// 每台目标的计划条数。
///
/// 靠**每行自带的主机前缀**统计,而不是先前的 `── host ──` 分段:
/// 段头是"当前在哪台"的隐式状态,一旦输出里插入别的段落就会串行;
/// 前缀让每一行自足,解析不依赖顺序。
fn per_host_counts(out: &str) -> Vec<(String, usize)> {
    let mut order: Vec<String> = Vec::new();
    let mut counts: std::collections::BTreeMap<String, usize> = Default::default();
    for line in out.lines() {
        // `n1  + copy …` —— 前缀与正文之间是两个空格。
        let Some((host, body)) = line.split_once("  ") else {
            continue;
        };
        let host = host.trim();
        if host.is_empty() || host.contains(' ') {
            continue;
        }
        if !counts.contains_key(host) {
            order.push(host.to_string());
            counts.insert(host.to_string(), 0);
        }
        let t = body.trim_start();
        if t.starts_with("+ ") || t.starts_with("✓ ") || t.starts_with("~ ") {
            *counts.get_mut(host).unwrap() += 1;
        }
    }
    order.into_iter().map(|h| (h.clone(), counts[&h])).collect()
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
        &[
            "plan",
            "-f",
            bp.to_str().unwrap(),
            "-i",
            inv.to_str().unwrap(),
        ],
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
        &[
            "plan",
            "-f",
            bp.to_str().unwrap(),
            "-i",
            inv.to_str().unwrap(),
        ],
    ));
    // 按主机前缀归拢各自的行(输出不再有 `── host ──` 段头,
    // 每行自带主机名 —— 这样解析不依赖行的先后顺序)。
    let lines_of = |h: &str| -> String {
        out.lines()
            .filter(|l| {
                l.split_once("  ")
                    .map(|(p, _)| p.trim() == h)
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (a, b) = (lines_of("n11"), lines_of("n12"));
    assert!(!a.is_empty() && !b.is_empty(), "两台都该有输出:{out}");
    // 首台拿到 init,不该拿到 join;第二台反之。这是 HA 编排的地基。
    assert!(a.contains("/init"), "n11:{a}");
    assert!(!a.contains("/join"), "n11 不该有 join:{a}");
    assert!(b.contains("/join"), "n12:{b}");
    assert!(!b.contains("/init"), "n12 不该有 init:{b}");
    // worker 两个都不该有
    // 第三台(worker)既不 init 也不 join —— 选择器没选中它。
    let c = lines_of("w01");
    // 先确认真取到了它的行:名字写错会让 c 为空,而 `!空.contains(..)`
    // 恒成立 —— 那是一条看着绿、其实什么都没验的断言。
    assert!(!c.is_empty(), "没取到 w01 的输出行:{out}");
    assert!(!c.contains("/init") && !c.contains("/join"), "w01:{c}");
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
        &[
            "apply",
            "-f",
            bp.to_str().unwrap(),
            "-i",
            inv.to_str().unwrap(),
        ],
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
        vec![
            "sel-demo_n11.yaml",
            "sel-demo_n12.yaml",
            "sel-demo_w01.yaml"
        ]
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
            "name: bad\nresources:\n  - file: {{ path: \"{}/x\", state: directory }}\n    target: role.controlplna\n",
            d.path().join("target").display()
        ),
    )
    .unwrap();

    let o = crater(
        &home,
        &[
            "plan",
            "-f",
            p.to_str().unwrap(),
            "-i",
            inv.to_str().unwrap(),
        ],
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
            "name: g\nresources:\n  - file: {{ path: \"{}/x\", state: directory }}\n    target: role.controlplane\n",
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
    target: role.worker
  - file: {{ path: "{root}/nobody", state: directory }}
    target: host.n99
"#,
            root = root.display()
        ),
    )
    .unwrap();

    let o = crater(
        &home,
        &[
            "apply",
            "-f",
            p.to_str().unwrap(),
            "-i",
            inv.to_str().unwrap(),
        ],
    );
    assert!(o.status.success(), "{}", stdout(&o));
    assert!(root.join("worker-only").is_dir(), "选中的该建");
    assert!(!root.join("nobody").exists(), "没选中的绝不能建");
}
