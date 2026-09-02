//! UI 写入的 git 记录 —— **检测到才做**(D-119 §3 的第 2 步)。
//!
//! 在 UI 上改一台主机的口令,以前不留任何痕迹:谁、什么时候、把哪台机器的
//! 什么改成了什么,全都查不到。工作区本来就是"可 git 的期望态",那就顺手把
//! 每次写入记成一条提交 —— 审计链当场闭合,版本回滚白拿。
//!
//! **但 crater 二进制本身不依赖 git(D-122)**,这条不能破:一个为了记版本
//! 就强制装 git 的部署工具,与 agentless、静态单二进制、闭包一个文件带走
//! 一切的气质自相矛盾。所以这里全程是"探测到才做":
//!
//! - `git` 不在 PATH → 不记,启动横幅说一句
//! - 工作区不是 git 仓库 → 不记,启动横幅说一句
//! - 记的过程中 git 失败 → 写入**已经成功**,请求照常返回,只留一条 warn
//!
//! 提交只带**我们自己写的那几个路径**(`git add -- <paths>` +
//! `git commit --only -- <paths>`),而不是 `git commit -a`。这是刻意的反证:
//! 工作区里常有人手工改了一半的文件,甚至已经 `git add` 进暂存区的改动 ——
//! UI 的自动提交把它们一起卷走,是比"不记录"严重得多的事故。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, OnceLock};

/// 探测结果:`Some(仓库根)` = 记;`None` = 不记。启动时钉一次。
static REPO: OnceLock<Option<PathBuf>> = OnceLock::new();

/// git 的索引是仓库级单锁的:两个 handler 同时提交会撞 `index.lock`,
/// 症状是随机一次写入不留记录 —— 正是最难查的那类静默失效。
static SERIAL: Mutex<()> = Mutex::new(());

/// 作者钉成 crater,不跟当前 shell 用户走。
///
/// UI 是通过 HTTP 来的改动,起 `crater ui` 的那个 unix 账号既不是改动的发起人,
/// 也不该被记成发起人 —— 把它写进 `git log` 是一条**看起来可信的假记录**。
const AUTHOR: &str = "crater";
const AUTHOR_EMAIL: &str = "crater@localhost";

/// 启动时调一次,返回**要印进启动横幅的那一行**。
///
/// 返回字符串而不是自己 println:横幅的排版归 `ui::serve` 管,这里只负责
/// 说清"记不记、为什么"。
pub(crate) fn init(ws: &Path) -> String {
    let (repo, line) = probe(ws);
    let _ = REPO.set(repo);
    line
}

fn probe(ws: &Path) -> (Option<PathBuf>, String) {
    // 一、git 在不在。用 `--version` 而不是查 PATH 字符串:PATH 里躺着一个
    // 不可执行的 git 也叫"不在"。
    match Command::new("git").arg("--version").output() {
        Ok(o) if o.status.success() => {}
        _ => {
            return (
                None,
                "git 未检测到:UI 的改动不会被记录(crater 本身不依赖 git)".to_string(),
            )
        }
    }
    // 二、工作区在不在仓库里。`--show-toplevel` 顺手给出仓库根 —— 之后所有
    // git 调用都用它当 `-C`,免得工作区是子目录时路径对不上。
    let out = Command::new("git")
        .arg("-C")
        .arg(ws)
        .args(["rev-parse", "--show-toplevel"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let top = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if top.is_empty() {
                return (
                    None,
                    "工作区不是 git 仓库,改动不会被记录(在工作区里 git init 即可开启)"
                        .to_string(),
                );
            }
            let top = PathBuf::from(top);
            let line = format!("改动自动记入 git({},作者 {AUTHOR})", top.display());
            (Some(top), line)
        }
        _ => (
            None,
            "工作区不是 git 仓库,改动不会被记录(在工作区里 git init 即可开启)".to_string(),
        ),
    }
}

/// 记一笔。`paths` 是**这次真正动过的绝对路径**,`action` 是给人看的动作。
///
/// 全程吞错:写入这时候已经落盘了,git 记不上不该把请求变成失败 ——
/// 但也不能一声不吭(本仓库栽在静默失效上不止一次),所以失败留 warn。
pub(crate) fn record(paths: &[&Path], action: &str) {
    let Some(Some(top)) = REPO.get() else { return };
    // 备份与回收站不进版本库:`.bak` 是防手滑的临时物,`.crater-trash/`
    // 是删除的暂存区 —— 把它们提交进去,`git log` 立刻被噪声淹没。
    let wanted: Vec<&Path> = paths.iter().copied().filter(|p| worth_recording(p)).collect();
    if wanted.is_empty() {
        return;
    }
    let rels: Vec<String> = wanted
        .iter()
        .map(|p| p.strip_prefix(top).unwrap_or(p).display().to_string())
        .collect();
    let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if let Err(e) = commit(top, &wanted, &rels, action) {
        tracing::warn!("git 记录失败(文件已写入):{e}");
    }
}

/// 值不值得记。判据只有一条:它是不是**期望态的一部分**。
fn worth_recording(p: &Path) -> bool {
    if p.extension().and_then(|s| s.to_str()) == Some("bak") {
        return false;
    }
    !p.components().any(|c| {
        c.as_os_str()
            .to_str()
            .is_some_and(|s| s.starts_with(".crater-"))
    })
}

fn commit(top: &Path, paths: &[&Path], rels: &[String], action: &str) -> Result<(), String> {
    // `add` 要在 `commit --only` 之前:新建的文件 git 还不认识,
    // 直接 commit 会是 "pathspec did not match"。删除也走 add(它记录移除)。
    let add = git(top).arg("add").arg("--").args(paths).output();
    match add {
        // add 失败最常见的原因是路径被 .gitignore 挡住 —— 那是配置意图,
        // 不是故障,但仍要说出来,否则人会以为"记录功能坏了"。
        Ok(o) if !o.status.success() => {
            return Err(format!(
                "git add:{}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))
        }
        Err(e) => return Err(format!("git add:{e}")),
        _ => {}
    }
    let out = git(top)
        .arg("commit")
        // `--only` + 路径 = 只提交这几个路径的工作区内容,暂存区里别人的
        // 改动原样留着。这就是那条反证的实现。
        .arg("--only")
        // 跳过 pre-commit 钩子:这是无人值守的机器提交,一个交互式或耗时的
        // 钩子会把 HTTP 请求线程挂在那里,而"UI 卡住"远比"少一条记录"严重。
        .arg("--no-verify")
        .arg("-q")
        .arg("-m")
        .arg(message(rels, action))
        .arg("--")
        .args(paths)
        .output()
        .map_err(|e| format!("git commit:{e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let both = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // 内容没变(保存了一份一模一样的文件)时 git 也返回非零 —— 那不是失败,
    // 报成失败等于每次原样保存都在日志里制造一条假警报。
    if both.contains("nothing to commit")
        || both.contains("nothing added to commit")
        || both.contains("no changes added to commit")
    {
        return Ok(());
    }
    Err(format!("git commit:{}", both.trim()))
}

fn git(top: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(top);
    // 用 `-c` 而不是改仓库配置:既盖住"作者=当前 shell 用户",也不要求
    // 工作区先配过 user.name(裸机上 git commit 会因此直接失败)。
    c.arg("-c").arg(format!("user.name={AUTHOR}"));
    c.arg("-c").arg(format!("user.email={AUTHOR_EMAIL}"));
    // 签名要么弹密码框、要么直接失败 —— 无人值守的自动提交上都不能要。
    c.arg("-c").arg("commit.gpgsign=false");
    c
}

/// 提交信息。**首行就要说清文件 + 动作** —— `git log --oneline` 只看得到首行,
/// 而"改了什么"正是这条记录存在的唯一理由。
fn message(rels: &[String], action: &str) -> String {
    let head = match rels.len() {
        1 => format!("ui: {action} {}", rels[0]),
        n => format!("ui: {action} {}(共 {n} 个文件)", rels[0]),
    };
    let body: Vec<String> = rels.iter().map(|r| format!("  {r}")).collect();
    format!("{head}\n\n经 crater UI 写入,自动记录。涉及文件:\n{}\n", body.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backups_and_trash_never_enter_the_history() {
        // 反证:`.bak` 与回收站是 UI 自己的临时物,不是期望态。
        assert!(worth_recording(Path::new("/ws/site.yaml")));
        assert!(!worth_recording(Path::new("/ws/site.yaml.bak")));
        assert!(!worth_recording(Path::new("/ws/.crater-trash/site.yaml.17")));
    }

    #[test]
    fn the_first_line_names_the_file_and_the_action() {
        // `git log --oneline` 只显示首行 —— 文件名掉到正文里等于没记。
        let m = message(&["lab.inventory.yaml".into()], "改主机 n1");
        assert_eq!(m.lines().next().unwrap(), "ui: 改主机 n1 lab.inventory.yaml");
        let m2 = message(&["a.yaml".into(), "b.yaml".into()], "改名");
        assert!(m2.lines().next().unwrap().contains("共 2 个文件"), "{m2}");
        assert!(m2.contains("  b.yaml"), "{m2}");
    }
}
