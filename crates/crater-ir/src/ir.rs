//! 七名词的类型化形状。本文件是**契约本体** —— 前端(YAML/HCL/UI/MCP)编译到这里,
//! 后端(plan/converge/drift/API)只认这里。
//!
//! 刻意**不存在**的东西,每一个都是一次裁定:
//! - 无 `handler` / `notify`:上游 changed 经指纹传播进下游 observe(rustfs 裁定 B);
//! - 无 `run_once`:并入 [`Selector::First`](crate::selector::Selector)(k8s 裁定 A);
//! - 无 `phase`:preflight = 断言、verify = health、install = 资源默认(k8s 裁定 G);
//! - 无 `teardown`:destroy 是五动词契约的推论,引擎逆序调用(k8s 裁定 F);
//! - 无 `offline` 开关:制品带 blob 即离线(承接旧约)。

use crate::expr::{CelExpr, Template};
use crate::schema::Params;
use crate::selector::Selector;
use std::collections::BTreeMap;

/// 字段值:静态字面量,或含 `${}` 插值的模板。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Lit(serde_yaml::Value),
    Tmpl(Template),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

impl Value {
    /// 递归收集其中出现的 CEL 根变量(供作用域 lint)。
    pub fn roots(&self, out: &mut std::collections::BTreeSet<String>) {
        match self {
            Value::Lit(_) => {}
            Value::Tmpl(t) => out.extend(t.roots()),
            Value::List(items) => items.iter().for_each(|v| v.roots(out)),
            Value::Map(m) => m.values().for_each(|v| v.roots(out)),
        }
    }
}

pub type Args = BTreeMap<String, Value>;

/// 一个命令行 flag 条目(D-117 §3.4)。
///
/// 「条件拼 flag」是最高频的把逻辑塞进字符串的诱因。解药不是换表达式语言,
/// 而是让**条件成为条目的属性**:
///
/// ```yaml
/// flags:
///   - name: --control-plane-endpoint
///     value: "${params.cp_endpoint}"
///     when: has(params.cp_endpoint)
///   - name: --upload-certs          # 无 value = 布尔 flag
///     when: params.ha
/// ```
///
/// `name` **禁止插值**(lint 强制)—— 于是 lint 能静态枚举出这条命令的全部展开形态,
/// plan 能逐条解释"哪个 flag 因为什么没出现"。
#[derive(Debug, Clone)]
pub struct Flag {
    pub name: String,
    /// 无值 = 布尔 flag(`--upload-certs`)。
    pub value: Option<Value>,
    pub when: Option<CelExpr>,
}

/// `each:` 的两种合法写法 —— **求值时发现的**歧义,收敛成一条规则:
/// 字符串 = CEL 表达式(与 `when:` 一致,不写 `${}`),列表 = 字面量。
/// 若字符串仍按模板解析,`each: params.dirs` 与 `each: "${params.dirs}"` 会是两种写法。
#[derive(Debug, Clone)]
pub enum Each {
    Expr(CelExpr),
    List(Vec<Value>),
}

/// 一条资源声明 —— 对账的最小单元。
#[derive(Debug, Clone)]
pub struct ResourceDecl {
    /// blueprint 内唯一;未写时按 `<type>[<序号>]` 自动生成(旧模型强制手写 `id:`)。
    pub id: String,
    /// 可选的人类标签(纯注释,不参与语义)。
    pub name: Option<String>,
    /// 资源类型名:内建(file/copy/service/…)或 blueprint `types:` 自定义。
    pub ty: String,
    pub args: Args,
    /// 定址:作用在哪些 substrate 上。
    pub on: Selector,
    /// 条件纳入(与 `on` 正交:on 管在谁身上,when 管要不要做)。
    pub when: Option<CelExpr>,
    /// 循环展开;项进 `item` 作用域。
    pub each: Option<Each>,
    /// 显式依赖。默认按**声明顺序**建边,这里只用于跨序/并行优化(进阶用法)。
    pub deps: Vec<String>,
    /// 源码行号(诊断用)。
    pub line: Option<usize>,
}

/// 自定义资源类型(k8s 裁定 D)= next-gen §6 的 **L2 数据模块层**正式形态:
/// 五动词由 YAML 声明的探针 + procedure 实现,**引擎无需理解具体产品**(守 D-017)。
#[derive(Debug, Clone)]
pub struct TypeDef {
    pub name: String,
    pub args: Params,
    /// 只读探针:cmd 的输出用 `parse` 映射成 observed 字段。
    pub observe: ObserveSpec,
    /// 不满足期望时跳哪支舞(procedure 名)。
    pub apply: String,
    /// 退役时跳哪支舞;缺省表示"无需过程,资源级 destroy 足够"。
    pub destroy: Option<String>,
    /// 升级路径(声明式 dance),供 `x upgrade` 调用。
    pub upgrade: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ObserveSpec {
    pub cmd: String,
    /// 输出片段 → observed 字段名(如 `{joined: "joined"}`)。
    pub parse: BTreeMap<String, String>,
}

/// 过渡程序 —— "从 A 态到 B 态怎么安全地走"。是资源类型的一部分,
/// **被调用而非被编写**:操作者说 `x upgrade --to 1.37`,不写 drain/join。
#[derive(Debug, Clone)]
pub struct Procedure {
    pub name: String,
    pub params: Params,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone)]
pub struct Step {
    pub id: String,
    pub name: Option<String>,
    /// 步骤动作:调用某资源类型(含 shell/wait 等内建)。
    pub ty: String,
    pub args: Args,
    pub on: Selector,
    pub when: Option<CelExpr>,
    pub each: Option<Each>,
    /// 跨主机 fact:**由产出它的这一步就近声明**(k8s 裁定 B),
    /// 作用域继承本步 `on:`;消费方写 `${facts.<name>}`,引擎按依赖阻塞等待。
    pub exports: BTreeMap<String, String>,
    pub strategy: Strategy,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Strategy {
    /// 同时最多几台跑这一步(护 etcd 之类的串行需求)。
    pub throttle: Option<usize>,
    pub retries: u32,
    pub ignore_errors: bool,
}

/// 离线闭包里的一项物料。`when` 使之成为 **flavor**:闭包 = f(values)。
#[derive(Debug, Clone)]
pub struct Material {
    pub name: String,
    pub kind: MaterialKind,
    /// `file`: URL(可含插值);`image`: 镜像 ref;`os_package`: 包名表。
    pub source: Value,
    /// 内容摘要。**和 `source` 一样可插值** —— URL 里能写
    /// `${params.version}`,摘要里就必须也能写,否则换版本时两者必然走散
    /// (见 `materials::render_sha`)。
    pub sha256: Option<Value>,
    /// 下载物是 zip 时,取其中这个成员作为物料本体(控制端解包)。
    pub unzip: Option<String>,
    /// 条件纳入:决定该物料属于哪个 flavor 子闭包。
    pub when: Option<CelExpr>,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterialKind {
    File,
    Image,
    OsPackage,
}

/// 只读准入断言(rustfs 裁定 A)。语义是"没被**别人**占",而不是"端口空闲" ——
/// 后者在已部署机器上重跑必然失败,与幂等承诺冲突。
#[derive(Debug, Clone)]
pub struct Assertion {
    pub expr: CelExpr,
    pub msg: Option<String>,
    pub on: Selector,
    pub line: Option<usize>,
}

/// 健康探针:verify 与 drift 的依据,恒为只读。
#[derive(Debug, Clone)]
pub struct HealthProbe {
    pub ty: String,
    pub args: Args,
    pub on: Selector,
    pub timeout: Option<String>,
    pub line: Option<usize>,
}

/// 环境准入契约(承接 D-102):这份 blueprint 支持哪些目标环境。
#[derive(Debug, Clone, Default)]
pub struct Requires {
    pub os: Vec<OsRequire>,
    pub arch: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct OsRequire {
    pub distro: String,
    pub versions: Vec<String>,
}

/// 机群契约:蓝图声明"我需要哪些组、最少几台"。
///
/// 有了它,"inventory 少了个组"或"HA 蓝图只给了一台 master"能在**连机器之前**
/// 就被拦下,而不是等 selector 求值时才逐条报错 —— 后者既晚又零散。
#[derive(Debug, Clone, Default)]
pub struct FleetContract {
    pub groups: BTreeMap<String, GroupContract>,
}

#[derive(Debug, Clone, Default)]
pub struct GroupContract {
    /// 最少几台。`0` 表示**允许为空**(单节点拓扑里的 worker 组),
    /// 与"没声明这个组"是两回事。
    pub min: usize,
}

/// **Blueprint** —— 一个子系统的完整知识:期望态 + 舞 + 闭包 + 契约 + 健康定义。
/// 可密封成内容寻址的 OCI 制品(digest 即身份);凭据永不进入(那是 Environment 的事)。
#[derive(Debug, Clone)]
pub struct Blueprint {
    pub name: String,
    /// 机群契约(可选):plan 之前校验 inventory 是否满足。
    pub fleet: FleetContract,
    /// 选角表:给 selector 起名字,单点定义、全篇引用。
    /// 解析期已展开进各处 selector,这里保留原表供 `inspect` 与报错使用。
    pub cast: BTreeMap<String, Selector>,
    /// **派生事实**:声明处做计算,值位置保持名词(D-136)。
    ///
    /// 为什么需要它:`interface: "${iface_in(params.vip_cidr)}"` 会被 E310
    /// 拒掉 —— 值位置只许名词,这是 D-117/A4 刻意关上的门。但"按网段找网卡"
    /// 这类计算确实要做,答案是**换个位置做**:在这里算一次,资源里写
    /// `${facts.vip_iface}`。
    ///
    /// 每台机器各算各的(网卡名本就因机而异),在事实探全之后求值一次。
    pub facts: BTreeMap<String, CelExpr>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub params: Params,
    pub requires: Requires,
    pub materials: Vec<Material>,
    pub preflight: Vec<Assertion>,
    pub types: Vec<TypeDef>,
    pub resources: Vec<ResourceDecl>,
    pub procedures: BTreeMap<String, Procedure>,
    pub health: Vec<HealthProbe>,
}

impl Blueprint {
    pub fn material(&self, name: &str) -> Option<&Material> {
        self.materials.iter().find(|m| m.name == name)
    }
    pub fn custom_type(&self, name: &str) -> Option<&TypeDef> {
        self.types.iter().find(|t| t.name == name)
    }
}
