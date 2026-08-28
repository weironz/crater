//! `crater verify` 的契约:**没部署过 ≠ 漂了**。
//!
//! 这是引入状态记录的全部理由 —— 没有记录时,一个"全是 +"的 plan 与一台真漂了的
//! 机器长得一模一样,verify 只能靠猜。这些测试守住这个区分。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

/// 每个测试独享一个 HOME,记录互不干扰。
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

fn blueprint(dir: &Path, root: &Path) -> PathBuf {
    let p = dir.join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: verify-demo
version: "1.0"
resources:
  - file: {{ path: "{root}/data", state: directory, mode: "0750" }}
  - copy: {{ dest: "{root}/app.env", mode: "0600", content: "PORT=9000\n" }}
"#,
            root = root.display()
        ),
    )
    .unwrap();
    p
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn never_deployed_is_reported_as_such_and_does_not_fail() {
    // 反面就是"verify 天天报警":从没部署过的东西不该被算成漂移。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let bp = blueprint(d.path(), &d.path().join("target"));

    let o = crater(&home, &["verify", "-f", bp.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stdout(&o));
    assert!(stdout(&o).contains("未部署过"), "{}", stdout(&o));
}

#[test]
fn after_apply_verify_reports_in_sync() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);
    let bp = bp.to_str().unwrap();

    assert!(crater(&home, &["apply", "-f", bp]).status.success());
    let o = crater(&home, &["verify", "-f", bp]);
    assert!(o.status.success(), "{}", stdout(&o));
    assert!(stdout(&o).contains("现实符合期望"), "{}", stdout(&o));
}

#[test]
fn drift_after_a_successful_deploy_is_detected_and_exits_nonzero() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);
    let bp = bp.to_str().unwrap();

    crater(&home, &["apply", "-f", bp]);
    set_mode(&root.join("data"), 0o777); // 有人手改了权限

    let o = crater(&home, &["verify", "-f", bp]);
    assert!(!o.status.success(), "漂移必须非零退出(CI/定时任务据此告警)");
    let out = stdout(&o);
    assert!(out.contains("检测到漂移"), "{out}");
    assert!(out.contains("mode: 777 → 0750"), "要指到字段:{out}");
    assert!(out.contains("漂移 "), "已知资源应标为漂移:{out}");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("未通过核对"), "verify 的失败不是'执行失败':{err}");
}

#[test]
fn a_newly_added_resource_is_labelled_differently_from_drift() {
    // blueprint 长出新资源 ≠ 目标机漂了。报告要分得清,否则没法归因。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);
    crater(&home, &["apply", "-f", bp.to_str().unwrap()]);

    std::fs::write(
        &bp,
        format!(
            r#"
name: verify-demo
version: "1.0"
resources:
  - file: {{ path: "{root}/data", state: directory, mode: "0750" }}
  - copy: {{ dest: "{root}/app.env", mode: "0600", content: "PORT=9000\n" }}
  - file: {{ path: "{root}/extra", state: directory }}
"#,
            root = root.display()
        ),
    )
    .unwrap();

    let out = stdout(&crater(&home, &["verify", "-f", bp.to_str().unwrap()]));
    assert!(out.contains("新声明"), "{out}");
}

#[test]
fn verify_is_read_only() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);

    crater(&home, &["verify", "-f", bp.to_str().unwrap()]);
    assert!(!root.exists(), "verify 写了东西");
}

#[test]
fn unknowns_prevent_a_green_verdict() {
    // 有说不清的项时宣称"一切正常"是假的安心 —— 宁可促使人去看。
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let p = d.path().join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            "name: unk\nresources:\n  - file: {{ path: \"{}/d\", state: directory }}\n  - shell: \"true\"\n",
            root.display()
        ),
    )
    .unwrap();
    let bp = p.to_str().unwrap();

    crater(&home, &["apply", "-f", bp]);
    let o = crater(&home, &["verify", "-f", bp]);
    let out = stdout(&o);
    assert!(out.contains("无法核对"), "{out}");
    assert!(!o.status.success(), "说不清就不能给绿灯");
}

#[test]
fn the_record_is_human_readable_and_survives_reapply() {
    let d = tempfile::tempdir().unwrap();
    let home = d.path().join("home");
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);
    let bp = bp.to_str().unwrap();

    crater(&home, &["apply", "-f", bp]);
    let dir = home.join(".crater").join("state");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
    assert_eq!(files.len(), 1, "一次部署一条记录");
    let text = std::fs::read_to_string(files[0].path()).unwrap();
    assert!(text.contains("blueprint: verify-demo"), "{text}");
    assert!(text.contains("version: '1.0'"), "{text}");
    assert!(text.contains("observed:"), "记录的是**现实**,不是意图:{text}");

    // 重复 apply 不该产生第二条记录
    crater(&home, &["apply", "-f", bp]);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}
