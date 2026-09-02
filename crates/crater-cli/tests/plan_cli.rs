//! `crater plan` 走新 IR 管线时的命令行契约。
//!
//! 用真实文件系统当目标(LocalCtx),所以这些测试同时是**"plan 不写入"这条纪律的
//! 端到端证据**:跑完 plan 之后目录必须仍然不存在。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn plan(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crater"))
        .arg("plan")
        .args(args)
        .current_dir(workspace())
        .output()
        .expect("run crater plan")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// 写一份以 `root` 为数据目录的 blueprint。
fn blueprint(dir: &Path, root: &Path) -> PathBuf {
    let p = dir.join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: cli-demo
params:
  port: {{ type: port, default: 9000 }}
resources:
  - file: {{ path: "{root}/data", state: directory, mode: "0750" }}
  - copy:
      dest: "{root}/app.env"
      mode: "0600"
      content: "PORT=${{params.port}}\n"
"#,
            root = root.display()
        ),
    )
    .unwrap();
    p
}

#[test]
fn plan_on_a_blank_target_shows_creates_and_touches_nothing() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);

    let o = plan(&["-f", bp.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stdout(&o));
    let out = stdout(&o);
    assert!(out.contains("+2 ~0 -0 ✓0"), "{out}");
    assert!(
        !root.exists(),
        "plan 期发生了写入 —— 整个设计的地基就是这条不能破"
    );
}

#[test]
fn plan_reports_convergence_once_reality_matches() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);

    std::fs::create_dir_all(root.join("data")).unwrap();
    set_mode(&root.join("data"), 0o750);
    std::fs::write(root.join("app.env"), "PORT=9000\n").unwrap();
    set_mode(&root.join("app.env"), 0o600);

    let out = stdout(&plan(&["-f", bp.to_str().unwrap()]));
    assert!(out.contains("+0 ~0 -0 ✓2"), "{out}");
    assert!(out.contains("已是期望态"), "{out}");
}

#[test]
fn plan_pinpoints_drift_field_by_field() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);

    std::fs::create_dir_all(root.join("data")).unwrap();
    set_mode(&root.join("data"), 0o777); // 权限漂了
    std::fs::write(root.join("app.env"), "PORT=9000\n").unwrap();
    set_mode(&root.join("app.env"), 0o600); // 内容/权限都对

    let out = stdout(&plan(&["-f", bp.to_str().unwrap()]));
    assert!(out.contains("mode: 777 → 0750"), "要指到字段:{out}");
    assert!(out.contains("+0 ~1 -0 ✓1"), "只该报一处漂移:{out}");
}

#[test]
fn set_overrides_are_typed_and_change_rendered_content() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);

    std::fs::create_dir_all(root.join("data")).unwrap();
    set_mode(&root.join("data"), 0o750);
    std::fs::write(root.join("app.env"), "PORT=9000\n").unwrap();
    set_mode(&root.join("app.env"), 0o600);

    // 默认端口下无变更;换端口后只有内容那一项变。
    assert!(stdout(&plan(&["-f", bp.to_str().unwrap()])).contains("✓2"));
    let out = stdout(&plan(&["-f", bp.to_str().unwrap(), "--set", "port=9443"]));
    assert!(out.contains("~1"), "{out}");
    assert!(out.contains("content:"), "{out}");
}

#[test]
fn lint_errors_block_the_plan_before_any_probing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("bad.yaml");
    std::fs::write(&p, "name: bad\nresources:\n  - servce: { name: x }\n").unwrap();

    let o = plan(&["-f", p.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("service"), "该给拼写建议:{err}");
    assert!(err.contains("先修再 plan"), "{err}");
}

#[test]
fn a_legacy_task_file_gets_a_migration_note_not_a_parse_error() {
    // 旧 task 管线已删(D-151)。喂给 `plan` 一个旧 task 文件时,**不能**报
    // 新管线的解析失败(那会让人以为是自己写错了,然后去调字段);要报的是
    // "这个形状的输入整个不存在了,新的这么写"。
    //
    // 这条测试原来钉的是"分流不能把旧 task 错抓进新管线"—— 删掉管线之后
    // 它当然会失败。改成钉新契约:**给出路,而不只是拒绝**。
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("legacy.yaml");
    std::fs::write(
        &p,
        "name: legacy
actions:
  - shell: { cmd: \"true\" }\n",
    )
    .unwrap();

    let o = plan(&["-f", p.to_str().unwrap()]);
    assert!(!o.status.success(), "旧 task 不该被当成能跑的东西");
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("蓝图"), "要说清现在接受什么:{err}");
    assert!(err.contains("resources"), "要给迁移方向:{err}");
    assert!(err.contains("library/"), "要指出去哪找例子:{err}");
}

#[cfg(unix)]
fn set_mode(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
}
