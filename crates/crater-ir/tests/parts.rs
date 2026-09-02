//! A1 约定式分节的契约。
//!
//! 核心不变式:**合并结果与写在一个文件里等价**。这是它与 `include` 的分界线 ——
//! 拒绝的是 Ansible 那种"参数化、条件化、任意路径"的三级跳读,不是拒绝多文件。

use crater_ir::parse::{blueprint_from_path, blueprint_from_str};

fn write(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, body).unwrap();
    p
}

const ROOT: &str = r#"
name: demo
parts: [procedures]
resources:
  - file: { path: /data, state: directory }
"#;

const PROCS: &str = r#"
boot:
  steps:
    - shell: { cmd: "true", check: "test -f /x" }
"#;

#[test]
fn an_externalised_section_is_merged_back() {
    let d = tempfile::tempdir().unwrap();
    let root = write(d.path(), "demo.blueprint.yaml", ROOT);
    write(d.path(), "demo.procedures.yaml", PROCS);

    let bp = blueprint_from_path(&root).unwrap();
    assert_eq!(bp.resources.len(), 1);
    assert!(
        bp.procedures.contains_key("boot"),
        "外置的 procedures 应已并入"
    );
}

#[test]
fn splitting_is_byte_for_byte_equivalent_to_a_single_file() {
    // 这是整个机制的地基:拆开与不拆,引擎看到的东西必须一模一样。
    let d = tempfile::tempdir().unwrap();
    let root = write(d.path(), "demo.blueprint.yaml", ROOT);
    write(d.path(), "demo.procedures.yaml", PROCS);
    let split = blueprint_from_path(&root).unwrap();

    let single = blueprint_from_str(
        r#"
name: demo
resources:
  - file: { path: /data, state: directory }
procedures:
  boot:
    steps:
      - shell: { cmd: "true", check: "test -f /x" }
"#,
    )
    .unwrap();

    assert_eq!(split.name, single.name);
    assert_eq!(split.resources.len(), single.resources.len());
    assert_eq!(
        split.procedures["boot"].steps.len(),
        single.procedures["boot"].steps.len()
    );
}

#[test]
fn a_declared_part_that_is_missing_names_the_expected_filename() {
    let d = tempfile::tempdir().unwrap();
    let root = write(d.path(), "demo.blueprint.yaml", ROOT);
    let err = blueprint_from_path(&root).unwrap_err().to_string();
    assert!(err.contains("E120"), "{err}");
    assert!(
        err.contains("demo.procedures.yaml"),
        "要说清该叫什么名字:{err}"
    );
}

#[test]
fn a_ghost_part_file_is_refused_rather_than_silently_ignored() {
    // 目录里有 demo.types.yaml 却没声明 —— 静默不生效是最难查的一类问题。
    let d = tempfile::tempdir().unwrap();
    let root = write(d.path(), "demo.blueprint.yaml", ROOT);
    write(d.path(), "demo.procedures.yaml", PROCS);
    write(
        d.path(),
        "demo.types.yaml",
        "thing:\n  observe: { cmd: x }\n",
    );

    let err = blueprint_from_path(&root).unwrap_err().to_string();
    assert!(err.contains("E122"), "{err}");
    assert!(err.contains("不会生效"), "{err}");
}

#[test]
fn defining_a_section_twice_is_refused() {
    // 内联与外置都写了,谁赢都是猜。
    let d = tempfile::tempdir().unwrap();
    let root = write(
        d.path(),
        "demo.blueprint.yaml",
        "name: demo\nparts: [procedures]\nprocedures:\n  inline:\n    steps: []\n",
    );
    write(d.path(), "demo.procedures.yaml", PROCS);
    let err = blueprint_from_path(&root).unwrap_err().to_string();
    assert!(err.contains("E121") && err.contains("二选一"), "{err}");
}

#[test]
fn parts_cannot_nest() {
    let d = tempfile::tempdir().unwrap();
    let root = write(d.path(), "demo.blueprint.yaml", ROOT);
    write(
        d.path(),
        "demo.procedures.yaml",
        "parts: [types]\nboot:\n  steps: []\n",
    );
    let err = blueprint_from_path(&root).unwrap_err().to_string();
    assert!(err.contains("外置只有一层"), "{err}");
}

#[test]
fn only_whole_top_level_sections_may_be_externalised() {
    let d = tempfile::tempdir().unwrap();
    let root = write(
        d.path(),
        "demo.blueprint.yaml",
        "name: demo\nparts: [params]\n",
    );
    let err = blueprint_from_path(&root).unwrap_err().to_string();
    assert!(err.contains("不能外置 `params`"), "{err}");
    assert!(err.contains("可外置:"), "要列出可外置的节:{err}");
}

#[test]
fn a_misspelled_section_gets_a_suggestion() {
    let d = tempfile::tempdir().unwrap();
    let root = write(
        d.path(),
        "demo.blueprint.yaml",
        "name: demo\nparts: [procedure]\n",
    );
    let err = blueprint_from_path(&root).unwrap_err().to_string();
    assert!(err.contains("是不是想写 `procedures`"), "{err}");
}

#[test]
fn a_plain_single_file_blueprint_still_loads_from_path() {
    let d = tempfile::tempdir().unwrap();
    let root = write(
        d.path(),
        "solo.yaml",
        "name: solo\nresources:\n  - file: { path: /x, state: directory }\n",
    );
    let bp = blueprint_from_path(&root).unwrap();
    assert_eq!(bp.name, "solo");
}
