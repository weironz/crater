//! [`Ctx`] 的两个实现:本机执行(真跑)与脚本化假执行(测试)。
//!
//! SSH / 自举 agent 的实现留在执行层(crater-core),本 crate 只依赖 trait ——
//! 这样五动词与 plan 推导可以在**零网络**下被完整测试,这正是"observe 必须只读"
//! 这条纪律能被机器验证的前提。

use std::sync::Mutex;
use std::collections::BTreeMap;
use std::process::Command;

use crate::verbs::Ctx;

/// 在本机执行 —— `crater plan --local` 与开发自测用。
pub struct LocalCtx;

impl LocalCtx {
    fn exec(cmd: &str) -> anyhow::Result<(i32, String)> {
        let out = Command::new("sh").arg("-c").arg(cmd).output()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.stderr.is_empty() {
            text.push_str(&String::from_utf8_lossy(&out.stderr));
        }
        Ok((out.status.code().unwrap_or(-1), text))
    }
}

impl Ctx for LocalCtx {
    fn probe(&self, cmd: &str) -> anyhow::Result<(i32, String)> {
        Self::exec(cmd)
    }
    fn run(&self, cmd: &str) -> anyhow::Result<(i32, String)> {
        Self::exec(cmd)
    }
    fn write_file(&self, path: &str, content: &str) -> anyhow::Result<()> {
        self.write_bytes(path, content.as_bytes())
    }
    fn write_bytes(&self, path: &str, content: &[u8]) -> anyhow::Result<()> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, content)?;
        Ok(())
    }
    fn place_material(&self, name: &str, dest: &str) -> anyhow::Result<()> {
        anyhow::bail!("本机上下文不解析物料(`{name}` → {dest});离线闭包由执行层提供")
    }
}

/// 脚本化的假目标:按"命令前缀 → (退出码, 输出)"应答,并**记录每一条命令**。
///
/// 记录是关键:测试据此断言 `observe` 期间一条写命令都没发出 —— plan 的可信度
/// 是本设计的核心卖点,不能只靠代码评审来守。
pub struct FakeCtx {
    responses: Vec<(String, (i32, String))>,
    // Mutex 而不是 RefCell:`Ctx` 要求 Send + Sync(并发调度会跨线程借它)。
    log: Mutex<Vec<Call>>,
    files: Mutex<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    Probe(String),
    Run(String),
    Write(String),
    Place(String, String),
}

impl Call {
    /// 是否是会改变目标的调用。
    pub fn is_write(&self) -> bool {
        !matches!(self, Call::Probe(_))
    }
    pub fn text(&self) -> &str {
        match self {
            Call::Probe(s) | Call::Run(s) | Call::Write(s) => s,
            Call::Place(_, d) => d,
        }
    }
}

impl FakeCtx {
    pub fn new() -> Self {
        FakeCtx {
            responses: Vec::new(),
            log: Mutex::new(Vec::new()),
            files: Mutex::new(BTreeMap::new()),
        }
    }

    /// 命令中**包含** `needle` 时返回该应答;先注册的先匹配。
    pub fn on(mut self, needle: &str, code: i32, stdout: &str) -> Self {
        self.responses.push((needle.to_string(), (code, stdout.to_string())));
        self
    }

    pub fn calls(&self) -> Vec<Call> {
        self.log.lock().unwrap().clone()
    }
    pub fn writes(&self) -> Vec<Call> {
        self.log.lock().unwrap().iter().filter(|c| c.is_write()).cloned().collect()
    }
    pub fn written_file(&self, path: &str) -> Option<String> {
        self.files.lock().unwrap().get(path).cloned()
    }
    pub fn reset_log(&self) {
        self.log.lock().unwrap().clear();
    }

    fn answer(&self, cmd: &str) -> (i32, String) {
        self.responses
            .iter()
            .find(|(needle, _)| cmd.contains(needle.as_str()))
            .map(|(_, r)| r.clone())
            // 未注册 = 目标上没有这东西(退出码 1),而不是 panic ——
            // 让"资源不存在"成为默认现实,测试才好写。
            .unwrap_or((1, String::new()))
    }
}

impl Default for FakeCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl Ctx for FakeCtx {
    fn probe(&self, cmd: &str) -> anyhow::Result<(i32, String)> {
        self.log.lock().unwrap().push(Call::Probe(cmd.to_string()));
        Ok(self.answer(cmd))
    }
    fn run(&self, cmd: &str) -> anyhow::Result<(i32, String)> {
        self.log.lock().unwrap().push(Call::Run(cmd.to_string()));
        Ok(self.answer(cmd))
    }
    fn write_file(&self, path: &str, content: &str) -> anyhow::Result<()> {
        self.log.lock().unwrap().push(Call::Write(path.to_string()));
        self.files.lock().unwrap().insert(path.to_string(), content.to_string());
        Ok(())
    }
    /// 假目标记住二进制的**长度**而不是内容 —— 测试要断言的是"推过去了多少",
    /// 不是把字节再抄一遍。
    fn write_bytes(&self, path: &str, content: &[u8]) -> anyhow::Result<()> {
        self.log.lock().unwrap().push(Call::Write(path.to_string()));
        match std::str::from_utf8(content) {
            Ok(text) => self.files.lock().unwrap().insert(path.into(), text.to_string()),
            Err(_) => self
                .files
                .lock()
                .unwrap()
                .insert(path.into(), format!("<binary {} bytes>", content.len())),
        };
        Ok(())
    }
    fn place_material(&self, name: &str, dest: &str) -> anyhow::Result<()> {
        self.log.lock().unwrap().push(Call::Place(name.to_string(), dest.to_string()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_answers_by_prefix_and_defaults_to_absent() {
        let c = FakeCtx::new().on("stat -c", 0, "directory|750");
        assert_eq!(c.probe("stat -c '%F|%a' /data").unwrap().0, 0);
        assert_eq!(c.probe("systemctl is-active x").unwrap(), (1, String::new()));
    }

    #[test]
    fn fake_records_writes_separately_from_probes() {
        let c = FakeCtx::new();
        c.probe("stat /a").unwrap();
        c.run("mkdir -p /a").unwrap();
        c.write_file("/etc/x", "body").unwrap();
        assert_eq!(c.calls().len(), 3);
        assert_eq!(c.writes().len(), 2, "probe 不算写");
        assert_eq!(c.written_file("/etc/x").as_deref(), Some("body"));
    }

    #[test]
    fn local_ctx_really_runs_commands() {
        let (code, out) = LocalCtx.probe("echo hello").unwrap();
        assert_eq!(code, 0);
        assert_eq!(out.trim(), "hello");
        assert_ne!(LocalCtx.probe("exit 3").unwrap().0, 0);
    }
}
