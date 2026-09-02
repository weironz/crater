//! `crater pkg index` 的命令行契约 —— 专盯**会把索引写没**的那几条路。
//!
//! 索引是发布产物:`--merge` 的输入就是上一次的输出。一旦某条路径把"读不到
//! 历史"当成"没有历史",命令会成功、文件会写出、终端看不出任何异样,而整份
//! 版本历史已经没了 —— 要等到有人装旧版本时才发现。所以这里每条用例都同时
//! 断言**退出码**和**文件字节没被动过**。
//!
//! 这些用例都不碰网络:坏索引在任何一次 registry 往返之前就该被拦下。
//! 真 registry 上的正向路径(a 与 b 都在、新版本并进去老版本还在)在
//! repo.rs 的单测 + issue #4 的手工验收里。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn index(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_crater"))
        .args(["index"])
        .args(args)
        .current_dir(dir)
        // 别碰开发机上真的 ~/.crater。
        .env("CRATER_HOME", dir.join("home"))
        .output()
        .expect("run crater pkg index")
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// 一份能用的索引,以及它的字节 —— 用来断言"没被动过"。
fn seed(dir: &Path) -> (PathBuf, Vec<u8>) {
    let p = dir.join("index.yaml");
    let body = "\
apiVersion: crater.pkg/v1
generated: 2026-09-02T00:00:00Z
entries:
  yq:
  - version: 4.44.3
    reference: reg/ns/yq:4.44.3
  - version: 4.40.5
    reference: reg/ns/yq:4.40.5
";
    std::fs::write(&p, body).unwrap();
    (p, body.as_bytes().to_vec())
}

/// **反证**:`--merge` 指向一份坏索引必须报错,而且不许碰原文件。
///
/// 默默当成空索引重写 = 一条命令删掉整份发布历史,零症状。
#[test]
fn merge_onto_a_broken_index_fails_and_leaves_it_alone() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("index.yaml");
    // 写了一半被打断的样子 —— YAML 截在半路。
    let broken =
        "apiVersion: crater.pkg/v1\ngenerated: 2026-09-02T00:00:00Z\nentries:\n  yq:\n  - vers";
    std::fs::write(&p, broken).unwrap();

    let o = index(
        d.path(),
        &["-o", "index.yaml", "--merge", "oci://127.0.0.1:1/ns/x:1"],
    );

    assert!(!o.status.success(), "坏索引却退出 0:\n{}", stderr(&o));
    assert!(
        stderr(&o).contains("不是 crater 索引"),
        "报错要说清是索引读不动,而不是别的:\n{}",
        stderr(&o)
    );
    assert_eq!(
        std::fs::read(&p).unwrap(),
        broken.as_bytes(),
        "原文件被动过了"
    );
}

/// **反证(真机上复现过的那条)**:截断恰好落在 `entries:` 之后的索引是
/// **合法 YAML** —— `entries` 成了 null,serde 收成空 map。
///
/// 于是 merge 从零开始、退出 0、把整份历史换成这一版,终端上和一次成功的
/// 增量发布长得一模一样。前一条用例里那种"截在半个 token 上"能被 YAML
/// 解析器挡住,这一条挡不住 —— 它得靠"空索引不是合法的 merge 底本"这条
/// 规矩才拦得下。
#[test]
fn merge_onto_an_index_truncated_at_entries_refuses_to_wipe_history() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("index.yaml");
    let cut = "apiVersion: crater.pkg/v1\ngenerated: 2026-09-02T00:00:00Z\nentries:\n";
    std::fs::write(&p, cut).unwrap();

    let o = index(
        d.path(),
        &["-o", "index.yaml", "--merge", "oci://127.0.0.1:1/ns/x:1"],
    );

    assert!(
        !o.status.success(),
        "零个包的索引被当成了合法底本:\n{}",
        stderr(&o)
    );
    assert!(stderr(&o).contains("零个包"), "{}", stderr(&o));
    assert_eq!(std::fs::read(&p).unwrap(), cut.as_bytes(), "原文件被动过了");
}

/// apiVersion 不认得,同理 —— 这是"旧版 crater 写的索引"那种坏法。
#[test]
fn merge_onto_a_foreign_api_version_fails_and_leaves_it_alone() {
    let d = tempfile::tempdir().unwrap();
    let (p, bytes) = seed(d.path());
    let swapped = String::from_utf8(bytes)
        .unwrap()
        .replace("crater.pkg/v1", "crater.pkg/v99");
    std::fs::write(&p, &swapped).unwrap();

    let o = index(
        d.path(),
        &["-o", "index.yaml", "--merge", "oci://127.0.0.1:1/ns/x:1"],
    );

    assert!(
        !o.status.success(),
        "apiVersion 不认得却退出 0:\n{}",
        stderr(&o)
    );
    assert!(stderr(&o).contains("apiVersion"), "{}", stderr(&o));
    assert_eq!(
        std::fs::read(&p).unwrap(),
        swapped.as_bytes(),
        "原文件被动过了"
    );
}

/// 不带 `--merge` 覆盖时,一个包都没收到就**拒绝写** —— 钉住现有行为。
///
/// 没有这道闸,一次网络抽风就能把线上索引换成一份空文件。
#[test]
fn an_empty_harvest_refuses_to_overwrite() {
    let d = tempfile::tempdir().unwrap();
    let (p, bytes) = seed(d.path());

    // 一个来源都不给 = 必然收不到任何包,而且不碰网络。
    let o = index(d.path(), &["-o", "index.yaml"]);

    assert!(!o.status.success(), "空收成却退出 0:\n{}", stderr(&o));
    assert!(stderr(&o).contains("一个包都没收进来"), "{}", stderr(&o));
    assert_eq!(std::fs::read(&p).unwrap(), bytes, "空收成把已有索引覆盖了");
}

/// `--merge` 指着一个**不存在**的文件时,得吭一声。
///
/// 后果与索引损坏一样(整份历史被这一版顶掉),区别只是它更常见:CI 里
/// `-o` 写错、或取上一版索引的那步没成。仍然放行是因为第一次发布必须能跑,
/// 但不能让它和一次成功的 merge 在终端上长得一模一样。
#[test]
fn merge_without_an_existing_index_says_so() {
    let d = tempfile::tempdir().unwrap();

    let o = index(d.path(), &["-o", "index.yaml", "--merge"]);

    let e = stderr(&o);
    assert!(
        e.contains("--merge") && e.contains("不存在"),
        "没提示 merge 目标不存在:\n{e}"
    );
}
