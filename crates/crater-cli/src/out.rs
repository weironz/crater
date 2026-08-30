//! 人读的终端输出:**每行自带主机名**,可选时间戳,可同时落盘。
//!
//! 此前部署管线是 53 处裸 `println!`:主机名只出现在段落头 `── n10 ──`,
//! 于是一行 `✓ copy /usr/local/bin/yq` 脱离上下文就不知道是哪台机器 ——
//! 五台机器一起跑时要来回滚屏找段落头,粘贴出来的片段更是无从判断。
//! ansible 的 `changed: [n10]` 之所以耐用,正是因为每行自足、可 grep。
//!
//! 与 [`crate::events`] 分工:那边是给机器读的 NDJSON(UI 的实况矩阵靠它),
//! 这边是给人读的文本。两者同源不同面,谁也不该迁就谁的格式。

use std::io::Write;
use std::sync::Mutex;

struct Sink {
    /// `--log-file`:终端有什么,它就有什么,外加时间戳。
    file: Option<std::fs::File>,
    /// `--timestamps`:终端也带上时间。默认关 —— 单机短命令带时间戳是噪音,
    /// 但十分钟的 k8s apply 没有时间戳就查不出哪一步慢。
    stamp: bool,
    /// 当前主机段。空 = 不在任何主机上下文里(全局消息)。
    host: String,
    /// 主机名列宽:各行的正文要对齐,否则加了前缀反而更难扫。
    width: usize,
}

fn sink() -> &'static Mutex<Sink> {
    static S: std::sync::OnceLock<Mutex<Sink>> = std::sync::OnceLock::new();
    S.get_or_init(|| {
        Mutex::new(Sink { file: None, stamp: false, host: String::new(), width: 0 })
    })
}

/// 进程启动时调一次。日志文件打不开就静默作罢 —— 落盘是增益,不该拦住部署。
pub fn init(log_file: Option<&std::path::Path>, stamp: bool) {
    let f = log_file
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    if let Ok(mut s) = sink().lock() {
        s.file = f;
        s.stamp = stamp;
    }
}

/// 本轮要跑哪些主机 —— 用来定前缀列宽(名字长短不一时正文才对得齐)。
pub fn fleet(names: &[String]) {
    if let Ok(mut s) = sink().lock() {
        s.width = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    }
}

/// 进入某台主机的上下文;之后每一行都带它的名字。
pub fn enter(host: &str) {
    if let Ok(mut s) = sink().lock() {
        s.host = host.to_string();
        if s.width < host.chars().count() {
            s.width = host.chars().count();
        }
    }
}

/// 离开主机上下文(机群级消息不该挂在最后一台机器名下)。
pub fn leave() {
    if let Ok(mut s) = sink().lock() {
        s.host.clear();
    }
}

fn hhmmss() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let d = secs % 86_400;
    format!("{:02}:{:02}:{:02}", d / 3600, (d % 3600) / 60, d % 60)
}

/// 输出一行。空串照样输出(段落之间的空行是可读性的一部分)。
pub fn line(msg: &str) {
    let Ok(s) = sink().lock() else {
        println!("{msg}");
        return;
    };
    // 空行不加前缀 —— 一列孤零零的主机名比空行更碍眼。
    let prefixed = if msg.is_empty() || s.host.is_empty() {
        msg.to_string()
    } else {
        format!("{:<w$}  {msg}", s.host, w = s.width)
    };
    let term = if s.stamp && !msg.is_empty() {
        format!("{} {prefixed}", hhmmss())
    } else {
        prefixed.clone()
    };
    println!("{term}");
    // 日志文件**恒带时间戳**:事后翻日志时"这步花了多久"是第一个问题。
    if let Some(mut f) = s.file.as_ref() {
        let _ = writeln!(f, "{} {prefixed}", hhmmss());
        let _ = f.flush();
    }
}

/// 错误行:走 stderr,但**同样带主机前缀、同样进日志**。
///
/// 一条 `执行失败 —— xxx` 不说是哪台机器,等于把定位工作原样退回给人;
/// 而错误恰恰是最需要事后翻日志的那类行。
pub fn err(msg: &str) {
    let Ok(s) = sink().lock() else {
        eprintln!("{msg}");
        return;
    };
    let prefixed = if msg.is_empty() || s.host.is_empty() {
        msg.to_string()
    } else {
        format!("{:<w$}  {msg}", s.host, w = s.width)
    };
    eprintln!("{}", if s.stamp && !msg.is_empty() {
        format!("{} {prefixed}", hhmmss())
    } else {
        prefixed.clone()
    });
    if let Some(mut f) = s.file.as_ref() {
        let _ = writeln!(f, "{} {prefixed}", hhmmss());
        let _ = f.flush();
    }
}

/// `say!("计划:{}", p.summary())` —— 用起来与 println! 一样。
#[macro_export]
macro_rules! say {
    () => { $crate::out::line("") };
    ($($arg:tt)*) => { $crate::out::line(&format!($($arg)*)) };
}

/// `oops!("执行失败 —— {e}")` —— stderr 版的 say!。
#[macro_export]
macro_rules! oops {
    ($($arg:tt)*) => { $crate::out::err(&format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_wall_clock_hhmmss() {
        let t = hhmmss();
        assert_eq!(t.len(), 8, "HH:MM:SS");
        assert_eq!(&t[2..3], ":");
        assert_eq!(&t[5..6], ":");
    }

    /// 列宽要按**字符数**算,不是字节数 —— 否则中文主机名会把对齐撑歪。
    #[test]
    fn width_counts_characters_not_bytes() {
        fleet(&["n1".to_string(), "控制面节点".to_string()]);
        let w = sink().lock().unwrap().width;
        assert_eq!(w, 5, "5 个汉字算 5 列,不是 15 字节");
    }
}
