//! 五动词契约 —— 一个资源类型必须能回答的五个问题。
//!
//! 与 Ansible 模块最根本的分歧在 [`observe`](ResourceType::observe):它是**强制**的,
//! 不是 check-mode 那种"模块作者有空就实现"的二等公民。因为强制,以下三件事**不是功能,
//! 是推论**(在 [`crate::plan`] 里兑现):
//! - `plan`   = ∀resource: observe + diff(零写入);
//! - `drift`  = 定时重跑 plan,与上次记录比对;
//! - `destroy`= 逆序调用每个资源的 destroy(所以 IR 里没有 `teardown:`)。
//!
//! 四层实现(next-gen §6)全部履行同一 trait:L1 Rust 内建 / L2 blueprint `types:`
//! (探针 + procedure)/ L3 WASM / L4 协议桥。

use crate::eval::{ResolvedArgs, Yaml};
use crate::schema::Params;

/// 一次 observe 的结果:目标上的**现实**。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observed {
    /// 资源是否存在。不存在时 `fields` 通常为空。
    pub present: bool,
    /// 类型自定义的现实字段(供 diff 与孪生视图)。
    pub fields: std::collections::BTreeMap<String, String>,
}

impl Observed {
    pub fn absent() -> Self {
        Observed { present: false, fields: Default::default() }
    }
    pub fn present(fields: impl IntoIterator<Item = (&'static str, String)>) -> Self {
        Observed {
            present: true,
            fields: fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        }
    }
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }
}

/// diff 的全部输入。
pub struct DiffInput<'a> {
    pub args: &'a ResolvedArgs,
    pub observed: &'a Observed,
    /// **上游资源本轮是否发生变更**。这是 handler/notify 被删掉之后的替代机制
    /// (rustfs 试金石裁定 B):二进制/配置/unit 任一变了,服务自然需要重启,
    /// 不需要作者写 `notify:`,也不会因为忘了写而漏重启。
    pub upstream_changed: bool,
}

/// diff 结果:plan 输出的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// 已是期望态。
    Ok,
    /// 将被创建。
    Create(Vec<FieldDiff>),
    /// 将被修改。
    Update(Vec<FieldDiff>),
    /// 将被删除。
    Destroy,
    /// **探测不到**(如无 `check:` 的裸 shell)—— plan 里显示 `?`,
    /// 并计入"模型化欠债"统计:接住,不羞辱,但可见(ir-draft §4-4)。
    Unknown(String),
}

impl Change {
    pub fn is_noop(&self) -> bool {
        matches!(self, Change::Ok)
    }
    /// plan 摘要里的符号,沿用 terraform 习惯。
    pub fn sigil(&self) -> char {
        match self {
            Change::Ok => '✓',
            Change::Create(_) => '+',
            Change::Update(_) => '~',
            Change::Destroy => '-',
            Change::Unknown(_) => '?',
        }
    }
    pub fn fields(&self) -> &[FieldDiff] {
        match self {
            Change::Create(f) | Change::Update(f) => f,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDiff {
    pub field: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

impl FieldDiff {
    pub fn set(field: &str, to: impl Into<String>) -> Self {
        FieldDiff { field: field.into(), from: None, to: Some(to.into()) }
    }
    pub fn change(field: &str, from: impl Into<String>, to: impl Into<String>) -> Self {
        FieldDiff { field: field.into(), from: Some(from.into()), to: Some(to.into()) }
    }
}

impl std::fmt::Display for FieldDiff {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.from, &self.to) {
            (Some(a), Some(b)) => write!(f, "{}: {a} → {b}", self.field),
            (None, Some(b)) => write!(f, "{}: {b}", self.field),
            (Some(a), None) => write!(f, "{}: {a} → (删除)", self.field),
            (None, None) => write!(f, "{}", self.field),
        }
    }
}

/// 一次写操作的结果,ansible 式回显(幂等契约的可观测面)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 本就是期望态,没动。
    Ok,
    /// 确实改了。
    Changed,
    /// 软失败:已上报,继续执行。
    Warn,
}

/// 执行上下文:资源类型借它访问目标(执行命令、写文件、取物料)。
/// 具体实现见 [`crate::ctx`](crate::ctx);SSH 实现在执行层,与本 crate 解耦。
/// `Send + Sync` 是并发调度的前提:一步之内多台机器同时跑时,
/// `&dyn Ctx` 要跨线程借出去。约束落在 trait 上而不是各处 `where`,
/// 是为了让"某个实现不是 Sync"在定义处就编译失败,而不是在调度处。
pub trait Ctx: Send + Sync {
    /// 在目标上执行**只读**命令 → (退出码, stdout)。observe 只许走这条。
    fn probe(&self, cmd: &str) -> anyhow::Result<(i32, String)>;
    /// 在目标上执行写命令 → (退出码, stdout+stderr)。
    fn run(&self, cmd: &str) -> anyhow::Result<(i32, String)>;
    /// 写一份**文本**文件内容到目标路径。
    fn write_file(&self, path: &str, content: &str) -> anyhow::Result<()>;

    /// 写**任意字节**到目标路径。
    ///
    /// 离线闭包运的是 containerd/kubeadm 这类二进制,不是配置文本 —— 只有
    /// 文本通道等于闭包白建。默认实现要求内容恰好是 UTF-8(测试上下文够用),
    /// 真实传输层必须覆盖它。
    fn write_bytes(&self, path: &str, content: &[u8]) -> anyhow::Result<()> {
        let text = std::str::from_utf8(content).map_err(|_| {
            anyhow::anyhow!("此上下文只有文本通道,写不了二进制({path},{} 字节)", content.len())
        })?;
        self.write_file(path, text)
    }
    /// 把一份物料落到目标路径(在线取 URL / 离线推 blob,由执行层决定)。
    fn place_material(&self, name: &str, dest: &str) -> anyhow::Result<()>;

    /// 这份物料的内容摘要(sha256 hex),**控制端**就能回答时才有值。
    ///
    /// 有它,`copy`/`systemd_unit` 这类"内容来自物料"的资源才能在 plan 期
    /// 与目标实际内容比对,而不是只能报 `?`(那会让 verify 永远给不出绿灯)。
    /// 返回 `None` = 这份物料的内容此刻算不出来(远端 URL 且未声明 sha256),
    /// 调用方应如实报"说不清",不要猜。
    fn material_digest(&self, _name: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }

    /// 在**控制端**渲染这份物料(模板)→ 最终字节。
    ///
    /// 目标机零依赖是硬约束,所以渲染只能发生在这一侧。渲染完成后 `template`
    /// 与 `copy` 再无区别:同样按内容寻址判幂等,plan 期也就说得出确定结论
    /// 而不是 `?`。返回 `None` = 这个上下文渲染不了(如 `LocalCtx`),
    /// 调用方应如实报"说不清"。
    fn render_material(&self, _name: &str) -> anyhow::Result<Option<String>> {
        Ok(None)
    }
}

/// 资源类型的契约。
pub trait ResourceType: Send + Sync {
    /// 类型名(YAML 里作为模块 key 出现)。
    fn name(&self) -> &'static str;
    /// 参数契约;lint 用 [`crate::types`] 的静态表,这里给执行期与文档用。
    fn schema(&self) -> Params {
        Params::new()
    }

    /// **只读**观察现实。禁止任何写入 —— plan 的可信度全押在这条纪律上。
    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> anyhow::Result<Observed>;

    /// 纯函数:期望 vs 现实。不碰目标,便于单测与 UI 复算。
    fn diff(&self, input: &DiffInput) -> Change;

    /// 弥合差异。只在 `diff` 非 noop 时被调用。
    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, change: &Change) -> anyhow::Result<Outcome>;

    /// 退役。teardown 由引擎逆序调用它组装,用户不写。
    fn destroy(
        &self,
        ctx: &dyn Ctx,
        args: &ResolvedArgs,
        observed: &Observed,
    ) -> anyhow::Result<Outcome>;

    /// 可选:声明式升级路径(过渡程序名)。有它才支持 `x upgrade`。
    fn upgrade_procedure(&self) -> Option<&str> {
        None
    }

    /// 退役时这个资源会发生什么 —— 默认"存在就删"。
    ///
    /// 之所以要独立于 `observed.present`:那个布尔服务的是**收敛**
    /// ("这个资源在不在,好比对期望态"),而退役问的是另一件事
    /// ("我们在这台机器上还留着什么痕迹")。两者常常不一致 ——
    /// `package` 在包全被卸掉后仍要 present,否则 diff 无从判断该不该装;
    /// 但它此刻已经没有可退役的东西了。
    ///
    /// 用一个布尔同时回答两个问题的代价是**退役不幂等**:重跑 destroy 永远
    /// 显示"还有活干",真机上正是这样。
    fn destroy_change(&self, observed: &Observed) -> Change {
        if observed.present {
            Change::Destroy
        } else {
            Change::Ok
        }
    }

    /// 这个类型**刻意不做退役**时,说明为什么;正常会删除则返回 `None`。
    ///
    /// 有些名词没有"退役"可言:改回哪个主机名?哪个时区?关掉时钟同步对这台
    /// 机器上其它东西是纯粹的伤害。这些类型的 `destroy` 是空操作 ——
    /// 但**计划必须照实说**,否则它会承诺一件执行根本不会做的事。
    /// plan 里承诺删除、apply 时悄悄跳过,比一开始就说"保留"糟得多。
    fn retire_note(&self) -> Option<&'static str> {
        None
    }
}

// ---------------------------------------------------------------- 参数取值助手

pub fn arg_str<'a>(args: &'a ResolvedArgs, key: &str) -> anyhow::Result<&'a str> {
    args.get(key)
        .and_then(Yaml::as_str)
        .ok_or_else(|| anyhow::anyhow!("参数 `{key}` 缺失或不是字符串"))
}

pub fn arg_str_opt<'a>(args: &'a ResolvedArgs, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Yaml::as_str)
}

pub fn arg_bool(args: &ResolvedArgs, key: &str) -> Option<bool> {
    args.get(key).and_then(Yaml::as_bool)
}

/// shell 单引号转义 —— 路径里带空格/引号也不会拼错命令。
pub fn sh(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sigils_match_terraform_habit() {
        assert_eq!(Change::Ok.sigil(), '✓');
        assert_eq!(Change::Create(vec![]).sigil(), '+');
        assert_eq!(Change::Update(vec![]).sigil(), '~');
        assert_eq!(Change::Destroy.sigil(), '-');
        assert_eq!(Change::Unknown("no check".into()).sigil(), '?');
    }

    #[test]
    fn only_ok_is_noop() {
        assert!(Change::Ok.is_noop());
        assert!(!Change::Unknown("x".into()).is_noop());
        assert!(!Change::Create(vec![]).is_noop());
    }

    #[test]
    fn shell_quoting_survives_nasty_paths() {
        assert_eq!(sh("/tmp/a b"), "'/tmp/a b'");
        assert_eq!(sh("it's"), r"'it'\''s'");
    }

    #[test]
    fn field_diff_renders_both_directions() {
        assert_eq!(FieldDiff::set("mode", "0755").to_string(), "mode: 0755");
        assert_eq!(FieldDiff::change("mode", "0644", "0755").to_string(), "mode: 0644 → 0755");
    }
}
