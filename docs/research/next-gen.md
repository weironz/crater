# 下一代交付引擎:调研 · 技术选型 · 架构设计

> 2026-08-28。本文**抛开 crater 现有代码**,从零回答:如果要做一个集 Ansible(配置管理)、
> IaC(状态/预演)、CI/CD(流水线/审批)于一身,单二进制、CLI + Web UI + API + AI/MCP
> 全接口的工具,应该怎么设计。主场景:平台/中间件/云资源的部署交付管理,复杂 k8s 集群
> 生命周期管理。最后一节回答"crater 现有资产怎么办"。

---

## 1. 目标画像

一句话:**"Terraform 的心智(plan/apply/状态) × Ansible 的作用域(SSH 到裸机装万物)×
Zarf 的交付(air-gap OCI 制品)× Semaphore 的形态(单二进制自带 UI/API)× AI-native
(MCP 一等公民)"**。

| 维度 | 要求 |
|---|---|
| 依赖 | 控制端单静态二进制;目标机只需 SSH + POSIX shell,零 agent 安装 |
| 接口 | CLI / Web UI / REST API / MCP 同权,同一个二进制 |
| 心智 | plan → apply 二段式;幂等;漂移检测;审批流 |
| 交付 | 在线/离线同一份声明;air-gap = 一个 OCI 文件 |
| 生态 | 模块可扩展,且**不牺牲单二进制**(这是最难的一条,见 §6) |
| AI | 生成候选 + 引擎确定性校验;MCP 让任意 agent 驱动整个生命周期 |

## 2. Ansible 的缺陷清单(要超越的对象)

1. **运行时依赖重**:控制端 Python + pip 生态;目标机也要 Python。气隙/信创/最小化系统全是坑。
2. **慢**:每 task 一次 SSH 往返 + 目标机上解包执行 AnsiballZ(python zip)。pyinfra 宣称同场景快 6 倍,自举 agent 模式差距更大。
3. **YAML 被 Jinja2 变成了图灵完备编程语言**:`when: "{{ ... }}"` 字符串里写逻辑,不可静态分析、错误运行到那一行才爆、模板套模板调试地狱。这是 Ansible 被骂最多的一点。
4. **无状态、无 plan**:check mode 是二等公民(很多模块不支持),没有 Terraform 式的"部署过什么、现实漂移了没有"。
5. **无 UI/API**:AWX/AAP 是另一套沉重系统(k8s 上跑一堆容器);Semaphore UI 的流行恰恰证明了这个空白。
6. **无原生离线**:air-gap 自己想办法(预下载 + 私搭源,kubespray 模式)。
7. **变量系统失控**:22 级优先级,没人背得全。
8. **生态质量参差**:collections 数千模块,但实证统计(对 Galaxy 1.8 万个 role、31.7 万个 task 的分析)显示 **74 个模块就覆盖 80% 的 role 用量**,中位数一个 role 只用 6 个模块——"千模块"是长尾,不是刚需。
9. **无回滚/生命周期**:装上去就不管了;卸载、升级、故障自愈全靠自己再写 playbook。
10. **秘密管理粗糙**:vault 只有文件级加密,无细粒度、无审计。

## 3. 竞品全景

### 3.1 配置管理(SSH 层)

| 工具 | 语言 | 状态 | 关键启示 |
|---|---|---|---|
| Ansible | Python | 事实标准 | 心智模型(play/role/inventory/幂等回显)值得 1:1 继承 |
| Salt | Python | 衰退 | agent 模式速度快,但运维 agent 本身成为负担 → 坚持 agentless(或自举临时 agent) |
| pyinfra | Python | 活跃小众 | 两段式(先探测再执行)带来免费 dry-run + 6 倍速;目标机只需 POSIX shell |
| **JetPorch** | **Rust** | **已死(2024)** | **最重要的一课**:Ansible 之父 Michael DeHaan 亲自用 Rust 重写 Ansible,一年后因"没有外部热度"停更。结论:**"Rust 版 Ansible"本身不构成产品,必须有 Ansible 做不到的事**(离线交付、状态/plan、UI/API 一体) |
| mgmt | Go | 学术 | 事件驱动实时收敛,太超前,无生态 |

### 3.2 IaC(云资源层)

| 工具 | 启示 |
|---|---|
| Terraform/OpenTofu | plan/apply/state/graph 的心智已是行业通识,必须继承;provider 生态(数千云资源)通过 gRPC 子进程协议隔离——协议可被非 Go 语言对接(已有 Rust `tf-provider` 库先例) |
| Pulumi | 证明了"桥接 Terraform provider 白嫖生态"是可行的商业策略 |
| Crossplane | 云资源进 k8s CRD——依赖先有 k8s,和我们"从裸机建出 k8s"的定位互补而非竞争 |
| System Initiative | 2025 发布"AI-native 基础设施自动化":数字孪生 + AI agent 提议变更 + 审批执行。验证了"AI 副驾 + 确定性引擎审批"方向,但它是 SaaS 云端产品,不做 air-gap——正好错开 |

### 3.3 UI/服务层

| 工具 | 启示 |
|---|---|
| AWX/AAP | 反面教材:UI 做成了独立的重型系统 |
| **Semaphore UI** | **正面教材**:Go 单二进制,内嵌 UI + REST API + RBAC + 定时任务,把 Ansible/Terraform/脚本当"执行类型"统一管理。热度很高,证明"单二进制自带 UI"有真实市场。但它只是**外壳**——底下还是要装 Ansible/Python。我们的机会:引擎和壳是同一个二进制 |
| Rundeck | 作业编排 + 审批流的功能参考 |

### 3.4 交付 / air-gap

| 工具 | 启示 |
|---|---|
| **Zarf**(CNCF) | 与 crater 离线故事最接近的先例:声明式打包 Helm chart + 镜像 + 清单成单个 OCI 制品,air-gap 一个 tar 进场,还往集群里注入临时 registry。验证了"OCI 制品 = air-gap 交付单元"。但 Zarf 只管 **k8s 之上**(前提是集群已存在),不管"从裸机把 k8s 建出来"——正好是我们的层 |
| sealos | clusterimage(集群镜像)思路同源 |
| kubespray | air-gap = 给你下载清单 + 自建源,是我们要淘汰的模式 |

### 3.5 CI/CD / 工作流

Dagger(pipeline as code + 容器化执行)、kestra/Windmill(声明式工作流 + UI)、Temporal(可靠执行)。
结论:**不做通用 CI**(不抢 GitLab CI 的活),要的是 CI/CD 的这几个要素:流水线(有序阶段 +
人工审批门)、事件触发(webhook/cron)、执行历史与日志流、环境提升(dev→staging→prod)。
这些恰好是"server 模式"的功能面,不需要独立产品。

### 3.6 市场空白(定位结论)

把五类工具摆在一张图上,**没有任何工具同时占据这四格**:

```
                 有状态/plan          无状态
   OS/裸机层   │  (空白!)      │  Ansible/Salt/pyinfra
   k8s之上     │  Helm+Argo     │  Zarf(交付)
   云API层     │  TF/Pulumi/SI  │  ansible cloud modules
   ────────────┼────────────────┼──────────────
   air-gap交付 │  (空白!)      │  Zarf(仅k8s之上)
   自带UI/API  │  SI(SaaS)      │  Semaphore(外壳)
```

**"OS/裸机层 + 状态/plan + air-gap OCI + 单二进制 UI/API/MCP"是无人区**,且每个单项都有
成功先例背书(TF 背书 plan、Zarf 背书 OCI、Semaphore 背书单二进制 UI、SI 背书 AI-native)。
这就是"集三者优点于一身"的具体落点——不是堆功能,是占住这个交叉点。

---

## 4. DSL 设计:优雅度是第一道生死关

### 4.1 病根诊断

当前 crater YAML 啰嗦的三个原因:
1. `action: copy` 内部标签——每步多一行,而 Ansible 是**模块名即 key**;
2. `id:` + `needs:` 显式 DAG——Ansible 是隐式顺序,90% 的场景顺序就够了;
3. 条件是闭合枚举字段(`when_os: [debian]`)——正交条件一多,字段爆炸。

### 4.2 解法:Ansible 的形状 + CEL 的表达式

**关键技术决策:用 CEL(Common Expression Language)替代 Jinja2 作为条件/插值表达式。**

CEL 是 Google 为安全求值设计的**非图灵完备**表达式语言:无循环、无自定义函数、可静态
分析、求值有上界。Kubernetes 已全面采用(CRD validation、Admission Policy,1.30 GA),
运维人群不陌生。Rust 有现成实现(`cel` / `cel-interpreter` crate,另有 `kube-cel`)。

这一步同时解决两个问题:
- 甩掉 Ansible"Jinja2 图灵完备"的历史包袱(**这正是 D-036 想守的东西,但 CEL 让你不用
  为了守它而牺牲表达力**);
- `when_os`/`when_role`/`when_offline` 三个字段收敛成一个 `when:`,以后再也不用为新条件加字段。

### 4.3 新 DSL 示例

```yaml
# rustfs.yaml —— play(= ansible play,含 hosts)
name: rustfs
hosts: storage
vars:
  version: 1.0.0-beta.5
  port: 9000

materials:                              # 保留:离线闭包声明(核心差异化)
  - name: bin
    file: https://dl.rustfs.dev/v${version}/rustfs-linux-{arch}
    sha256: "…"

tasks:                                  # 模块名即 key,隐式顺序,ansible 老手零学习
  - file: { path: /data/rustfs, state: directory }

  - name: 安装二进制                     # name 可选,纯注释
    copy: { material: bin, dest: /usr/local/bin/rustfs, mode: "0755" }

  - template: { src: rustfs.env.j2, dest: /etc/rustfs/env }
    notify: restart rustfs

  - service: { name: rustfs, state: started, enabled: true }
    when: os.family == "debian" || os.family == "rhel"    # CEL,非 Jinja 字符串

  - wait_for: { port: "${port}" }
    retries: 3

  - shell: rustfs --version             # 自由形式短写法,同 ansible
    check: test -x /usr/local/bin/rustfs

handlers:
  - name: restart rustfs
    service: { name: rustfs, state: restarted }
```

对比:同样内容当前 crater 写法约多 40% 行数(每步 `id:`+`action:`+`needs:`);Ansible 写法
行数相当,但 `when:` 是不可分析的 Jinja 字符串且没有 `materials`/离线能力。

解析实现要点(serde):一个 task 条目 = map,**步骤级关键字**(`name/when/loop/notify/
register/retries/check/tags/throttle/run_once/needs`)先剥离,剩下**恰好一个 key** 即模块名,
value 即模块参数(dict 或自由形式标量)。未知模块名/未知关键字 → **解析期报错**(而非
Ansible 的运行期);拼错参数名 → `serde(deny_unknown_fields)` 直接指出。这是"静态可分析"
落到用户体验上的样子——`plan` 之前就能把整个仓库 lint 一遍。

### 4.4 分层(1:1 对齐 Ansible 概念)

| Ansible | 新设计 | 说明 |
|---|---|---|
| module | module | 内建 Rust / WASM / 外部进程(§6) |
| task | task(module 调用 + 步骤关键字) | 隐式顺序;`needs:` 保留为**可选**进阶(跨 host 编排、并行优化时才用) |
| role | **role 目录**:`roles/<name>/{role.yaml, files/, templates/}` | `role.yaml` 含 defaults/params/tasks/handlers/**materials**(离线闭包挂 role——这是对 ansible role 的超集) |
| play | play(`hosts:` + `roles:[]` 和/或 `tasks:`) | 支持 `roles: [common, {role: k8s-ha, vars: {...}}]` 列表语法 |
| playbook | **一个 YAML 文件 = play 列表**(顶层数组,同 site.yml) | 现有 project `plays:[{source:...}]` 间接层取消,可直接内联 |
| inventory | 同 Ansible(hosts/groups/group_vars/host_vars) | 已基本对齐 |
| `-e` / vault / tags / `--limit` | `--set` / SOPS·age 集成 / tags / `--limit` | 补齐 |

变量优先级压到 **5 级**(CLI --set > play vars > host > group > role defaults),文档一句话讲完
——对 Ansible 22 级的直接回应。

### 4.5 为什么不做"语法级 100% 兼容"(能直接跑现成 playbook)

- 等于用 Rust 重写 ansible-core + Jinja2 + 模板求值语义,工程量 10 倍;
- 兼容了语法也兼容不了生态(数千 Python 模块),用户实测"基本能跑"和"完全能跑"之间是无底洞;
- JetPorch 已用生命验证:形似 Ansible 不构成产品。
- **替代方案:做 `import` 转换器**——`x import playbook.yml` 把常见 Ansible playbook/role
  静态转换成新 DSL(74 个高频模块覆盖 80% 的现实 role;转不动的地方注释标出人工改)。
  迁移成本"一次转换"远低于"永久维持运行时兼容",还能借 AI(MCP)半自动完成。

### 4.6 选型空间:为什么是 YAML+CEL,而不是 HCL / KYAML / KCL / CUE

判断标准先行:playbook 的本质是**有序步骤序列**(命令式骨架 + 每步声明式),不是资源树。
这正是当年 Ansible 选 YAML、Terraform 选 HCL 的根本分野——两者都对,因为负载不同。我们
的主负载是前者;"生成大量配置"的抽象需求(KCL/CUE 的强项)在本设计里由 role + vars +
materials 承担,不需要语言级抽象。

| 候选 | 是什么 | Rust 可嵌入 | 结论 |
|---|---|---|---|
| YAML(ansible 形状) | 数据格式 | serde_yaml,成熟 | **主语法**。运维受众零成本、AI 训练语料最多;脚枪用纪律修(见下) |
| **KYAML** | k8s 官方定义的 YAML 严格子集(KEP-5295,1.37 已 GA):必花括号/引号、无空白敏感、无锚点,任何 YAML 解析器可读,支持注释和尾逗号 | 就是 YAML | **采纳,但用在机器侧**:UI 保存、`fmt`、`import` 转换器输出、state 导出的**规范输出格式**(diff 稳定、patch 安全)。人手写仍用块式 YAML——KYAML 的花括号风格牺牲了人读性换机器安全,恰好各归其位 |
| HCL | 块 + 内建表达式,为"命名资源 + 相互引用"的资源图设计 | hcl-rs 可解析,表达式语义需自实现 | 不做主语法:有序异构步骤列表在 HCL 里表达笨拙(`task { copy { … } }` 嵌套噪音),受众也窄(TF 用户写资源,不写步骤)。若 P3 的 TF provider 桥做大,可加为**可选前端**(编译到同一 IR) |
| KCL(CNCF) | 面向配置的编程语言,恰好 Rust 实现 | 可以 | 是编程语言(schema/lambda/抽象),学习成本高;为"批量生成 k8s manifest"而生,不是为步骤序列。放弃 |
| CUE | types=values 的约束语言 | **Go 实现,无法嵌入** | 理念最优雅,但嵌不进 Rust 单二进制,直接出局 |
| Nickel | Rust 的合约式配置语言 | 可以 | 受众为零,教育成本买不回收益 |
| Starlark / Jsonnet / Pkl | Python 子集 / 模板语言 / Apple 配置语言 | starlark-rust 可嵌 | 图灵完备(或事实上),等于把 Jinja 的坟再挖开:失去静态可分析、plan 前置校验、UI 往返编辑。哲学性拒绝 |
| 通用语言 SDK(Pulumi 式) | TS/Python 写部署 | — | 同上,且 AI 生成后引擎无法确定性校验。可作为 P3 之后的**生成器**生态(程序生成 YAML),不作为执行输入 |

**表达式层为什么锁死 CEL**(与上表正交——CEL 只出现在值位置,永不做控制流):

| 候选 | 淘汰理由 |
|---|---|
| Jinja2 字符串(`when: "{{ x > 3 }}"`) | 字符串里藏程序:不可类型检查、运行到才爆、模板套模板。Ansible 最大的坑,本设计的头号敌人 |
| Rhai / Lua 嵌入脚本 | 图灵完备,"YAML 变程序"换个方言重演 |
| HCL 表达式引擎 | 与 HCL 语法绑定,单拆出来没有生态 |
| Starlark | 整个 Python 子集,对"一行条件"杀鸡用牛刀,且允许定义函数 → 又是程序 |
| **CEL** | 非图灵完备、求值成本有上界、**编译期类型检查**(`os.family` 拼错在 lint 期报,不用连目标机)、k8s GA 背书(受众已在学)、Rust 实现成熟(cel / kube-cel) |

配套纪律:
- `${…}` 插值与 `when:` 用**同一个 CEL 求值器**(`${version}` 就是 CEL 表达式的插值语法),
  全工具只有一种表达式语义,不搞"插值一套、条件一套";
- YAML 解析锁 1.2 严格模式 + lint:禁自定义 tag、禁 merge key、锚点仅 lint 警告,Norway
  problem(`no` → false)在 1.2 + schema 强类型下天然消失;
- **IR 才是契约,语法只是前端**:解析后的类型化模型(IR)发布 JSON Schema,所有前端
  (YAML 今天、HCL/生成器未来)编译到同一 IR,所有后端(plan/UI/MCP/import)只认 IR。
  这条把"DSL 选错了怎么办"从生死题降级为"再写一个前端"的工程题。

---

## 5. 执行模型:把 Terraform 心智装进 SSH 世界

```
 声明(plays + roles + inventory)
      │ parse + lint(静态,毫秒级)
      ▼
 Resource Graph(每个 task 实例 = 一个资源节点,host × task 展开)
      │
      ├─ plan:只跑只读探针(SSH 上执行 stat/systemctl is-active/…)
      │        → ✓ok / ~would-change / +create / -destroy / ?unknown
      │        (云资源节点:调 provider 的 Read/Diff)
      ▼
 apply:自举 agent 模式(把自身推到目标机本地执行,消灭逐 task SSH 往返)
      │  幂等契约:check → act → report(ok/changed/failed,ansible 式回显)
      ▼
 State:部署记录 + 指纹(嵌入式 SQLite)
      │
      └─ 生命周期:verify(漂移检测)/ heal / upgrade / destroy(逆序 teardown)
```

要点:
1. **plan 是一等公民**:所有内建模块**必须**实现只读探针,不是可选(Ansible check mode 的
   教训)。这是"每个模块多写 30 行 Rust"换来的核心卖点。
2. **状态分两种**:SSH 资源的"真状态"永远以目标机现实为准(state 只是缓存 + 历史),避免
   Terraform state 漂移地狱;云 API 资源(无处可探时)才依赖 state 文件。
3. **执行默认顺序、并发按 host**(ansible 心智);`needs:` 显式声明后引擎自动提升为 DAG 并行
   (crater 已验证的 3-master k8s 编排能力保留为进阶特性)。`serial`/`throttle`/`run_once`
   照搬 ansible 语义。
4. **生命周期完整**:apply 不是终点——`verify`(定时漂移检测,server 模式后台跑)、
   `upgrade`(role 声明升级路径)、`destroy`(teardown 逆序)、审批门(server 模式)。
   这是"CI/CD 特性"的真正落点:环境 → 流水线 → 审批 → 执行历史。

### 5.1 离线闭包:可判定性分级与对策(向 Zarf 学什么、补什么)

"要打包什么"是 air-gap 的核心难题:依赖套依赖(apt 树)、镜像藏在渲染结果里(helm
values 决定拉什么)、operator 运行时才拉镜像——**闭包发现在一般意义上是不可判定的**
(helm 模板图灵完备、安装脚本任意联网)。所有成功者(Zarf/Nix)的共同答案不是"更聪明的
静态分析",而是**把问题换掉**:声明是唯一事实源 + 发现只是辅助 + 运行时收口让遗漏响亮
地暴露 + 迭代收敛。

**Zarf 的三段式解法**(实证过的参照系):
1. **build 侧——显式声明,辅助发现**:`zarf.yaml` 里 images/repos/charts 全部显式列出;
   `zarf dev find-images` 用"**渲染后扫描**"辅助生成清单——把 helm chart 按**部署时将用的
   values** 真渲染一遍再抽取镜像引用(而非静态猜),`helm.sh/images` 注解补充 operator 类
   声明。它不保证全,只保证"你声明的=你打包的",并给 SBOM(syft)审计。
2. **deploy 侧——运行时收口(这是 Zarf 最精彩的部分)**:先用 injector 破解鸡生蛋(把
   registry 镜像切块塞进 configmap + 一个静态 Rust 二进制拼回并起临时 pull-only registry,
   引导出正式内部 registry);再用 **mutating webhook(zarf-agent)把所有 Pod 镜像引用、
   Flux/ArgoCD 的 git/helm 源改写到内部 registry/Gitea**——manifest 零修改,而任何**没打包
   的镜像会在内部 registry 上 404,立刻、明确地失败**,而不是静默去公网拉。
3. **分发侧**:包=OCI 制品,可 publish/pull,cosign 签名。
Zarf 的边界:只管 k8s 之上(假设集群已存在),不管 OS 包/裸机——正好是我们的层。

**依赖形态的可判定性分级与逐形态对策**:

| 形态 | 可判定性 | 对策 |
|---|---|---|
| 二进制/文件 | 完全可判定 | `url + sha256` 显式声明(现状,够了) |
| apt/yum 闭包 | **可判定,但必须钉死解析环境** | 在目标 OS×版本的容器里(buildah)解析依赖树打全 .deb/.rpm——crater D-062 已走对;错误做法是在控制机上猜 |
| pip/npm | 同上 | 同容器内 `pip download --platform` 锁 wheel;lockfile 即闭包声明 |
| 容器镜像(manifest 直书) | 静态可发现 | `discover` 渲染扫描 |
| helm chart(values 决定镜像集) | **条件可判定:闭包=f(values)** | 铁律:**发现时的 values 必须=部署时的 values**。变体(监控开/关)= materials 带 `when:` CEL 条件的**flavor**,同一制品可含多变体层(内容寻址自动去重),plan 期按 values 选子闭包 |
| operator 运行时拉镜像 / 安装脚本任意联网 | **静态不可判定** | 只能靠**记录模式 + 运行时收口**(见下) |

**在 Zarf 之上,我们补两件事**:

1. **记录模式(record mode)——把不可判定问题变成经验问题**。`x build --record`:在有网的
   构建/CI 环境里把部署**真跑一遍**,所有出网流量过捕获代理(registry pull-through cache +
   apt/pip/https 代理),记录实际拉取的每个 URL/镜像/包 → **自动生成/校对 materials 清单**
   (与声明 diff,揪出漏报和多报)。这是对"依赖套依赖梳理不清"的终极答案:不靠人梳理,
   靠一次真实运行采集。(kubespray 的 generate_list 是它的手工版,Nix 的封闭构建是它的
   理想版——我们取中间:经验采集 + 内容寻址锁定。)
2. **运行时收口推广到非 k8s 层**(Zarf 只有 webhook 改写 Pod):air-gap apply 时,目标机的
   apt 源指向控制机推送的本地仓、pip index 指向本地、镜像拉取指向临时 registry——**全部
   出网口都被收口**,任何遗漏 = 立刻显式失败 + 记入报告,反馈回 materials 声明,下一轮
   build 补上。闭包不是一次做对的,是**收敛**出来的;工具的职责是让每次 miss 都响亮、
   可归因、可自动回填。

k8s 之上的能力(内部 registry 引导、镜像改写)不必自研到 Zarf 深度:P1 先做"临时 registry +
containerd mirror 配置"(裸机装集群时我们本来就控制 containerd 配置,比 webhook 更早、更简单
的收口点);真到"往已存集群交付应用"的场景,直接**集成/内嵌 Zarf 包格式**也是选项。

## 6. 插件生态:千模块问题的四层答案(本设计最关键的原创点)

实证数据先行:Galaxy 31.7 万 task 的统计里,**74 个模块覆盖 80% 用量,中位 role 只用 6 个
模块**。所以"上千插件"是心理安全感需求,不是工程需求——策略是**核心做深,长尾开门**:

```
 L1  Rust 内建 ~70 个       编译进二进制。file/copy/template/service/package/user/git/
     (覆盖 80% 场景)        lineinfile/unarchive/docker_*/wait_for/sysctl/mount/cron/…
                            每个都带:只读探针(plan)+ 幂等 + teardown。质量即卖点。

 L2  数据模块(YAML)        role 即模块:纯 YAML 组合 L1,零编译、放目录即生效。
     (领域知识层)          "装 nginx/mysql/k8s" 全在这层,官方库 + 社区仓库分发,
                            OCI 制品化(带离线闭包)。≈ ansible galaxy role 的位置。

 L3  WASM 插件              真正需要新原语逻辑时:任意语言(Rust/Go/JS/Python→wasm)
     (社区扩展层)          写模块,Extism/wasmtime 加载,沙箱+资源限额,单 .wasm 文件
                            OCI 分发,不破坏单二进制承诺。插件在【控制端】跑,产出
                            仍是"要在目标机执行的原语序列"——插件生成计划,引擎执行,
                            保住确定性与安全审计。

 L4  外部进程桥             ① Terraform/OpenTofu provider 桥:gRPC 子进程协议是公开的,
     (胖生态层,可选)          Rust 侧已有 tf-provider 先例 → 数千云资源生态直接可用
                              (Pulumi 验证过这条路),这是"IaC 特性"最便宜的获得方式。
                            ② Ansible 模块垫片:ansible 模块本质=目标机上执行的自包含
                              脚本+JSON 协议;目标机有 Python 时可直接投放执行现有
                              collection 模块。标记为"兼容模式"(无 plan 探针、性能降级),
                              只作迁移期逃生门。
```

原则:**L1/L2 解决 95% 的真实需求且质量可控;L3 让"扩展"不等于"改引擎重编译";L4 用协议
桥接白嫖两大生态而不背包袱**。任何一层都不把 Python/Go 运行时带进控制端二进制。

## 7. 形态架构:library-first,一个二进制四种脸

```
┌────────────────────────── 单静态二进制(musl)──────────────────────────┐
│                                                                        │
│  cli(clap)      serve(axum)         mcp(rmcp)        agent(自举)   │
│    │                │  ├─ REST API(utoipa→OpenAPI)     │              │
│    │                │  ├─ SSE:任务日志流/事件           │              │
│    │                │  ├─ 内嵌 Web UI(静态资源 embed)  │              │
│    │                │  └─ RBAC/token/审计               │              │
│    └────────┬───────┴──────────┬───────────────────────┘              │
│             ▼                  ▼                                       │
│   ┌─────────────────────────────────────────────┐                     │
│   │ core(lib):parse/lint → graph → plan →     │  ← 所有接口共用,   │
│   │ apply → state;module registry(L1–L4);    │    CLI 只是薄壳     │
│   │ OCI store(build/save/load/push/pull)      │                     │
│   └─────────────────────────────────────────────┘                     │
│             ▼                                                          │
│   executor:russh(agentless)/ 自举 agent(推自身到目标机)          │
│   state:Store trait → SQLite(单机默认)/ Postgres(HA 多实例,§7.1)│
└────────────────────────────────────────────────────────────────────────┘
```

- **CLI-only 与 server 不是两个产品**:`x apply` 本地直跑(无 daemon);`x serve` 起来后,
  同一份 core 多了持久化历史、定时漂移检测、审批流、多人 RBAC。渐进升级,像 SQLite→server。

### 7.1 存储与高可用(双后端是硬要求)

先算负载:1000 台主机 × 50 步的 play 并发跑 = 5 万条 step 结果,摊在几分钟内 → 峰值每秒
几百条写入;真正先到极限的是 SSH 并发与目标机,不是 DB。所以存储选型的驱动因素**不是
吞吐,是部署形态**:单机零依赖(SQLite)vs 控制面 HA 多实例(必须共享外部 DB)。

```
 本地/单机模式                     HA/分布式模式(P2+)
 ┌─────────────┐                  ┌──────────┐  ┌──────────┐
 │ x (单二进制) │                  │ x serve  │  │ x serve  │ … N 实例
 │  Store──────┼─ SQLite(WAL)     │  Store───┼──┼──Store───┼─→ Postgres
 │  logs ──────┼─ 追加文件         │  logs────┼──┼──logs────┼─→ 对象存储/共享盘
 └─────────────┘                  └────┬─────┘  └────┬─────┘
                                       └── 任务队列/租约/选主:全用 Postgres
                                           (SKIP LOCKED 队列 + advisory lock 选主
                                            + LISTEN/NOTIFY 推事件)──不引入
                                           Redis/etcd,保住"除 PG 外零依赖"
```

四条设计纪律:
1. **Store trait 收口**:state 访问全走 repository 接口,业务代码不碰 SQL;`sqlx` 双后端
   (SQLite/Postgres)从 P1 起同仓 CI 双跑,而不是"留个门以后再说"——双后端不同期建设,
   后补的那个永远是二等公民。
2. **SQL 保持朴素**:不用任何单方言特性;迁移用编号 SQL 文件,两个后端各一份等价迁移。
3. **日志不进 DB**:任务日志(数据大头)走追加文件(按 run 内容寻址),DB 只存元数据 +
   偏移索引;SSE 直接 tail。单机存本地盘,HA 模式指向共享存储/对象存储(S3 兼容接口,
   air-gap 场景恰好总有 MinIO/RustFS)。DB 里永远没有大 blob。
4. **SQLite 侧单写者纪律**:专职写者 task + mpsc channel,天然批量化,规避 `SQLITE_BUSY`;
   Postgres 侧同一接口自然并发。

HA 语义:多实例**无主对等**,靠 Postgres 原语协调——执行任务进 `SKIP LOCKED` 队列被任一
实例认领(执行器天然水平扩展);定时任务(漂移检测/cron)由 advisory lock 选主触发;
实例挂掉,租约超时后任务被其他实例接管重跑(幂等契约保证重跑安全——这里引擎的幂等
设计和 HA 设计互相成全)。先例:Semaphore/Gitea/Grafana 均为"默认嵌入式、生产 Postgres";
队列不引入 Redis 是 GitLab CI runner 反例换来的教训——协调状态越少,air-gap 交付越简单。
- **API-first**:CLI 的每个动作都对应 core 的一个 public fn,serve 只是把它们暴露成 REST。
  UI/MCP/CLI 三者永远等能力,不会出现"这功能只有 CLI 有"。
- **MCP 内建**:`x mcp` 暴露 plan/apply/inspect/state 查询等工具 → 任意 AI agent(Claude 等)
  可以安全驱动:AI 只能提交声明,引擎 lint + plan,人(或策略)批准才 apply。这就是
  System Initiative 的"AI 提议 + 审批执行"模式,但开源、单二进制、可离线。

## 8. 技术选型表

| 领域 | 选型 | 备选 | 理由 |
|---|---|---|---|
| 语言 | Rust(musl 静态) | Go | 零运行时、单二进制、russh/wasmtime/oci 生态齐;Go 的优势(生态)通过 L4 协议桥获得 |
| 异步 | tokio | — | 事实标准 |
| SSH | russh | openssh 子进程 | 纯 Rust 可静态编译,已验证 |
| 表达式 | **cel**(cel-interpreter) | Rhai/Lua/starlark | 非图灵完备+k8s 背书+可静态分析;Rhai/Lua 会重蹈"YAML 变程序"覆辙 |
| 文件模板 | minijinja | tera | 只用于 `template` 模块渲染配置文件,不进控制流 |
| WASM 插件 | Extism(wasmtime) | wasmtime 裸用/组件模型 | 多语言 PDK 现成、host 函数/限额/HTTP 管控开箱即用 |
| Web 框架 | axum + utoipa + SSE | actix | tokio 官方系;utoipa 自动出 OpenAPI |
| UI | 独立 SPA(Svelte/Solid + TS),构建产物 embed 进二进制 | htmx(现状)/ Leptos | 复杂看板(graph 可视化、日志流、diff 视图)超出 htmx 舒适区;全 Rust 前端(Leptos)生态成本高。SPA 构建产物 `include_dir!` 嵌入,不破坏单二进制 |
| 状态存储 | **sqlx 双后端:SQLite(默认)+ Postgres(HA 必选项)** | rusqlite 单后端/sled | 单机零依赖靠 SQLite(WAL);HA 多实例共享状态、任务队列(SKIP LOCKED)、选主(advisory lock)、事件推送(LISTEN/NOTIFY)全靠 Postgres,不另引 Redis/etcd。Store trait 收口,双后端 CI 同期双跑(§7.1) |
| OCI | oci-client(oras 系) | — | 已验证(协议靠库、制品语义自写) |
| MCP | rmcp(官方 Rust SDK) | 自写 JSON-RPC | 官方维护 |
| 秘密 | age 加密 + SOPS 兼容读取 | vault 协议 | 离线友好;server 模式再加 KMS 接口 |
| CLI | clap | — | 现状即可 |

## 9. 风险与现实检查

1. **范围爆炸是最大死因**。"CM+IaC+CI/CD+UI+AI"每个词都是一家公司。解法:分期,且每期
   独立可用、独立有卖点(见 §10);IaC 走 provider 桥而非自研 provider;CI/CD 只做交付
   流水线不做构建农场。
2. **JetPorch 教训**:技术情怀(Rust 重写)不构成需求。每一期的卖点必须是 Ansible **做不到**
   的事:离线 OCI 交付、真 plan、单二进制 UI/API、MCP。
3. **DSL 迁移成本**:现有 library/(k8s-ha、rustfs、zot…)要跟着改写——但这些 YAML 总量
   不大(几千行),且可写脚本半自动转换。
4. **WASM 插件层可以晚做**:L1+L2 已覆盖主场景,不要为"生态叙事"提前付复杂度。
5. **UI 换 SPA 的成本**:htmx 版继续用到 P2,SPA 与 API-first 重构同期做。

## 10. 对 crater 现有资产的评估(回到现实)

抛开代码调研的结论,恰好**反向验证了 crater 已做对的部分**:

| 现有资产 | 评估 | 处置 |
|---|---|---|
| OCI 制品 store/bundle(内容寻址、闭包 push/pull、瘦拉) | **核心差异化,Zarf 验证了赛道** | 原样保留 |
| 自举 agent + russh 执行器(60KB 分块上传等真机结论) | 性能差异化来源 | 原样保留 |
| `plan`(只读探针预演) | 方向正确,先例(TF)背书 | 保留,升级为"模块必须实现探针"的强契约 |
| 幂等契约 check→act→report、StepStatus | ansible 心智,正确 | 保留 |
| inventory 三级 vars / groups / derive_roles | 已对齐 ansible | 保留 |
| **task/action DSL(`action:` 标签 + id/needs 显式 DAG)** | **啰嗦的病根** | **重做**:§4 的新 DSL(模块名即 key、隐式顺序、CEL when) |
| `when_os/when_role/when_offline` 枚举字段 | 字段爆炸路线 | 收敛为 CEL `when:` |
| role 单文件 / project `plays:[{source}]` 间接层 | 与 ansible 形状不符 | 重做:role 目录 + playbook 即 play 列表 |
| 14 个模块的 Rust 实现逻辑 | 逻辑可复用 | 保留实现,换新参数 schema 挂载 |
| htmx UI / 后台任务流 | 可用 | P1 前继续用,P2 换 SPA+API |

粗算:**engine/executor/store/bundle(约 60% 代码)保留,parse 层(task/component/project,
约 25%)重写,CLI/UI(15%)渐进改**。所以这不是推翻重来,是"换脸不换心"。

### 分期路线(每期独立交付)

- **P0 · DSL 重生**(最高杠杆):新 parser(模块名即 key、role 目录、playbook 列表、CEL
  `when:`、5 级变量)+ lint 命令 + library/ 迁移。做完,"写起来比 ansible 舒服"成立。
- **P1 · 生命周期闭环**:state 进 Store trait(sqlx 双后端 SQLite/Postgres 同期建、CI 双跑)、
  `verify` 漂移检测常态化、teardown/upgrade 补全、`import`(ansible playbook 转换器)。
  做完,"比 ansible 多状态"成立。
- **P2 · server 形态**:core 提库、axum API + OpenAPI、SPA UI、RBAC/审批/定时、MCP server、
  **HA 多实例**(Postgres 队列/选主/事件,§7.1)。做完,"单二进制 = AWX+Semaphore+MCP,
  且控制面可高可用"成立。
- **P3 · 生态开门**:WASM 插件(Extism)、TF provider 桥、社区 role 仓库(OCI 分发)。

---

### Sources

- JetPorch 停更:[Farewell to JetPorch](https://www.ansiblepilot.com/articles/farewell-to-jetporch-automation) · [项目主页](https://www.jetporch.com/)
- Semaphore UI:[官网](https://semaphoreui.com/) · [AWX vs Semaphore (2026)](https://semaphoreui.com/blog/awx-vs-semaphore)
- Zarf:[GitHub](https://github.com/zarf-dev/zarf) · [OCI publish/deploy](https://docs.zarf.dev/tutorials/6-publish-and-deploy/)
- KYAML:[KEP-5295](https://github.com/kubernetes/enhancements/tree/master/keps/sig-cli/5295-kyaml) · [The New Stack 报道](https://thenewstack.io/kubernetes-is-getting-a-better-yaml/)
- CEL:[K8s CEL 文档](https://www.kubernetes.io/docs/reference/using-api/cel) · [CNCF: K8s policy with CEL](https://www.cncf.io/blog/2025/01/13/cel-ebrating-simplicity-mastering-kubernetes-policy-enforcement-with-cel/) · [kube-cel crate](https://docs.rs/kube-cel)
- Extism:[GitHub](https://github.com/extism/extism) · [Host quickstart](https://extism.org/docs/quickstart/host-quickstart/)
- Ansible 模块用量实证:[The top 100 Ansible modules](https://mike42.me/blog/2019-01-the-top-100-ansible-modules)
- TF provider 非 Go 实现:[non-go-terraform-provider-assessment](https://github.com/smheidrich/non-go-terraform-provider-assessment)
- System Initiative:[InfoQ 报道](https://www.infoq.com/news/2025/09/system-initiative-ai-platform/)
- pyinfra:[官网](https://pyinfra.com/)
