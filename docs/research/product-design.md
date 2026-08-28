# 白纸产品设计:如果不从 Ansible 出发

> 2026-08-28。姊妹篇 [next-gen.md](next-gen.md) 回答"技术上怎么建";本文回答一个更前置的
> 问题:**抛开 Ansible 的概念框架(play/role/task/插件)、抛开现有代码,从问题本身出发,
> 这个产品的最优解长什么样**。结论不推翻 next-gen.md,而是给它换了地基:ansible 形状的
> DSL 从"产品本体"降位为"作者层前端"。

---

## 1. 重新定义要解决的问题

Ansible 对问题的定义是:"帮我在很多机器上自动化地执行任务"。——这个定义本身就是限制。

我们场景(平台/中间件/云资源交付、k8s 集群生命周期、air-gap)的真实问题是:

> **"让一个环境(一堆机器 + 集群 + 云资源)达到并保持某个期望状态,在它的整个生命周期里
> (交付→验证→漂移→升级→扩缩→退役),哪怕这个环境根本连不上互联网。"**

注意这个定义里**没有"执行任务"四个字**。执行是手段,不是产品。从这个定义出发推导,而不是
从"怎么把 playbook 写得更好"出发。

## 2. 五个第一性不变量

不管用什么技术,这个问题域里有五件事是不变的:

### 不变量一:一切都是"期望态 vs 现实态"的对账

存在一份期望(what should be)和一个现实(what is),全部工作 = 观察现实 → 与期望求差 →
弥合差异 → 再观察。**Ansible 没有这个模型**(它是一次性推送脚本,幂等靠模块作者自觉);
Terraform 有一半(有 diff 但 state 文件会说谎);K8s 控制器是完全体(持续对账)但只管
集群之内。对 OS/裸机层做对账模型,是无人区。

推论:产品的核心循环不是 `run`,是 **`observe → diff → plan → (approve) → converge → record`**。
"部署"只是这个循环的第一圈;漂移检测是第二圈起的常态;升级是带过渡程序的一圈。
人在环审批(plan gate)是运维场景对 K8s 式全自动对账的必要修正。

### 不变量二:知识的最小单元是"类型化资源",不是"步骤"

"nginx 服务在跑"、"这个文件内容是 X"、"集群有 3 个 control-plane"——知识的自然形状是
**资源(名词)**,每种资源类型有五个动词:

```
observe()   现实是什么(只读探针 → 支撑 plan/drift,必须实现)
diff()      与期望差在哪
apply()     弥合
destroy()   退役
upgrade()   版本迁移(可选,声明式升级路径)
```

Ansible 的模块只有 `apply()`(check-mode 是可选二等公民)——所以它永远给不了可信的 plan
和漂移检测。**把五动词做成模块的强制契约**,plan/drift/teardown/升级就不是功能,是推论。

### 不变量三:程序性(dance)不可消除,但必须与状态分离

运维现实里有真正的过程知识:kubeadm init → join、drain → 换二进制 → uncordon、滚动重启。
Terraform 假装它不存在(所以没人用 TF 管升级);Ansible 把一切都当过程(所以没有状态)。
**最优解是分离**:资源声明"应该是什么",**procedure(过渡程序)**声明"从 A 态到 B 态怎么
安全地走"——procedure 是资源类型的一部分(k8s-cluster 资源的 upgrade procedure),不是
用户散写的脚本。用户说 `version: 1.36 → 1.37`,dance 是被调用的,不是被编写的。

### 不变量四:交付单元是"密封闭包",身份即内容

air-gap 教给我们的普适真理:一个可交付单元 = 配方 + 全部依赖物料,内容寻址、可签名、可
搬运(这是 Nix 的洞察 + OCI 的分发)。密封闭包不只服务离线——它同时给出**可复现交付**
(同一制品到处部署结果一致)、**供应链审计**(SBOM 免费)、**环境提升**(dev→prod 提升的
是同一个 digest,不是"同一个分支")。离线是副产品,密封才是本体。

### 不变量五:作者和操作者是两种人(Ansible 最大的产品级错误)

Helm 分 chart 作者 / values 用户;App Store 分开发者 / 消费者。**Ansible 不分**——每个用户
都直面 task 级 YAML,于是每个企业都在重复编写质量参差的 playbook,专家知识无法产品化沉淀。

我们场景里这两种人格外分明:
- **作者**(专家,少数):把"怎么装 HA k8s"的知识封装成可复用、可验证、带离线闭包的包;
- **操作者**(交付工程师,多数):选包、填 values、看 plan、批准、观察。**操作者永远不该
  看见 task**。

推论:产品的主界面是操作者体验("Helm/应用商店级体验,但作用到裸机层"),作者体验
(DSL)是生态供给侧。这是对"如何超越 ansible"最根本的回答:**Ansible 卖动词(playbook),
我们卖名词(可安装的系统)**。

## 3. 对象模型(名词系统)

从五个不变量推导出的概念集,总共七个名词:

```
Substrate   基底:一台机器 / 一个既存集群 / 一个云账号 / 一个 registry(带凭据)
Resource    类型化状态单元(五动词契约):file / service / pkg / container / cluster-node /…
Blueprint   包:一个子系统的完整知识 = 资源模板 + procedures + 离线闭包(materials)
            + 参数契约(values schema)+ 健康定义。内容寻址,OCI 分发。≈ chart+role+SBOM
Stack       组合:一套环境的装配声明 = blueprints × 拓扑约束 × values。(≈ 一张订单)
Environment 环境:substrates 清单 + 环境 values + 凭据 + 策略(审批规则/维护窗口)
Deployment  部署实例 = Stack × Environment 的绑定,携带全生命周期状态机:
            planned → applied → verified → drifted → upgrading → retired
Run         一次执行记录(plan 或 converge),append-only 事件流,审计与回放的最小单元
```

关系:`Deployment = Stack(Blueprints…) × Environment`,引擎围着 Deployment 转对账循环。
所有接口(CLI/UI/API/MCP)都是这七个名词的视图——**不存在"只有 CLI 能做的事"**。

这个模型自然长出一个 Ansible 给不了的东西:**环境数字孪生**。因为 observe() 是强制契约、
Run 全记录,数据库里始终有"哪个环境部署了什么版本、现实与期望差多少"的实况——
"所有环境里 openssl 什么版本?哪些集群还在 1.35?"变成一句查询。这既是企业刚需
(资产/合规),也是 AI 操作的事实基础(AI 查孪生提准确提案,而不是猜)。

## 4. 三种人的产品体验(从体验倒推,而不是从实现顺推)

**操作者(主体验,产品的脸)**:
```console
$ x install k8s-ha --env prod            # 从库里拿 blueprint(本地/OCI registry/air-gap 文件)
? control_plane 选哪些机器 → [n1,n2,n3]   # 交互式满足参数契约(或 -f values.yaml)
? enable_monitoring → false               # ← 闭包变体在此决定(见 next-gen §5.1)
plan: +38 ~2 资源,3 台机器,需要物料 1.2GB(air-gap 完备 ✓)
approve? y
[████████░░] converging… done: 40 applied, 0 failed
$ x status --env prod                     # 孪生视图:期望 vs 现实,健康,漂移
$ x upgrade k8s-ha --to 1.37 --env prod   # 调用 blueprint 内置的升级 procedure(dance)
```
全程没出现 task、playbook、inventory 语法——出现的是**产品、环境、计划、批准**。

**作者(供给侧)**:编写 blueprint = next-gen.md §4 的 DSL 场景(资源声明 + procedures +
materials + values schema)。作者需要的是好的语言、lint、`--record` 闭包采集、测试骨架
(`x dev test` 起容器验收五动词契约)。

**AI(第三种用户,一等公民)**:MCP 暴露的不是"执行命令",是七名词的**查询与提案**:
读孪生(事实)→ 提交 Stack/values 变更(提案)→ 引擎 lint+plan(确定性校验)→ 人批准
(策略可放行低危)。AI 的第二个位置在供给侧:从上游文档半自动生成 blueprint 草稿,
作者审校——生态冷启动的加速器。

## 5. 与 next-gen.md 的关系:换地基,不换楼

| next-gen.md 的结论 | 白纸推导后的修正 |
|---|---|
| ansible 形状 DSL(§4) | **保留,但降位**:那是作者层前端;操作者层是 install/values/plan/approve,产品的脸是后者 |
| IR 是契约、语法是前端(§4.6) | **升格**:IR 就是资源图 + 五动词,不只是"解析产物" |
| 模块四层(§6) | 不变,但模块契约从"实现 apply + 尽量给探针"**收紧为五动词强制契约** |
| plan/state/对账(§5) | 不变,从"特性"升格为"核心循环" |
| OCI 闭包(§5.1) | 不变,从"离线方案"升格为"交付单元本体"(密封才是本体,离线是副产品) |
| server/UI/MCP(§7) | 不变,UI 的信息架构围绕七名词而非"任务列表" |
| 分期(§10) | P0 需前置一件事:先冻结七名词与五动词契约(IR),再动 DSL——名词错了后面全错 |

## 6. 诚实的风险:模型驱动的采纳陷阱

纯模型驱动的先例大多曲高和寡(mgmt、System Initiative 的早期、各种"声明式运维平台"):
模型越优雅,离一线运维的 shell 直觉越远。三条逃生舱**必须**保留,且不以羞耻感设计:
1. `shell` 是合法资源类型(带 check 探针即可入模型)——专家知识总是先以脚本形态存在,
   产品要接住它,再引导它渐进类型化(shell → procedure → 资源);
2. 单文件快乐路径:一个 YAML + 一条命令就能跑(不强迫先理解七个名词——Blueprint/Stack/
   Environment 在单机小场景可以全部隐式);
3. `import`(ansible 转换器)让存量知识低成本进门。
增长路径 = "从一个 shell 步骤开始,永远不撞墙":小用法是大模型的严格子集,而不是两套心智。

---

**一句话总结**:从白纸推导,这个产品不是"更好的 Ansible",而是
**"环境的包管理器 + 对账引擎"——把 Helm 的作者/用户分离、Nix 的密封闭包、Terraform 的
plan、K8s 的对账循环,合并作用到 OS/裸机层,单二进制交付,AI 可全程操作**。
Ansible 形状只是给作者和存量迁移者的一扇门,不是房子本身。
