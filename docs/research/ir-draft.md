# IR 契约草案 v0:七名词 + 五动词

> 2026-08-28,承接 [product-design.md](product-design.md)(D-106)。本文冻结**名词、动词、
> 语义**;不冻结字段细节与语法糖(那些在实现中迭代)。原则:IR 是唯一契约,YAML/HCL/UI/
> MCP 都是它的前端;错这里的名词,后面全错,所以本文求"形状对",不求"字段全"。

---

## 1. 七名词总览(所有权与引用关系)

```
Environment ──owns──▶ Substrate*        (机器/既存集群/云账号/registry + 凭据)
            ──owns──▶ values / policy   (环境级参数、审批规则、维护窗口)

Blueprint   ──owns──▶ params schema     (values 契约)
            ──owns──▶ ResourceDecl*     (期望态模板,参数化)
            ──owns──▶ Procedure*        (过渡程序:upgrade/backup/… 的 dance)
            ──owns──▶ Material*         (离线闭包声明)
            ──owns──▶ HealthProbe*      (健康/verify 定义)

Stack       ──refs──▶ Blueprint*(按 digest/版本) × 拓扑约束 × values

Deployment  = Stack × Environment 的绑定实例(生命周期状态机)
            ──owns──▶ Run*              (append-only 执行记录)

Resource(实例) = ResourceDecl × 参数展开 × Substrate 定址后的最小对账单元
```

内容寻址边界:**Blueprint 与 Stack 可密封成 OCI 制品**(digest 即身份);Environment 永不
入制品(凭据红线,承接 crater 现约);Deployment/Run 只存在于 state(SQLite/PG)。

## 2. Rust 形状(核心类型草案)

```rust
// ---- 表达式与值:全 IR 只有一种表达式语义(CEL) ----
pub struct CelExpr(String);              // 编译期 parse + 类型检查,lint 就能报错
pub enum Value {                          // 参数/字段值:静态值或表达式
    Lit(serde_json::Value),
    Expr(CelExpr),                        // "${...}" 插值与 when: 同一求值器
}

// ---- Resource:对账的最小单元 ----
pub struct ResourceDecl {
    pub id: String,                       // blueprint 内唯一;实例 id = deployment/substrate/id
    pub r#type: TypeName,                 // "file" | "service" | … | "custom.x"(注册表解析)
    pub args: Map<String, Value>,         // 按 type schema 校验(deny_unknown_fields)
    pub on: Selector,                     // 定址(裁定 A):all|role.X|first(sel)|rest(sel)|sel where <CEL>|host.X
    pub when: Option<CelExpr>,            // 条件纳入(替代 when_os/when_role/when_offline)
    pub each: Option<Value>,              // 循环展开(替代 loop:),项进 `item` 作用域
    // 注:无 notify/handler —— 上游 changed 经指纹传播进下游 observe(裁定 B)
    pub deps: Vec<String>,                // 资源间依赖(默认按声明顺序推导,显式为进阶)
}

// ---- 五动词:资源类型的强制契约(模块=此 trait 的实现,不论 L1 Rust/L3 WASM/L4 桥) ----
pub trait ResourceType {
    fn schema(&self) -> Schema;                                  // args 契约 + 文档
    fn observe(&self, ctx: &Ctx, args: &Args) -> Result<Observed>;   // 只读,必须实现
    fn diff(&self, desired: &Args, observed: &Observed) -> DiffReport; // 纯函数
    fn apply(&self, ctx: &Ctx, plan: &DiffReport) -> Result<Outcome>;
    fn destroy(&self, ctx: &Ctx, observed: &Observed) -> Result<Outcome>;
    fn upgrade(&self) -> Option<&Procedure> { None }             // 可选:声明式升级 dance
}
// plan = ∀resource: observe+diff(零写);drift = 定时重跑 plan;teardown = 逆序 destroy。
// 这三个能力是契约的推论,不是各模块的自选动作。

// ---- Procedure:过渡程序(状态之外的过程知识,被调用不被编写) ----
pub struct Procedure {
    pub name: String,                     // "upgrade" | "backup" | 自定义动作
    pub params: Schema,
    pub steps: Vec<Step>,                 // 有序;Step = 资源动作或受限原语(shell/wait/…)
}
pub struct Step {
    pub action: StepAction,               // Invoke{type,args} | Shell{cmd,check} | Wait{...}
    pub on: Selector,
    pub when: Option<CelExpr>,
    pub exports: Map<String, FactSpec>,   // 跨主机 fact 就近声明(裁定 B);消费方 ${facts.X},引擎阻塞等待
    pub strategy: Strategy,               // { throttle, retries, ignore_errors } —— run_once 已并入 Selector
}

// ---- Blueprint / Stack / Environment ----
pub struct Blueprint {
    pub name: String, pub version: Version,
    pub params: Schema,                   // values 契约:type/secret/stage(build|deploy)/desc/default/required(裁定 D/E)
    pub requires: Requires,               // 环境准入(distro/version/arch,承接 D-102)
    pub preflight: Vec<Assertion>,        // 只读准入断言(rustfs 裁定 A):CEL + 封闭探针函数白名单
    pub types: Vec<TypeDef>,              // 自定义资源类型(k8s 裁定 D = L2 数据模块层):
                                          //   observe: 只读 cmd/parse;apply/destroy: procedure 引用
    pub resources: Vec<ResourceDecl>,
    pub procedures: Map<String, Procedure>,
    pub materials: Vec<Material>,         // 离线闭包;when: CEL ⇒ flavor 子闭包
    pub health: Vec<HealthProbe>,         // verify 依据(端口/HTTP/cmd,均只读)
}
pub struct Stack {                        // 一套环境的装配单
    pub name: String,
    pub uses: Vec<StackEntry>,            // { blueprint(ref/digest), on: Selector, values }
}
pub struct Environment {
    pub name: String,
    pub substrates: Vec<Substrate>,       // Machine{ssh…} | K8s{kubeconfig…} | Cloud{…} | Registry{…}
    pub groups: Map<String, Selector>,    // 角色/分组(承接 inventory groups)
    pub values: Map<String, Value>,       // 环境级 values(优先级见 §4)
    pub policy: Policy,                   // 审批规则/维护窗口/并发上限
}

// ---- Deployment / Run:引擎围着转的东西 ----
pub struct Deployment {
    pub id: Uuid,
    pub stack: StackRef,                  // digest 锁定
    pub env: EnvRef,
    pub state: DeployState,               // Planned→Applied→Verified⇄Drifted→Upgrading→Retired
    pub resources: Vec<ResourceRecord>,   // 实例级期望/现实指纹(数字孪生的行)
}
pub struct Run {                          // append-only;审计/回放/UI 日志流的最小单元
    pub id: Uuid, pub deployment: Uuid,
    pub kind: RunKind,                    // Plan | Converge | Verify | Procedure(name) | Destroy
    pub events: Vec<Event>,               // step 开始/结束/输出偏移/结果(ok|changed|failed)
}
```

## 3. 前端映射(同一 IR,三张脸)

**作者层 YAML**(next-gen §4 的 DSL)→ Blueprint:`tasks:` 列表按序生成 `ResourceDecl`
(deps 按顺序推导),`- shell:` 生成 `type: shell` 资源(check 即其 observe),`handlers`
并入 procedures,`materials/params` 直落。**操作者层**:`x install <blueprint> --env <e>`
= 生成单条目 Stack + Deployment;`x upgrade --to X` = 调 blueprint 的 `procedures.upgrade`。
**MCP**:资源即工具面——`query_twin` / `propose_change(stack_patch)` / `plan` / `approve` /
`run_procedure`,全部是七名词的读写,无自由命令面。

## 3.5 试金石 ① 回填(rustfs,见 [ir-example-rustfs.md](ir-example-rustfs.md))

结果:表达顺畅,且**样板减少 70%**(143 → 43 行)——teardown / handler+notify / phase /
id / needs 五类写法全部成为契约的推论而非用户输入。撞出 5 处 schema 修正,已并入本文:

| # | 问题 | 裁定 |
|---|---|---|
| A | "端口空闲"型准入在已部署机器上重跑必失败(不幂等) | 新增 `preflight: [{assert: <CEL>, msg}]`;CEL 可调**封闭白名单只读探针函数**(`port_owner()`/`path_exists()`/`cmd_ok()`),断言语义改为"没被**别人**占" |
| B | handler/notify 在对账模型里是冗余的 push 补丁 | **从 IR 删除 handler**:引擎把上游资源 changed 指纹喂进下游 observe(D-092 的 `crater.spec` 指纹法从模块技巧提升为引擎通用机制);ansible 前端的 `notify:` 编译成一条 deps 边 |
| C | systemd unit 用 `copy` 塞 INI 文本(不可校验/不可字段级 diff) | L1 内建增 `systemd_unit` 类型;并据此重拟内建模块清单——按**类型化层次**拟,不照抄 ansible 模块表 |
| D | params 缺类型与密级(`data_dirs` 被压成空格分隔字符串) | params schema = `type`(string/int/bool/[T]/enum)+ `secret`(孪生/日志/API 自动打码)+ `stage` + `desc` + `default` + `required` |
| E | `stage: apply` 与七名词冲突(apply 是动词) | 改名 `stage: build \| deploy` |

冻结确认(未撞到问题):`each` / `when` / CEL 插值 / materials 变体 / health 探针。

## 3.6 试金石 ② 回填(k8s-ha,见 [ir-example-k8s.md](ir-example-k8s.md))

最难的案例:kubeadm init/join 是**舞**不是收敛。结果 **519 → ~130 行(-77%)**,且升级
role 被吸收进 blueprint。**"状态/过程分离"成立**,但要求四处结构性补齐:

| # | 问题 | 裁定 |
|---|---|---|
| A | `when_role + run_once` 表达不了"除首台外的其余 master"(现行靠"全组跑+check 守卫"的诡计,一行三注释) | **Selector 升为一等语法**:`all \| role.X \| first(sel) \| rest(sel) \| sel where <CEL> \| host.X`;`on:` 取代 `when_role`/`run_once`;`when:` 只管条件纳入(与"在谁身上"正交) |
| B | 跨主机 fact 顶层 `register:` 声明,300 行外消费,角色信息重复 | fact 由**产出它的步骤** `exports:` 就近声明,作用域继承该步 `on:`;消费写 `${facts.X}`,引擎按依赖阻塞等待;lint 可查未消费/无导出 |
| C | 目标侧运行时值(网卡名)plan 期不可知 → 现行写 `__IFACE__` 占位再 sed 回填 | CEL 的 `substrate.*` 纳入**目标侧探测事实**(os/arch/hostname/网卡/IP/…),模板在事实已知后渲染;探测项封闭白名单 + 惰性采集(不做 ansible 全量 setup 开销) |
| D | 引擎不能懂 kubeadm(D-017),但集群成员资格必须是资源 | **`types:` 进 Blueprint schema**:blueprint 可自定义资源类型,五动词由 `observe: <只读 cmd/parse>` + `apply: procedure X` + `destroy: procedure Y` 实现 = next-gen §6 的 **L2 数据模块层正式形态**。用户面对名词("这台应是集群成员"),舞被封在类型里 |
| E | 一批"仪式型 shell"(swapoff+fstab、modprobe+modules-load、sysctl+--system、hostnamectl)各占 2 步 + 手写 check | L1 内建追加 `swap`/`kernel_modules`/`sysctl`/`hostname`/`image_present`(+ rustfs 撞出的 `systemd_unit`) |
| F | teardown 12 步 | 10 步是各资源 destroy 的手写版 → 自动逆序;只剩 `kubeadm reset` + 杀 shim 残留是真过程知识,封进 `procedures.reset` |

冻结确认(第二次未撞到问题):`each` / `when` / CEL 插值 / materials / health /
`strategy.throttle` / deps 隐式顺序。**`phase:` 概念确认删除**(preflight=断言、
verify=health、install=资源默认)。

## 4. 冻结的语义裁定(容易返工的点,先钉死)

1. **values 优先级 5 级**:CLI `--set` > Stack entry values > Environment values(host>group>env)
   > Blueprint params default。全工具唯一一张表。
2. **排序**:资源默认按声明顺序建边;`deps:` 显式覆盖;跨 substrate 并发、同一资源序列
   由引擎调度(throttle/run_once 是 Step/Strategy 的事,不是资源的事)。
3. **CEL 作用域**(lint 期可查的全部名字):`params.*`、`env.*`(values)、`substrate.*`
   (os/arch/roles/name + **目标侧探测事实**:网卡/IP/内存…,封闭白名单惰性采集,裁定 C)、
   `item`(each 内)、`facts.*`(procedure 内跨主机 fact,裁定 B)、`observed.*`
   (仅 procedures/health 内)。
4. **shell 逃生舱**:`type: shell` 是一等资源类型;`check:` 即其 observe(exit 0 ⇒ ok);
   无 check 的 shell 在 plan 里显示 `?unknown` 并计入"模型化欠债"统计——接住,不羞辱,可见。
5. **闭包与变体**:material.when(CEL over params)决定 flavor;build 可打全集(内容寻址
   去重),plan 期按 values 选子闭包核对完备性。
6. **offline 不是模式**:承接现约——制品带 blob 即离线,IR 无 offline 开关。

## 5. 验证路径(schema 冻结前的三个试金石)

拿三个现有交付重写成 IR 示例(不写引擎,只写 YAML + 人工推演 plan 输出):
1. **rustfs**(单 blueprint、单机+多机、有 flavor:单盘/多盘)——考 params/材料变体;
2. **k8s-ha**(多角色、init/join 次序、VIP)——考 Selector/deps/procedure(upgrade dance);
3. **demo-stack**(yq+rustfs 组合)——考 Stack 组合与 values 传递。
三个都能自然表达且比现行写法短,IR v0 即冻结;哪里别扭改哪里,再冻。

## 6. 开放问题(暂不裁定,标记即可)

- ResourceRecord 的现实指纹粒度(全 observed 快照 vs 摘要 hash)——影响 drift 精度与 state 体积;
- procedure 失败的补偿语义(halt / rollback-step / resume)——P1 结合真实升级场景定;
- WASM 模块(L3)如何履行 observe(沙箱内只读探针的宿主函数面)——P3 前定;
- Blueprint 间依赖(meta.dependencies)进不进 v0——倾向不进,Stack 显式排序够用到 P2。
