# crater Web UI 设计方案 —— 调研、裁定与实现路线

> 产出方式:4 路并行调研(AWX/AAP 深挖、Semaphore、容器平台 YAML 编排、本仓库现状盘点)
> → 3 个取向的独立设计提案(IDE 取向 / 运维平台取向 / 期望态取向)
> → 对抗性评审(逐案挑错、打分、合成裁定)。本文是合成后的定案。
> 调研出处见文末;评审对每条"既有代码"引用都做过代码核对。

---

## 一、调研结论:AWX/Semaphore 为什么只能是"任务启动器"

用户的原话——"底层编排文件要脱离 UI 提前写好注入里面,UI 上从零编排基本不可能"——
在 AWX 里**不是功能缺失,是架构必然**。四个环环相扣的结构性原因:

1. **模块参数在远端 Python 里**。Ansible 模块的 `argument_spec` 写在模块源码内部,
   打包送到**被管节点**执行时才校验。AWX 数据库里不存在任何模块参数 schema ——
   表单无从生成,只能给一个 extra_vars 大文本框。
2. **playbook 是图灵完备混合体**(YAML + Jinja2 惰性模板 + 动态 include + 22 级变量
   优先级),无法静态展开为表单;结构化往返必然摧毁模板与注释。
3. **role/collection 是目录树协议**,不是单文档。UI 要编辑它等于要实现完整 IDE。
   2018 年就有人提 playbook 编辑器(awx#2206),关闭未实现。
4. **校验只有运行时一份**。UI 若做表单校验就得再造一份规则,必然与真相漂移。
   AWX 的理性选择是彻底不做,把内容正确性外包给 git。

AAP 2.x 三代演进(Execution Environments / Automation Hub / Event-Driven Ansible /
2.5 统一 UI)**全部在执行环境、内容分发、触发方式上打转,从未触碰"平台理解内容
本身"** —— EDA 的 rulebook 同样必须来自 git 仓库,注入模式被原样复制到新组件。
Semaphore 同病:从 2017(#327)到 2024(#1764),"UI 内创建 playbook"的呼声从未
停,因为它同样没有内容契约。

**crater 的镜像对照**:26 类型/121 字段登记表在控制端 → JSON Schema 自特化 →
lint 前后端同源 → 蓝图是单文档 → observe 强制只读使 plan 无副作用。
**AWX 缺的四块地基 crater 全有。"UI 从零编排"对 AWX 是不可能问题,对 crater
是投影问题。**

容器平台侧的关键参照:
- **OpenShift Console** 的 Form⇄YAML 双视图由 CRD 的 OpenAPI schema 驱动 ——
  与 crater 登记表思路同构;但它的 Form↔YAML 往返**丢注释**,crater 的
  "YAML 是模型、表单是投影"正是这个方案的修正版。
- **ArgoCD** 敢做对账(desired vs live 的 diff、Synced/Healthy 两轴)却把编辑
  外包给 Git+IDE。crater 的契约允许两者兼得。
- AWX 值得直接偷的:Prompt-on-Launch 逐字段开关、Survey 的 6 类型最小集、
  Approval 节点、仅失败主机重跑(升级为"漂移子集收敛")、"一切副作用皆 job"、
  凭据 write-only。必须避开的:内容注入鸿沟、六步对象链、参数三套叠加、
  websocket 重架构(AWX 自认技术债 #11346)、提前建 RBAC 表。

## 二、总取向裁定:对账中心(ArgoCD 的世界观 + 超越 OpenShift 的编辑)

三案评审计分:IDE 取向 39 / 运维平台取向 33 / 期望态取向 39;
**总取向采用期望态**,决定性理由:

- 只有它把 crater 与 AWX 的差异(**对账引擎**)做成 UI 的中心命题,
  而不是做一个更好的任务触发器;
- 与项目方向决议 D-106("包管理器 + 对账引擎,IR 先行,DSL 降为作者前端")同构。

否决运维平台取向的对象数据库:Deployment 存 DB 行 + 路径字符串,在 git 工作目录
切分支/改名时**集体失联**;参数值进 DB 后注释永久无处安放 —— 这正是 AWX
"survey 与 playbook 脱节"的病根重演。

**存储铁律(全案适用)**:一切期望态在文件里(可 git、可 diff、可带注释);
数据库只存观测结果与执行历史。UI 永远不把期望态写进数据库。

## 三、对象模型(回答"项目管理/任务管理")

### 项目 = 工作目录(Workspace)

`crater ui` 启动于哪个目录,哪个目录就是项目。已有的路径禁闭
(canonicalize + starts_with,根不可配置)就是安全边界。多项目 = 多目录多实例。

- **创建**:新建向导(空白 / 骨架 / library 模板三入口)。
- **删除**:不做永久删除 —— 移入 `.crater-trash/`(带时间戳),UI 可恢复;
  删除前做**引用检查**:被 app 文件引用的蓝图/inventory 拒删并列出引用者。
- **重命名**:同样过引用检查,提示"N 个 app 引用旧名,是否一并改写"
  (对 app 文件做定点文本替换,注释不动)。
- git 是项目的属性(可选,先只显示 dirty/clean 徽章),不是项目的定义 ——
  这是与 AWX Project(SCM 指针)的根本分界。

### 任务 = App 绑定文件("绑定即身份")

对应 ArgoCD 的 Application、AWX 的 Job Template,但它是**带注释的 YAML 文件**:

```yaml
# prod-k8s.app.yaml —— 把这份蓝图钉在这群机器上
app:
  name: prod-k8s
  blueprint: library/k8s/k8s-ha.blueprint.yaml   # 也可指向 stack 或 oci:// 闭包
  inventory: inventory.prod.yaml
  params:            # 固化值;未固化的项 = "启动时问"(Prompt-on-Launch 的正确版)
    version: "1.37.0"
  verify:
    interval: 30m    # 定时漂移巡检;省略 = 只手动
```

- 与 stack 的分界:stack 是**作者侧**组合(蓝图拼蓝图),app 是**运行侧**绑定
  (拼好的东西钉到哪群机器);app.blueprint 可指向 stack,两者正交。
- CRUD:创建 = 4 项小表单(名字/选蓝图/选 inventory/填 params,全部由 lint
  返回的声明驱动),产出**带注释的 YAML 文本**(模板生成,非 JSON 序列化);
  编辑 = 打开文件;删除 = 删文件(FileStore 记录标"孤儿",可一键重建或发 destroy)。
- **实现警示(评审裁定)**:app 是**文档**类型,26 类型登记表是**资源**类型表
  (五动词契约对文档无意义)—— app **不进** t!/f! 登记表,另做轻量"文档 schema"
  层(app / inventory 共用),classify() 加分支。动手前先做一天量级 spike
  验证改造量;超 5 天则降级为"约定形状 YAML + 专用校验函数"。

### 两轴状态(每个 app / 每台机 / 每个资源都挂,绝不合并成一个灯)

| 轴 | 取值 | 判定来源 |
|---|---|---|
| 同步轴 | Synced / Drifted(n) / **OutOfDate**(文件改了没 apply)/ Progressing / NeverApplied / Unknown | verify 快照 + 文件 hash 对比 + job 锁 |
| 健康轴 | Healthy / Degraded(n) / Unreachable / Unknown | verify 的 assess 分类 |

OutOfDate 的地基:FileStore 记录**新增** `blueprint_sha256` / `inventory_sha256`
(apply 时的期望态指纹)。当前文件 hash ≠ 记录 hash ⇒ "期望态领先现实" ——
ArgoCD OutOfSync 的对应物,AWX 概念上不存在此物。

### 数据库(最小盘)

只新增两张表,放 `~/.crater/state.db`:
- `ui_jobs(id, kind, app, verb, args_json, started, finished, ok, pid, canceled)`
  —— job 索引;日志与事件**落盘** `~/.crater/jobs/<id>/`。
- `verify_snapshots(app, ts, status, drifted_n, json)` —— 定性为**历史缓存**
  (诚实承认不可重建,与 job 日志同级)。

否决:projects/environments/deployments/tokens/audit 五张表(评审:在单 token
的 v1 里连"谁"都记不出差别,纯 migration 维护税;schema 留扩展位即可)。

## 四、信息架构与导航

| 页 | 路由 | 职责 |
|---|---|---|
| **概览**(新首页) | /view/overview | App 卡片墙:两轴徽章 + 漂移数 + 上次 verify + 版本;顶部统计条;**空态 = 三步引导卡**(新建蓝图→生成 inventory→创建 App),对着 AWX 六步对象链打 |
| **部署** | /view/app/{name} | 资源树(机群→机器→资源,每节点两徽章)· 常驻 diff 面板(expected/actual 两列)· 动作条(Verify / Plan / **Apply 需先 Plan** / Upgrade)· 版本卡 · job 时间线 · 折叠危险区(Destroy 阶梯) |
| **期望态** | /view/edit(扩展现有) | 编排工作台:文件树(五分类)+ 现有编辑器 + 右侧字段卡;单文件编辑 + 最近文件(评审裁定砍掉多 tab —— 前端税) |
| **机群** | /view/fleet | 聚合所有 inventory:机器×组表,标"被哪些 app 引用";每个 app 的 fleet-check 常驻 |
| **作业** | /view/jobs,/view/job/{id} | 全部动词的历史(lint/plan/build 也进 —— "一切副作用皆 job");详情 = 执行树 + 事件表 + RAW |
| **制品** | /view/artifacts | 闭包列表(digest/tag/被哪个 app 用)、build 入口;升级 = 换 tag → diff → upgrade |
| Legacy | /view/tasks 等 | 旧 task 管线页面挂 Legacy 标签冻结 |

## 五、Job 执行与日志(回答"job 执行、执行日志打印")

- **执行器**:现有 spawn-CLI 模型演进(不重写)。每 app 互斥 + 同 app 排队 +
  全局并发上限 4。**重启恢复矩阵是第一类验收**:running→interrupted、
  queued→canceled、错过的定时 verify 记 skipped 不补跑 —— 每条一个集成测试。
- **结构化事件**:CLI 加 `--events <path>`,把机器×资源×动词的结构化事件写成
  NDJSON **独立文件**(不占 stdout,人类 CLI 体验零改动)。这是对 AWX 的降维:
  它要靠回调插件逆向解析 stdout,crater 的 IR 天生知道这些维度。
- **传输:htmx 游标轮询**(`?after=<seq>`,286 状态码停轮)。**明确不上
  websocket/SSE** —— AWX 为"实时感"背上 Django Channels+daphne+redis 三件套,
  自认技术债(#11346),air-gap 场景纯负债。
- **视图**:机器×资源状态矩阵(执行树)+ 事件过滤 + 点击行看 JSON 详情 + RAW。
  矩阵由进程内 per-job reducer 维护,重启后从 NDJSON 重放一次重建。
- **plan-gated apply**(把"先看 diff 再动手"做成结构而非美德):plan 完成时记录
  `(blueprint_sha, inventory_sha, params 快照)`,Apply 校验当前三者与 plan 时
  一致,不等则 409"期望态已变更,请重新 Plan"。UI 上 Apply 只在存在有效 plan
  时成为主按钮。**params 必须进快照**(评审抓的洞:否则两次不同 --set 可骑同
  一个 plan 过闸)。
- **确认阶梯**:verify/plan 零确认 → apply 确认框复述范围(几台/几资源/几销毁)
  → destroy 先跑预演 job 展示将删清单 → 输入 app 名等值确认 → --yes 真跑。
  upgrade 强制先 plan diff。
- **定时 verify**:`verify.interval` 写在 app 文件里(监控意图也是期望态),
  UI 内 tokio 调度器只是点火器,逻辑全在 CLI。全局同时最多 1 个 auto-verify,
  间隔下限 5m(air-gap 环境对 SSH 压力敏感)。**不许晚于漂移看板上线**
  (评审抓的自相矛盾:看板没有供血就是旧闻)。
- 缓行 `--only` 漂移子集收敛:蓝图资源有顺序与传导依赖,部分收敛可能产出任何
  完整 plan 都到不了的中间态。v1 的"收敛"按钮 = 整 app 重新 plan→apply
  (可重入保证正确);等引擎定义好"子集+前置依赖闭包"语义再上。

## 六、纯 UI 从零编排:能,分三档说清"完全"到什么程度

**第一档 —— 引导式文本编排:确定能,阶段 3 即达成。**
机制链每环都已存在或改造量明确:登记表 → 带注释骨架生成(登记表的 doc 即注释)
→ 编辑器 + 同源 lint 实时行级诊断(已有)→ inventory 骨架按 fleet 契约生成
(已有,需从 JSON 改为**带注释 YAML 文本**)→ fleet 台数核对在连机器之前报错
→ plan 零写入预演 → apply。用户的痛点在这一档就正面解决。

从零旅程(空目录 → nginx 上两台新机,全程不碰终端不碰 git):
新建向导选类型(26 卡片,含 implemented 徽章)→ 骨架蓝图落盘 → 编辑器填字段
(光标字段卡 + 实时 lint)→ 声明 fleet.groups 后顶栏提示"尚无匹配 inventory"
→ 一键生成 inventory 骨架(带 TODO 注释)→ 填 IP,fleet-check 变绿 →
创建 App(4 项表单)→ Plan(矩阵逐格点亮)→ 基于此 Plan 发起 Apply → 两轴变绿。

**第二档 —— 光标字段卡 + 候选字段插入:大概率能,是增强件而非承重墙。**
技术前提:编辑中的 YAML 大部分时间残缺非法,"位置→IR 节点"反查必须对坏输入
降级(退回上次成功 parse + 缩进启发)。列为**第一顺位可砍项**,砍掉后旅程仍闭环。

**第三档 —— 完整 form⇄YAML 双视图:能,但只有一条路。**
load→改→dump 的结构化往返在"注释一等公民"约束下**永久出局**(OpenShift 前车
之鉴);唯一方案是 span 级定点补丁(锚点/flow-style 字段降级只读、请去 YAML
视图),约 400–600 行引擎投资,放最后。**诚实边界**:"一行 YAML 都不想看"的
用户在此档之前没有完整归宿;另一个永久边界是凭据 —— SSH 私钥内容永不经 UI。

## 七、跨文件校验的架构决策(评审抓出的共同暗礁)

app 的引用完整性(blueprint/inventory 路径存在、params 与蓝图声明对得上)是
**跨文件**校验,而 `/api/lint` 是对 body 的纯函数(零文件 I/O)。裁定:

- `/api/lint` 保持纯函数,一行不改;
- 新开 `POST /api/lint-project`(收路径,服务端经 resolve() 禁闭后读文件),
  负责 app 引用完整性、fleet×inventory 对照;
- **两者调用同一个 crater-ir lint 核心** —— "校验只有一份"的承诺从端点层
  下沉到库函数层,CLI 同走此核心。

## 八、分阶段落地(每阶段独立可交付)

| 阶段 | 内容 | 关键改动 |
|---|---|---|
| ① 执行打通 | /api/run 参数化 inventory(消灭 ui.rs 里写死的 `INVENTORY` 常量);ui_jobs 落盘;job 历史页 | ui.rs、state.db |
| ② 对账供血 | FileStore 接入 UI;两轴看板(概览页);`verify --json`;调度器(interval);verify_snapshots | ui.rs、crater-ir state、CLI |
| ③ 从零闭环 | app 文档类型(**先 spike**);plan-gated;inventory 骨架 YAML 化;文件 CRUD(trash/引用检查/重命名);新建向导 + 蓝图骨架生成 | 文档 schema 层、ui_edit、ui_contract |
| ④ 执行呈现 | `--events` NDJSON;游标轮询;机器×资源矩阵;事件过滤 | CLI、ui.rs |
| ⑤ 表单投影 | /api/context 光标字段卡(可砍);span 级定点补丁(第三档) | crater-ir loc、前端 |

## 九、三大翻车点与降险

1. **app 文档类型的元模型改造超预算** → 动手前一天量级 spike(只做 parse +
   两条交叉校验规则),按实测重估;超 5 天走降级路线。
2. **UI 升格常驻服务的可靠性语义** → "重启恢复矩阵"当第一类验收,每格一个
   集成测试(running→interrupted / queued→canceled / 错过 interval 记 skipped /
   reducer 从 NDJSON 重放)。
3. **跨文件校验危及"lint 只有一份"** → 见第七节的分层单源方案。

---

### 调研出处(节选)

- AWX 24.6.1 用户指南 §16/§20/§22/§23/§25;awx 仓库 docs/websockets.md、
  survey spec 模板;issues #20 #1346 #1831 #2206 #3952 #11346 #12685 #13112
- Semaphore issues #327 #1764 #2023,v2.16–2.18 release notes
- OpenShift Console Form/YAML 双视图文档;ArgoCD Application/Sync 文档;
  Rancher / Lens / Portainer / K8s Dashboard 对比
- 本仓库:ui.rs / ui_contract.rs / ui_edit.rs / types.rs / state.rs 逐文件核对
