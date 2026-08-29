//! 结构化执行事件流(NDJSON)——"执行呈现"的机器可读出口。
//!
//! 通道用环境变量 `CRATER_EVENTS=<path>` 打开:这是 UI→CLI 的机器契约,
//! 不是给人的旗标 —— 人看 stdout 的叙事,机器读这份事件流,同源不同面。
//! 没设环境变量时,`emit` 是一次锁探测的空操作,叙事路径零负担。

use std::io::Write;
use std::sync::Mutex;

static SINK: Mutex<Option<std::fs::File>> = Mutex::new(None);

/// 进程启动时调一次;通道打不开就静默作罢(事件流是增益,不是依赖)。
pub fn init_from_env() {
    let Ok(p) = std::env::var("CRATER_EVENTS") else { return };
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        if let Ok(mut g) = SINK.lock() {
            *g = Some(f);
        }
    }
}

/// 发一条事件,自动盖时间戳。**逐条 flush**:崩掉那一刻之前的事件必须
/// 已经落盘;可能断尾的最后一行由读侧丢弃(只解析到最后一个换行)。
pub fn emit(mut v: serde_json::Value) {
    let Ok(mut g) = SINK.lock() else { return };
    let Some(f) = g.as_mut() else { return };
    if let Some(o) = v.as_object_mut() {
        o.insert(
            "ts".into(),
            serde_json::json!(std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)),
        );
    }
    let _ = writeln!(f, "{v}");
    let _ = f.flush();
}
