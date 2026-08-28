//! `crater lint` 的命令行契约:退出码、目录扫描的宽容度、strict、JSON。
//!
//! 这些是 CI 会依赖的行为,比诊断内容本身更不能悄悄变(诊断内容的回归在
//! crater-ir 的 tests/diagnostics.rs)。

use std::path::PathBuf;
use std::process::{Command, Output};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn lint(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crater"))
        .arg("lint")
        .args(args)
        .current_dir(workspace())
        .output()
        .expect("run crater lint")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// 写一个临时 blueprint,返回它的路径(测试结束后随 tempdir 一起消失)。
fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let p = dir.path().join(name);
    std::fs::write(&p, body).unwrap();
    p
}

const GOOD: &str = r#"
name: good
params:
  port: { type: port, default: 9000 }
resources:
  - file: { path: /data, state: directory }
  - service: { name: app, state: started }
"#;

const BAD: &str = r#"
name: bad
resources:
  - servce: { name: app }
"#;

#[test]
fn clean_blueprints_exit_zero() {
    let o = lint(&["crates/crater-ir/tests/fixtures"]);
    assert!(o.status.success(), "{}", stdout(&o));
    let out = stdout(&o);
    assert!(out.contains("✓ rustfs"), "{out}");
    assert!(out.contains("✓ k8s-ha"), "{out}");
    assert!(out.contains("0 error"), "{out}");
}

#[test]
fn errors_exit_nonzero_and_point_at_a_line() {
    let d = tempfile::tempdir().unwrap();
    let p = write(&d, "bad.yaml", BAD);
    let o = lint(&[p.to_str().unwrap()]);
    assert!(!o.status.success(), "有 error 必须非零退出");
    let out = stdout(&o);
    assert!(out.contains("bad.yaml:4"), "诊断要带行号:{out}");
    assert!(out.contains("service"), "要给拼写建议:{out}");
}

#[test]
fn warnings_alone_pass_unless_strict() {
    // 裸 shell 无 check → warn,不阻断;--strict 才阻断(CI 用)。
    let d = tempfile::tempdir().unwrap();
    let p = write(
        &d,
        "warn.yaml",
        "name: w\nprocedures:\n  boot:\n    steps:\n      - shell: \"do-thing\"\n",
    );
    let path = p.to_str().unwrap();

    let o = lint(&[path]);
    assert!(o.status.success(), "warn 不该阻断:{}", stdout(&o));
    assert!(stdout(&o).contains("1 warn"));

    let o = lint(&[path, "--strict"]);
    assert!(!o.status.success(), "--strict 下 warn 必须阻断");
}

#[test]
fn directory_scan_skips_non_blueprints_but_still_reads_named_ones() {
    // 仓库里绝大多数 YAML 不是 blueprint(inventory / CI 配置 / manifest)。
    // 扫描时静默跳过,点名时照常解析并报错 —— 否则 `lint .` 全是噪音。
    let scan = lint(&["library/rustfs"]);
    assert!(scan.status.success(), "目录扫描不该被 inventory 绊倒:{}", stdout(&scan));
    assert!(!stdout(&scan).contains("解析失败"), "{}", stdout(&scan));

    let named = lint(&["library/rustfs/inventory.example.yaml"]);
    assert!(!named.status.success(), "点名的文件必须给答复");
    assert!(stdout(&named).contains("解析失败"));
}

#[test]
fn legacy_tasks_are_reported_as_pending_migration_not_as_errors() {
    let o = lint(&["library"]);
    let out = stdout(&o);
    assert!(o.status.success(), "旧 task 不是错:{out}");
    assert!(out.contains("旧版 task 格式"), "{out}");
    assert!(out.contains("旧版 task 跳过"), "汇总要有迁移待办计数:{out}");
}

#[test]
fn whole_repo_scan_is_clean() {
    // 这条是长期护栏:仓库里任何新写的 blueprint 一旦有 error,这里就红。
    let o = lint(&["."]);
    assert!(o.status.success(), "仓库扫描应零 error:\n{}", stdout(&o));
}

#[test]
fn json_output_is_machine_readable() {
    let o = lint(&["crates/crater-ir/tests/fixtures/rustfs.yaml", "--json"]);
    let out = stdout(&o);
    assert!(out.starts_with("{\"files\":["), "{out}");
    assert!(out.contains("\"status\":\"ok\""), "{out}");
    assert!(out.contains("\"blueprint\":\"rustfs\""), "{out}");
    assert!(!out.contains('✓'), "JSON 模式不该混入人类输出:{out}");
}

#[test]
fn json_output_carries_severity_and_line() {
    let d = tempfile::tempdir().unwrap();
    let p = write(&d, "bad.yaml", BAD);
    let o = lint(&[p.to_str().unwrap(), "--json"]);
    let out = stdout(&o);
    assert!(out.contains("\"severity\":\"error\""), "{out}");
    assert!(out.contains("\"line\":4"), "{out}");
}

#[test]
fn missing_path_fails_loudly() {
    let o = lint(&["no/such/dir"]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("路径不存在"), "{err}");
}

#[test]
fn good_blueprint_summary_counts_resources() {
    let d = tempfile::tempdir().unwrap();
    let p = write(&d, "good.yaml", GOOD);
    let o = lint(&[p.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stdout(&o));
    assert!(stdout(&o).contains("2 资源"), "{}", stdout(&o));
}
