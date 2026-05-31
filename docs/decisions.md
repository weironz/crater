# 决策 / 沟通记录

> ADR 风格，按时间倒序追加。每条记录：背景、决策、理由、影响。
> **每次关键沟通后在此追加。**

---

## 2026-05-31 · 项目立项与 M1 启动

### D-001 项目命名为 crater
- **决策**：工具与项目名定为 `crater`，CLI 主命令 `crater`。
- **理由**：cluster + crate(Rust) 合成，短、好记；终极目标 `crater <target>` 部署一切。
- **影响**：仓库、二进制、文档统一用此名。待办：crates.io / GitHub / npm 占用核查。

### D-002 目标 OS 首批仅 Ubuntu/Debian 系 + RHEL 系
- **决策**：首批支持 Debian/Ubuntu 系与 RHEL 系；国产化 OS（麒麟 Kylin / 统信 UOS）暂不纳入。
- **理由**：先跑通主流；国产 OS 多为两系衍生，按系族归类后续扩展，且需额外处理 SELinux/安全模块/内核差异。
- **影响**：`OsProvider` 按 Debian/RHEL 抽象，留扩展位不堵死。

### D-003 组件用「声明式描述文件」定义
- **决策**：组件以 YAML 描述文件定义，核心是「确定性执行引擎 + 内置动作原语」；内置与第三方组件同构。
- **理由**：第三方扩展门槛低、可热加载、放目录即加载。
- **影响**：核心实现动作原语集（download/extract/render_template/systemd_unit/pkg_install/run_cmd/preflight…）；复杂分支（如 k8s 证书/join）预留 `run_cmd` + 脚本钩子逃生口。

### D-004 AI 能力后置（M4/M5）
- **决策**：M1–M3 先做硬核、可靠的部署能力，AI 放 M4/M5。
- **理由**：先有稳妥可用产品；地基不稳不上 AI。
- **影响**：M1 不依赖 AI；但 spec / 数据结构预留 AI 字段，避免返工。AI 始终是副驾，可 `--ai off` 关闭。

### D-005 MVP 首个组件为 docker/containerd
- **决策**：M1 只打通 docker 的在线安装全链路。
- **理由**：依赖少、是众多组件的基础，适合验证「在线安装 + SSH 执行 + 声明式组件」主路径，不触碰声明式表达力边界。
- **影响**：`components/docker/component.yaml` 作为首个内置组件与狗粮。

### D-006 文档分多部分、持续记录沟通
- **决策**：需求文档拆分多文件、持续增补；关键沟通沉淀到 `decisions.md`。
- **理由**：防止上下文窗口丢失导致需求遗忘。
- **影响**：见 [docs/README.md](README.md) 维护约定；新会话先读 `requirements.md` + 本文件。

---

## 2026-05-31 夜 · M1–M5 自主开发（均真机验证）

### D-007 在线下载交给目标机；控制端 reqwest 仅用于制包
agentless 下在线依赖由目标机自身 curl/apt 拉取;控制端 reqwest 只在 `crater build` 制离线包时拉取。符合 F9,简化 M1。

### D-008 SSH 用 russh 0.45 直连
从 russh-0.45.0 源码确认 API(check_server_key 形参 `&russh::keys::key::PublicKey`,authenticate_password 返 `Result<bool>`)。放弃 async-ssh2-tokio(需联网且 API 仍要猜)。

### D-009 写远端文件用 base64 over exec + 大文件分块
小文件:`printf %s <b64> | base64 -d > file`,免单独 SFTP 通道,Local/SSH 通用。大文件(如 10MB 离线制品)单条 exec 过大会导致 SSH channel 无 exit-status(code -1),且 >128KB 触发 Linux MAX_ARG_STRLEN;故 SshExecutor::write_file 改为**60KB 分块** append 到临时文件后一次性解码。已验证 10.6MB OK。

### D-010 docker 用发行版包路径
首个端到端最稳(docker.io / docker);静态二进制(引擎已支持 download/extract)留作 node_exporter 演示与后续 profile。

### D-011 通用换源加速（待做）
apt/dnf 国内镜像 + 启用 universe。测试机 192.168.73.11 本就是 tuna/aliyun 源 + universe,故 M1 未被阻塞;其它环境需要时再补。

### D-012 离线 bundle = tar.gz 纯 Rust
tar + flate2(rust_backend),免 C 工具链,Windows 控制端可直接编。OCI layout / zstd 留后续。

### D-013 k8s 代表选 k3s
轻量、单二进制、CN 镜像友好(INSTALL_K3S_MIRROR=cn)。kubeadm 完整集群、k3s 多节点 join、k3s air-gap 离线均留后续。安装脚本自身 enable+start,组件**不再加** systemd restart(避免 race),verify 用 `|| true` 容忍首启慢。已真机验证节点 Ready。

### D-014 组件依赖用 requires + 拓扑排序
`dag.rs` Kahn 拓扑排序,确定性(字母序断平),检测环与缺失依赖;spec 内未列出的依赖被忽略(宽松)。

### D-015 AI 副驾不司机（OpenAI 兼容 + 确定性护栏）
provider 用 OpenAI 兼容协议(通吃 OpenAI/DeepSeek/Qwen/内网 endpoint,契合政企离线)。`nl_to_spec` 让模型只产候选,再经 schema 反序列化 + 组件存在性校验;幻觉被拒,绝不静默驱动部署。离线诊断 `diagnose.rs` 固化规则零网络零模型先行,内网模型可叠加。

### D-016 组件别名（已被 D-017 取代）
~~`resolve_alias`:`k8s`/`kubernetes`→`k3s`,`es`→`elasticsearch`,使用户字面命令 `crater k8s` / `crater es` 可用。~~
**作废**:别名被焊死在代码里，违反「引擎零产品知识」(D-017)。现改为组件描述文件的 `aliases:` 字段（数据）。

---

## 2026-05-31 · 架构铁律确立 + 还债（A 批）

### D-017 引擎零产品知识：能装万物 = 引擎通用 + 知识是数据
- **背景**:用户重申理念——「代码里不能有任何 docker/k3s/mysql 等我期望装的东西的逻辑，因为安装万物是我的理念」。审计发现多处违规:`resolve_alias`(别名焊死)、`doctor` 探针写死 `journalctl -u docker/-u k3s`、`LoadImage` 写死 `docker pull`、`source.rs` 写死 `registry.k8s.io` 镜像。
- **决策**:确立铁律——**引擎(Rust)只持有「通用原语」(file/copy/template/service/cmd/download/extract/load_image…，类比 ansible-core 的 module)，不得持有任何「具体产品」的名字/服务名/别名/镜像源/诊断规则;后者一律是数据(YAML)**。加一个可部署对象 = 丢一个描述文件，绝不改 Rust 重编译。
- **理由**:这正是 ansible 的架构本质(ansible-core 不知道 nginx 是什么);「类似 ansible」与「装万物」是同一目标。引擎是领域无关的、跑在 SSH 上的声明式 task 引擎。
- **影响（本批已做，build 0 错、21 tests 绿、`crater k8s` dry-run 经数据别名解析到 k3s 验证）**:
  1. `ComponentDescriptor.aliases: Vec<String>`;删 `resolve_alias`，改 `resolve_component` 扫 `components/` 从数据建表;`k3s` 声明 `aliases: [k8s, kubernetes]`、`elasticsearch` 声明 `[es]`。
  2. `doctor` 探针改为从所有组件的 `SystemdUnit` 名(`ComponentDescriptor::systemd_units`)推导 + 通用 `journalctl -p err`;不再写死服务名。
  3. `Action::LoadImage` 加 `runtime: Option<String>`;无声明则探测 `nerdctl/docker/podman/ctr`，引擎不假定运行时。
  4. 镜像表(registry 改写 + github 镜像)移入 `crater-core/src/mirrors.default.yaml`(include_str! 烤进二进制，仍是数据);`$CRATER_MIRRORS`/`./mirrors.yaml` 可外部覆盖(政企内网镜像)。代码只 parse，不命名产品。
- **后续（B 批，未做）**:幂等契约 + `changed/ok/skipped/failed` 回显;crater.yaml 轻量 task/play 层(module + when/loop/notify);ansible 习惯 module 库(file/copy/service/user/lineinfile/cron/git)。详见 progress.md。

---

## 2026-05-31 · 设计方向重整（离线转 OCI + 自举 agent）

### D-018 离线包基于 OCI 镜像（取代 D-012 的 tar.gz）
- **背景**:用户定向「离线打包基于 OCI 镜像」。M2 现状是 tar.gz + 手写 manifest + sha256(D-012)，装不下容器镜像、无去重、校验靠手写。
- **决策**:离线包格式定为 **OCI Image Layout**(序列化为单个 oci-archive tar)。制品与容器镜像均以 OCI blob **内容寻址**存放;crater 在其上加一层 `crater-manifest`(自定义 mediaType)做逻辑索引(spec + 制品名→digest→落地动作)。**取代 D-012 的 tar.gz 为离线主格式**(tar.gz 在 OCI 跑通前保留，渐进迁移)。
- **理由**:内容寻址=校验免费;分层=多组件共享 base 去重;容器镜像本就是 OCI，可原生打包/导入(k8s/mysql/es 离线的前提);生态(registry/`ctr import`/`docker load`/oras)通吃。参考 sealos clusterimage。
- **影响（守 D-017）**:引擎只懂「打/解 OCI、导入镜像」机制;**哪些镜像/制品打包由组件数据声明**——组件新增 `images:` 字段(列离线所需镜像，可带 digest 锁定)，`artifacts` 来自 `download:` 原语。在线/离线共用同一套组件与引擎，仅在 ArtifactSource 分叉(Online 目标机自拉 / OciBundle 从包取)。选型:`oci-spec`+`oci-client`(rustls，纯 Rust)，musl 可编性待验证。详见 [offline-format.md](offline-format.md)、[design.md](design.md)。

### D-019 自举 agent（`crater agent`）是离线落地的执行基座，非可选优化
- **背景**:F16 的 `crater agent` 一直是 TODO 桩(`main.rs` 打印 TODO)。用户重申「需要实现自举 agent」。
- **决策**:把 crater **二进制本身**推送到目标机，以 `crater agent` 在本地执行计划/解包。**一次性自举，用完即走，不留常驻服务**(仍守 agentless 精神)。控制端与 agent **复用同一套引擎代码**，仅 Executor 换 LocalExecutor。
- **理由**:OCI 离线 deploy 的解包/digest 校验/`image import`/推临时 registry，用 shell 片段拼极脆弱;用推过去的同一二进制在本地做，干净、可测、复用引擎。并减少弱网下的 SSH 往返;为 B 批幂等 check/回滚提供本地执行环境。
- **影响**:agentless shell 推送(现状)与自举 agent **互补共存**——简单在线步骤走 shell，离线/复杂逻辑走 agent。详见 [design.md §5](design.md)。

---

## 2026-05-31 · CLI 终极契约 + 生命周期/状态 + AI 接口

### D-020 CLI 终极形态：`apply` 为主入口，`<source>` 与 `<release>` 两类对象
- **背景**:用户敲定终极命令。早先猜想用 `install`，改为 **`apply`**——声明式幂等动词(收敛到期望态，含 install+upgrade)，与 B1 幂等契约同源，且统一现有 `crater apply -f`。
- **决策**:动词全集 `plan/apply/bundle/inspect/verify`(吃 `<source>`) + `rollback/remove/ai diagnose`(吃 `<release>`)。
  - **`<source>`**=recipe+制品来源:组件名 / `./x.yaml` playbook / `oci://…` 镜像 / `./x.crater` 归档 / `-f spec.yaml`，引擎自动识别、统一对待。
  - **`<release>`**=已部署实例:名字+修订历史+落在哪些主机。`apply` 把 source 收敛成/更新 release;`--release` 区分实例。
  - **inventory 永远在镜像外**(CLI `--host`/`-i` 或 spec)，不入可分发的 OCI 镜像(密钥/复用)。镜像内 recipe 是默认参数，`--set`/`--values` 覆盖。
  - 现命令映射:`build`→`bundle`(别名保留)、`doctor`→`ai diagnose`、`crater ai`→`ai generate`、`crater <component>`快捷式=`apply <component>`糖。
- **理由**:一个 `apply` 收敛所有部署入口;source/release 分离让"装"与"管(回滚/卸载/诊断)"各有清晰对象。
- **影响**:CLI 重构按此;破坏性动词(apply/rollback/remove)支持 `--dry-run`，`plan`=apply 预览。详见 [design.md §7](design.md)。

### D-021 release 状态记录放目标机；回滚=重放旧期望态，卸载=声明式 `uninstall:`
- **背景**:rollback/remove/诊断需要"记住装过什么"——crater 现在无状态。
- **决策**:
  - **状态放目标机**(非控制端):`/var/lib/crater/releases/<release>/{history.json, rev-<n>/{plan.json, touched.json}}`。契合 agentless/air-gap/政企:控制端可丢、主机自描述、可被任意控制端接管。
  - **rollback=声明式回滚**:默认重放上一修订存下的期望态(吃幂等红利，不靠逐动作逆操作);OCI 修订钉 `source_digest` 可重放;删过的文件用快照恢复;**有副作用的 `run_cmd` 诚实标记不可自动回滚**，`plan rollback` 先列可逆/不可逆。
  - **remove=声明式 `uninstall:` 阶段**(组件新增数据字段，与 install/verify 对称，守 D-017):引擎只跑数据声明的拆除步骤;未声明则按 `touched.json` 反向尽力清(默认不动系统包，`--purge` 才删并明示可能不彻底)。
- **理由**:逐动作写逆操作脆且组合爆炸;声明式重放+记录追踪更稳，且复用幂等地基(B1)。
- **影响**:apply 每次写新修订;component schema 增 `uninstall:`;依赖 B1 幂等。详见 [design.md §7.4](design.md)。

### D-022 AI 接口保留：独立子命令 + 三个生命周期钩子（实现后置）
- **背景**:用户要求保留 AI 接口(后置实现)，并明确 AI 要贯穿:制包时分析依赖、部署报错时分析日志、部署后 doctor 检查+智能修复。
- **决策**:AI 两种存在形式，统一守 D-015(副驾不司机、可 `--ai off`):
  - **(a) 子命令** `crater ai <sub>`:`diagnose <release>`(现 doctor)、`generate "<nl>"`(现 crater ai)，预留 explain/suggest。
  - **(b) 生命周期钩子**(默认关，`--ai` 开):**bundle 时**依赖分析/补全(AI2)、**apply 报错时**日志根因+修复命令(AI5/6)、**apply 后**doctor 健康检查+智能修复建议(**仅建议，人工确认**，AI10)。
  - **降级链**(AI9):固化规则(零网络，离线必可用)→ 内网 endpoint → 云端 OpenAI 兼容;制包时可把专属诊断规则/runbook 固化进 OCI 包(AI4)。
- **理由**:把 AI 钉成"关键点可选叠加"而非散落，接口/钩子点先占位、契约稳定，多数实现后置不阻塞主线。
- **影响**:`ai` namespace + 钩子点先占;规则侧已可用(M5)，模型侧叠加后续。详见 [design.md §7.5](design.md)、requirements §5。

---

## 2026-05-31 · B1 幂等回显（已实现 + 真机验证）

### D-023 幂等契约：每步 check→act→report，回显 changed/ok/warn
- **背景**:B 批地基。原 `execute` 每步无脑跑、无"已就绪则跳过"，重跑 yq 会重新 curl。要 ansible 式幂等回显。
- **决策**:`Op::Shell` 增可选 `check`(幂等探针);`StepStatus{Ok,Changed,Warn}`;`execute` 按相分流——
  - **读类**(Preflight/Verify):跑命令，exit0→`ok`，soft_fail 非零→`warn`，从不算 changed。
  - **安装类**(Install):先跑 `check`，exit0→`ok`(跳过主命令)，否则执行→`changed`(失败且非 soft_fail 则 bail)。
  - **写文件**(WriteFile/PushFile):比对远端 `sha256sum` 与期望;相同→`ok` 跳过，否则写→`changed`。
  - 收尾汇总 `changed=.. ok=.. warn=..`。
- **探针来源(守 D-017，全是数据/通用规则)**:`download`=`test -s dest`、`pkg_install`=`dpkg -s`/`rpm -q`、`systemd_unit`=`is-enabled`/`is-active`(按 enable/start)、`run_cmd` 支持组件 YAML 里写 `check:`(ansible `creates:` 风格)、`extract`/`load_image` 暂无可靠通用探针(总跑，报 changed)。
- **真机验证(192.168.73.11，yq)**:清空后首次 `changed=2 ok=1`;再次 `changed=0 ok=3`(download/chmod 跳过、verify ok)。23 tests 绿(+2 幂等单测)。
- **已知边界(后续)**:download 的 `test -s` 不校验版本(版本升级需带版本/校验和的探针);`run_cmd` 默认无 check 则总 changed;`when:`/`skipped`(条件跳过)属 B2 未做。

### D-024 CLI 默认执行：去掉 `--apply`，预览改 `--dry-run`
- **背景**:用户指出命令尾部的 `--apply` 多余。按 D-020，动词 `apply` 本身即"执行"，`plan`/`--dry-run` 才是预览。
- **决策**:**默认执行**;`--apply` flag 删除;新增 `--dry-run` 只打印计划不执行。覆盖 Apply/Deploy 子命令与 `crater <component>` 快捷式。
- **理由**:消除"动词说执行、还要再加 flag 才执行"的冗余;与 D-020 的 `plan`/`apply` 语义一致。
- **影响**:`crater yq --host .. --password ..` 直接执行(真机验证 idempotent ok=3);`--dry-run` 预览;内部 `do_apply` 语义保留(默认 true)。注意:无 `--host` 的本地快捷式现在也会**真执行**于控制机，预览须显式 `--dry-run`。

### D-025 spec 支持内联 recipe（Path B）：一个 yaml 即可，`components/` 变可选复用库
- **背景**:用户质疑"recipe(`components/<name>/component.yaml`) + spec(`yq.yaml`) 两个文件"是否必须。结论:分离是为**复用 + 密钥隔离**(recipe 可发布/签名/进 OCI 镜像，inventory 含密码不能进)，但不应**强制**两个文件。
- **决策**:`ComponentRef` 增内联字段 `preflight/install/verify/requires/supported_os`;任一阶段非空即 `is_inline()`，直接由它构 `ComponentDescriptor`，不再读 `components/<name>/`。一个 spec 文件即可完整描述部署;`components/` 退化为**可选**复用库。
- **理由**:简单场景不该被迫拆两文件;同时保留分离能力(复用/打包)。三种用法并存:零 spec(`crater apply yq --host`)/ 单文件内联(本决策)/ 分离(复用时)。
- **影响**:`resolve_descriptor` 统一"内联 vs 磁盘"加载(apply/order/bundle 共用);内联模板路径相对 spec 目录;bundle 把内联 recipe 序列化进包。真机验证 `examples/yq-inline.yaml`(单文件)在线部署 idempotent ok=3。+2 单测(共 25)。详见 [design.md §3.1](design.md)。

---

## 2026-05-31 · 自举 agent 实现（D-019 落地）

### D-026 自举 agent 实现：推二进制+计划，目标机本地执行（在线先行）
- **背景**:D-019 决策的实现。`crater agent` 原为 TODO 桩。
- **决策/实现**:
  - `Op`/`Phase` 加 `Serialize/Deserialize`;`engine::plan_to_yaml`/`plan_from_yaml` 作 agent 线格式(内部 YAML，自产自销)。
  - `crater agent --plan <file>`:在目标机读计划、用 **LocalExecutor** 本地执行(复用同一 `engine::execute`，幂等回显照旧)。
  - 控制端 `--agent`:连 SSH→探 OS→**控制端 lower 出具体计划**→推 `/tmp/crater-agent`(二进制，分块 base64)+`/tmp/crater-plan.yaml`→一条 exec 跑 `crater agent --plan`→流式回显→**清理临时文件**(一次性自举，不留常驻)。`--agent-bin` 可指定要推的二进制。
- **真机验证(192.168.73.11)**:推 9.8MB release 二进制 + 计划，目标机本地执行;清空后首跑 `changed=2 ok=1`、再跑 `changed=0 ok=3`(幂等贯穿 agent 路径)。`Done on local` 证实是 LocalExecutor 在目标机跑。+1 round-trip 单测(共 26)。
- **二进制兼容**:demo 用 glibc release(控制端与目标同为 Ubuntu 24.04/glibc 2.39);异构目标用 `--agent-bin` 指 musl 静态构建(N1/N2)。
- **已知边界(后续)**:仅在线计划(Shell/WriteFile);**离线 `PushFile` 未支持**(blob 在控制端，需随计划一并 ship——并入 OCI 离线 D-018 时做);agent 暂不回传结构化结果(只流 stdout)。详见 [design.md §5.2](design.md)。

---

## 2026-05-31 · agent 成为默认执行模型

### D-027 agent 是默认执行模型（强制默认 + `--shell` 逃生，取代 D-026 的"用完即走"）
- **背景**:用户指出"agent/非 agent 两种模式按情况选"增加心智负担，要求 agent 作底层默认。讨论确认:agent 需目标机能跑 crater 二进制(arch+libc)，shell 仅需 SSH+shell 是最纯 agentless 兼引导层。用户选**强制默认 + `--shell` 逃生**(不做兼容探测/自动兜底)。
- **决策**:
  - **默认 = agent**(快捷式与 `apply -f` 统一);`--shell` 强制 agentless shell 执行(逃生口);本地目标(无 `--host`)天然走本地执行;`--agent` 保留为 no-op(向后兼容)。
  - **二进制按 sha256 缓存**在目标机 `/var/lib/crater/agent`:命中则跳过推送(推一次/版本)，仅 plan 文件(`/tmp/crater-plan.yaml`)瞬时。**取代 D-026 的"用完即走"**——缓存换"每次少推 ~10MB"。
  - 二进制无法执行(exit 126/127)时**报错并提示** `--shell` 或 `--agent-bin <musl>`(不自动兜底，符合用户选择)。
- **理由**:消除"选模式"的心智负担(默认即最强模型)，缓存让代价可接受;异构目标用 `--shell`/`--agent-bin` 兜。
- **真机验证(192.168.73.11)**:`crater yq --host ..`(无 flag)首跑 `Mode: APPLY (agent)`、推 9.9MB、`changed=2 ok=1`;再跑"binary cached, reusing"、`changed=0 ok=3`;`--shell` → `Mode: APPLY (shell)`、`Done on root@host`(逐步 SSH)。26 tests 绿。
- **后续**:异构全覆盖需备 musl 多架构二进制(N1/N2) + 控制端按目标 arch 选二进制;真正的自动兼容探测+兜底(用户当前选了不做)。
- **更新(musl 可移植二进制已就绪)**:`scripts/build-musl.sh`(musl-tools + `CC_x86_64_unknown_linux_musl=musl-gcc cargo build --target x86_64-unknown-linux-musl`)产出 `dist/crater-linux-x86_64`(`ldd`→statically linked，9.3M)。真机验证:`--agent-bin dist/crater-linux-x86_64` 推送后目标机 `file` 确认 `static-pie linked`、本地执行 `changed=2 ok=1`。这是真正"放之四海"的 agent 二进制(不挑 glibc);aarch64 同法(`scripts/build-musl.sh aarch64`，待装 target)。**控制端按目标 arch 自动选/内置 musl 二进制**仍待做(当前需手动 `--agent-bin`)。
- **通信模型澄清**:控制↔agent 全靠 SSH 一发一收(写文件+一次性 exec+收 stdout)，无常驻进程/端口/RPC;静态与否不影响通信(仅链接方式)。详见 [design.md §5.3](design.md)。
- **更新(arch 自动选已实现，x86_64)**:`select_agent_binary` 探测目标 `uname -m` → 优先用匹配 arch 的 bundled musl 静态(`dist/crater-linux-<arch>`，也顺带规避同 arch 的 glibc 偏差) → 否则 arch 与控制端相同则回退 `current_exe` → 都不行报错提示 `scripts/build-musl.sh <arch>` / `--agent-bin` / `--shell`。优先级:`--agent-bin` > bundled musl > current_exe(同 arch)。候选目录:`$CRATER_AGENT_DIR`、控制二进制旁(+`dist/`)、`./dist/`。真机验证:glibc debug 控制端也**自动选了 dist 的 musl 静态**推送(`bundled musl static for x86_64`)。**aarch64 仍需装 target + 真机验证**。
- **目标机二进制更名**:缓存路径 `/var/lib/crater/agent` → **`/var/lib/crater/crater`**(它就是同一个完整 crater 二进制，只是跑 `agent` 子命令;旧名易误解为"另一个 agent 程序")。

---

## 2026-05-31 · 日志规范化

### D-028 统一用 tracing 输出：时间戳 + 级别 + 颜色(按 TTY) + verbosity
- **背景**:输出全是 `println!`、无时间戳/级别/颜色;且 **apply 会先 dump 整个计划(带 `$ cmd`)再逐步执行**，重复且乱。
- **决策**:统一走已引入的 `tracing`:
  - 自定义紧凑计时器 `ClockTime`(UTC `HH:MM:SS`，零新依赖);`with_target(false)`;级别由 `CRATER_LOG`/`RUST_LOG` 控制(默认 INFO)。
  - **ANSI 按 `stdout().is_terminal()` 开关**——终端着色，管道/重定向/agent 经 SSH(非 TTY) 则纯文本无转义码。
  - engine `execute`:步骤行 `[n/total] {desc} → {status}` 走 info、warn 走 warn、失败 error+bail;**命令 stdout 降到 debug**，但 **verify 输出留 info**(那是结果证明);状态词 ansible 式上色(ok 绿/changed 黄/warn 黄，同样按 TTY)。
  - **apply 不再预 dump 计划**(只 dry-run 才 `print_plan`)，消除重复。
- **理由**:可读、可 grep、可控 verbosity、重定向干净;agent 转发的目标机输出与控制端同格式(目标机非 TTY 自动无色)。
- **影响**:`crater apply` 输出变为 `HH:MM:SS INFO …` 结构化行;真机验证控制端 + agent 转发输出格式一致、管道无转义码。`CRATER_LOG=debug` 看命令细节。其余子命令(build/deploy/ai/doctor)的 println 后续一并迁移。

---

## 2026-05-31 · module 模块化契约

### D-029 module 四层模型 + 契约地基（数据定义 module 先行）
- **背景**:`Action` 是固定 Rust enum（8 个），表达力靠 `run_cmd` 兜但幂等/声明式覆盖薄;用户问"功能复杂起来怎么定义、是否借鉴 ansible 模块化"。
- **决策**:借鉴 ansible，按复杂度/通用度分**四层** module（详见 [design.md §6.1](design.md)）：① 内置类型化(Rust enum，core) ② 数据定义(`modules/<name>.yaml` 的 params+check+act 模板) ③ 外部 module(脚本/静态二进制 + JSON 契约，agent 送达) ④ `run_cmd`+`check` 逃生口。**统一契约 = `check→act→StepStatus`(B1)**;第 1/2 层 lower 成 `Op::Shell{check,cmd}`(shell 模式可用)，第 3 层需 agent(优先静态二进制/纯 shell，守目标机零依赖)。
- **理由**:复用已造零件(StepStatus=结果契约、agent=送达、数据驱动=放目录即加载);避免"每加 module 就改 Rust+fork";与 D-017(产品=数据、原语=代码)、D-027(agent 默认) 自洽。
- **影响（本批做的地基）**:新增 `Action::Module{uses, with}` + `module.rs`(ModuleDescriptor: params/check/act) + `PlanContext.modules_dir`;`module` action 解析 `modules/<uses>.yaml`、用 `with`(+vars) 渲染 check/act → `Op::Shell{check,cmd}`，直接吃 B1 幂等回显。缺参报错。第 2 层(数据定义)即可用、零代码扩;内置集扩充(B3)与外部 JSON 协议后续。

---

## 2026-05-31 · 跨节点 fact 传递（真集群钥匙）

### D-030 register/hostvars：跨节点 fact 传递
- **背景**:基础多节点(fan-out)已验证，但无跨节点协调——k3s server 的 join token 没法传给 agent 节点。真集群缺这一环。
- **决策**:组件可声明 `register: [{name, cmd}]`;某组件在某 host 装完后，**控制端**经该 host 的 executor 跑 `cmd`、捕获 stdout、存入 `hostvars[host][name]`。其它 host 的组件用 `{{ hostvars.<host>.<name> }}` 引用(渲染时注入为扁平 var)。主机按 inventory 顺序处理(leader 在前→先 register)。
- **理由**:控制端捕获最简单可靠，agent/shell 模式都适用(register 是一条直连 SSH exec，不走 agent plan);复用现有 render + vars 机制(扩 `{{ key }}` 空格 + 点号键)。这是 ansible `register`/`hostvars`/`run_once` 的最小子集。
- **影响**:`ComponentDescriptor.register` + `RegisterSpec` + `ComponentRef` 内联同字段;`apply_spec` 维护 `hostvars` 跨主机、注入 `hostvars.<h>.<k>`、装后捕获;`engine::render` 转 pub 且支持空格/点号键。**describe 仍显示模板原文(不泄漏 token 等敏感值到日志)**，实际执行渲染后命令。真机验证(192.168.73.11→.12):leader register token-from-ubuntu → follower 经 `{{hostvars.leader.token}}` 收到。+1 单测(共 31)。
- **已知边界/后续**:register 为**组件级**(非步骤级);跨主机顺序靠 inventory 顺序(未按 role 自动排);敏感值未标 `no_log`(目前靠 describe 不渲染规避);并发(F17)正交待加。
- **终极验收(k3s 两节点真集群)**:`components/k3s` 加 `register:[token,url]`、新增 `components/k3s-agent`(用 `{{hostvars.server.token/url}}` join)、`examples/k3s-cluster.yaml`。真机 192.168.73.11(server)+.12(agent)→ `kubectl get nodes` 两节点全 Ready。**D-030 传值成立**(诊断 `curl server:6443/ping→pong` 已证)。踩坑:克隆 VM hostname 同为 `ubuntu`，k3s 拒重名节点→组件加 `K3S_NODE_NAME=agent-<ip-dashed>` 唯一化(数据修复，守 D-017)。

---

## 2026-05-31 · 并发（F17）

### D-031 同 role 主机并行：按 role 分组、组间串行、组内并发
- **背景**:多节点原为串行逐台(N 台慢)。要并行，但不能破坏 D-030 的"register→消费"跨节点顺序。
- **决策**:hosts 按 **role-set 签名分组**(签名=排序后的 roles 拼接)，**组按首次出现顺序串行**(producer 角色先于 consumer 角色 register)，**组内主机并发**(同 role 是对等节点、互不依赖)。每台 host 跑完返回自己的 register facts，**整组结束后再合并进 hostvars**(避免并发写竞争 + 组内本就不该互相依赖)。
- **理由**:同 role 并行是最常见的提速场景(多 agent/多 web);组间串行守住 D-030(server 组先 register、agent 组后 join)。
- **影响**:抽出 `run_host`(返回 `(host, Vec<(name,val)>)`);`group_hosts_by_role` + `forks_limit`(`CRATER_FORKS`，默认 10);组内用 `futures::stream::buffer_unordered(forks)` 并发(加 `futures` 依赖);组内任一 host 失败→整组跑完后返回首个错误。控制端日志加 `[host]`/`[root@ip]` 前缀;agent 转发输出为连续块(由前面的 `[host] agent: executing ↓` 标记)。真机:`examples/multi-node.yaml` 两台同 role `[yq]` 同时 07:12:57 启动、总时长≈max(各主机)。
- **已知边界**:跨组顺序仍靠 role 首次出现序(未显式声明 role 依赖);组内 host 失败不停其对等(但整体 apply 失败);并发日志按行可能交错(块级连续、有 host 前缀)。

---

## 2026-05-31 · OCI 离线（D-018 增量 1）

### D-018 落地（增量 1）：离线包转合规 OCI Image Layout（制品先行）
- **背景**:D-018 决策离线转 OCI;M2 是 tar.gz + manifest.yaml。本批落地第一增量。
- **决策/实现**:`bundle.rs` 重写为 **OCI Image Layout**——`oci-layout` + `index.json`(OCI image index，`org.crater.manifest` 注解指向 crater-manifest blob) + `blobs/sha256/<digest>`(内容寻址:OCI image manifest/config + components 层 tar + crater-manifest JSON + 各制品 blob，制品层带 `org.crater.source-url` 注解)。打包为 `oci-archive`(纯 tar，不再 gzip——制品本身已压缩)。`BundleStage` API 不变(`store_blob`/`blob_path`/`write_manifest`/`read_manifest`/`verify`)，故 `build_bundle`/`deploy_bundle` 几乎零改动。加 `serde_json` 依赖;`BUNDLE_FORMAT_VERSION`=2。
- **理由**:内容寻址=校验免费(digest 即文件名);结构 skopeo/oras 可读;为容器镜像(嵌套 OCI blob)与临时 registry 铺路。守 D-017(引擎只懂打/解 OCI，装什么仍是数据)。
- **真机验证**:`crater build -f node_exporter.yaml -o ne.oci`→ 解开确认 oci-layout/index.json/blobs/sha256 齐全、image manifest 引用 config+layers;`crater deploy --bundle ne.oci --host .12`→ `push (offline)` 推制品、node_exporter 1.8.2 `:9100` 出 metrics，`changed=4 ok=3`。+ 单测断言 OCI 结构。
- **后续增量**:② 容器镜像打包(组件 `images:` → `oci-client` 拉取 → 嵌套 OCI blob → 目标机 `ctr image import`，解锁 k8s/mysql/es 离线);③ 临时 registry(F13)多节点分发;④ agent 解 OCI(D-019 接力)。deploy_bundle 仍 print_plan + 走 shell(未接 agent/去 dump)，后续统一。

---

## 2026-05-31 · OCI 离线（D-018 增量 2：crater 原生 build/save/load）

### D-018 增量 2：crater 把制品封装进 OCI 镜像，自己 save/load（目标机零运行时）
- **背景**:纠偏——离线镜像只是**打包载体**，不该依赖目标机的容器运行时(ctr/docker)来导入。crater 需自备 build/pull-push/save/load 能力。从 yq 起步。
- **决策/实现**:
  - **build**(`crater build --image`):把组件的文件产物(download→dest、write_file、render_template)渲染成一个 **rootfs 层**(tar，含可执行位)，封装为一个真正的 OCI 镜像(`crater/<name>:<ver>`，config+manifest+layer，进 index.json 带 ref.name)。`store_rootfs_layer`。
  - **save**:打包为 oci-archive(纯 tar)。
  - **load+install**(`crater deploy`):crater **自己**解包 oci tar、取 rootfs 层、`tar -xpf -C /` 展开到目标机——**无 ctr/docker**;随后跑组件 verify 步骤确认。
  - **pull**(增量①已起):`oci-client` 从 registry 拉镜像 blob 进包(rustls，纯 Rust)。**push** 后续。
- **理由**:守"目标机零依赖"(agentless);镜像是标准 OCI(可签名/可 registry 流转)，但安装不绑容器运行时——crater 自身即 save/load。
- **真机验证(192.168.73.12)**:`crater build --image -f yq.yaml -o yq-img.oci`→ 封装 `crater/yq:4.53.2`(rootfs 层 0755)、13.7MB oci-archive;`crater deploy --bundle yq-img.oci`→ crater 解包展开到 `/`→ `/usr/local/bin/yq` -rwxr-xr-x、`yq --version` v4.53.2。+1 单测(rootfs 层 round-trip，含 0755)，共 30。
- **边界/后续**:rootfs 模型只 bake 文件类动作(systemd/run_cmd 不入层，daemon 类仍走 recipe-replay 离线路径);registry **push**;多 arch rootfs;镜像签名(N4)。

---

## 2026-05-31 · build --image 完整化（修静默遗漏）

### D-018 增量 2 修订：build --image 按动作分类、零静默丢弃
- **背景**:用户质疑"怎么识别 component.yaml 哪些进镜像，不会遗漏吗、学习成本高吗"。审视发现初版 `build --image` 只处理 download(dest)+write_file、**静默跳过 extract/run_cmd 等**——`node_exporter --image` 会产出**没有二进制的残缺镜像且不报错**（extract 被漏）。
- **决策**:`Action::produces_files()` 显式分类。build --image：**文件类**(download/extract/write_file/render_template)由 crater **真实物化文件效果**进 staging rootfs（extract 纯 Rust 解 gz+tar+strip，download 落盘），tar 成层；**命令式**(run_cmd/pkg_install/systemd_unit/module)作**残留 recipe** 随镜像带走、load 时目标机 replay。build **打印** `baked N file action(s); will replay on target: [...]`，**绝不静默丢**；无任何文件产物则报错引导用 plain build。load = 展开层 + replay 残留(非文件类 install)+verify。
- **理由**:用户只写一份 component.yaml、不选模式不懂拆分;每个动作都有归宿且透明;修掉 extract 遗漏。守"两端零容器运行时"。
- **真机(192.168.73.12)**:`node_exporter --image` baked 3(含 extract)+replay[systemd_unit]→ 展开 binary+unit、systemd replay、`:9100` metrics;`yq --image` baked 1+replay[run_cmd chmod]→ yq v4.53.2。`untar_gz_into`/`store_rootfs_layer_dir` + 下载 scratch 剔除。

---

## 2026-05-31 · apply 统一在线/离线（D-020 落地）

### D-020 落地：`crater apply <source>` 自动识别（在线 spec / 离线 OCI / 组件名）
- **背景**:用户要在线/离线**逻辑一致**——`crater apply -f xx.yaml` 与 `crater apply xxx-oci-image` 同一条命令。
- **决策/实现**:`apply` 收位置参数 `<source>`，`apply_source` 自动识别并路由：
  - `.yaml`（spec 文件）→ **在线**（apply_spec，inventory 在 spec 内）;
  - OCI 归档（`bundle::is_oci_archive` peek `oci-layout`）→ **离线** load+install（deploy_bundle，inventory 由 `--host`/`-i` 提供，守 D-020"inventory 不入镜像"）;
  - 组件名 → 在线快捷式。
  `-f` 保留为别名;`crater deploy` 保留为离线等价命令。
- **理由**:一条 `apply` 收敛所有部署入口;**执行引擎一致**（engine::execute 的幂等/changed-ok/tracing 两端共用），在线/离线只差"制品从哪来"（ArtifactSource）。
- **真机**:`crater apply yq.yaml`→online(spec);`crater apply yq --host`→online(component);`crater apply yq-img.oci --host .12`→offline，load+install yq、`changed=1 ok=1`。
- **后续（更深统一）**:让离线也走 apply_spec 的同一主机循环（agent/并发/register）——目前离线走 deploy_bundle 的较简单循环（shell、单流程）。把"在线/离线"彻底收敛成"同一 host-pipeline + ArtifactSource 分叉"是下一步。

---

## 2026-05-31 · 彻底单管线（离线并入 run_pipeline）

### D-020 终极落地：在线/离线共用 run_pipeline，唯一分叉 = ArtifactSource
- **背景**:此前 apply 路由统一了命令，但离线仍走 deploy_bundle 的较简单循环（单流程、无并发/register）。要彻底单管线。
- **决策/实现**:抽出 `run_pipeline(spec, artifacts, components_dir, spec_dir, ...)`——order DAG → 按 role 分组（组内并发、组间串行）→ run_host → 合并 register facts。新增 `enum Artifacts { Online | Offline{blobmap, rootfs} }`，是**唯一的在线/离线差异**：
  - run_host 按 artifacts 建计划：Online=build_plan;Offline blob=ctx.offline_blobs(download→push-from-blob);Offline rootfs=push 层 + `tar -xpf -C /` + 残留(非文件 install)+verify。
  - 离线强制 shell executor（blob 在控制端;agent 需先 ship blob，后续）。
  - 离线 inventory 由 CLI 提供：`crater apply x.oci -i inv.yaml`(多主机) / `--host`(单)。`apply_oci_bundle` 解包→合成 spec(components 来自 manifest)→run_pipeline。`deploy_bundle` 成单主机包装。
- **真机(两台)**:`crater apply yq.oci -i two-hosts.yaml` → `▷ group [] — 2 hosts in parallel` → 两台并发 push 层+extract+chmod+verify yq v4.53.2、各 changed=3 ok=1。**离线获得了与在线相同的多主机/并发/register/幂等/tracing**。
- **意义**:在线/离线真正只剩"制品从哪来"一处差异（design.md §3 的"双形态单管线"从设计变为实现）。

---

## 2026-05-31 · 镜像管理（images/pull/push/login + apply <ref>）

### D-018 增量：本地镜像库 + registry 客户端 + apply 直接吃镜像地址
- **背景**:用户要 `crater images`/`registry login`/`pull`/`push`，且 `crater apply` 支持直接跟镜像地址（registry 或本地）。
- **决策/实现**:`store.rs` —— 本地 OCI 镜像库 `~/.crater/store`（累积 OCI layout，index.json 每 tag 一条）；registry I/O 用 `oci-client`（rustls，纯 Rust）；凭据 `~/.crater/auth.json`（按 registry）。命令：`images`（列库）/`pull <ref>`（registry→库）/`push <ref>`（库→registry）/`registry login`（存凭据）。`apply <ref>`：识别镜像地址（含 `/` 或 `:` 且非文件）→ 库命中即用、否则 pull → 把镜像**所有 rootfs 层展开到 `/`** 安装（多主机并发，crater 自解包、零运行时）。原 SSH 拷文件 `crater push` **更名 `crater cp`**（push 让给镜像）。
- **真机(192.168.73.12)**:`registry login` 写 auth.json（并实际影响 pull 认证）;匿名 `pull hello-world`→库;`images` 列出;`apply docker.io/library/hello-world:latest --host .12`→库命中→展开→目标机 `/hello`(ELF) 落地。
- **边界**:`push` 已实现(oci-client client.push) 但无可写 registry **未 live 验证**;`apply <ref>` 是 rootfs 覆盖语义(适合 crater/sealos 式镜像);manifest-list 平台选择/签名/库 GC 后续。

---

## 2026-05-31 · 临时 registry 闭环（zot）

### D-018 增量：build→push→pull/apply 闭环 + crater 自装 zot（守 D-017）
- **背景**:用户要搭临时 registry（zot，本机 192.168.73.5，systemd），跑通 build→push→另一台 pull/apply 闭环。
- **决策/实现**:
  - **zot 用 crater 自己装**（狗粮）:`components/zot/component.yaml`（download 二进制 + write config/systemd unit + systemd_unit + verify curl /v2/）——纯数据，引擎零 zot 知识（grep 确认 `.rs` 仅文档注释提 zot）。`crater zot`（本机 LocalExecutor）装好，`/v2/ -> 200`。
  - **HTTP registry**:通用 env `CRATER_INSECURE_REGISTRIES=host:port` → oci-client `ClientProtocol::HttpsExcept`（不认识具体 registry）。
  - **`crater load <file.oci> --as <ref>`**:把 build --image 的 oci-archive 导入本地库并打 tag（`ImageStore::import_oci_archive`）。
  - **+x 进层**:build --image 的 download 落地置 0755，使纯 `apply <ref>`（extract-only，无残留 replay）也得到可执行二进制（修 yq exit 126）。
- **真机闭环**:`crater zot` → `build --image yq` → `load --as 192.168.73.5:5000/yq:4.53.2` → `push`（zot catalog `{"repositories":["yq"]}`）→ 清本地库 → `apply 192.168.73.5:5000/yq:4.53.2 --host .12`（真从 zot pull）→ n12 `/usr/local/bin/yq` 0755、`yq --version` v4.53.2。**push 至此 live 验证通过**。

---

## 2026-05-31 · OCI 用法分 A/B（外部架构评审）

### D-032 OCI 分 A 类(image) / B 类(artifact)；crater 主力 B 类，迁移伪 image
- **背景**:外部架构评审确认 OCI 方向正确（内容寻址去重、天然增量、Harbor/oras 生态、registry+tar 双形态），并点出关键分叉——「用 OCI 封装依赖」有两类：**A 类容器镜像**(image-spec，被 containerd run) vs **B 类 OCI artifact**(artifactType，落地宿主机的物料)。评审指出常见反模式：把单二进制伪装成永不被 run 的畸形 image。审视发现 crater 现状 `build --image` 正中此反模式（image-manifest + rootfs config 假装可运行）。
- **决策**:
  - 明确 A/B：A 类 crater **只搬运**（pull blob→layout→目标机 import，零自造 build，现 `pull` 已对）；**B 类是主力**——自造 OCI artifact。
  - **迁移 `build --image`**：伪 image → 正经 artifact：`artifactType: application/vnd.crater.component.v1`；分层有边界(binary/config/recipe/ospkg)；annotations 自描述(名/版本/run-mode)；recipe 进包。load 语义从"extract rootfs 到 /"→"按 layer mediaType + 包内 recipe 驱动落地"（与 crater recipe 模型收敛，去伪 rootfs）。
  - 聚合包用 **OCI image index** 引用多组件 artifact → 跨 bundle blob 全局去重。
  - 纯 Rust 构建(oci-spec/手写 blob/ocipkg)，不 shell docker（已守）。
- **对评审的补充（crater 约束）**:① **zstd 非免费**——zstd 编码=zstd-sys(C 依赖)，冲突 D-012/N1，默认仍 gzip(flate2)、zstd 可选、解码用 ruzstd;② **生态验证**——确认 oci-client/Harbor/zot 对 artifactType+referrers 的支持，不行则回退"带 annotations 的 image-manifest"(B 类语义、最大兼容);③ **A 类保留**——容器型组件的镜像走 A 类搬运，按 `run-mode` 数据路由;④ 守 D-017——A/B/run-mode/mediaType 全是数据，引擎只按 mediaType 通用处理。
- **影响**:现状 `build --image`(伪 image) 标为过渡实现，迁 artifact 为路线项（不阻塞现功能）。crater 定位明确为 **B 类 artifact 分发器 + A 类镜像搬运**。详见 [design.md §4.3](design.md)。

---

## 2026-05-31 · B 类 artifact 迁移落地 + 物料闭包显式化

### D-033 build --image → 正经 B 类 OCI artifact；artifactType 全程保真
- **背景**：落地 D-032 的迁移项——把 `build --image` 从伪 rootfs image 改成正经 B 类 OCI artifact，并跑通 registry 往返。
- **决策/实现**：
  - `build --image` 产出 `artifactType: application/vnd.crater.component.v1` 的 image-manifest（image-spec 1.1 兼容形态，最大化 oci-client/zot 支持），分层：recipe 层(`vnd.crater.recipe.v1+yaml`) + 每物料一层(`vnd.crater.material.v1`) + 组件 config(`vnd.crater.component.config`)；自描述 annotations（名/版本/run-mode）。**无伪 rootfs config、无假 image 层**。
  - load/apply 语义：检测 artifactType → **recipe-replay**（materials 喂 recipe 的离线动作），不再 extract rootfs 到 /。
  - **artifactType 全程保真（关键修复）**：oci-client 高层 `pull`/`push` 是面向 image 的，会**合成 image-manifest 丢掉 artifactType + 自定义 layer mediaType**（D-032 标的"生态验证项"被验证）。改为：
    - push：`OciImageManifest`(deser 自存储清单，保 artifactType) + `push_blob`(config+各层) + `push_manifest`。
    - pull：`pull_manifest_raw`(原样存清单字节) + `pull_blob`(按 digest 取各 blob，**绕开 `pull` 对非 image layer mediaType 的拒绝**——曾报 `Incompatible layer media type: ...recipe.v1+yaml`)。
  - 验证用 `CRATER_INSECURE_REGISTRIES`（D-018 增量）走 http zot。
- **真机闭环**：`build --image yq`（artifact，13.7MB）→ `load --as zot/yq:art` → `push`（zot 上 manifest 带 `artifactType` + recipe/material 层）→ 清本地库 → `apply zot/yq:art --host .12`（pull_blob 取自定义层 → recipe-replay）→ n12 `yq --version` v4.53.2。**B 类 artifact 端到端通过**。

### D-034 物料闭包显式化：组件加 `materials:` 段 + `action: place`（解耦"装什么/怎么装"）
- **背景**：外部评审指出严重设计问题——`component.yaml` 还不足以驱动打包：`build` 靠**扫描结构化 install 动作**发现物料（只看得见 `download`），一旦依赖藏在 `run_cmd` 自由文本（`apt-get install mysql-server`）或容器镜像里，打包器就**瞎了**，离线包必然残缺。根因：依赖与动作耦合在 install 里。
- **决策**（采纳评审，命名做工程取舍）：
  - **顶层 `materials:` 段**显式声明物料闭包（`kind: binary|image|os_package`，可带 `sha256`）。`build` **只读 materials**（`collect_materials`），不再扫 install——藏在 run_cmd 里的依赖再也不会漏。（取名 `materials` 而非评审的 `artifacts`：`artifacts` 与 OCI artifact 冲突，`requires` 已被组件 DAG 占用。）
  - **`action: place`** 按**逻辑名**引用 material（不写死物理 URL）：在线→目标机自己 curl material 的 `url_tmpl`；离线→控制端推打包好的 blob（按 material **名**索引，内容寻址自校验）。**在线/离线由引擎的 source 决定，不由 spec 决定**——评审要的"构建一次两种形态通吃"在数据层真正落地。`mode`（如 0755）折进 place，一步落地可执行（去掉单独 chmod run_cmd）。
  - B 类 artifact 的 material 层改按 **material 名**标注（`org.crater.material.name`），materialize→blobmap 按名建键，与 place 离线索引一致（旧 source-url 标注保留兼容）。
- **真机验证（yq 最小闭环，新模型）**：
  - 在线 `crater yq --host .11`：`place yq-bin <- <url>` → changed=1（chmod 折入）；再跑 changed=0（幂等 ok）。
  - 离线 `build`（日志 `fetch material yq-bin`，按名打包）→ load→push→清库→`apply zot/yq:m --host .12`：`place (offline) yq-bin -> /usr/local/bin/yq` → n12 `yq --version` v4.53.2。
- **范围**：本期把 `binary` 全链路打通（yq 闭环），`image`/`os_package` 与 build 的 version×os 矩阵（用 OCI image index 按平台/annotation 组织）为**已设计、待 mysql/docker 落地**的下一阶段（评审问题四）。yq 作最小闭环先证模型，再推到有真实依赖闭包的组件。

---

## 2026-05-31 · apply 三层目标 + SSH key 认证 + 本机部署

### D-035 apply 按规模分三层目标（本机 / --host 少量 / -i 大量）+ key 认证
- **背景**：用户要 apply 按部署规模自然分层，命令形态 `crater apply <name> <source>`：
  - 不指定主机 → **本机单机**；`--host a,b,c`（共用一套凭据）→ **少量机器**；`-i inventory.yaml`（每主机独立凭据）→ **大量机器**。
  - 提出 `--host` 共用密码的限制，要 key 认证 + 异构凭据解法。
- **决策/实现**：
  - **目标解析三层**（`target_hosts`）：`-i` 读文件 inventory（每主机各自 password/key）；`--host` 按逗号拆多主机、共用 `--user`+`--password|--key`+`--port`；都没有 → 单个**本机 host**（`Host::local()`，`address=@local`）。
  - **本机执行**：`Host::is_local()` 时 `run_host` 用 `LocalExecutor`，且强制 `use_shell=true`（本机不 bootstrap agent）。`LocalExecutor::write_file` 改为直写文件系统（绕开 trait 默认的 shell-base64，13MB 二进制会撑爆 `MAX_ARG_STRLEN`）。
  - **SSH key 认证**：`SshAuth::{Password, Key{path,passphrase}}` + `SshExecutor::connect_auth`；key 走 `russh::keys::load_secret_key` + `authenticate_publickey`。`Host` 加 `key: Option<PathBuf>`，key 优先于 password。
  - **`apply <name> <source>` 双位置参数**：两个位置参数时首个为部署 label、次个为 source；单个时即 source（向后兼容）。
  - **异构凭据解法**：`--host` 故意只共用一套凭据（少量同构机器）；每主机不同密码/key → 用 `-i inventory.yaml`（每 host 各带 password/key）。
- **真机验证**（zot 上 yq B 类 artifact，pull→recipe-replay）：
  - 层1：`apply app01 <zot>/yq:m`（无主机）→ 本机 `/usr/local/bin/yq` v4.53.2。
  - 层2：`--host 192.168.73.11,192.168.73.12 --password 123456` → 两台并行装好。
  - 层3：`-i inv.yaml` → n11/n12 装好。
  - key：`ssh-keygen` 装公钥到 n12 → `--key /tmp/crater_key`（无密码）→ n12 装好。

---

## 2026-06-01 · 不可妥协铁律：YAML 是数据，逻辑在引擎

### D-036 YAML 纯声明（数据），绝不变成编程语言；机制上堵死
- **原则（凌驾所有功能需求）**：crater 用户写的 YAML（action.yaml/component.yaml）永远是**纯声明式数据**。所有"怎么做"的逻辑——条件、循环、计算、重试、幂等、错误处理、依赖排序——**全在 Rust 引擎**。YAML 只允许两类内容：(1) 声明用哪个 action 原语 + 参数；(2) 取值引用 `{{ variable.path }}`（纯代入，无运算）。
- **判据（每次设计 YAML 字段/模板必过）**：这份 YAML 能否被另一个程序**静态读取/分析/diff/生成而无需执行**？能→数据（对）；必须执行才知道干什么→程序（错，重蹈 Ansible）。可静态分析是 **dry-run / preflight / AI 生成后人工审核**三大能力的前提，YAML 一旦有逻辑这些全失效。
- **理论根基**：声明式配置 + 机制/策略分离（机制=引擎能力，策略=YAML 数据）。正面参照 Kubernetes（逻辑全在 controller）、Terraform（HCL 故意非图灵完备）。**反面教材 Ansible**：给 YAML 加 `when`/`loop`/Jinja2，最终 YAML 成了无类型检查、无调试器、不可静态分析的烂语言。**要 Ansible 的能力，不要它把 YAML 变程序的覆辙。**
- **遇到"似乎要在 YAML 写逻辑"时的标准动作**（默认把逻辑挪进 Rust）：
  - 循环 → 引擎在 Rust 遍历算好，作为普通变量喂给 YAML（`{{ seed_hosts }}`）。
  - 计算 → 引擎算出结果存变量，YAML 只取结果（不放公式）。
  - 条件 → 引擎预定义的**封闭枚举开关**（如 `when_offline: true`，有限可知），或拆成不同 action 变体；**不是**用户可自由书写的布尔表达式。
  - 新操作 → 先用现有原语（尤其 `run_cmd`）组合；仅当高频 + run_cmd 别扭 + 值得引擎白盒理解时，才**新增 action 原语（Rust 代码，收敛在 ~20–30 个内）**。新增的是"种类"，不是 YAML 语法。
- **机制上堵死（不靠自觉）**：模板引擎必须是"残废"的——只做 `{{ path }}` 替换，用户想写 if/for/表达式/filter 时**直接报错**，而非默默放过。等同 Terraform 主动选择"配置语言能力不足"。
- **当前代码审计（D-036 立时）**：
  - `engine::render` 已是纯 `{{ path }}` 替换（结构合规），但 ① 注释"Tera/minijinja can replace this later"是违背原则的邀请，需删除并反转为"故意残废、禁止升级"；② 对 `{{ env=='prod' }}` 这类**默默放过**而非报错——待加：替换后扫描残留 `{{...}}`，凡非可解析的纯点路径（含 `|`/`==`/运算符等）即报错。
  - `Action`/`ComponentRef` 模型无 `when`/`loop`/表达式字段（合规）；`RunCmd.check` 是 shell 探针（run_cmd 白盒逃生口，非 YAML 表达式语言）。
- **影响**：取代/强化 [D-017]（引擎零产品知识）——D-017 管"引擎不认识产品"，D-036 管"YAML 不写逻辑"。后续 ansible 化 task/play 层必须在此约束下设计：能力进引擎，YAML 保持愚蠢。

---

## 2026-06-01 · action 层:通用 task 模型(Ansible 能力,守 D-036)

### D-037 通用 task 模型形态定案 + 分期实现
- **心智**:`crater apply <动作>`——task = "在目标机达成的一组状态"(装产品只是其一),crater 成为通用声明式远程执行引擎。详见 [action-layer.md](action-layer.md)。**严守 D-036**:task 纯数据,控制流全在 Rust。
- **形态定案**(§8 六项)：
  1. 顶层动作列表字段名 **`actions:`**。
  2. **phase 并入**有序 `actions`,每项可选 `phase:`(默认 `install`),取代独立三段;旧 `component.yaml` 的 preflight/install/verify 三段仍兼容加载。
  3. 命名 task 库目录**保留 `components/`**(概念上即 task 库)。
  4. targeting:task 文件 `hosts: <group>|all` + `-i` 提供 inventory/groups + `--host` 临时覆盖(共用凭据)+ 无→本机(复用 D-035 三层)。
  5. **handlers/notify 后置**。
  6. 条件开关首批 **`when_os` + `when_offline`**(封闭枚举,非自由表达式)。
- **命令识别(方案 A,后缀自识别)**:裸名→命名 task;`.yaml/.yml`→文件;`.tar/.oci`→离线包;含 `/`或`:`→镜像;`-f` 强制文件。优先级:`-f` > `/`或`:` > 后缀 > 裸名。
- **D-036 在 task 模型的落地**:循环→引擎遍历(action 收列表参数)或 targeting(组内逐台);条件→`when_os`/`when_offline` 封闭枚举或拆 action;计算→引擎 fact 算好再 `{{ }}` 取;排序→`needs`(复用 dag 拓扑);模板→残废渲染器(D-036/#1 已落地)。
- **分期**：
  - **本期(D-037-a)**：`TaskFile` schema(`name/hosts/vars/materials/actions[]`,action 项含 `id/needs/phase/when_os/when_offline/retries/ignore_errors` + 原语)+ 引擎 `plan_from_task`(when 过滤 → needs 拓扑 → 生成 plan)+ apply 识别 task 文件 + 三层 targeting 执行 + 真机 yq task 验证。`retries/ignore_errors` 字段已解析,**运行时行为下期**;`hosts` 本期支持 `all`(组过滤下期)。
  - **后期(D-037-b)**：handlers/notify、retries/ignore_errors 运行时、`hosts` group 过滤、原语补齐(file/copy/service/lineinfile/user/group)、register/hostvars 在 task 模型下打通。
- **守 D-036**：实现中任何"想给 YAML 加条件/循环/表达式"的冲动 → 停 → 挪进 Rust。

---

## 2026-06-01 · build → 本地库 / save → 文件(docker 式分工)

### D-038 去掉 build 的 --image/-o;build 进本地库,新增 save 导出文件
- **背景**:`crater build --image -f spec -o x.oci` 把"构建"和"出文件"耦合,且 `--image` 是历史开关(B 类 artifact 已是唯一形态,D-033)。对齐 docker 心智:build 产物进库,save 导出文件。
- **决策**:
  - **`crater build -f spec [-t ref]`**:总是构建 B 类 OCI artifact 进**本地库** `~/.crater/store`(去掉 `--image` 和 `-o`)。`-t` 指定引用(默认 `crater/<name>:<ver>`)。实现:复用 build_image_bundle 产临时 .oci → `ImageStore::import_all` 导入库。
  - **`crater save <ref> -o x.oci`**:从库导出 oci-archive 文件(`ImageStore::export_oci_archive`,import 的逆;保留 index 上的 `artifactType` 使文件可被 load/apply 识别)。`-o` 归 save。
  - 分工:**build→库 / save→文件 / load←文件 / push·pull↔registry / apply←库·registry**(docker 同构)。
  - legacy tar-bundle `build_bundle`(crater-manifest 格式)不再被任何命令调用,标 `#[allow(dead_code)]` 保留(apply/deploy 仍能读老包)。
- **真机**:`build -t .../yq:built`→库;`crater images` 见;`save -o /tmp/saved.oci`(13MB);清库→`load`(包内 ref)→`apply` recipe-replay 装 yq v4.53.2;`build` 无 -t → `crater/yq:4.53.2`;`save` 不存在 ref 报错。33 tests 绿。

---

## 2026-06-01 · action 原语补齐(D-037-b 首批)

### D-039 file / copy / service 原语
- **file**:`state: directory|absent|touch` + `mode/owner/group`;引擎生成 `mkdir -p`/`rm -rf`/`touch` + chmod/chown,幂等探针 `test -d` / `test ! -e` / `test -e`。
- **copy**:控制端文件(`src` 相对 task 目录)**内联进 plan**(agent 也能写,不依赖控制端路径)+ sha256 幂等 + chmod;文本 only(二进制走 `place`)。
- **service**:systemd `started/stopped/restarted` + `enabled`;started/stopped 用 `is-active` 探针幂等,restart 总执行。
- **实现**:`Op::WriteFile` 加 `mode` + sha256 幂等(`copy` 复用;`render_template`/`write_file` 同获幂等)。三原语全是 Shell/WriteFile 这类既有 Op,旧 agent 也能执行(mode 经新逻辑 shell/local 通道 chmod)。
- **守 D-036**:全声明式数据,幂等/排序/条件由引擎,YAML 只声明原语 + 参数。
- **真机**:本机 + n11 `--shell` 跑 `examples/fcs-demo.yaml`:`file` 建 `/etc/crater-demo`(755)、`copy` 推 `app.conf`(644+内容)、`service ssh` 幂等 ok、`needs` 排序正确;再跑 `changed=0`。33 tests 绿。

---

## 2026-06-01 · action 原语补齐(D-037-b 第二批)

### D-040 lineinfile / user / group 原语
- **lineinfile**:`state: present|absent`(共享 `Presence` 枚举)+ 可选 `regexp` + `create`。present 时(有 regexp)`sed` 删匹配行再 append(实现"替换为 line"),幂等探针 `grep -qxF '<line>'`;absent 删行,探针 `! grep`。line/regexp 单引号入 shell(假定不含 `'`)。
- **user**:`useradd`(`-r/-s/-d -m/-G`)/`userdel -r`;幂等探针 `id`。
- **group**:`groupadd`(`-r`)/`groupdel`;幂等探针 `getent group`。
- **守 D-036**:全声明式,幂等/排序由引擎。
- **真机本机**(`examples/lug-demo.yaml`):`group craterdemo` + `user crateruser`(附加组生效)+ `lineinfile` 把预置的 `max_connections=50` 经 regexp 替换为 `=100`(仅 1 行);`needs` 排序(group→user);再跑 `changed=0`。33 tests 绿。

---

## 2026-06-01 · register/hostvars 进 task 模型

### D-041 task 模型的 register/hostvars(复用 D-030)
- **TaskFile 加顶层 `register: [{name, cmd}]`**(复用 `component::RegisterSpec`):每个 host 跑完 actions 后采集 fact。
- **apply_task host 编排改为复刻组件管线**(D-030/D-031):`group_hosts_by_role` 把 host 按 role-set 分组,**组间串行**(producer 组的 register 先落 hostvars)、**组内并行**;每组完成后把各 host 的 fact 合入全局 `hostvars`。
- **run_task_on_host** 注入 `hostvars.<host>.<name>` 到 ctx.vars(供 `plan_from_task` 渲染),执行 plan 后采集 `register` cmds → 返回 `(host, registered)`。
- **守 D-036**:排序/分组/合并在引擎;`{{ hostvars.* }}` 纯取值,尚无值时残废渲染器保留(不报错)。
- **真机**(`examples/cross-node-task.yaml` + 两节点 inventory,roles first/second):n11(first 组)注册 ip → 组间串行 → n12(second 组)的 action 渲染出 `first-node = 192.168.73.11`;n11 自身无 peer 时留字面。33 tests 绿。

---

## 2026-06-01 · task 运行时:retries/ignore_errors/handlers + hosts 组过滤

### D-042 task 控制端驱动 + retries/ignore_errors/notify/handlers + hosts 组过滤
- **task 模型改为控制端逐 step 驱动**(不再走 agent):per-step 的 retries/ignore_errors/notify 需要控制端看到每步结果。component 模型仍保留 agent;task 的 agent 化(把策略编码进 plan)列为后续。
- **`plan_from_task` 返回 `Vec<TaskStep>`**(op + retries + ignore_errors + notify);`plan_handlers` 把 `handlers:` lower 成 `id->Op`;`execute_task` 在控制端逐 step 执行:
  - `retries: N` —— 失败重试至多 N 次。
  - `ignore_errors` —— 失败(含重试用尽)转 `warn`,不中断。
  - `notify: [hid]` + `changed` —— 排入 handler;所有 actions 后,被触发的 handler 去重、按 notify 顺序执行一次(ansible 语义);step 为 `ok` 不触发。
- **`hosts: <group>` 组过滤**:`apply_task` 只取 `roles` 含该组的 host(`all`=全部;无 roles 的 CLI/本机 host 视为已选,总匹配)。
- **守 D-036**:retries/ignore_errors/notify/hosts 全是封闭数据字段,控制流在 Rust。
- **真机**(`examples/d037b-demo.yaml` 本机):`ignore_demo`(exit 3)→ warn 不中断;`retry_demo`(首次 exit 1)→ retry 1/2 → changed;`conf` changed → 末尾 handler 执行(再跑幂等 ok → handler 不触发)。`examples/hostfilter-demo.yaml`(`hosts: first` + first/second inventory)→ 仅 n11 跑。33 tests 绿。

---

## 2026-06-01 · 命名 task 库 + inventory 嵌套 groups

### D-043 命名 task 库 + inventory `groups:`(嵌套组)
- **命名 task 库**:`crater apply <name>`(裸名,无后缀/无 `/:`)→ 解析 `tasks/<name>.yaml`(actions 格式)并 apply_task;不存在则回退组件 `components/<name>`(旧路径,兼容)。
- **inventory 嵌套 `groups:`**:`Inventory` 加 `groups: BTreeMap<String, Vec<String>>`(组→成员,成员是 role 名或其它组名,可嵌套)。task 的 `hosts: <group>` 由 `expand_group` 递归展开为 role 集合(防循环),保留 roles 与之相交的主机(`all`=全部;无 roles 的 CLI/本机主机视为已选)。
- **守 D-036**:groups 纯声明数据,展开/过滤在 Rust;不把 k8s 拓扑词汇固化进引擎(组名是某部署的数据)。
- **真机**:`crater apply yq --host n11`→`tasks/yq.yaml` 装 yq v4.53.2;`examples/group-demo.yaml`(`hosts: cluster`)+ inventory `groups: {cluster:[control,worker]}` → n11(control)+n12(worker)各跑一次。33 tests 绿。

---

## 2026-06-01 · task 默认走自举 agent(修正 D-042)

### D-044 task 经自举 agent 执行(自举 agent 贯穿 component+task)
- **背景**:D-042 把 task 做成控制端逐 step 驱动,理由(控制端要看每步)不充分——component 的 agent 模式本就目标本地跑、回传汇总,用户一直接受。task 单独绕开 agent 破坏了"自举 agent 是 crater 全局默认执行模型"的一致性。
- **修正**:**task 默认走自举 agent**(与 component 一致);`--shell`/本机走控制端 `execute_task`(agentless 逃生,D-027)。
- **实现**:
  - `TaskStep`/`TaskPlan`(steps+handlers)可序列化;`task_plan_to_yaml`/`from_yaml`。
  - `crater agent --task-plan <file>`:目标本地读 task plan → `execute_task`(LocalExecutor);retries/ignore_errors/notify/handlers 全在目标内执行,输出转发。
  - 抽 `push_agent_binary`(component/task 共享 binary sha256 缓存推送)+ `forward_agent_output`;新增 `run_task_via_agent`。
  - `run_task_on_host`:默认 `run_task_via_agent`,`do_shell`/`is_local` 走控制端 `execute_task`。register/hostvars 仍控制端组间串行采集(不变)。
  - 重建 bundled musl agent(`dist/crater-linux-x86_64`,含 `--task-plan`);sha256 变 → 自动重推。
- **真机**:命名 task `apply yq --host`(推新 agent → executing task on target → yq v4.53.2);`d037b-demo --host`(ignore_errors/retry/handler 全在目标 agent 内,输出转发);`--shell`(控制端,无 agent)。33 tests 绿。

---

## 2026-06-01 · task 离线打包(收敛前提)

### D-045 task → B 类 OCI artifact + recipe-replay(task 格式)
- **build**:`crater build -f <task>.yaml [-t ref]` 检测 `is_task_file` → `build_task_to_store`:抓 `binary` materials(按 material 名)+ task YAML 作 recipe → `store_component_artifact`(run-mode "task")→ 本地库。component spec 仍走旧 build。
- **recipe-replay**:apply 侧(`apply_image_ref` 的 registry/store ref + `apply_oci_bundle` 的 `.oci` 文件)materialize 后检测 recipe `is_task_file`:
  - task → `apply_task(offline_blobmap=Some)`:`plan_from_task` 离线(`place` 从包内 blob 推)、控制端 `execute_task`(blobs 在控制端)。
  - component → 旧 `run_pipeline`(兼容)。
- **apply_task 加 `offline_blobmap` 参数**;`run_task_on_host` 据此设 `ctx.offline_blobs` 并走控制端(离线不走 agent,与 component 离线一致)。
- **守 D-036/D-034**:materials 按名打包/取用;recipe 是声明数据。
- **真机**:`build -f tasks/yq.yaml -t <zot>/yqtask:1.0`(recipe=task,含 actions/place)→ `push` → 清库 → `apply <ref>`(pull→recipe-replay→place offline)→ n12 yq v4.53.2;`save -o yq.oci` → `apply yq.oci`(offline task artifact)→ n12 yq。33 tests 绿。
- **意义**:补上 task 模型最后短板(离线),为把 component 模型收敛到 task 扫清前提。
