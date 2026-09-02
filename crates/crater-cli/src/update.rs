//! `crater update` —— 把自己换成最新版。
//!
//! 与 `scripts/install.sh` 是**同一套规矩**,只是入口不同:同样从 GitHub
//! Release 取 musl 静态包、同样按 `SHA256SUMS` 核对、同样原子替换。规矩写两遍
//! 迟早会漂,所以改一处就要改两处 —— 这一点在 `docs/install.md` 里写明了。
//!
//! 三条与安装脚本相同的纪律:
//!
//! - **摘要必须核对,没有关闭的开关。** 自更新是"用网上的字节替换正在运行的
//!   自己",跳过校验等于把信道当成可信的。
//! - **原子替换。** 先写同目录的临时文件再 rename —— 中途断电/断网留下的是
//!   旧的那个,不是半个。跨文件系统的 rename 会失败,所以临时文件必须是兄弟。
//! - **失败就退,不"尽力而为"。** 换到一半的 CLI 比没换更难查。
//!
//! 一条它独有的:**装在哪就换哪**(`std::env::current_exe`),不猜 `PATH`。
//! 用户可能同时有 `~/.local/bin/crater` 与 `/usr/local/bin/crater`,换错一个
//! 的表现是"更新成功了但版本没变"。

use std::path::Path;

use anyhow::{bail, Context as _, Result};

use crate::say;

const REPO: &str = "weironz/crater";

/// `crater update [--version vX.Y.Z] [--check]`
pub async fn run(version: Option<String>, check_only: bool) -> Result<()> {
    let current = env!("CARGO_PKG_VERSION");
    let target = musl_target()?;

    let want = match version {
        Some(v) => v,
        None => latest_tag().await?,
    };
    // tag 是 `v0.2.0`,`--version` 报的是 `0.2.0` —— 比之前先对齐,否则每次
    // 都会判成"有新版本"。
    let want_bare = want.trim_start_matches('v');

    if want_bare == current {
        say!("已经是最新版 {current}。");
        return Ok(());
    }
    say!("{current} → {want_bare}");
    if check_only {
        say!("(--check:只看不换)");
        return Ok(());
    }

    // 装在哪就换哪。解析符号链接:有人把 `~/.local/bin/crater` 链到别处,
    // 换掉链接本身会留下一个悬空的链。
    let exe = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .context("找不到当前可执行文件的位置")?;
    let dir = exe.parent().unwrap_or(Path::new(".")).to_path_buf();

    let base = format!("https://github.com/{REPO}/releases/download/{want}");
    let tarball = format!("crater-{target}.tar.gz");

    let bytes = fetch(&format!("{base}/{tarball}")).await?;
    let sums = String::from_utf8(fetch(&format!("{base}/SHA256SUMS")).await?)
        .context("SHA256SUMS 不是文本")?;

    // 只核对**要装的那一个**:SHA256SUMS 里还列着别的架构,整份校验会因为
    // "那些文件不在本地"而失败,而那种失败会被当成噪音、然后被学会忽略。
    let want_sum = sums
        .lines()
        .find(|l| l.ends_with(&tarball))
        .and_then(|l| l.split_whitespace().next())
        .ok_or_else(|| anyhow::anyhow!("SHA256SUMS 里没有 {tarball} —— 这一版可能没发这个架构"))?;
    let got_sum = crater_core::bundle::sha256_hex(&bytes);
    if want_sum != got_sum {
        bail!(
            "摘要不符!\n  期望 {want_sum}\n  实得 {got_sum}\n\
             下载物被动过,或者传输出错。什么都没有替换。"
        );
    }
    say!("  摘要校验通过");

    let new_bin = extract(&bytes)?;

    // 先写兄弟文件再 rename。Linux 允许替换**正在运行**的可执行文件(inode
    // 还被进程持有),所以这条在自更新场景下成立;而直接覆盖会撞 ETXTBSY。
    let tmp = dir.join(".crater.update");
    std::fs::write(&tmp, &new_bin)
        .with_context(|| format!("写不了 {}(需要 sudo?)", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }
    if let Err(e) = std::fs::rename(&tmp, &exe) {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow::Error::from(e))
            .with_context(|| format!("替换 {} 失败(需要 sudo?)", exe.display()));
    }

    say!("已更新 → {}", exe.display());
    // 换完立刻问它自己是谁 —— 不问的话,"换成了一个跑不起来的二进制"要等到
    // 下一次用才发现。
    match std::process::Command::new(&exe).arg("--version").output() {
        Ok(o) if o.status.success() => say!("  {}", String::from_utf8_lossy(&o.stdout).trim()),
        _ => bail!("换上了但跑不起来 —— 用 scripts/install.sh 重装一次"),
    }
    Ok(())
}

/// 本机对应哪个发布产物。只发 musl:一份二进制在 glibc 与 musl 上都能跑。
fn musl_target() -> Result<&'static str> {
    if !cfg!(target_os = "linux") {
        bail!("`crater update` 目前只支持 Linux —— 别的平台请从源码构建");
    }
    Ok(match std::env::consts::ARCH {
        "x86_64" => "x86_64-unknown-linux-musl",
        "aarch64" => "aarch64-unknown-linux-musl",
        other => bail!("不支持的架构 {other}(有 x86_64 与 aarch64)"),
    })
}

async fn latest_tag() -> Result<String> {
    let body = fetch(&format!(
        "https://api.github.com/repos/{REPO}/releases/latest"
    ))
    .await?;
    let v: serde_json::Value =
        serde_json::from_slice(&body).context("GitHub API 返回的不是 JSON")?;
    v["tag_name"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("问不到最新版本(GitHub API 限流?可用 --version 指定)"))
}

async fn fetch(url: &str) -> Result<Vec<u8>> {
    crater_core::source::fetch(url)
        .await
        .with_context(|| format!("下载失败:{url}"))
}

/// 从 tar.gz 里取出 `crater` 那个成员。
fn extract(gz: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(gz));
    for e in ar.entries().context("解包失败")? {
        let mut e = e?;
        let is_bin = e
            .path()
            .map(|p| p.file_name().map(|n| n == "crater").unwrap_or(false))
            .unwrap_or(false);
        if is_bin {
            let mut buf = Vec::new();
            e.read_to_end(&mut buf)?;
            return Ok(buf);
        }
    }
    bail!("包里没有 crater 可执行文件")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 版本比对必须先剥掉 tag 的 `v` —— 不剥的话 `v0.2.0` 与 `0.2.0` 永远
    /// 不等,每次 `update` 都会去下载一遍同一个版本。
    #[test]
    fn a_tag_and_a_crate_version_compare_after_stripping_v() {
        assert_eq!("v0.2.0".trim_start_matches('v'), "0.2.0");
        assert_eq!("0.2.0".trim_start_matches('v'), "0.2.0");
    }

    /// 只认名为 `crater` 的成员:包里将来可能多出 README/LICENSE,按"第一个
    /// 文件"取会取错。
    #[test]
    fn extract_picks_the_binary_not_the_first_entry() {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut tar_buf = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_buf);
            for (name, body) in [("README.md", &b"readme"[..]), ("crater", &b"ELF"[..])] {
                let mut h = tar::Header::new_gnu();
                h.set_size(body.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                b.append_data(&mut h, name, body).unwrap();
            }
            b.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_buf).unwrap();
        assert_eq!(extract(&gz.finish().unwrap()).unwrap(), b"ELF");
    }

    /// 包里没有 `crater` 时必须报错,不能返回空字节 —— 后者会把一个 0 字节
    /// 的"可执行文件"写到你的 PATH 上。
    #[test]
    fn a_tarball_without_the_binary_is_an_error() {
        use flate2::write::GzEncoder;
        use std::io::Write;
        let mut tar_buf = Vec::new();
        {
            let mut b = tar::Builder::new(&mut tar_buf);
            let mut h = tar::Header::new_gnu();
            h.set_size(6);
            h.set_mode(0o644);
            h.set_cksum();
            b.append_data(&mut h, "README.md", &b"readme"[..]).unwrap();
            b.finish().unwrap();
        }
        let mut gz = GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar_buf).unwrap();
        assert!(extract(&gz.finish().unwrap()).is_err());
    }
}
