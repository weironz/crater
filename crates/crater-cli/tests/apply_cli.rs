//! `crater apply -f <blueprint>` 走新 IR 管线的端到端契约。
//!
//! 用真实文件系统当目标:先 apply、再复查磁盘、再重跑验幂等 —— 这三步是
//! 「对账引擎」最基本的承诺,单测替代不了。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn crater(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crater"))
        .args(args)
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
name: apply-demo
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

#[cfg(unix)]
fn mode_of(p: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn apply_creates_reality_then_reruns_clean() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);
    let bp = bp.to_str().unwrap();

    // 1) 第一次:全部创建
    let first = crater(&["apply", "-f", bp]);
    assert!(first.status.success(), "{}", stdout(&first));
    assert!(
        stdout(&first).contains("changed=2 ok=0"),
        "{}",
        stdout(&first)
    );

    // 2) 磁盘上确实成了期望的样子(含权限 —— 不看权限就等于没验)
    assert!(root.join("data").is_dir());
    assert_eq!(
        std::fs::read_to_string(root.join("app.env")).unwrap(),
        "PORT=9000\n"
    );
    assert_eq!(mode_of(&root.join("data")), 0o750);
    assert_eq!(mode_of(&root.join("app.env")), 0o600);

    // 3) 立刻重跑:一次写入都不该再发生
    let second = crater(&["apply", "-f", bp]);
    let out = stdout(&second);
    assert!(out.contains("+0 ~0 -0 ✓2"), "{out}");
    assert!(out.contains("无需变更"), "{out}");
}

#[test]
fn apply_repairs_only_what_drifted() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);
    let bp = bp.to_str().unwrap();
    crater(&["apply", "-f", bp]);

    // 只把内容弄脏,权限和目录都不动
    std::fs::write(root.join("app.env"), "PORT=1\n").unwrap();

    let out = stdout(&crater(&["apply", "-f", bp]));
    assert!(out.contains("~1"), "只该修一处:{out}");
    assert!(out.contains("✓1"), "没漂的那项该保持不动:{out}");
    assert_eq!(
        std::fs::read_to_string(root.join("app.env")).unwrap(),
        "PORT=9000\n"
    );
}

#[test]
fn dry_run_apply_is_just_a_plan() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);

    let out = stdout(&crater(&["apply", "-f", bp.to_str().unwrap(), "--dry-run"]));
    assert!(out.contains("+2"), "{out}");
    assert!(!root.exists(), "--dry-run 写了东西:{out}");
}

#[test]
fn set_overrides_reach_the_written_file() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let bp = blueprint(d.path(), &root);

    crater(&["apply", "-f", bp.to_str().unwrap(), "--set", "port=9443"]);
    assert_eq!(
        std::fs::read_to_string(root.join("app.env")).unwrap(),
        "PORT=9443\n"
    );
}

#[test]
fn lint_errors_stop_apply_before_touching_anything() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let p = d.path().join("bad.yaml");
    std::fs::write(
        &p,
        format!(
            "name: bad\nresources:\n  - file: {{ path: \"{}/x\", state: directory }}\n  - servce: {{ name: y }}\n",
            root.display()
        ),
    )
    .unwrap();

    let o = crater(&["apply", "-f", p.to_str().unwrap()]);
    assert!(!o.status.success());
    assert!(!root.exists(), "lint 失败却已经动了目标");
    assert!(String::from_utf8_lossy(&o.stderr).contains("service"));
}

#[test]
fn a_material_backed_resource_fails_loudly_rather_than_pretending() {
    // 闭包解析尚未接入新管线 —— 必须响亮失败,不能悄悄写个空文件报成功。
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let p = d.path().join("mat.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: mat
materials:
  - name: bin
    file: "https://example.invalid/bin"
resources:
  - copy: {{ material: bin, dest: "{}/bin", mode: "0755" }}
"#,
            root.display()
        ),
    )
    .unwrap();

    let o = crater(&["apply", "-f", p.to_str().unwrap()]);
    assert!(!o.status.success(), "{}", stdout(&o));
    assert!(!root.join("bin").exists(), "不该留下半成品");
}

// ---------------------------------------------------------------- 物料闭包

/// 用 `file://` URL 当"上游" —— 测试不依赖外网,但走的是与 https 完全相同的路径。
#[test]
fn material_backed_copy_downloads_selects_variant_and_verifies_digest() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let upstream = d.path().join("upstream.bin");
    std::fs::write(&upstream, "hello-material").unwrap();
    let digest = sha256_of("hello-material");

    let arch = std::process::Command::new("uname")
        .arg("-m")
        .output()
        .unwrap();
    let arch = String::from_utf8_lossy(&arch.stdout).trim().to_string();
    let this_arch = match arch.as_str() {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        other => other,
    }
    .to_string();

    let p = d.path().join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: mat
materials:
  - name: tool
    file: "file://{up}"
    sha256: "{digest}"
    when: substrate.arch == '{this_arch}'
  - name: tool
    file: "file:///nonexistent-for-other-arch"
    when: substrate.arch != '{this_arch}'
resources:
  - copy: {{ material: tool, dest: "{root}/tool", mode: "0755" }}
"#,
            up = upstream.display(),
            root = root.display(),
        ),
    )
    .unwrap();

    let o = crater(&["apply", "-f", p.to_str().unwrap()]);
    assert!(o.status.success(), "{}", stdout(&o));
    // 变体按**目标机实际架构**选出,不是作者猜的
    assert!(
        stdout(&o).contains(&"upstream.bin".to_string()),
        "闭包清单要报出来:{}",
        stdout(&o)
    );
    assert_eq!(
        std::fs::read_to_string(root.join("tool")).unwrap(),
        "hello-material"
    );
    assert_eq!(mode_of(&root.join("tool")), 0o755);

    // 幂等:内容寻址一致 → 第二次零变更
    let again = stdout(&crater(&["apply", "-f", p.to_str().unwrap()]));
    assert!(again.contains("✓1"), "{again}");
}

#[test]
fn a_wrong_digest_aborts_and_leaves_nothing_behind() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let upstream = d.path().join("upstream.bin");
    std::fs::write(&upstream, "real-content").unwrap();

    let p = d.path().join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: mat
materials:
  - name: tool
    file: "file://{up}"
    sha256: "0000000000000000000000000000000000000000000000000000000000000000"
resources:
  - copy: {{ material: tool, dest: "{root}/tool" }}
"#,
            up = upstream.display(),
            root = root.display(),
        ),
    )
    .unwrap();

    let o = crater(&["apply", "-f", p.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("摘要不符"), "{err}");
    // 内容寻址是离线可信的根 —— 校验失败必须不留半成品。
    assert!(!root.join("tool").exists(), "留下了未经校验的文件");
}

#[test]
fn an_architecture_the_closure_does_not_cover_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("target");
    let p = d.path().join("bp.yaml");
    std::fs::write(
        &p,
        format!(
            r#"
name: mat
materials:
  - name: tool
    file: "file:///whatever"
    when: substrate.arch == 's390x'
resources:
  - copy: {{ material: tool, dest: "{}/tool" }}
"#,
            root.display()
        ),
    )
    .unwrap();

    let o = crater(&["apply", "-f", p.to_str().unwrap()]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("拒绝装半套"), "{err}");
    assert!(err.contains("arch="), "报错要说清这台机器长什么样:{err}");
}

fn sha256_of(s: &str) -> String {
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("printf '%s' '{s}' | sha256sum | cut -d' ' -f1"))
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}
