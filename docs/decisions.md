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

---

## 2026-06-01 · component 模型彻底收敛到 task

### D-046 删除 component 模型,统一为 task(2a–2d)
- **背景**:task 模型功能已 ≥ component(D-037~D-045:actions/needs/phase/when、materials/place、register/hostvars、retries/ignore_errors、handlers/notify、hosts 组过滤、命名库、嵌套 groups、自举 agent、离线打包、16 原语),component 模型冗余。
- **2a**:迁 mysql/zot → `tasks/`,删 elasticsearch/node_exporter。
- **2b**:`crater <name>` + 裸名 `apply` 统一路由命名 task `tasks/<name>.yaml`;删 `deploy_shortcut`/`parse_flags`/`ShortcutFlags`/`resolve_component`/component-fleet。
- **2c**:删 component 执行路径(`run_pipeline`/`run_host`/`build_image_bundle`/`build_bundle`/`apply_spec`/`Artifacts`/`order_components`/`resolve_descriptor`/`execute_plan`/`run_via_agent`);`apply`/`build` 全归 task;删 spec examples。
- **2d**:core 删 `ComponentDescriptor`/`Check`/`build_plan`/`check_op`/`collect_*`;`CraterSpec` 精简为仅 inventory;删 `ComponentRef`/`to_inline_descriptor`/`AiConfig`;**AI 改生成 task**(`nl_to_task`);`doctor` 改扫 `tasks/` 的 service/systemd_unit;删 `components/` 目录 + materials-preview。
- **结果**:单一 task 模型。`crater apply`(本机/`--host`/`-i`)、命名 task、镜像、离线 artifact 全归 task;自举 agent 贯穿;`Action` 16 原语保留。
- **残留**(后续):`bundle.rs` legacy(`Manifest`/`store_blob`/`store_rootfs_layer` 等,仅 bundle 单测用);`README`/`docs/features` 历史 component 描述待刷新。
- **真机**:2a–2c 各步已真机验证;2d 后命名 task agent 冒烟通过。33 tests 绿,0 警告。

---

## 2026-06-01 · 删除 `action: download`,materials 成为获取外部内容的唯一途径

### D-047 删 `download` 原语,外部文件统一走 `materials:` + `place`
- **背景**:讨论 docker/arch 离线打包时澄清——`build_task_to_store` 实际**只读 `task.materials`**(`main.rs`),早已不扫 actions(component 时代的 `collect_downloads` 在 D-046 随 `build_plan` 删除)。但 `Action::Download` 仍在,它是个「写了能在线 `curl`、但永远进不了离线包」的原语:build 不打它,离线 `apply` 必挂。这违反「打什么进包」的唯一真相源 = `materials:`。
- **决策**:**删除 `Action::Download`**。凡需获取外部文件(二进制/tarball)一律声明 `materials:` 条目 + `action: place`。`place` 在线 `curl` material 的 `url_tmpl`、离线从包内 blob 取(按 material 名),一份数据两态通吃。
- **理由**:① 唯一真相源——「需要外部内容」⇔「必须声明 material」,build 永不漏、也永不需扫 action 反推依赖;② 消除离线陷阱(download 在离线静默失败);③ 把 arch 维度收敛到 `Material` 一处(download 的 arch 问题随之消失);④ 守 D-034「依赖与动作解耦」、D-017「引擎零产品知识」。
- **影响**:`component.rs` 删 `Download` 变体 + `produces_files`(已无引用,`build --image` 早删);`engine.rs` 删 `action_op` 的 download 分支,两处测试改用 `place`;`ai.rs` 系统提示去掉 `download`;`bundle.rs`/`engine.rs` 注释刷成 material/place 口径;文档(action-tasks/modules/idempotency/action-layer/engine-zero/design/offline-format §4)的原语清单与 `collect_downloads` 描述一并刷新。现有 task/example 无一使用 `action: download`,数据零改动。
- **现存原语 9 个**:`pkg_install`/`place`/`extract`/`render_template`/`write_file`/`systemd_unit`/`run_cmd`/`load_image`/`module`(+ D-037-b 的 file/copy/service/lineinfile/user/group)。
- **结果**:30 tests 绿,0 警告。为下一步 binary 物料的 **arch 维度**(per-host `uname -m` 探测 + `Material.arch` + build 多 arch 出 OCI index)扫清前提。

---

## 2026-06-01 · `kind: binary` 物料的 arch 维度

### D-048 material 按 arch 分变体,apply 探测 `uname -m` 选,build 多 arch 打包
- **背景**:讨论 docker 离线时发现 arch 缺口——`tasks/yq.yaml` 写死 `yq_linux_amd64`,推到 arm64 机器是个跑不起来的二进制(`Exec format error`),只是一直在 amd64 的 .11/.12 上测没撞到。三种 `MaterialKind`(binary/image/os_package)全吃 arch:binary 按 arch、image 复用镜像原生 index、os_package 是 os×arch 二维。本期只做实已全链路的 `binary`,把 arch 地基建好供后两者复用。
- **决策**:
  - 新增 `arch::Arch`(amd64/arm64/unknown),canonical 用 OCI `platform.architecture` 拼写,`uname -m` 的 `x86_64`/`aarch64` 作 serde alias;`detect_via`(`uname -m`)/`detect_local`(`std::env::consts::ARCH`,dry-run 预览用)。
  - `Material` 加 `arch: Option<Arch>`。**省略=arch 中立**(脚本/配置);**显式=arch 专属**(二进制)。
  - `PlanContext.materials` 由 `Map<name,Material>` 改 `Map<name,Vec<Material>>`(同名变体);加 `target_arch`、`resolve_material`(规则:精确 arch → 中立 → 报错)、`material_blob_key`(`name` 或 `name@arch`)。
  - apply per-host 探测 arch 喂 `ctx.target_arch`;`place` 在线选变体 curl、离线按 `name@arch` 取 blob(留 `name` 旧包 fallback)。
  - `crater build` 默认打**所有**声明 arch 变体(blob 按 `name@arch` 标注),`--arch amd64[,arm64]` 收窄。
- **理由**:① arch 是横切轴,建在 `Material` 一处(D-047 删 download 后获取外部内容只此一途);② 单 arch 也强制写 `arch` → 错配目标**响亮失败**而非静默推错二进制(气隙刚需);③ 守 D-036(YAML 只声明 arch 维度,选哪条是 Rust 逻辑,无 `when: arch==`)。
- **docker 转 B1**:`tasks/docker.yaml` 由 `pkg_install`(离线空包,D-047 评估)改**官方 static tarball**(`docker-<ver>.tgz` amd64/arm64 双变体)→ extract 到 `/usr/local/bin` → 写 containerd/docker systemd unit → daemon-reload → 起服务。纯 binary material,真离线可行。
- **真机/验证**:`build -f tasks/yq.yaml`(`fetch yq-bin@amd64` + `yq-bin@arm64`,`recipe + 2 material(s)`)→ 本机离线 `apply`(`place (offline) yq-bin@amd64`,按 `uname -m` 选)→ yq v4.53.2;`--arch arm64` 只打 1 个;docker B1 真机 .11 装成(client 27.3.1,9 步 changed=5)。`resolve_material` 单测覆盖精确/中立/无变体报错。34+2 tests 绿。
- **待续**:① **OCI image index** 按 arch 组织(registry 路径每台只 pull 自己 arch;现状 `name@arch` 多 blob 同装一 artifact,离线正确但 registry 拉全量);② `kind: image`(复用镜像原生 index)、`kind: os_package`(os×arch 矩阵)接线时一并做 arch。

---

## 2026-06-01 · crater delete:由 task 的 `teardown:` 卸载,opt-in 不强制

### D-049 `crater delete` 跑 task 声明的 `teardown:`,绝不自动逆向 `actions:`
- **背景**:讨论 apply 的反向操作时澄清——**真实软件的清理不是 apply 步骤的逆**。`run_cmd` 不可逆(引擎不知道任意命令的逆);更关键的是清理对象多是**运行时生成的状态**,install 步骤从没创建过:k8s 的 etcd/CNI/iptables(故 k8s 用 `kubeadm reset` 而非倒撤 init)、mysql 的 `/var/lib/mysql`、docker 的 `/var/lib/docker`/`/var/lib/containerd`(运行期拉的镜像/容器)。逆推 `actions:` 或逆推 apply 的 journal 都碰不到这些 → 自动逆向**必然残留、必然不一致**。
- **决策**:清理是**产品知识 → 数据**(和 install 对称)。`TaskFile` 加可选 `teardown: Vec<ActionStep>`,`crater delete <source>` 用**同一引擎**(`plan_from_task`/`execute_task`)跑这段。**opt-in 不强制**:task 没声明 `teardown:` → `delete` 明确报错「has no delete capability」,**绝不**拿 `actions:` 自动逆向兜底(那是虚假安全感)。
- **理由**:① 守 D-017(引擎零产品知识——`kubeadm reset`、删 `/var/lib/docker` 这些只有作者知道,写成声明,引擎只跑);② 守 D-036(teardown 也是纯数据 actions);③ 复用全部既有能力:同原语、幂等(`file: absent`/`service: stopped` 天生幂等)、dry-run、多机、agent、离线 recipe-replay。
- **实现**:CLI 加 `Cmd::Delete`(镜像 `Apply` 的 source/targeting);`teardown: bool` 贯穿 `apply_source → apply_task → run_task_on_host`,真则选 `task.teardown`、空则提前 bail;`apply_oci_bundle`/`apply_image_ref` 同步带参(artifact/离线也能 delete,recipe 含 teardown)。
- **tasks/docker.yaml** 加 `teardown:`:stop+disable docker/containerd → 删 `/var/lib/docker`+`/var/lib/containerd`(**运行时状态,作者声明**)→ 删 binary/unit/`/etc/docker` → daemon-reload。
- **真机/验证**:`delete yq` → 报错(yq 无 teardown,opt-in 生效);`delete docker --dry-run` → 12 步拓扑有序;`delete docker --host .12`(已装 docker)→ changed=12,docker/containerd inactive、`/var/lib/docker` 等全清;再删幂等 `changed=1 ok=11`(仅 daemon-reload 重跑)。34+2 tests 绿。

---

## 2026-06-01 · 删 `deploy` 子命令、`agent` 隐藏并精简

### D-050 `deploy` 删除(被 `apply <x.oci>` 覆盖);`agent` 转内部隐藏、只留 `--task-plan`
- **背景**:盘点 CLI 冗余。`crater deploy --bundle x.oci --host` 与 `crater apply x.oci --host` 走同一个 `apply_oci_bundle`,且 deploy 还少 `-i`/`--key` → 是 apply 的子集。`agent` 是自举执行的**内部入口**(控制机推二进制后在目标跑 `crater agent --task-plan`),不是给人用的;其 `--plan`(component Op 计划)分支自 D-046 删 component 模型后已成死代码。
- **决策**:① **删 `Cmd::Deploy` + `deploy_bundle`**——离线部署统一走 `crater apply x.oci`。② `agent` 加 `#[command(hide = true)]` 从 `--help` 隐去(对人不可见,仍可被控制机调用);删死的 `--plan`,`run_agent` 精简为只收 `--task-plan`。
- **理由**:单一入口(apply 即部署,守 D-020 在线/离线单管线);CLI 表面更小、更诚实(内部命令不暴露给用户);清死代码。`engine::plan_from_yaml/plan_to_yaml/execute`(Op 级)保留——引擎测试仍用。
- **影响**:`apply_oci_bundle`/`apply_image_ref` 不变(apply 路径仍用);mod 顶部用法、`offline-format §5`、`requirements F12`、`self-bootstrap-agent.md` 刷成 `apply x.oci` / `agent --task-plan`;`progress.md` 历史日志保留 deploy 记录。
- **验证**:`crater deploy` 已无;`--help` 不含 deploy/agent;`crater agent` 仍在(要求 `--task-plan`);`apply yq --host .12` 经 agent 路径执行成功(yq 重装)。34+2 tests 绿,0 警告。

---

## 2026-06-01 · 部署状态管理 Phase 1a:marker + 控制端 Turso DB + `crater task`

### D-051 部署态:目标机 marker(真相)+ 控制端 Turso 聚合库(`crater task list/history`)
- **背景**:要做"部署 task 状态监控 + Web UI"。但 crater 一直**无状态/agentless**——不知道"什么装在哪"。两功能都要状态,核心决策是**状态存哪、谁是真相源**(详见对话:k8s `kubeadm reset`/mysql `/var/lib/mysql`/docker `/var/lib/docker` 那轮)。
- **决策(两层状态)**:
  - **目标机 marker**(`/var/lib/crater/state/<task>.json`)= **真相源**:apply 成功写、delete 删,**就是个文件、executor 读写,目标机仍 agentless**。任何控制机都能读、抗控制机丢失。
  - **控制端 Turso DB**(`~/.crater/state.db`)= 聚合/缓存 + job 历史,喂 `crater task list/history` 和将来的 Web UI,不必每次 SSH 每台。
- **DB 选型 Turso**(纯 Rust SQLite 重写,`tursodatabase/turso` v0.6.1):`default-features=false` + `pure-rust-crypto`,**关 mimalloc(C)/sync(云同步)**。实测 **musl 静态编过、纯 Rust 无 C**(守 N1);代价是依赖树大(~493 crate,拉 wit/wasm 工具链)、编译变慢——**N1 由"免 C"理解为"免 C + 纯 Rust",体积/依赖不算极简,但运行时仍单静态二进制**。藏在 `StateStore` trait 后(可换 redb/rusqlite)。
- **接线**:`apply`/`delete` 成功后 → 目标机写/删 marker(`run_task_on_host`,best-effort)+ 控制端 DB upsert/delete + 记 job_run(`record_deployments`,best-effort,不影响部署结果)。`apply_source→apply_task→run_task_on_host` 贯穿 `source` 串。
- **命令**:`crater task list`(默认读控制 DB;`--host`/`-i` 读目标机权威 marker)、`crater task history [--limit]`。
- **真机验证**:`apply yq` 本机 + `--host .12` → `task list` 两条、`task history` 两条 apply;`task list --host .12` 读到 .12 的 marker(JSON 内容核对);`delete yq --host .12` → marker 删、DB 删、list 中 .12 消失、history 多一条 delete。34+2 tests 绿,0 警告。
- **Phase 1b(下一步)**:`crater task status`(漂移检测——重跑 verify 阶段比对声明态 vs 实际)。**Phase 2**:`crater ui`(Axum + htmx 只读看板读同一 DB)。

---

## 2026-06-01 · 定义 "task" 语义:逻辑部署单元,`task list` 按 task 维度

### D-052 `crater task list` 改 task 维度(hosts 为属性),加 `task show` 下钻
- **背景**:`task list` 原按 `(host, task)` 平铺——`apply yq -i inv` 明明是**一个 task**(部署到两台),却列成两行,把"task(逻辑部署单元)"和"per-host 实例"混了。
- **决策**:**task = 一次部署的逻辑单元**,hosts 是它的属性(对标 Helm release / `helm list`)。
  - `crater task list` → **一行一个 task**:`TASK｜VERSION｜HOSTS(聚合,含计数)｜LAST APPLIED`;版本不一致显示 `… (mixed)`。
  - `crater task show <name>` → 下钻该 task 的 per-host 明细(`HOST｜VERSION｜APPLIED｜SOURCE`)。
  - per-host marker 仍是真相源,只是默认不平铺。数据模型不变(仍按 `(host,task)` 存),只在 CLI 侧 Rust 聚合(小数据量,无需 SQL GROUP BY——也印证 SQL 非刚需)。
- **影响**:`gather_deployments` 统一从控制 DB 或目标机 marker 取实例;`task_list` 聚合渲染、`task_show` 过滤渲染。`list`/`show` 都支持 `--host`/`-i`(读权威 marker)。
- **真机验证**:`apply yq -i inventory(n11/n12)` → `task list` 一行 `yq 4.53.2 n11,n12 (2)`;`task show yq` 两行 per-host;本机再 apply → list 聚合成 `localhost,n11,n12 (3)`。34+2 tests 绿。
- **Phase 1b 顺延**:漂移用 `list --verify`/`show <name> --verify`,不单开 `status`(用户定)。

---

## 2026-06-01 · AWX 启示:run 历史为主视图,可选部署名作分组身份

### D-053 唯一性是 list 的分组问题(非 apply 概念);history 主视图,部署名可选分组
- **背景**:追问"apply 里哪个是唯一值"→ 厘清:**apply/delete 是无状态收敛 `(配方, 主机)`,本身不需要任何身份**;唯一性是 `task list` 才逼出来的**分组/展示**问题。参考 **AWX**:Ansible 同样无状态,AWX 是 **run/活动中心**(Jobs 主屏),**没有 release/部署对象**;其"身份"= **Job Template**(命名的 playbook+inventory 绑定),A/B 用两个命名模板区分。
- **决策**:
  - **apply/delete 不引入身份**——保持无状态收敛,一行不为身份改。
  - **可选部署名(deployment,默认=task 名)**作为**纯分组标签**:`apply <name> <source>`(复用已有两段式),只影响 `task list` 分组,apply/delete 行为不变。对标 AWX 的"命名绑定"。
  - **`task history` 是主视图**(AWX Jobs 式,不可变活动流);**`task list` 是 crater 的额外福利**(当前部署快照,AWX 都没有——因 crater 有 marker;可漂移,留 `--verify`);`task show <name>` 下钻。
- **实现**:`Marker`/`Deployment`/`JobRun` + DB 三表加 `deployment` 列(默认=task 名,marker 仍按 task 名存一份/host);`apply` 的 `name` 串入 `apply_task→run_task_on_host/record_deployments`;`task list` 按 deployment 聚合(`DEPLOYMENT｜TASK｜VERSION｜HOSTS 计数｜LAST APPLIED`),`history` 加 DEPLOYMENT 列。**HOSTS 改计数**(大批量不平铺,主机名去 `task show`)。
- **真机验证**:`apply yq-a yq --host .11` + `apply yq-b yq --host .12`(同一 task.yaml)→ `task list` 两行 yq-a/yq-b 各 1 host;`task show yq-a` → .11;`history` DEPLOYMENT 列区分;`apply yq`(无名)→ deployment 默认 `yq`。schema 变更需清旧 `~/.crater/state.db`。34+2 tests 绿。

---

## 2026-06-01 · 部署状态 Phase 2:crater ui(Axum + htmx 只读看板)

### D-054 `crater ui`:Axum 后端 + htmx 前端,只读看板,读同一 Turso 库
- **背景**:Phase 1 已有部署状态(marker + Turso + `crater task`)。Phase 2 给它一个 Web 看板。受 AWX 启发(D-053):run/活动为主、当前态为辅。
- **决策**:`crater ui [--bind 127.0.0.1] [--port 8080]` 起 **Axum** 服务,读控制端 Turso 库,渲染 HTML 片段;前端 **htmx** 轮询(`hx-trigger="every 5s"`)刷新。**只读**(列部署 + 历史),后续再加从 UI 触发 apply/delete。
- **守约**:① 纯 Rust(axum/hyper/tower/tokio,无 C,musl 静态编过——31.7MB,+13MB);② **htmx.js vendor 进仓库 + `include_bytes!` 嵌入二进制**,气隙零网络可用;③ 默认 `bind 127.0.0.1`(最小攻击面);④ UI 是**视图**,逻辑留引擎/CLI(D-036),不持产品逻辑;HTML 服务端渲染、JS 近乎为零。
- **路由**:`/`(页面壳)、`/api/deployments`(片段,按 deployment 聚合)、`/api/history`(片段)、`/htmx.min.js`(嵌入资源)。handler 经 `Arc<TursoStore>`(axum State)查库。
- **验证**:`crater ui --port 8090` → `/` 出页面、`/api/deployments` 列 yq(2 hosts)、`/api/history` 含 apply/delete + DEPLOYMENT 列(my-yq01/yq 区分)、`/htmx.min.js` 50917 字节。musl 静态、34+2 tests 绿、0 警告。
- **后续**:从 UI 触发 apply/delete(写操作);`--verify` 漂移列(Phase 1b)接进看板;鉴权(对外暴露时)。

---

## 2026-06-01 · 部署状态 Phase 1b:`--verify` 漂移检测(检测→re-apply 自愈)

### D-055 `crater task list/show --verify`:重跑 verify 阶段检测漂移
- **背景**:讨论"检测到以后呢"——厘清:对幂等收敛工具,**修复 = 直接 re-apply**(一趟发现+修,changed 即被纠正的漂移),检测**不是修复前置**;检测的价值是**只读态势感知/合规**(政企"看谁漂了但别动")。
- **决策**:`crater task list --verify` / `task show --verify`——对每个 deployment,**重跑其 task 的 verify 阶段**(如 `yq --version`/`docker --version`,本就只读),全过=ok、有失败=DRIFT。需 `--host`/`-i`(要连机器)。
- **实现**:`verify_on_host(exec, source)` 从 marker 的 `source` 解析 task 文件(命名 task/路径;artifact ref 暂不支持→`?`)→ 过滤 verify 阶段动作(清 `needs`)→ `plan_from_task` → 逐个 `Op::Shell` 在目标跑、非 0 即 DRIFT;无 verify 阶段→`?`(无探针可判)。`list --verify` 聚合(`ok N/M`/`DRIFT x/M`),`show --verify` per-host(ok/DRIFT/?)。
- **"然后"= re-apply 自愈**:检测只读;要修就 `crater apply`(幂等收敛,changed 即修复的漂移)。
- **真机验证(.11/.12)**:`list --verify` → `ok 2/2`;`rm /usr/local/bin/yq` on .12 → `show yq --verify` → n11=ok/n12=DRIFT,`list --verify` → `DRIFT 1/2`;`apply yq -i inv` → .12 changed=1 自愈;`list --verify` → 回到 `ok 2/2`。34+2 tests 绿,0 警告。
- **后续**:`--verify` 状态进 `crater ui` 看板;artifact-source 的 verify(拉 recipe)。

---

## 2026-06-01 · 漂移状态接进 crater ui 看板

### D-056 `--verify` 结果持久化到 DB,`crater ui` 只读显示漂移
- **背景**:UI 是被动只读、无凭据、不连主机;而 `--verify` 要连机器跑。所以**不能让 UI 去 verify**——而是 **`--verify`(CLI,有凭据)把结果写进 DB,UI 只读显示**(契合"CLI 改/查、UI 是视图")。
- **决策/实现**:
  - `deployments` 表加 `status`(ok/drift/unknown)+ `checked_at`;`Deployment` 加同名字段。
  - **apply 成功 → status='ok'、checked_at=applied_at**(apply 本就跑了 verify 阶段)。
  - 新 `StateStore::record_verify(host,task,ok,checked_at)`(UPDATE);`task list/show --verify` 在连机器 verify 的同时**把每台结果写回 DB**。
  - `crater ui` 部署表加 **Status 列(DRIFT 标红 / ok 绿 / unknown 灰)+ Checked 列**;聚合 `ok N/M` / `DRIFT x/M`。
  - **UI handler 每请求重开 DB**(Turso 跨进程写可见性:长期句柄看不到 CLI 进程的新写入;fresh open 读到已提交状态)。
- **真机验证(.11/.12)**:`apply yq -i inv` → UI `ok 2/2`(绿);`rm yq@.12` + `task list --verify -i inv` → UI **`DRIFT 1/2`(红)**+ checked 时间;`apply` 自愈 → UI 回 `ok`。34+2 tests 绿,0 警告。schema 变更需清旧 `~/.crater/state.db`。

---

## 2026-06-01 · crater ui 改现代深色主题(D-057)

### D-057 Web 看板重设计:深色 + 熔岩橙强调 + 统计卡片 + 状态 pill
- **背景**:初版 UI 过于朴素。参考 Uptime Kuma / Portainer / Teleport / Signal Dashboard 等现代 infra 看板。
- **决策/实现**:全部纯 CSS 内联(离线嵌入,无构建工具链/CDN,守 N1):
  - 深色主题(`#0c0e15`)+ **crater 熔岩橙强调 `#ff6b35`**(呼应 crater);顶栏毛玻璃 + 脉动 live 点。
  - 顶部**统计卡片**(`/api/stats`):Deployments / Hosts / Healthy / Drift(Drift>0 标红 + 左色条)。
  - **状态 pill 徽章**:ok 绿 / DRIFT 红 / unknown 灰 / apply·delete 动作 / result;带圆点。
  - 现代表格(行 hover、mono 技术字段、`<code>` 名)、卡片化 panel、响应式。
  - 新增 `/api/stats`;`index` 改静态 const(CSS 大括号免转义);htmx 轮询不变。
- **验证**:`/api/stats` 4 卡片、`/api/deployments` pill、页面含新主题。musl 静态 31.8MB,34+2 tests 绿,0 警告。

---

## 2026-06-01 · crater ui 写操作:Verify / Heal(D-058)

### D-058 UI 写操作(opt-in `--inventory`),经子进程调 CLI
- **背景**:看板从只读升级到可触发动作。但 Web UI 改机群是真实权限升级。
- **决策**:
  - **opt-in**:`crater ui -i inventory.yaml` 才启用写动作(UI 据此持有机群凭据,类似 AWX);无 `-i` 则严格只读、按钮不出现。默认 `bind 127.0.0.1`。
  - **动作**:**Verify now**(全局,重跑 verify 写回 DB,只读探测安全)+ 每行 **Heal**(re-apply 自愈,带 `hx-confirm` 确认)。**Delete 不做**(最危险)。
  - **实现走子进程**:handler `run_crater(args)` 用 `current_exe` 跑 `crater task list --verify -i ...` / `crater apply <dep> <source> -i ...`。**理由**:apply_task 内部 `buffer_unordered` 借用闭包的 future 非 Send,直接在 axum handler 里 await 触发 HRTB/Send 编译错误;子进程彻底隔离,且逐字复用 CLI 行为、零耦合。
  - htmx:按钮 `hx-post` → 返回刷新后的 deployments 片段(`#deps-pane` swap);heal 带 `hx-confirm`;`htmx-request` 期间按钮禁用。
- **真机验证(.11/.12)**:`ui -i inv` → GET 出 Verify now + 每行 heal;POST `/api/verify` → ok 2/2;`rm yq@.12` → POST verify → **DRIFT 1/2**;POST `/api/apply/yq`(heal)→ 自愈 → 返回表 ok 2/2。34+2 tests 绿,0 警告。
- **后续**:Delete(需更强确认)、操作鉴权(对外暴露)、操作进度/日志流。

---

## 2026-06-01 · crater ui:侧边栏导航 + 浅色默认/深色切换 + 动作不再依赖配置文件

### D-059 侧边栏多视图(仪表盘/主机/主机组/任务)+ 主题切换 + 动作显示不依赖 inventory 文件
- **背景**:用户反馈——① UI 展示行为不该依赖主机配置文件(我之前用 `./inventory.yaml` 是否存在来决定按钮是否显示,错);② 要侧边栏导航(仪表盘/主机/主机组/任务),像 AWX/Portainer/Teleport。
- **决策/实现**:
  - **侧边栏导航**:仪表盘(stats+activity)、主机(DB 按 host 透视:每台的 deployment/状态/最近)、主机组(读 `inventory.yaml` 的 groups+hosts 作**数据**展示)、任务(deployment 表 + Verify/Heal)。nav 点击 htmx 换 `#view`,小 JS 切 active。
  - **动作按钮始终显示**(去掉"文件存在才显示"的门控);点击时才用 `./inventory.yaml`,缺失则返回提示(运行时所需凭据,非展示门控)。
  - **主题**:浅色为**默认**,右上角按钮切换深色;`[data-theme=dark]` 覆盖 CSS 变量,`localStorage` 记忆;纯内联 + 一小段 JS(UI chrome,非业务逻辑)。
  - 主机视图来自**状态 DB**(crater 已部署的主机,不依赖配置文件);主机组来自 inventory 数据(无则提示创建)。
- **验证**:shell 出侧边栏 4 项;`/view/dashboard|hosts|groups|tasks` 各自渲染;`/api/hosts` DB 透视;`/view/groups` 读 inventory 出 n11/n12;`/api/deployments` 始终带 Verify now+heal;浅色默认 + 切换。musl 静态,34+2 tests 绿,0 警告。
- **遗留**:主机组目前只读 inventory 文件(crater 尚未把 inventory 作为受管数据存库);未来可把 hosts/groups 纳入状态库作一等对象。

---

## 2026-06-01 · 硬核 task:单节点 Kubernetes(kubeadm)+ 多节点的 when_role 缺口

### D-060 tasks/k8s.yaml(单节点 control-plane,真机验证)+ when_role 提案
- **做了什么**:`tasks/k8s.yaml` —— 用 crater 原语表达完整 kubeadm 单节点:关 swap(run_cmd+check)、内核模块(write_file+modprobe)、sysctl、containerd+SystemdCgroup+CN pause(pkg_install+sed)、pkgs.k8s.io apt 源(file/run_cmd/write_file)、kubeadm·kubelet·kubectl+conntrack/socat/ethtool(pkg_install)、`kubeadm init`(幂等 check=admin.conf)、kubeconfig、flannel CNI、去 control-plane 污点、verify;handlers reload;**teardown=`kubeadm reset`+删 etcd/CNI/iptables**(D-049 的活例子)。CN 友好:`--image-repository registry.aliyuncs.com/...` + containerd sandbox_image 同改。
- **真机验证(.11,Ubuntu 24.04)**:首跑在 `kubeadm init` preflight 挂在 `conntrack not found` → 补 conntrack/socat/ethtool → 重跑(幂等续跑)→ 19 步全过,`kubeadm init` ~76s,`kubectl get nodes` → `ubuntu Ready control-plane v1.31.14 / containerd 1.7.22`,flannel/coredns Running。`crater task list` 同时记录 k8s + yq 部署。
- **范围与诚实**:**单节点 + 在线**。离线需 kind:image(拉控制面镜像进包)+ os_package(D-034 待做)。
- **多节点缺口 → 提案 `when_role`**:真 k8s 多节点角色不对称(control 跑 `kubeadm init` 出 join token,worker 跑 `kubeadm join`)。crater 的 action 只有 `when_os`/`when_offline`,**没有 `when_role`**,且任务内所有 action 在所有匹配主机上都跑——无法在一个 task 里区分 control/worker。提案:给 `ActionStep` 加 **`when_role: [..]`**(闭合枚举开关,和 when_os 同性质,守 D-036),配合已有 **register/hostvars + group_hosts_by_role(serial-between/parallel-within)**,即可:control 角色跑 init + `register` join 命令,worker 角色跑 `kubeadm join {{ hostvars.<control>.join }}`。这是把 crater 推到"能装 k8s 集群"的关键一小步,后续单独做。

---

## 2026-06-01 · 接线 kind: image —— 镜像作为 oci-archive material 打包/离线导入

### D-061 kind:image 全链路:build 打镜像进包,apply 运行时导入
- **背景**:离线 k8s/mysql 需要把容器镜像装进 OCI 包(D-047/D-060 暴露:run_cmd/pkg_install/镜像都进不了包;唯有 materials 进包)。kind:image 此前留位未接线(D-034)。
- **模型(最简)**:**一个镜像 = 一个自包含 oci-archive blob,当 material 打包**(和 binary 同 material 层机制,bytes 是 oci-archive)。无需新 bundle 层类型。
- **build**(`build_task_to_store`):`kind: image` → `store.pull(ref)` → `export_oci_archive` → 读 tar bytes → 按 `name`(或 `name@arch`)打成 material。
- **修 `store.pull` 解多 arch**:多 arch 镜像是 manifest list/index,原 pull 只读 `config`/`layers`(list 没有)→ 只打了 14KB 空壳。现先解析 list → 选 linux/amd64 子 manifest → pull 它的 config+layers,打成完整镜像(B 类 artifact 是单 manifest,不受影响)。
- **apply**:新 `Op::ImageImport`(reference / local_archive / namespace / runtime)。`exec_one`:探测运行时(nerdctl/ctr/docker/podman);**离线**=推 oci-archive 到目标 + `ctr -n <ns> images import` / `<rt> load -i`;**在线**=`<rt> pull <ref>`。`namespace`(如 k8s.io)给 ctr/nerdctl 用。
- **action**:`load_image` 改为**引用 material**(`{material, namespace, runtime}`,弃写死 ref);在线取 material 的 `ref` pull、离线导入打进包的 blob——与 `place` 双态语义一致。
- **真机验证(.11,containerd)**:`build`(pull alpine 解 list→amd64,打 **3.6MB** oci-archive)→ `apply --host .11` 离线 → `ctr -n k8s.io images import` → `ctr -n k8s.io images ls` 出 `docker.io/library/alpine:3.20`。34+2 tests 绿,0 警告。
- **边界**:目前镜像按 build 机/amd64 单 arch 打(多 arch 镜像复用原生 index 留待);幂等(已存在则跳)未做,每次 import(无害)。

---

## 2026-06-01 · 接线 kind: os_package —— buildah 自建依赖闭包,离线本地装

### D-062 os_package:buildah(无daemon)解闭包打 tar,apply `apt-get install ./*.deb`
- **背景**:离线 k8s 的最后一块——conntrack/socat 等发行版包必须进 OCI(D-060)。研究 KubeKey 离线 ISO 机制(`hack/gen-repository-iso`:`apt-get install --print-uris` 解闭包→wget→dpkg-scanpackages→genisoimage;use 端 mount ISO + file:// 本地源 + 装 + 还原)后,与用户敲定两点简化:
  - **不消费预构建 blob,而是 crater 自己用 buildah 在目标 OS 容器里解闭包**(daemonless,不要 dockerd;用户要求"收集依赖要在环境里")。
  - **不打 ISO、不挂载、不改 apt 源**:直接打 .deb 闭包 tar,apply 用 `apt-get install ./*.deb`(apt 在本地集合内解依赖序、只装缺的,闭包完整则全程离线)——比 KubeKey 那套 mount/改源/还原/umount 简一大截(用户:"不是有 dpkg -i 吗")。
- **OS×版本×arch 维度**:闭包是 `base`(如 ubuntu:24.04)× arch 绑定的;artifact 离线支持 = 打进的 base 集合;apply 选匹配的,缺则报错(沿用 arch 的"声明变体+选+响亮失败")。v1 目标 ubuntu 24.04/amd64。
- **实现**:`Material` 加 `base`;`pkg_install` 加 `material`(引用 os_package);build `build_os_package_repo`(buildah from→run 解闭包+wget→mount→tar);新 `Op::PackageInstall`(离线推 tar+解+`apt-get install ./*.deb`/`dnf ./*.rpm`,在线 apt/yum 装 names;dpkg-s/rpm-q 幂等)。
- **代价**:构建机需装 buildah(仅构建时、daemonless;目标机仍纯净)。跨 arch 需 qemu(v1 同 arch)。
- **真机验证**:buildah 解 conntrack+socat 闭包(含 libmnl0/libnetfilter-conntrack3/libnfnetlink0/libwrap0)→ 530KB tar;`apply --host .12`(原无这俩)离线 → `apt-get install ./*.deb` → conntrack v1.4.8 + socat 装上。34+2 tests 绿,0 警告。
- **里程碑**:三种 material 全接线(binary D-048 / image D-061 / os_package D-062)——离线打包能力闭合,k8s 全 material 离线形态已无阻塞。

---

## 2026-06-01 · 离线 k8s 全 material 形态(终极验证)

### D-063 tasks/k8s-offline.yaml:一个 OCI 包离线装 k8s(干净节点真机验证通过)
- **做了什么**:把 k8s 改成全 material 离线形态,三种 material 协同:
  - **binary**:containerd/runc/cni-plugins/crictl/kubeadm/kubelet/kubectl 静态二进制 + systemd unit + kubeadm drop-in。
  - **image**:7 控制面镜像(CN 镜像源 registry.aliyuncs.com)+ flannel×2(ghcr);build pull 进包、apply `ctr -n k8s.io import` 预载。
  - **os_package**:conntrack/socat/ipset/ethtool/ebtables(buildah 解闭包)。
  - 内联 flannel 清单;`kubeadm init --image-repository <CN源>`(镜像预载 → 零联网拉)。
- **修 store.pull(D-061 续)**:① 多 arch manifest list 子 manifest 走 manifests 端点(非 /blobs,aliyun/harbor 404);② accepted 加 OCI image/index 媒体类型(ghcr 是 OCI index)。
- **build 验证**:`crater build` → recipe + 17 material(s),**488MB** .oci;9 镜像全 pull+打包。
- **干净节点离线部署验证通过**:清理 .12(去遗留 containerd)→ `apply crater/k8s-offline:1.31.14 --host .12`(488MB 经 SSH base64 推送 ~7min)→ 40 步全过 → `kubectl get nodes` **ubuntu Ready control-plane v1.31.14**,所有系统 Pod(etcd/apiserver/cm/scheduler/kube-proxy/coredns×2/flannel)Running,镜像全来自本地导入,kubeadm init **零联网拉**。**离线 k8s 完整闭环。**
- **已知限制**:大 artifact(488MB)经 SSH base64 分块推送慢(~7min)——后续可加压缩/更快传输;镜像/os_package 单 arch(amd64);单节点(多节点需 when_role,D-060)。
- **用户后续重构(待做)**:① 二进制 version/arch 抽成 vars;② flannel 清单从官方途径下载打包 + 分析 yaml 里的镜像自动入 materials(避免手列+版本漂移)。
- **里程碑**:crater 实现"一个 OCI 包离线装 k8s"——三种 material 协同的终极兑现。

---

## 2026-06-01 · material url 的 {{arch}} 自动注入 + 版本抽 vars

### D-064 渲染 material url 时自动注入 {{arch}}=该 material 的 arch 字段;二进制版本抽成 task vars
- **背景**:用户重构①——二进制 version/arch 不该写死在 url 里。
- **决策**:① 渲染某 material 的 `url_tmpl` 时(build 端 `build_task_to_store`、apply-online 端 `place`),**把该 material 声明的 `arch:` 字段作为 `{{arch}}` 注入**——arch 单一来源(material 的 arch 字段同时驱动选择 + url),多 arch 变体天然正确;② 版本抽成 task `vars`。
- **实现**:build 循环渲染前 `ctx.vars["arch"]=m.arch`;place online 用注入 arch 的临时 vars 渲染。`tasks/k8s-offline.yaml` 加 `containerd_ver/runc_ver/cni_ver/crictl_ver/flannel_ver/flannel_cni_ver` vars,url 用 `{{*_ver}}`+`{{arch}}`。
- **验证**:dry-run 渲染出的 url 与改前逐字一致(containerd-1.7.22-linux-amd64.tar.gz / runc.amd64 / dl.k8s.io/.../amd64/kubeadm …),产物等价(离线 k8s 已 D-063 端到端验证)。34+2 tests 绿。
- **遗留**:flannel 内联清单里的镜像 tag 仍写死(与 vars 默认值一致)——用户重构②(flannel 从官方拉+分析镜像)会一并解决。

---

## 2026-06-01 · `kind: binary` 统一为 `kind: file`;暂缓 yaml 镜像解析

### D-065 material 的 `kind: binary` 更名为 `kind: file`(`binary` 保留为别名)
- **背景**:重构②要把 flannel 清单也当 material 走「下载一个文件」的同一套逻辑。用户指出 kube-flannel.yml 和二进制本质都是「下载一个文件」,download 逻辑是统一的;`binary` 这个名字把它窄化成了「可执行二进制」,而实际上下载的可以是 tarball、YAML 清单、配置文件。
- **决策**:`MaterialKind::Binary` → `MaterialKind::File`,`kind: file` 为规范写法;`#[serde(alias = "binary")]` 保留旧拼写,既有 task(yq/docker/zot/k8s-offline)零破坏。`kind` 的语义明确为「**怎么获取**这份内容」:`file`(url 下载任意文件)/ `image`(pull 容器镜像成 oci-archive)/ `os_package`(buildah 解依赖闭包)。
- **理由**:概念收口——material 是「外部内容 + 获取方式」,获取方式只有三类,与「内容是不是可执行」无关。flannel.yml、10-kubeadm.conf 这类配置/清单天然落进 `file`,无需新 kind。
- **影响**:仓库 4 个 task 的 `kind: binary` 全部迁到 `file`;ai.rs 生成提示同步;新增回归测试 `file_kind_accepts_binary_alias`(两种拼写都 → `File`)。35+0 tests 绿。

### D-065b 暂缓「解析 yaml 自动提取镜像入 materials」
- **背景**:重构②原计划让 build 扫 flannel.yml 里的 `image:` 字段,自动把镜像加进 materials,免手列、防漂移。
- **决策**:**暂不实现**,等用户想清边界。
- **理由**:(1) 还有 **Helm chart** ——镜像在 values/模板里,不是字面 `image:`,扫不到;(2) crater **无法可靠判断一个下载下来的 yaml 是 k8s 清单、还是普通配置文件**,盲目按 k8s 规则提 `image:` 会误伤。自动化的收益抵不过误判风险。
- **现状**:镜像仍由 task 显式声明 `kind: image` material(手列,如 k8s-offline 的 9 个镜像)。flannel.yml 走 `kind: file` 下载,其引用的镜像单独声明为 `kind: image`。

---

## 2026-06-01 · `kind: file` 增加本地源 `src`;k8s-offline 去内联(重构②收尾)

### D-066 `kind: file` 支持 `src`(人工维护的本地文件)+ flannel 清单从官方 url 下载
- **背景**:重构②——(a) flannel 清单应从官方 release 下载而非手抄内联;(b) 还有一批官方不提供、必须人工维护的文件(systemd unit / drop-in / containerd 配置 / crictl 配置 / sysctl / modules),内联进 task.yaml 会「YAML 爆炸」。用户定调:flannel 走官方 url file material,人工文件也落 `file`。
- **决策**:`Material` 加 `src: Option<PathBuf>`(task 同级相对路径,如 `files/containerd.service`),与 `url_tmpl` 二选一。`build` 对 `src` 物料**读本地文件**打成 blob(与 url 下载同一套,同 key);`place` 在线从控制机 task 目录 `PushFile` 推送、离线从包内取 blob——**copy 语义,原样推送不做 `{{}}` 渲染**(对标 Ansible `copy: src=files/`,区别于会渲染的 `template:`)。
- **理由**:`kind` = 「怎么获取」已收口(D-065),`src` 只是 file 的第二种获取方式(本地 vs url),不新增 kind。task.yaml 从此**不内联任何文件内容**——要么 url 下载、要么 files/ 维护,爆炸问题根除,且文件可独立 diff/审阅/复用。
- **顺带修**:`build_task_to_store` **漏了把 task.vars 灌进渲染 ctx**(自 D-064 潜伏,当时只 dry-run 等价验证、未真机重建),导致 `{{containerd_ver}}` 等非 version/arch 变量原样漏进 url(404)。补上 `for (k,v) in &task.vars` 循环。
- **k8s-offline 重写**:flannel→`kind: file` 官方 url(`.../v{{flannel_ver}}/kube-flannel.yml`);7 个人工文件移到 `tasks/files/` 声明成 `file`+`src`;8 处 `write_file` 全改 `place`。核对官方 v0.28.4 清单的镜像 tag(`flannel:v0.28.4`/`flannel-cni-plugin:v1.9.1-flannel1`)与 CIDR(`10.244.0.0/16`)和手列 image material、pod_cidr 逐一对上(无漂移,这正是暂缓自动扫 image 时要人工守的点)。
- **验证**:`crater build`→25 material OCI(7 二进制+flannel清单+7 src 文件+sysdeps+9 镜像);**彻底擦净** .12(删二进制/配置/`/var/lib/containerd`)后离线 `apply` 40 步全过,7 个 src place + flannel place 全 changed,节点 Ready、8 个 Pod(含 kube-flannel)全 Running、kubeadm init 零联网拉。36 tests 绿。

---

## 2026-06-01 · task.yaml 改块式 + action 名对齐 Ansible

### D-067a 去 flow-style:task.yaml 一律块式(block-style)YAML
- **背景**:用户嫌每条动作 `- { ... }` 一行式「太难阅读太难编写」,问 `{}` 是不是 YAML 必须的。
- **决策**:`{}`(flow style)不是必须,block style(缩进)是同一份 YAML、serde 解析完全一样。仓库所有 task / `ai.rs` 生成提示 / 文档示例的动作与物料**一律改块式**,字段全保留(零引擎改动)。例外保留 flow:inventory 每主机一行 `{name,address,roles}`、`needs:[..]`/`packages:{debian:[..]}` 这类短列表小映射。
- **理由**:flow 对 AI 省事、对人难读;crater 用户是政企运维。
- **影响**:`ai.rs` 加硬性要求「Always emit block-style」;新增用户偏好记忆。

### D-067b action 名对齐 Ansible 模块名;原语统称「模块」,原 module 概念更名「角色」
- **背景**:用户指出 action 名都是 crater 自造(`run_cmd` 等),要求能对齐 Ansible 的就对齐,降低学习成本;并提议像 Ansible 一样把原语叫「模块」、单独出文档分两类。
- **术语反转**:Ansible 的 **module** = 任务原语;Ansible 的 **role** = 可复用参数化子程序。而 crater 原有的 `module`(`modules/*.yaml` + `action: module`,D-029)其实是 Ansible 的 role。故为真对齐:**原语→模块(module)**,**原 module 概念→角色(role)**。
- **决策**:
  - 原语改名(serde rename + 旧名 alias,非破坏):`run_cmd`→`shell`(alias `command`/`run_cmd`)、`pkg_install`→`package`、`extract`→`unarchive`、`render_template`→`template`。`run_cmd` 对齐的是 **`shell` 不是 `command`**——crater 命令经 shell(管道/`&&`/重定向/env 都用),叫 command 会误导 Ansible 老手。
  - 原 `module` action → `role`(alias `module`);`modules/` 目录 → `roles/`(loader 回退兼容旧 `modules/`)。
  - 已对齐的(`file`/`copy`/`service`/`lineinfile`/`user`/`group`)不动;crater 特有的(`place`/`load_image`/`write_file`/`systemd_unit`)保持原名,文档归入「crater 自有」类。
- **影响**:`tasks/*.yaml` 全量改用规范名;`ai.rs` 模块清单重写并分两类;新文档 [features/modules.md](features/modules.md)(内置模块,两类)+ 旧 modules.md 更名 [roles.md](roles.md);新增回归测试 `action_names_align_with_ansible_and_keep_aliases`(11 个规范名+别名全解析正确)。37 tests 绿。
- **后续**:`write_file` 并入 `copy` —— 见 D-068。

### D-068 `write_file` 并入 `copy`(ansible 形态:content= 或 src=)
- **背景**:D-067 把原语对齐 Ansible 后,只剩 `write_file`(写内联内容)和 `copy`(拷控制端文件)是分开的;Ansible 是**同一个 `copy`** 用 `content:` 或 `src:` 区分。
- **决策**:删除 `WriteFile` 变体,`Copy` 改为 `{ dest, src?, content?, mode? }` —— 二选一(都给/都不给则报错)。`write_file` 留作 `copy` 的 tag 别名、`dst` 留作 `dest` 的字段别名,旧 task 零破坏。两条路径都 lower 成同一个 `Op::WriteFile`(content 渲染 {{var}};src 读控制端文件内联进 plan,文本 only,二进制走 `place`)。
- **影响**:`tasks/*.yaml`(docker/k8s/zot)8 处 `write_file`+`dst` 改 `copy`+`dest`;`ai.rs`、modules.md、action-layer/action-tasks 文档同步;新增测试 `copy_merges_write_file`(content/src/write_file 别名都解析为 `Copy`)。38 tests 绿。

### D-069 `systemd_unit` 并入 `service`
- **背景**:D-068 后 crater 自有模块只剩 `place`/`load_image`/`systemd_unit`;而 `systemd_unit`(daemon-reload + enable + start)其实是 `service`(ansible `service`/`systemd`)的子集——`service` 同样先 daemon-reload,且能 start/stop/restart + enable/disable,幂等更全。
- **决策**:删除 `SystemdUnit` 变体。`service` 加 tag 别名 `systemd_unit`、字段别名 `enable`→`enabled`,并保留 back-compat 字段 `start: Option<bool>`(`start: true` 在引擎里等价 `state: started`)。旧 `systemd_unit` task 零破坏。
- **理由**:无任务在用 `systemd_unit`;合并后「crater 自有模块」只剩 `place`/`load_image` 两个真·物料模型特有的,其余全部对齐 Ansible。
- **影响**:engine 删 SystemdUnit arm、Service arm 折入 `start`;`kind()`、main.rs(gather hosts 的 match)、ai.rs、modules.md(从自有表移除 + 别名表加行)、action-layer/action-tasks 同步;新增测试 `service_subsumes_systemd_unit`。39 tests 绿。

---

## 2026-06-02 · 彻底改名,移除全部别名

### D-070 删除所有 back-compat 别名,旧名不再解析
- **背景**:D-065~069 改名时都保留了旧名 serde 别名(渐进迁移)。用户决定**彻底改变、不留旧名**——仓库 task/example 已全部迁到新名,别名只剩历史包袱。
- **决策**:删除全部别名:material `kind` 的 `binary`;action 的 `run_cmd`/`command`(→`shell`)、`pkg_install`(→`package`)、`extract`(→`unarchive`)、`render_template`(→`template`)、`module`(→`role`)、`write_file`+`dst`(→`copy`+`dest`)、`systemd_unit`+`enable`/`start`(→`service`+`enabled`/`state`);role 的 `modules/` 目录回退。旧名写进 task 直接报错(`unknown variant`)。
- **理由**:单一拼写、无歧义;文档/AI 提示不必再解释"两种写法"。这是 crater 自己的项目、仓库内已全部迁移,破坏性可控。
- **影响**:`examples/*.yaml` 8 处 `run_cmd`→`shell`、`install-yq.yaml` 2 处 `kind: binary`→`file`;Service 删回 `{name,state,enabled}`(去掉 `start` 字段)、engine 去 `modules/` 回退;4 个别名测试改为"现名解析 + 旧名报错"断言;modules.md 别名表改"改名对照(已废弃)"、ai.rs 提示加"用这些确切名字"。38 tests 绿。**破坏性**:任何外部用旧名的 task 需手动迁移(对照表见 [features/modules.md](features/modules.md))。

---

## 2026-06-02 · 多节点 / HA Kubernetes —— when_role + groups + serial(D-071)

### D-071 多节点能力:`when_role`、`{{ groups.<role> }}`、角色键 hostvars、`serial_roles`
- **背景**:用户要一份 task 统一支持 单节点 / 1主N从 / HA多主多从。k8s 多节点角色不对称(control 跑 init、worker 跑 join、HA 还要额外 master control-plane join + VIP)。crater 此前只有 `when_os`/`when_offline`,且一个 task 所有 action 在所有匹配主机上都跑——无法按角色分流。
- **已有的地基**(D-030/F17,无需重做):inventory 主机带 `roles`;`group_hosts_by_role`(按出现顺序**组间串行、组内并行**);`register`/`hostvars` 跨节点传值(上一组的 fact 下一组可读)。
- **新增 4 件**:
  1. **`ActionStep.when_role: [..]`** + **`RegisterSpec.when_role`**(闭合枚举,守 D-036):步骤/fact 只在持该角色的主机上跑。引擎在 `plan_from_task` 过滤(`PlanContext.host_roles`);register 在 `run_task_on_host` 过滤。
  2. **`{{ groups.<role> }}`**:从 inventory 算出每角色成员地址(空格连接)注入渲染——haproxy backend = `{{ groups.controlplane }}`。
  3. **角色键 hostvars**:某主机 register 的 fact 额外发布为 `hostvars.<role>.<name>`(给单例角色 bootstrap 用,免写死主机名)——master/worker 用 `{{ hostvars.bootstrap.join }}` 取 join 命令。
  4. **`TaskFile.serial_roles: [..]`**:角色集命中的组用 `forks=1` 逐台跑——control-plane join 必须串行(防 etcd quorum 抢)。
- **HA 入口设计**:keepalived + haproxy **co-located 在 controlplane 上、跑 systemd 服务**(不是 static pod:避免与 kubeadm init 的先有鸡蛋问题、不必额外打镜像;离线靠 `os_package` 装)。haproxy 前端 `*:8443` → 后端各 master `:6443`(L4 tcp,VIP:8443 = `--control-plane-endpoint`);keepalived VRRP 在 master 间浮 VIP(192.168.73.14),健康检查盯 haproxy 进程。VIP 先于 init 起来。
- **统一拓扑(一份 `tasks/k8s-ha.yaml`)**:公共步骤无 when_role;`[controlplane]` 装 keepalived/haproxy;`[bootstrap]` init --upload-certs + flannel + 注册 join/certkey + 去污点;`[master]` control-plane join(serial);`[worker]` join。单节点=1 台 [controlplane,bootstrap];1主N从=+[worker];HA=+[controlplane,master]。全 material 离线、可 `crater build` 成 OCI。
- **测试约束**:真 HA 要奇数 ≥3 master(etcd quorum);用 .11(bootstrap)+.12/.13(master)做 3-master、VIP .14。
- **验证**:见下条(构建 + 真机)。新增测试 `when_role_filters_steps_by_host_roles`。39 tests 绿。

### D-071 验证(真机 3-master HA,离线)
- `crater build -f tasks/k8s-ha.yaml` → 26-material OCI;`crater apply -i inventory.yaml`(.11 bootstrap+controlplane、.12/.13 master+controlplane,VIP .14)。
- 结果:3 节点全 Ready、**etcd 3 成员 quorum**、VIP .14 在 n11、apiserver 经 `https://192.168.73.14:8443` readyz passed、kubeconfig 指向 VIP、全部 Pod Running。**全程离线**(镜像/包/二进制都来自 OCID)。
- 证明:when_role(n11=45 步、n12/n13=41 步)、`{{groups.controlplane}}`(haproxy 后端自动填 3 IP)、角色键 hostvars(cp_join 取 `{{hostvars.bootstrap.join}}`+certkey)、serial(n13 等 n12)。
- **运行时注意**:containerd 不一定动态加载 flannel 写的 CNI 配置——脏状态(反复 reset/重跑)的节点会 NotReady,`systemctl restart containerd` 即恢复;干净首装(n13)无此问题。后续可在 task 里加固。

## 2026-06-02 · SSH 传输提速:单 channel 流式取代分块 exec(D-072)

### D-072 `write_file` 改为单 channel 原始字节流(去 base64、去 ~12k 次往返)
- **背景**:离线 apply 推 530MB 物料耗时 ~7min,用户质疑「理论上应秒级」。
- **根因**:SSH executor 的 `write_file` 把内容 base64 后**按 60KB 分块,每块一次独立 SSH `exec`**(`printf >> tmp`)——500MB → base64 ~707MB → **~12000 次串行往返**,延迟受限,有效 ~1MB/s(≈100× 慢于链路,网络几乎空闲),再 `base64 -d`。
- **决策**:开**一个** channel `exec("mkdir -p ... && cat > path")`,把**原始字节**(无 base64——stdin 不是 shell 参数,不受 MAX_ARG_STRLEN 限)用 russh `channel.data()` 流给远端 stdin,`eof()` 收尾,等 exit。SSH 自带窗口流控,line-rate。
- **理由**:crater 用纯 Rust 的 **russh**(不 shell-out 系统 ssh/scp,守单二进制零依赖),russh 的 channel 流式即可,无需 scp/sftp。
- **影响**:530MB 从 ~7min → **~17s**(D-071 真机实测,干净节点全 material apply 17s 到 init)。文件二进制完整(集群正常起来即证)。删掉旧的分块+base64+临时文件逻辑。

## 2026-06-02 · unarchive 直接吃 material(取+解压一步,D-073)

### D-073 `unarchive` 原语支持 `material`,内部完成「取+解压」
- **背景**:9 个 tarball 类物料原来每个都要两步——`place material → /tmp/x.tgz`,再 `unarchive from: /tmp/x.tgz`,中间一个临时文件。重复暴露的是引擎缺「取一个压缩包并解压到位」的复合能力。
- **决策**:`Extract`(action `unarchive`)加 `material` 字段(与 `from` 互斥)。引擎 `action_op` 解析:离线→该 material 的 blob 路径,在线→其 `url_tmpl`(同 `place` 注入 `{{arch}}`),产出新 Op `UnarchiveMaterial{to,strip,creates,local_archive,url}`。`exec` 里一步搞定:`creates` 已存在则跳过(**连取都不取**);否则离线把 blob 流到 `/tmp/crater-arc-*.tar`、在线 `curl` 下载,再 `tar -xf … -C to` 并删临时文件。与 `package`/`load_image` 的 `local_archive` 模式同构。
- **理由**:声明意图(把这个压缩包解到这)而非手写中转步骤;少一个临时文件、少一步、`creates` 幂等还能省掉传输。
- **影响**:`tasks/{k8s-ha,k8s-offline,docker}.yaml` 的 place+unarchive 对(及 cni 的 mkdir)收口成单条 `unarchive material:`(k8s 每个少 1~2 步、docker 少 1 步)。`ai.rs` 改 `unarchive(to, material, strip, creates)`。新增测试 `unarchive_takes_material_directly`。41 tests 绿。`from:`(已在目标上的路径)仍保留。**未重建 OCI**(用户:不着急;改动经单测验证,既有集群不受影响)。

### D-074 `load_image` 支持 material 列表(一步导入多镜像)
- **背景**:k8s 任务里 9 个 `load_image` 几乎一模一样,只有 material 名不同——同样暴露「批量」缺口(同 D-073)。
- **决策**:`LoadImage` 加 `materials: [..]`(与单数 `material` 并存,合并去重);`action_op` 产出一个 `Op::ImageImport{ images: Vec<ImageItem>, namespace, runtime }`(`ImageItem{reference, local_archive}`)。`exec` 里**只探测一次运行时**,然后顺序导入每个镜像(离线 push+import、在线 pull)。不是 YAML 循环(守 D-036),只是「一个动作吃一个列表」,同 `package` 吃包列表。
- **影响**:`tasks/{k8s-ha,k8s-offline}.yaml` 的 9 个 load_image → 1 个 `load_image materials:[..]`,相关 `needs` 改引 `load_images`。`ai.rs` 同步。新增测试 `load_image_takes_a_materials_list`。42 tests 绿。`material:` 单数保留。未重建 OCI(同 D-073)。

## 2026-06-02 · 模板层引入 minijinja(D-036 修正)+ haproxy 模板化(D-075)

### D-075 `template` 动作用 minijinja 渲染;D-036 修正为「模板层允许声明式迭代」
- **背景**:haproxy backend 的 server 列表原来靠「place 占位模板 + shell `for` 循环 sed 拼接」,丑陋。用户要 Jinja 式 `{% for node in masters %}server {{node.name}} {{node.ip}}…{% endfor %}`。
- **D-036 修正**:原 D-036「render 残废,只 `{{ bare.path }}`,所有逻辑在 Rust」收窄为——**`cmd`/`content`/`url_tmpl` 等仍只做字面 `{{ }}` 替换**(`render()` 不变,守住「shell 命令里不藏逻辑」);但 **`template` 动作这一层允许声明式迭代**(`{% for %}`/`{{ }}`,minijinja)。业务决策(when/needs/拓扑)仍在 Rust。理由:配置文件按 inventory 成员展开是数据模板,不是业务逻辑;一路向 Ansible 对齐,模板循环是合理且高频的诉求。
- **实现**:加 `minijinja`(纯 Rust,契合单二进制/离线)。`RenderTemplate` 动作改为 `{ material, dst }`——模板是一个 `kind: file` 物料(`.j2`),**打进 OCI**(离线自洽);`action_op` 在控制端读其字节(离线→打进的 blob、在线→task 目录 src),用 minijinja + 结构化上下文渲染,lower 成普通 `Op::WriteFile`(随 recipe 走、离线可replay)。上下文:所有标量 task var + **`groups.<role>` = `[{name, ip}]`**(新增 `PlanContext.groups`,main 从 inventory 填)。开 `trim_blocks`/`lstrip_blocks` 让配置文件不留空行。
- **影响**:`tasks/templates/haproxy.cfg.j2`(`{% for node in groups.controlplane %}server …{% endfor %}`);k8s-ha 的 haproxy「place+shell 循环」两步 → 一步 `action: template`;删 `files/haproxy.cfg.tmpl`;加 `apiserver_port` var。keepalived 仍 place+sed(网卡是运行时探测,非循环)。新增测试 `template_renders_minijinja_loop_over_groups`。43 tests 绿。**未重建 OCI/真机复验**(改了 recipe + 新增离线读模板 blob 路径;下次 build+apply 时验证)。

## 2026-06-02 · 全组件升最新稳定版 + CNI NotReady 真因修复(D-076)

### D-076 升级到 containerd 2.3.1 / k8s 1.36.1 等最新稳定版;修复 CNI 卡 NotReady 的真因
- **CNI 真因(此前误诊)**:此前以为"containerd 不重载 CNI、必须重启",并写了 join 后重启 containerd 的 `cni_reload`——结果**重启弹掉 bootstrap 的 etcd,导致后续 master join 报 `etcd: can only promote a learner ... in sync with leader`,HA 直接挂**(已回退)。containerd 官方文档根本没有"装 CNI 要重启"一说。查 containerd 日志发现真因:`failed to reload cni configuration after receiving fs change event(REMOVE "/etc/cni/net.d")`——**containerd 启动时 `/etc/cni/net.d` 不存在(或被 reset `rm -rf` 删过),它对该目录的 fsnotify watch 建不起来/watch 的是已删 inode**,于是 flannel 后写的配置永远不被加载 → 节点卡 NotReady。重启只是"碰巧此时目录在了"。
- **修复**:任务里在 **containerd 启动前** 加一步 `file: /etc/cni/net.d state: directory`(`svc_ctd` 依赖它)。containerd 启动时目录已存在 → watch 正常 → flannel 写配置即被 fsnotify 加载 → 节点**自动转 Ready,无需任何重启**。彻底移除 `cni_reload`。
- **版本全升最新稳定**(用户要求):k8s `1.31.14→1.36.1`、containerd `1.7.22→2.3.1`、runc `1.1.14→1.4.2`、cni-plugins `1.5.1→1.9.1`、crictl `1.31.1→1.36.0`;镜像随之 coredns `v1.14.2`、etcd `3.6.8-0`、pause `3.10.2`(`kubeadm config images list` 取)。containerd **2.x 配置 schema 变了(`version=4`,CRI 插件改名 `io.containerd.cri.v1.{runtime,images}`,pause 在 `pinned_images.sandbox`)**——`files/containerd-config.toml` 用 `containerd config default` 重新生成 + 设 SystemdCgroup=true、sandbox=阿里云 pause。k8s-ha 与 k8s-offline 共用此配置,均升 2.3.1。
- **teardown 补全(用户要求"完整反向")**:两个 task 的 teardown 从"reset+删 /etc/kubernetes"扩成 install 的完整逆:reset 运行态 → 停禁所有服务 → 删数据(/var/lib/{containerd,kubelet,etcd})、配置、systemd unit、我们装的全部二进制(containerd/runc/cni/crictl/kube*)→ apt purge keepalived/haproxy/sysdeps → daemon-reload。
- **验证(真机 3-master HA,全离线,最新稳定)**:`crater build`→`crater/k8s-ha:1.36.1`(28 material,阿里云源有全部 1.36.1 镜像);擦净 .11/.12/.13 后 apply → 3 master join 全过(无 learner 错)、**三节点全自动 Ready(零手动重启)**、etcd 3 成员、Pod 全 Running、VIP:8443、containerd 2.3.1。

## 2026-06-02 · inventory 改 kubekey 式结构化 groups + 去 bootstrap 角色(D-077)

### D-077 成员关系移入嵌套 groups;`run_once` 表达「组内首台」;组内渐进式 fact 传递
- **背景**:用户参考 kubekey 的 inventory,提出两点:① 别在每台 host 上内联 `roles:`,把成员关系集中到可嵌套的 `groups:`(host 只留连接信息);② 觉得 `bootstrap` 角色是多余复杂度。查 kubekey 源码证实:它**没有 bootstrap 组**,init 节点是隐式的「`kube_control_plane` 组第一台」(`GetHostsByRole(Master)[0]` / v4 的 `init_kubernetes_node == inventory_hostname`),其余 control-plane 因「不是 init 节点」而走 join;worker vs control-plane join 仅由是否属于控制面组决定;init 节点 `upload-certs` + `token create` 后把 cert-key/token 广播给全体。
- **决策**:
  - **inventory `groups` 结构化**:`BTreeMap<String, Group>`,`Group{ hosts: [主机名], groups: [子组名] }`,可嵌套。host 不再写 `roles`;加载后 `Inventory::derive_roles()` 按组成员(含嵌套**向上传播**)推导每台 `host.roles`——于是 control-plane 主机自动也带父组 `k8s_cluster` 角色。内联 `roles:` 仍兼容(与组推导取并集),`--host` 路径不受影响。下游(`when_role`/`serial_roles`/`{{groups.<role>}}`/`group_hosts_by_role`/`task.hosts`)全部读 `host.roles`,**零改动**;`expand_group`/`inventory_groups` 删除,`task.hosts` 匹配收成 `host.roles.contains`。
  - **`run_once`(新 step + register 字段)**:仿 kubekey `run_once`——步骤只在「匹配其 `when_role` 的**第一台**目标(inventory 顺序)」执行,即隐式 init 节点。`PlanContext` 加 `self_host` + 有序 `target_hosts`,`plan_from_task` 据此门控。**不再需要 bootstrap/master 角色**:`kubeadm init`/flannel/去污点/verify 用 `when_role:[controlplane] + run_once`;其余 master 的 `cp_join` 用 `when_role:[controlplane]`(非 run_once,靠 `check: test -f kubelet.conf` 让 init 节点自动跳过);worker 用 `when_role:[worker]`。
  - **组内渐进式 fact 传递**:此前 crater 靠「不同角色集 = 不同组 = 组间 barrier」才能把 init 节点 register 的 join-token 传给其余 master——这正是 bootstrap/master 角色存在的唯一原因(实现拐杖)。改 `apply_task`:serial 组改为**顺序执行,每台跑完即把 register 合并进 hostvars 供下一台**,于是单个 `controlplane` 组内 token 自然流转(`hostvars.controlplane.join`),无需拆角色。非 serial 组仍并行 + 批量合并。
- **影响**:`spec.rs`(`Group`/`derive_roles`/`group_hosts`)、`component.rs`(`RegisterSpec.run_once`)、`task.rs`(`ActionStep.run_once`)、`engine.rs`(`PlanContext.self_host/target_hosts` + run_once 门控)、`main.rs`(`target_hosts` 接线、serial 组渐进合并、删 expand_group)、`ui.rs`(组渲染)。`inventory.yaml` 改成 hosts 列表 + `groups{k8s_cluster→[controlplane,worker], controlplane→[n11,n12,n13], worker→[]}`,无 bootstrap/master。k8s-ha.yaml 门控全改 run_once/check,`serial_roles:[master]→[controlplane]`,`hostvars.bootstrap.*→hostvars.controlplane.*`。starter 模板示例改组式。新增测试 `derives_roles_from_nested_groups`/`inline_roles_union_with_groups`/`run_once_runs_only_on_first_matching_target`。dry-run 验证 n11=38 步(含 init/verify)、n12/n13=32 步(6 个 run_once 步被过滤)。

### D-077b 步骤级 `throttle` + 可等待 cross-host fact(仿 kubespray,去整组 serial)
- **背景**:用户质疑 `serial_roles:[controlplane]` 过度设计——它把 controlplane 三台的**前置**(传输/解压/装 containerd)也一并串了。研究 kubekey/kubespray 源码:**两者都不用整组/整机串行**。kubespray(严谨派)前置并行,只在 `kubeadm join --control-plane` 这条命令上挂**任务级 `throttle: 1`**(`roles/kubernetes/control-plane/tasks/kubeadm-secondary.yml`),证书 key `run_once` 取;kubekey 干脆并行 join + `Retry:5` 硬扛 etcd 竞争。结论:整机 serial 粒度太粗,该下放到步骤级。
- **决策(仿 kubespray,守零产品知识)**:加两个**通用原语**,引擎对 k8s/etcd 一无所知——
  - **`ActionStep.throttle: Option<usize>`**:跑这一步的主机里同时最多 N 台(`1`=逐台)。引擎只数主机,不知为何。
  - **可等待 cross-host fact**:某步命令插了 `{{hostvars.<role>.<fact>}}` 而该 fact 由别的主机 register,引擎执行这步前阻塞到生产者发布。纯数据依赖。
- **实现**:`engine::HostCoord`(每个并行主机组一个,`Arc` 共享):`facts`(tokio Mutex<map>)+ `wait_facts`(轮询 500ms,`CRATER_FACT_TIMEOUT` 默认 1800s 超时报错)+ `sem(key,n)`(按步 id 的命名信号量)。`Op` 加 `unresolved_hostvar_keys()`(扫 `{{hostvars.*}}` 模板 token)/`resolve_templates()`(执行时回填)。`TaskStep` 加 `id/throttle/awaited_facts`(规划期算出,**减去本机自产 fact**:`PlanContext.self_produced`,防"等自己产的 fact"自锁——leader 的 cp_join 自产 token 故不等,靠 check 跳过)。`execute_task(coord)` 每步:**先 await fact(不持 permit)→ 再抢 throttle permit → 跑**(顺序关键:防生产者要同一步 permit 时被消费者卡死)。`apply_task` 非 serial 组建 `HostCoord`(seed 既有 hostvars)、并行跑;`run_task_on_host` register 后 `publish` 给 coord。
- **影响**:`k8s-ha.yaml` 删 `serial_roles`,`cp_join` 加 `throttle: 1`——controlplane 三台**前置并行**,仅 join 这步逐台、且自动等 init 的 token。`serial_roles` 保留为遗留粗粒度。新增测试 `op_scans_and_resolves_cross_host_facts`/`host_coord_awaits_facts_and_throttles`。**50 tests 绿**,dry-run 通过。限制:`throttle`/可等待 fact 只在控制端 execute(离线/`--shell`)生效,在线 agent 路径不协调(HA 一律离线)。**未重建 OCI/真机复验**。

## 2026-06-02 · OCI build 提速:增量镜像拉取 + 并行 file 取材(D-078)

### D-078 `crater build` 不再全量重取:① pull 跳过已有 blob + ④ 并行 file fetch
- **背景**:用户问 build 是否每次全量拉取。审 `build_task_to_store`:`kind:file` 每次 `fetch_best` 整文件重下(无缓存)、`kind:image` 的 `store.pull` **不查 `has()`**、逐 blob `pull_blob` 走网络(只在 `store_raw` 写时按 sha256 去重),`kind:os_package` 每次 buildah 重算闭包;且 material fetch **串行**。即换 tag/同版本重 build → 530MB 二进制 + 9 镜像全重来。
- **决策(本轮先做 ①+④,均通用、无产品知识)**:
  - **① `pull()` 跳过已有 blob**(`store.rs`):`pull_blob(d)` 前判 `blob_path(strip(d)).exists()` —— blob 内容寻址,digest 命中即内容命中,跳过网络。manifest 仍每次重取(便宜),故移动的 tag 仍会拉新 layer、只跳未变的。镜像拉取由此变**增量**(公共基础层一次即够)。
  - **④ file material 并行取**(`main.rs`):URL 渲染仍在顺序段(要改 `ctx.vars["arch"]`),实际 fetch 收集成 job 后 `buffer_unordered(8)` 并发(纯网络/磁盘无共享态)。`kind:image`/`os_package` 仍串行(image 的 `tag()` 读改写 `index.json` 非并发安全;buildah 重)。
- **验证(真机网络 + 阿里云/ghcr)**:`crater build -f tasks/k8s-ha.yaml` 全程 76s——9 镜像各 1~3s(① 命中既有 292 blob)、8 个 file material 在 ~5s 内**并发**完成(kubeadm 73MB/kubelet·kubectl 60MB/cni 55MB 几乎同时返回),产出 `crater/k8s-ha:1.36.1`(28 material)。50 tests 绿。
- **后续(本轮未做)**:② file/os_package 下载缓存(`~/.crater/cache`,file 按声明 `sha256` 或 URL、os_package 按 `hash(base+pkgs)`)、③ 整体构建缓存(ref 已存在且源未变即跳过)、`--no-cache`/`--pull` 逃生开关、并行镜像拉取(需给 `index.json` 加锁)。

## 2026-06-02 · role 长全:materials 挂到 role + 展开扁平化(D-080)

### D-080 role 升为可复用捆绑(自带 materials + 多 actions),展开期扁平化
- **背景**:对齐 Ansible 后(见 [architecture.md](architecture.md)),role 应是**可复用、可分发、自带离线闭包**的单元(= ansible role + 可烤 materials),而当前 crater 的 role 只是单 `check→act` 瘦模板(D-029)。"materials 挂到 role"是这条线的地基第一步。
- **决策**:`roles/<name>.yaml` 升为 = task 减 `hosts:`(`materials` + `actions` + `handlers` + `params`)。`ModuleDescriptor` 扩字段;`is_bundle()` = 有 `actions:`。**机制 = plan/build 前的展开扁平化**(`TaskFile::expand_roles`,不动引擎 DAG 核心):每个 `action: role` 被替换为 role 的 actions —— id 与 material 名按**调用步 id**(无则 role 名)前缀(`install_yq.bin`/`install_yq.place`),`with` 参数经 `engine::render` 渲染进 actions/materials(yaml round-trip),内部 `needs` 同步前缀,入口动作(无内部 needs)继承调用步的 `needs`/`when_role`/`when_os`,role 的 `materials`/`handlers` **上浮并入 task**;别的步 `needs: [role-step-id]` → 重写到 role 的终端动作(role 内无人依赖的)。瘦角色(无 actions)保持旧路 lower 成单 `Op::Shell`。
- **接线**:`build_task_to_store` 在收 materials 前 `expand_roles`,且 **recipe 改存"扁平化后的 task"**(`serde_yaml::to_string`)而非原文 → OCI 自包含,**离线 apply 不需 role 文件**。`apply_task` 加载后也 `expand_roles`(在线从文件→读 ./roles;离线从 OCI→recipe 已扁平,no-op)。roles 解析沿用引擎约定 `./roles`(cwd 相对)。
- **影响**:`module.rs`(RoleDescriptor)、`task.rs`(expand_roles + 单测)、`engine.rs`(Module 仅 lower 瘦形态,撞到 bundle 报错=未展开 bug)、`main.rs`(build/apply 接线)。新增 `roles/yq.yaml` + `tasks/demo-roles.yaml` 示范 + `roles.md` 文档。新增测试 `expand_roles_flattens_bundle_and_hoists_materials`。
- **验证**:49 tests 绿;真机网络 dry-run/build:`action: role uses: yq` → 展开成 `place install_yq.bin`(url 渲染 v4.44.3),build 收到 `install_yq.bin` 并打包 `crater/demo-roles:latest`;**移走 `roles/yq.yaml` 后 apply 该 OCI 仍 `place (offline) install_yq.bin`**(证 OCI 自包含)。
- **后续(规划)**:role `params` 加默认值/类型(契约,接 D 系列 inspect)、`meta.dependencies`(role 依赖 role,闭包沿图组合)、task/role/play/project 分层(见 architecture.md §3/§11)。

## 2026-06-02 · params 契约 + crater inspect(自描述/可发现)(D-081)

### D-081 富 params 声明 + apply 前校验 + `crater inspect`(对标 Helm values + show)
- **背景**:OCI/task 是黑盒,消费者不知道支持哪些变量、该往 inventory 填什么(见 architecture.md §7)。需要可 introspect 的输入契约(Helm `values.schema` + `helm show` 形态)。
- **决策**:
  - `TaskFile` 加 `description` + `params: BTreeMap<String, ParamSpec>`,`ParamSpec{description, default, required, stage: build|apply}`(`task.rs`)。**契约 = 声明的 params**;裸 `vars` 降为"内部默认"(可覆盖、不对外宣告)。
  - `effective_vars()` = params 的 default ⊕ vars 覆盖 —— build/apply 全改用它(取代直接读 `vars`),故声明 param 默认即生效;`version` 等取值也走它。
  - `validate_params(provided, stage)`:`required` 且无 default 又没提供 → 报错。**apply 前校验全部**,**build 前只校验 build 期**(缺 apply 参数不挡 build)。
  - **`crater inspect <file|ref>`**:file → 加载+`expand_roles`;OCI ref → `materialize_component` 读内嵌(已扁平)recipe。打印 name/version/description、**所需角色**(`roles_needed()` 扫 actions/teardown/handlers/register 的 `when_role` 并集)、**契约 params**(stage/必填/default/desc)、内部 vars 计数、materials 摘要。`--gen-inventory` 吐骨架(只列 apply 期 params + 必需组)。
- **影响**:`task.rs`(ParamSpec/ParamStage + 方法 + 单测)、`main.rs`(`Cmd::Inspect` + `inspect_source` + build/apply 接 effective_vars/validate)。`k8s-ha.yaml` 声明 `description` + 5 个 params(version=build;vip/control_plane_endpoint/subnet/pod_cidr=apply),其余组件版本留 vars(内部默认)——`effective_vars` 产出与原先同,recipe 行为不变。新增测试 `params_effective_vars_and_validation`。
- **验证**:50 tests 绿;`crater inspect tasks/k8s-ha.yaml` 打印 5 params(+11 内部 vars)+ 角色 controlplane/worker + 28 materials(17 file/9 image/2 os_package);`--gen-inventory` 只吐 4 个 apply 参数 + controlplane/worker 组;dry-run 渲染 `--control-plane-endpoint 192.168.73.14:8443 … -v1.36.1`(参数从 params 默认经 effective_vars 解析)。
- **后续(规划)**:inventory vars 三级(全局/组/主机)落地后,apply 期 params 改由 inventory 提供、可标 `required` 无 default;`--set k=v` CLI 覆盖;OCI annotations 写摘要(registry 可见);校验扩到角色(inventory 是否定义了所需组)。

## 2026-06-02 · inventory 三级 vars(环境配置出 OCI)(D-082)

### D-082 inventory 全局/组/主机 vars,覆盖 task params 默认
- **背景**:D-081 的 apply 期 params(vip/网段)默认值还写在 task 里 → 烤进 OCI,制品绑环境。要把环境配置移到 inventory(仿 Ansible group_vars/host_vars),让 OCI 与环境无关。
- **决策**:`spec.rs` 给 `Inventory`/`Group`/`Host` 各加 `vars: BTreeMap<String,String>`(全 serde default)。`Inventory::resolve_host_vars()` 按 **全局 < 组(host 所属,按组名排序) < 主机** 合并进每台 `host.vars`(host.vars 最终持完整合并集);`Inventory::resolve()` = `derive_roles()` + `resolve_host_vars()`,在 `target_hosts` 加载 inventory 后调用。`run_task_on_host` 在 `task.effective_vars()` 之上 **overlay `host.vars`** 进 `ctx.vars`(故 **优先级 主机 > 组 > 全局 > task params 默认**),并在此处(合并后)`validate_params(&ctx.vars, None)` —— 校验移到 per-host,使 inventory 提供的 required 参数生效、报错在该台 plan 前。`apply_task` 的一次性校验移除。
- **影响**:`spec.rs`(三级 vars + resolve_host_vars/resolve + 单测)、`main.rs`(target_hosts 调 resolve、`--host` Host 字面量补 vars、run_task_on_host overlay+校验、移除 apply_task 校验)。`INVENTORY_TEMPLATE`/inventory.md 加三级 vars 示范。`verify_on_host`(drift 只读路径)无 inventory host,暂用 task 默认(其 verify 动作不需环境 var)。
- **验证**:51 tests 绿(新增 `resolve_host_vars_precedence`);真机 inventory 放 `vars.pod_cidr: 10.99.0.0/16` → dry-run `kubeadm init --pod-network-cidr=10.99.0.0/16`(覆盖 task 默认 10.244),证 inventory > task params。
- **后续(规划)**:`--set k=v` CLI 覆盖(最高优先级);校验扩到角色(inventory 是否定义所需组);verify_on_host 也接 inventory vars。

## 2026-06-02 · project 编排层(= Ansible playbook)(D-083)

### D-083 project:有序 plays 编排多个 task(大型交付)
- **背景**:单 task/role 装不下整套平台交付(主机初始化→k8s→CNI→存储→监控→裸机 mysql/redis)。需要 Ansible playbook 那层:有序 plays,每个 play 在一个主机组上跑一件事。架构层级:**project(plays of tasks)→ task(actions,含 `action: role`)→ role(materials+actions)**。
- **决策(增量 1:在线 + file source,复用现有 task 管线)**:
  - `crater-core/project.rs`:`Project{name, description, plays}` + `Play{name?, source, hosts?, vars}` + `is_project_file`(顶层 `plays:`)。
  - `apply_task` 加 `hosts_override` + `var_overrides`(play 重定向目标组 / 叠加 vars)。
  - `apply_project`:按序逐 play 解析 `source`(路径或 `tasks/<source>.yaml`)→ `apply_task`(带覆盖);**delete 逆序**;play 间 barrier(每个 apply_task 跑完才下一个),故 host-init→k8s→cni 的顺序成立;全 play 共用一个 deployment 标签(project 名或 `--name`)。
  - `apply_source` 检测 `plays:` → 路由到 project;命名 `crater apply <name>` 也识别 project。
- **影响**:`project.rs`(+ 单测)、`lib.rs` 注册、`main.rs`(apply_task 覆盖参数 + apply_project + 路由 + 4 处旧 apply_task 调用补参)。示范 `tasks/demo-project.yaml`(yq→k8s 两 play)。新增测试 `parses_project_with_plays`。
- **验证**:52 tests 绿;dry-run `crater apply -f tasks/demo-project.yaml -i inventory.yaml`:play 1 demo-roles 在全 3 台、play 2 k8s-ha 仅 controlplane(n11),按序、各自 host 过滤;命名 `crater apply demo-project` 同样路由到 project。
- **后续(规划)**:离线 project(`crater build -f project.yaml` → bundle 各 play task 的 OCI、内容寻址去重);纯 Ansible `roles: [...]` 内联 play;跨 play hostvars 传递;project 级 `crater inspect`;project delete 对无 teardown 的 play 优雅跳过(当前沿用单 task 的 opt-in,无 teardown 会报错)。

## 2026-06-03 · task 内嵌 inventory + `hosts:` 认主机名(D-084)

### D-084 自包含单文件(task + inventory)+ `hosts:` 匹配主机名/组名
- **背景**:用户想要"一个完整 yq 单文件,含 inventory + task,`crater apply -f yq.yaml` 直接跑"。原来 task 不带机器清单,必须 `-i`/`--host`。且 `hosts:` 只认组名,不认主机名,不符合 Ansible 直觉。
- **决策**:
  - `TaskFile` 加可选 `inventory: Option<Inventory>`。`task_hosts()` 解析优先级:**`-i`/`--host` > 任务内嵌 inventory(`resolve()` 做角色推导+三级 vars)> localhost**。`crater build` **剥掉** `task.inventory`(`= None`)—— 绝不把明文凭据烤进可分发 OCI。
  - `hosts:` 过滤改为匹配 **`all` | 组名(role) | 主机名(host.name)**(原只认组名)。
- **影响**:`task.rs`(TaskFile.inventory 字段)、`main.rs`(task_hosts 辅助 + 两处 task 分支用它 + hosts 过滤加 host 名 + build 剥 inventory)。示范 `examples/yq.yaml`(自包含,演示密码占位)。
- **验证**:52 tests 绿;`crater apply -f examples/yq.yaml --dry-run`(无 -i)→ 目标取自内嵌 inventory 的 n1;`hosts: all` 与 `hosts: n1`(主机名)均命中 n1。
- **安全**:内嵌 inventory 含明文密码,仅本机自用,勿分发;build 已自动剥离。分发用"纯 task + 各自 `-i`"分离形态。

## 2026-06-03 · 模板/示例库 library/ + 命名递归解析(D-085)

### D-085 把 tasks/ + examples/ 整合成 library/(自包含分类),命名快捷搜 library
- **背景**:`tasks/` 平铺把真实 task、project、共享 files/templates 全混一起,`examples/` 又堆了一堆早期特性 demo。需要一个干净的模板/示例库。
- **决策**:新建 `library/`,按交付物分类:`apps/`(yq/docker/mysql/zot)、`k8s/`(k8s-ha/k8s-offline/k8s-online + 共享 files/templates + inventory.example)、`projects/`(demo-platform)、`demos/`(cross-node/group/hostfilter/lug/fcs/d037b/demo-roles)。+ `library/README.md` 索引。`roles/` 与 `inventory.yaml` 留仓库根(role 解析为 `./roles`,inventory 含密码 gitignore)。
- **命名解析(`find_named`)**:`crater apply <名>` 与 project play 的 `source:` 都改用 `find_named` —— 显式路径 > `library/` 下递归首个 `<名>.yaml` > `tasks/`(back-compat)。故命名快捷继续可用(`crater apply k8s-ha` → `library/k8s/k8s-ha.yaml`),project 的 `source: k8s-ha` 也自动解析。
- **路径不变性**:k8s 三个 task 与它们共享的 `files/`+`templates/` 整体搬到 `library/k8s/`,material `src:` 相对 task 目录解析,路径自然不变(inspect 仍 28 materials)。yq 三个冗余变体合并为一个自包含模板 `library/apps/yq.yaml`(占位密码)。
- **影响**:`main.rs`(find_named/find_yaml_under + apply_source 命名块 + apply_project source 用之);大量 `git mv`;README/库 README。涉密的真 inventory(examples/ 下的)删除不入库。
- **验证**:52 tests 绿;dry-run:命名 `crater apply k8s-ha`/`demo-platform`、`inspect library/k8s/k8s-ha.yaml`(28 materials)、自包含 `-f library/apps/yq.yaml` 全部正常解析。

## 2026-06-03 · 每个交付自闭环 + Ansible 对齐目录(D-086)

### D-086 role-dir 形态 + 私有 files/templates + role 相对交付解析
- **背景**:crater 不像 kubespray 只有"一个 k8s 对象";用户要 `library/` 每个子目录都是**一个自闭环交付包**,含一套标准目录结构。选型在「flat `roles/<n>.yaml`」与「Ansible 式 role 目录」之间,用户选 **A**:对齐 Ansible 目录 + 紧凑 `role.yaml`(params+materials+actions+handlers 合一)+ 私有 `files/`/`templates/`,role 相对交付解析。
- **决策**:
  - **role 双形态**:`load_role(roles_dir, uses)` 兼容 flat(`roles/<uses>.yaml`,base=roles_dir)与 dir(`roles/<uses>/role.yaml`,base=`roles_dir/<uses>` 并 canonicalize)。返回 `(ModuleDescriptor, role_base)`。
  - **role 私有物料**:展开时 role 内 `material.src` 若为相对路径 → 改写成相对 **role 目录**的绝对路径(`role_base.join(src)`),故 role 自带 `files/`/`templates/` 可独立搬动。
  - **role 相对交付**:`roles_dir_for(spec_dir)` = 交付目录下 `roles/` 优先,回退仓库根 `./roles`。apply/build/inspect 三路均按入口文件所在目录解析 role。
  - **标准交付目录**(`library/_template/`):`<名>.yaml`(project 或 task)+ `inventory.example.yaml` + `README.md` + `roles/<role>/{role.yaml,files/,templates/}`。唯一超出 Ansible 的:`role.yaml` 的 `materials:`(离线闭包,烤进 OCI)。
- **影响**:`task.rs`(load_role 双形态 + role 私有 src→绝对 + expand_roles 用 role_base);`main.rs`(roles_dir_for + ctx.roles_dir 按 spec_dir + inspect_source 用 find_named + apply_project 跳过空组 play);`library/` 重组(apps→`yq/docker/mysql/zot/`、`kube-upgrade` 移入 `library/k8s/roles/`、`k8s-upgrade.yaml` 移入 `library/k8s/`、demos/projects→`library/_examples/`)+ 各交付 README + `library/_template/` 骨架。
- **验证**:52 tests 绿;`crater inspect yq` 读 `library/yq/yq.yaml`(不当 OCI ref 拉取);`crater apply k8s-upgrade` 解析交付内 `roles/kube-upgrade/`(v1.37.0 渲染),worker play 空组优雅跳过;`_template` 骨架 dry-run 从 role 私有绝对路径 place 私有文件,通过。
- **后续(规划)**:把 `k8s-ha` 大 task 拆成交付内多 role(`roles/preflight`/`containerd`/`controlplane`/`worker`/`cni`),进一步对齐 Ansible role 分解(用户尚未确认,大改)。

## 2026-06-03 · OCI 操作栈选型:协议靠 oci-client,语义自写(D-087)

### D-087 不自实现 OCI 协议,registry I/O 全用 oci-client(=oras-project/rust-oci-client)
- **背景**:crater 频繁操作 OCI(build/push/pull/save/load/apply ref)。需明确:协议层是不是自己造的轮子?是否该引入 oras。
- **结论(现状,非新决策,补记)**:crater **从未自实现 OCI registry 协议** —— 依赖 crates.io 的 `oci-client = "0.17.0"`,它就是 **`oras-project/rust-oci-client`**(前身 `oci-distribution`,归入 oras-project 后改名)。纯 Rust + rustls,无 C 依赖,musl 可静态编译,契合 D-012(单二进制零运行时)。**无需也不引入 oras CLI**。
- **分层**:
  - **协议层 = `oci-client`**:HTTP/distribution、token 认证、manifest 与 blob 的 GET/PUT、multi-arch index 解析。用到 `Client`/`Reference`/`secrets::RegistryAuth`/`pull_manifest_raw`/`pull_blob`。
  - **制品语义层 = crater 自写**(`store.rs`/`bundle.rs`):本地 store(index.json/oci-layout/内容寻址/tag/retag/list/增量去重)+ B类 artifact 合成与还原(`materialize_component`)。这层任何工具(含 oras)都不提供 —— artifact 的 recipe/material 结构是 crater 的语义。
- **关键经验**:刻意用低层 `pull_blob`(而非高层 `pull`)—— 高层 `pull` 面向 image,会合成 image-manifest 丢掉 `artifactType` + 自定义 layer mediaType(D-032 已踩坑验证)。
- **直接收益**:`pull_manifest_raw` + `pull_blob` 天然分离 → **partial pull(选择性拉层)零新依赖**,是「瘦在线部署」(默认只拉 recipe + 自建文件层、依赖层在线现拉;`--offline` 全量)的实现基础。详见 [architecture.md §5.1](architecture.md)。

## 2026-06-03 · 瘦在线部署:三态(纯在线/瘦在线/纯离线)+ 层按来源分类(D-088)

### D-088 apply &lt;ref&gt; 默认只拉 recipe + 自建文件,依赖在线现拉;--offline 才全量
- **背景**:`apply <ref>` 原本全量拉所有 material blob、走离线 replay。用户要:默认只拉 recipe + 自建配置/模板(轻),依赖在线现拉;`--offline` 才全量拉走离线。
- **关键洞见(用户)**:自建 config/模板(`src`,**无下载源**)与可联网依赖(`url_tmpl`/`ref`/`packages`)是两类层 —— 前者必须随 recipe 走(否则 ref 部署时控制机无处可寻),后者可在线获取。不是"边界打补丁",是**层重新分类**。
- **决策**:
  - **build**:material 层按来源打 annotation `org.crater.material.fetch` = `embedded`(src,无源)| `dependency`(url/ref/pkgs)。缺失视为 `embedded`(老制品安全全拉)。
  - **pull**:`store.pull_thin` 跳过 `dependency` 层(用 oci-client `pull_manifest_raw` + `pull_blob` 分离 API,零新依赖,D-087);`store.pull` 全量;`has_all_layers` 判本地是否完整。
  - **引擎**:`PlanContext` 加显式 `offline` 字段(脱离 `offline_blobs.is_some()` 推导);`place`/`extract`/`render_template`/`os_package`/`load_image` 五处取数改 **per-material 三分支**:本地有 blob→用 blob;无 + `offline`→报错(离线包缺料);无 + 在线→现拉(url/registry/apt;`src` 回退控制机本地路径)。
  - **apply `<ref>`**:默认瘦在线(`pull_thin` + 部分 blobmap + `offline=false`);`--offline` 全量(`pull` + `offline=true`,本地不完整自动补全)。`.oci` 包恒 `offline=true`,`apply -f <file>` 恒在线。
- **三态对照**:纯在线(`apply -f`,全现拉)/ 瘦在线(`apply <ref>`,recipe+自建文件本地、依赖在线)/ 纯离线(`apply <ref> --offline` 或 `.oci`,全 blob)。
- **影响**:`engine.rs`(PlanContext.offline + `blob_for` + 五取数点 + 测试 `three_state_place_resolution`)、`bundle.rs`(层 `fetch` annotation + `materialize_component` 只收本地存在的 blob)、`store.rs`(`pull_thin`/`pull_layers`/`has_all_layers` + 常量)、`main.rs`(`--offline` flag + `apply_image_ref` 三态 + `offline` 贯通 `apply_task`/`run_task_on_host`/`ctx.offline`)。
- **验证**:53 tests 绿;yq 实测:manifest 层 `yq-bin` 标 `fetch=dependency`;blob 在→`place (blob)`;删 blob(模拟瘦拉)默认→`place 在线 curl`;删 blob + `--offline`→识别本地不完整、试图全量补拉。
- **真机验证(2026-06-03)**:build → `crater push willdockerhub/yq:thin`(Docker Hub),干净 `CRATER_HOME` apply 到 n11(192.168.73.11)。瘦在线:控制机只拉 **16K**(recipe 733B + config 49B + manifest 863B,**无** 10MB yq-bin 层),日志 `thin pull`,n11 `place yq-bin <- https://github.com/...` 自行在线 curl,实装 yq v4.44.3。`--offline` 同制品:控制机 `pulling full closure` 拉 **9.6M**(含 yq-bin blob),`place (blob)` 由控制机推送、目标零联网。同一 recipe、一个 flag 切换,registry 端无需任何特殊基础设施。

## 2026-06-03 · crater build --set 覆盖 build 期 param(D-089)

### D-089 build 期 param 可覆盖,补齐 params 契约的对称性
- **背景**:`yq.yaml` 把 `params.version` 的 `default` 写在 yaml 里。用户问"版本写死合适吗"。`default` 作"基线/缺省版本"是对的(声明式、`inspect` 可见、制品即某确定版本的闭包),但 `crater build` 只有 `-f/-t/--arch`,**没有覆盖 param 的入口**——要出别的版本只能改 yaml,`default` 退化成"写死"。更隐蔽:justfile `just version=X` 只改 tag、不改实际拉取版本(crater 仍从 yaml 读)→ tag 与内容不符的 footgun。本质是 **D-081 契约不对称**:apply 能覆盖 param(inventory/`-i`),build 不能。
- **决策**:`crater build` 加 `--set key=val`(可重复)。解析后注入 `task.vars`——`effective_vars` 中 vars 排在 param `default` 之上,故覆盖值贯穿:material URL 渲染、默认 tag 的 `<version>`、烤进 OCI 的 recipe 全用它。注入在 `expand_roles` 与物料抓取**之前**(role params 也按新 vars 渲染)。
- **影响**:`main.rs`(`Cmd::Build` 加 `--set`、`parse_set_overrides`、`build_to_store`/`build_task_to_store` 透传并注入)。`library/yq/justfile`:build recipe 改 `crater build … --set version={{version}}`,`version` 默认从 `yq.yaml` 取、`just version=X` 临时覆盖时 tag 与内容一并改,footgun 消除。
- **验证**:53 tests 绿;`crater build -f library/yq/yq.yaml --set version=4.40.5`(不带 `-t`)→ 拉 `v4.40.5`、默认 tag `crater/yq:4.40.5`;`just version=4.40.5 build` → URL 与 tag 均 4.40.5,默认 `just build` 仍 yaml 的 4.44.3。
- **后续(规划)**:`apply --set` 若加,**必须 gate 到只允许 apply 期 param**(`stage: apply`,如 vip/subnet —— 不影响 materials,离线安全);build 期 param(如 `version`)在 apply 时 `--set` 要**报错**,引导去 `crater build --set version=X` 重建。理由(用户指出 2026-06-03):**已 build 的 OCI 是某确定版本的冻结闭包** —— 离线 `place` 从 blob 取(按 material key),apply 时改 `version` 要么无效、要么让 blob 与 recipe 失配,等于废掉这个制品(在线 `apply -f` 因 material 现拉才侥幸无害)。crater 已有 `ParamStage`(build 时 `validate_params(.., Some(Build))`、apply 时 `None` 校验全部),gate 天然可做:apply 的 `--set` 只接 `stage: apply` 的 key。**单机 `apply --host` 无 inventory 时的 ad-hoc 覆盖入口**也走这个受限 `--set`。

## 2026-06-04 · place 并入 copy:一个原语三种来源(D-090)

### D-090 删除 place,copy 的来源三选一:content / src / material

- **背景**:`copy`(content 内联 / src 控制端文件)与 `place`(material 物料引用)本质都是"把一份内容放到目标机路径",只是**来源**不同。两个原语让作者多记一个名字、多一次"该用哪个"的选择;Ansible 用户的直觉是 `copy`。用户拍板:彻底合并,**不留别名**。
- **决策**:`Action::Copy` 增加 `material: Option<String>`,与 `content`/`src` **三选一**(多给/不给均报错)。material 分支原样继承 place 的全部语义:三态解析(blob → `PushFile`;缺 blob + 严格离线 → 报错;在线 → `src` 推送或目标机 curl `url_tmpl`)、arch 变体(D-048)、`mode` 折入。`Action::Place` 变体**删除**——`action: place` 直接解析失败,不做别名(D-070 同款"无别名"原则)。
- **影响**:`component.rs`(Copy 加字段、删 Place)、`engine.rs`(三态逻辑并入 Copy 分支,describe `place …` → `copy …`/`copy (blob) …`)、`task.rs`(`rewrite_material_refs`)、库内 7 个 yaml 与 docs 全量改写(`materials-and-place.md` → `materials.md`)。**破坏性**:旧 OCI 制品的 recipe 含 `action: place`,新 crater 解析失败 → **需重 build/push**(本仓 willdockerhub/yq 已重发)。
- **顺带修复(agent 路径 PushFile 缺陷)**:`copy material:` 在 material 带本地 `src:` 时产出 `PushFile`,其 `local_path` 是**控制机路径**;纯在线默认 agent 执行(计划运到目标机跑)会读不到该文件。修复:计划含 `PushFile` 步骤 → 强制控制面 `execute_task`(`run_task_on_host` 分支加 `has_push_file`)。此缺陷在 place 时代即存在,过往验证恰好都走 offline blobmap/本机路径而未暴露。
- **验证**:55 tests 绿(三态测试改造为 `copy material:` 同逻辑);yq/docker/k8s-ha dry-run 输出同前(describe 前缀变 `copy`);重建 yq OCI 后 `apply <ref>` recipe-replay 正常。

## 2026-06-04 · 镜像物料导出双格式:OCI layout + docker-archive 垫片(D-091)

### D-091 export_oci_archive 给 plain image 附带 manifest.json,老 docker 也能 load

- **背景**:`kind: image` 物料打包为 OCI layout tar(`oci-layout` + `index.json` + `blobs/`)。真机(192.168.73.11,docker 27.3.1 静态包 + 经典 overlay2 存储)离线导入时 `docker load -i` 报 `open …/blobs/json: no such file or directory`——经典存储(未开 containerd image store)的 docker load **不认 OCI layout**,回退 legacy v1 路径(找 `<目录>/json`)。控制机 docker 29 能 load,纯属版本差。
- **决策**:`store.rs export_oci_archive` 对 **plain image**(manifest 无 `artifactType`)在 OCI layout 之外**附带一个 `manifest.json`**(docker-archive 格式:Config/RepoTags/Layers 指向同一批 `blobs/sha256/...`)。即 buildx `-o type=docker` 的同款双格式:老 docker 读 manifest.json,新 docker / ctr / nerdctl 读 index.json,互不干扰、零体积成本(blob 不重复)。B 类 artifact(有 `artifactType`)永远不会被 docker load,不加垫片。
- **验证**:本机 docker 29 load 双格式 OK;真机 73.11(docker 27.3.1)离线 `apply willdockerhub/rustfs:1.0.0-beta.5 --offline` → `load image (blob)` 导入成功 → 容器 /health 200。55 tests 绿。
- **关联**:D-061(image 物料)。rustfs 交付顺带改为**收敛语义**:`docker inspect` 探针比"running + 镜像版本",不符(没容器/崩溃循环/版本变)→ 删旧起新——修复了"Restarting 容器撞 docker run 名字冲突"与"换版本重 apply 不生效"。

## 2026-06-04 · docker_container 模块:指纹收敛 + 模块准入治理(D-092)

### D-092 内置 docker_container(精简集),收敛靠 spec 指纹;同时立模块准入四条

- **背景**:rustfs 容器化交付把收敛逻辑手写进 shell(参数 label、`rm -f`+`run`、`docker ps` 三过滤),连踩三坑(探针只认 running 撞名字冲突 / 只比版本漏掉端口变更 / 凭据明文进 label),每个容器交付都要复制一遍——典型的"幂等抽象缺位"。用户同时提出元问题:helm/kubectl 接踵而至,是否要顶层设计/插件机制,代码量会不会爆。
- **决策(模块)**:内置 `docker_container`,**community.docker.docker_container 的刻意精简版**:name/image/state(started|stopped|absent)/restart_policy/ports/volumes/env/command/args(裸 flag 逃逸)/runtime(默认 docker,podman 兼容)。收敛:渲染后的 spec 规范化 → sha256 取 12 位 → `--label crater.spec=<指纹>`;探针 = name+running+label 三过滤;不符(没容器/崩溃循环/任何参数变)→ `rm -f`+`run` 重建。容器不可变,无原地 update;**不做** Ansible 的 `comparisons:` 逐项 diff(那需要 API client,crater 是 shell lowering)。凭据只进 Env 不进 label(label 是哈希)。
- **决策(治理,见 docs/module-charter.md)**:不做代码插件机制(dylib/WASM/外挂进程都破坏单二进制承诺或重复 collections 版本矩阵之痛);"插件"= role + library/ + OCI 分发。新模块准入四条:通用基础设施 / ≥2-3 交付复用 / 幂等收敛须引擎帮忙 / 参数能闭集。晋升路径 shell → role → 模块,需求驱动。规模预期 20-30 个模块;helm/kubectl 因底层自带收敛属"薄模块",到需求时再建。
- **验证**:56 tests 绿(指纹同/异、absent lowering、action 名表);rustfs.yaml run+teardown 共 24 行裸 shell 缩成 15 行声明;真机:部署 api/console 200 → 重跑 ok(同指纹)→ 改 console_port → 指纹变 → 自动重建且新端口 200 → delete 干净。

## 2026-06-04 · apply --set 受 ParamStage gate(D-093)

### D-093 `apply/delete --set` 只接 `stage: apply` 参数;build 参数报错引导重建

- **背景**:D-089 给 `crater build --set` 后,apply 侧一直没有 CLI 覆盖入口(单机 `--host` 无 inventory 时 apply 期参数只能改 yaml)。但不能照抄 build 的"任意 key 注入 vars":**已 build 的 OCI 是 build 期参数的冻结闭包**——物料按 `version` 取好、blob 按 material key 寻址,apply 时改 `version` 要么无效、要么 recipe 与 blob 失配,等于废掉制品(用户 2026-06-03 指出)。
- **决策**:`apply`/`delete` 加 `--set KEY=VAL`(可重复),经 **gate**:① 声明为 `stage: apply` 的 param → 放行,作**最高优先级**(盖过 inventory vars,显式操作员意图最大);② `stage: build` → 报错并引导 `crater build --set` 重建;③ 未在 `params:` 声明 → 报错(防 typo;要开放就声明契约,接 D-081)。delete 也带 `--set`:teardown 渲染用的是同一套 vars,卸载要与部署同值。gate 在 `apply_task` 统一执行,五个入口(task 文件/named/project/OCI bundle/image ref)全走它;通过的值同时种进 `task.vars`(role 展开可见)+ `ctx.vars` 最后插入(优先级最高)。
- **优先级链(低→高)**:param default → task vars → inventory 全局/组/主机 vars → CLI `--set`。
- **验证**:59 tests 绿(gate 放行/拒 build 参数/拒未声明 key);CLI 实测:`--set vip=…` 渲染进 plan、`--set version=…` 与 `--set vipp=…` 报错文案符合预期。
- **关联**:D-081(params 契约)、D-082(inventory vars)、D-089(build --set)。

## 2026-06-04 · SSH host key 校验:known_hosts + TOFU(D-094)

### D-094 `check_server_key` 从 accept-all 改为钉 `~/.crater/known_hosts`(accept-new)

- **背景**:`executor.rs` 的 `check_server_key` 一直是 accept-all(代码里唯一的 `TODO(security)`),公网仓库挂着扎眼,且离线交付场景控制端↔目标机之间完全可被中间人。
- **决策**:ansible 式 **accept-new**:首连记录(TOFU)→ 再连校验 → **key 变了拒连**(ERROR 给指纹+行号+处置指引);`CRATER_HOST_KEY_CHECKING=0|false|no|off` 整体跳过(临时 VM/重装频繁)。known_hosts 用 **crater 自己的 `~/.crater/known_hosts`**(`$CRATER_HOME` 可挪):绝不写坏操作员的 `~/.ssh/known_hosts`,测试/CI 也可密闭。实现全靠 russh-keys 现成的 `check_known_hosts_path`(三态:匹配/未知/`KeyChanged{line}`)+ `learn_known_hosts_path`,零新依赖;钉不进去(只读 fs)降级 warn 放行——首用信任本来就没指望持久化,but key 冲突永远 fail-closed。
- **验证**:60 tests 绿(TOFU round-trip:首连钉/重连过/换 key 拒/异端口各自 TOFU);真机 73.11 五场景:首连钉 key → 重连静默 → 篡改成假 key 拒连(`Unknown server key`)→ env 跳过可连 → 删行重钉。
- **意外发现**:73.11/73.12 是同模板克隆,**host key 完全相同**——TOFU 防不了克隆机互相冒充(文档已提醒:生产模板应清 `/etc/ssh/ssh_host_*` 重新生成)。也回头解释了 D-030 时代 k3s node-password 撞名的环境背景。
- **关联**:D-008(russh 直连)。代码里最后一个 `TODO(security)` 清零。

## 2026-06-04 · 离线路径 agent 化:blob 先推后 agent 跑(D-095)

### D-095 agent 路径吃下离线计划:控制端 blob 内容寻址 staged 到目标机,计划改写后本地执行

- **背景**:task 的 agent 化(D-044)一直留着一个洞——计划里只要有读**控制端文件**的步骤(`PushFile`,以及 `unarchive`/`load_image`/`os_package` 的离线 `local_archive`),就整体退回控制端逐步驱动,每步一次 SSH 往返;离线大制品部署(正是 crater 的主场)反而享受不到 agent 的提速。
- **决策**:`run_task_via_agent` 起跑前 **stage blobs**:扫描计划(含 handlers)收集控制端路径 → 去重 → 逐个按 **sha256 内容寻址**推到目标机 `/var/lib/crater/blobs/<digest>`(已有同 hash 跳过,重复 apply 零传输)→ 把 Op 里的路径改写成 staged 目标本地路径(新增 `Op::offline_blob_paths_mut` 统一四种变体)。之后 agent 的 LocalExecutor `std::fs::read` staged blob,与控制端读原 blob 语义完全一致,执行代码零改动。控制端逐步驱动只剩三种情况:`--shell`(显式逃生)、本机目标、**需要跨主机协调的步骤**(`throttle`/`awaited_facts` 非空且在并发组里,D-077——agent 无互联通道,k8s-HA 串行 join 不受影响)。顺带修掉 `images.rs` 里 `do_shell: true` 的硬编码(镜像源现在尊重 `--shell`,默认 agent)。
- **验证**:62 tests 绿(staging 去重/改写、缓存跳过、blob 路径收集);真机 73.11/73.12:yq 离线(PushFile)staging 10MB → agent `done on local`,重跑 `blob cached, reusing` + ok;rustfs 离线(ImageImport 110MB)staged 导入 → api/console 200,重跑零传输;双主机并发各自 agent;`--shell` 逃生口回归正常。
- **踩坑**:目标机缓存的旧 dist musl agent 解析新 plan 报 `missing field reference`(D-074 批量 load_image 改了 ImageItem 结构)——以前离线不走 agent 没暴露。**dist/ 二进制须随代码重建**(scripts/build-musl.sh),文档已记。
- **版本检查(同日补)**:TaskPlan 加 `format: N`(`PLAN_FORMAT` 常量,Op/TaskStep/ImageItem 序列化变更时手动 bump),控制端打戳、agent 解析时校验,偏斜报「重建 scripts/build-musl.sh / --agent-bin / 应急 --shell」而非 serde `missing field`。预存量旧 agent 不认识该字段救不了,从 v1 起的偏斜全可检出。63 tests 绿;实测 format:99 喂 agent 报版本偏斜、正常 apply 无感。
- **边界**:staged blob 是缓存,delete 不清(回收 `rm -rf /var/lib/crater/blobs`);跨主机协调步骤仍控制端(agent 化它=引入常驻通信,违背 design.md §5.3 一发一收模型,不做)。
- **关联**:D-044(task agent 化)、D-045(recipe-replay)、D-077(协调)、D-087(thin/offline)。

## 2026-06-04 · 构建缓存:下载缓存 + 整体指纹缓存 + --no-cache(D-096)

### D-096 build 两层缓存:物料下载按内容/源寻址,整体按源指纹跳过

- **背景**:D-078 做了①增量镜像拉取④并行 file 取材,②下载缓存③整体缓存一直挂着。每次 `just build` 即使源毫无变化也要重新下二进制、重新打包、image 物料重触 registry——迭代体验差,且控制端断网时连"重建一个内容没变的制品"都做不到。
- **决策**:两层缓存,都在 `~/.crater/cache/`(可随时 rm -rf):
  - **下载缓存**:file 物料 key=声明 `sha256`(内容寻址)否则渲染 URL 哈希,存 `cache/file/`;os_package key=`hash(base+family+pkgs)` 存 `cache/ospkg/`。声明 sha256 的物料取回与读缓存都校验,损坏条目删除重取(`cache_get` 不信任坏缓存)。image 物料不另设缓存(D-078① pull 已增量)。
  - **整体缓存**:指纹=hash(役展开 recipe + 物料源描述符(URL/ref/src 内容哈希/base+pkgs)+ arch 过滤),sidecar 存 `cache/builds/<sanitized-ref>`;`store.has(ref)` 且指纹同 → 整体跳过。**取材前**计算,命中时零 I/O 零网络。
  - **`--no-cache`** 绕过两层。**诚实边界**:指纹只看声明的源,上游 tag/asset 内容漂移不可见——钉版本,或 --no-cache。
- **验证**:66 tests 绿(cache key 优先级、指纹对 recipe/物料/arch 各输入敏感、坏缓存丢弃);实测:yq 重建 4.4s→0.2s、同源换 tag 0.4s(下载缓存命中零网络)、--no-cache 强制重取、改 zot `src:` 文件→指纹变→重建→revert 后回写一致再命中、rustfs image 物料重建 10.2s→0.16s(不再触 registry)。os_package 缓存与 file 同构未真机验证(buildah 重)。
- **后续**:并行镜像拉取(`index.json` 锁)仍未做;cache GC(目前只增不减,同 /var/lib/crater/blobs)。
- **关联**:D-078(build 提速①④)、D-089(--set 进指纹经 recipe)。

## 2026-06-04 · crater rmi + crater gc:库引用删除与四类缓存/孤儿回收(D-097)

### D-097 `rmi` 删引用、`gc` mark-and-sweep 回收孤儿 blob + 三类缓存

- **背景**:本地库只进不出——没有删引用的命令,重建同 ref 会把旧 manifest/layers 留成孤儿(实测攒了 172 个 blob、3.1GB);D-095 的目标机 staged-blob 缓存与 D-096 的下载缓存也只增不减。
- **决策**:
  - **`crater rmi <ref>`**:从 index.json 删引用(像 docker rmi);blob 内容寻址、可能与其他 ref 共享,**不动**。
  - **`crater gc [--dry-run] [--cache] [--host/-i]`**:① store mark-and-sweep——从 index 出发标记 manifest→config/layers,经嵌套 `manifests`(multi-arch index)递归,扫掉 `blobs/sha256/` 里无人引用的;② `cache/builds/` 里 ref 已不在库的过期指纹 sidecar;③ `--cache` 连下载缓存(file/ospkg)一起清;④ 显式给目标(`--host`/`-i`)时清各目标机 `/var/lib/crater/blobs`(staged 缓存,下次 apply 重 stage)。四类全是缓存/孤儿,可随时重建。裸 `gc` 不碰本机目标(`has_explicit_targets` gate,localhost 兜底语义在这里是错的)。
- **验证**:67 tests 绿(假 store 上 dry-run 不删/孤儿扫掉/链保留/rmi 后整链可扫/重复 rmi no-op);真仓库:dry-run 报 172 个 3.1GB,**与独立 python 实现的可达集逐一一致**(双算法核对)后真删,35 个引用完好、离线 apply 正常;目标机 119.6MB staged 缓存清除后 apply 自动重 stage。
- **边界**:gc 与并发 build/pull 不互斥(单用户 CLI,先不加锁);旧版残留的「索引注解藏 digest」格式不存在(已核对全部注解仅 ref.name)。
- **关联**:D-078/096(缓存)、D-095(staged blobs)、D-018(store)。

## 2026-06-04 · 离线 project:整套环境一包 build→save→apply(D-098)

### D-098 项目制品锁定 play ref;save 导出项目+全部 task 闭包;apply .oci 离线编排

- **背景**:D-083 的 project 只有在线形态——大型交付(host-init→docker→k8s→存储)离线时只能逐 task build/save/load,"一个 U 盘交付整套环境"的主场故事缺最后一环。
- **决策**:
  - **build** `crater build -f project.yaml`:逐 play 构建 task 制品(默认 tag,play `vars` 参与 build 覆盖、CLI `--set` 更高;D-096 缓存逐 task 生效),然后造**项目制品**(`artifactType: application/vnd.crater.project.v1`,recipe = play `source` 全部**锁定**为构建出的 task ref 的 project yaml,无物料层)。同 ref 不同输入(文件/vars)→ 报错(锁定不许静默指向后者)。
  - **save**:导出项目制品时**连同每个锁定 ref 的 task 闭包**进同一 OCI layout——blob 内容寻址,跨 task 共享物料天然去重;index 多 manifest(项目 + 各 task),`write_artifact_index` 按 manifest 真实 artifactType 标注。
  - **apply/delete .oci**:解包发现项目制品(`read_artifact_project`)→ 按 play 顺序(delete 逆序)对包内 task 制品编排,play 的 hosts 匹配/跳过、vars 覆盖与在线一致;每个 play 走标准 task 离线管线(blobmap → D-095 staging → agent)。**顺手落 D-083 后续**:project delete 对无 teardown 的 play 优雅跳过(在线+离线;单 task delete 仍硬错误)。
  - **守卫**:`push <project-ref>` / `apply <project-ref>`(store/registry 直连)报错引导 save/.oci——push 是单 ref,task 制品不会跟着走;registry 闭包分发列为后续。
- **验证**:68 tests 绿(项目制品 round-trip:locked recipe 解析、component 带 ref、task-only 包无项目);真机 73.11:demo-stack(yq+rustfs)build(yq 缓存命中)→ save 115MB 单包(1 project + 2 component,11 blobs)→ 清机 apply:2 play 顺序离线部署(rustfs api/console 200)→ 重跑全幂等(blob cached,changed=0)→ delete 逆序:rustfs 拆净、yq 优雅跳过、零残留。
- **边界**:同名 task 两版本进一个 project 暂不支持(recipe 目录按名落盘互覆;同 ref 不同输入已在 build 报错兜住);跨 play hostvars、project inspect 仍后续。
- **关联**:D-083(project)、D-095(blob staging)、D-096(构建缓存)、D-087(thin/offline)。

## 2026-06-04 · UI Phase 1b:后台任务流 + Delete 强确认 + token 鉴权(D-099)

### D-099 UI 写操作改后台任务(286 停轮询日志面板);Delete 输名确认;--token 守门

- **背景**:D-058 的 verify/heal 是**同步 handler**——阻塞 HTTP 请求直到 CLI 跑完,真实部署要几分钟,页面假死且看不到过程;Delete(最危险)当时刻意没做;`--bind 0.0.0.0` 裸奔无鉴权。这三项就是 D-054/058 记下的 Phase 1b 后续。
- **决策**:
  - **后台任务流**:写操作 spawn `crater <args>` 子进程(stdout+stderr 逐行进内存 JobStore),立即返回任务面板;面板每 1s 轮询 `/api/job/{id}` 显示日志尾部,完成时服务端回 **htmx 286 状态码**(htmx 原生"停止轮询"语义)+ 成败 pill。面板在独立 `#jobs` 区,不被部署表的 5s 自刷覆盖。日志在内存,UI 重启即失(286 兜底停轮询)。
  - **Delete**:每行 delete 按钮,`hx-prompt` 弹输入框,服务端校验 `HX-Prompt` 头**等于部署名**才执行(GitHub/AWX 式 type-the-name;heal 仍是普通 confirm)。跑 `crater delete <source> -i inventory.yaml`,凭据约定同 D-058(当前目录 inventory.yaml)。
  - **鉴权**:`--token <t>` → axum 中间件统一校验 `?token=`(首访 303 + Set-Cookie)/ cookie / `Authorization: Bearer`,失败 401;**暴露守卫**:`--bind` 非 localhost 且无 token → 启动直接报错(UI 能 apply/delete,不允许裸奔到 LAN)。
- **验证**:68 tests 绿;curl 全矩阵:401/303+cookie/Bearer 200/错 token 401/无 token 非本机 bind 启动拒绝;verify 任务:POST → 面板 → 轮询 200(running)→ 286(完成/失败 + 日志尾,实测 73.13 关机的 `No route to host` 直接呈现);delete 门:空输入/输错名拒绝、输对过门。
- **边界**:单静态令牌(无多用户/审计);TLS 交给反代;任务日志不落盘。
- **关联**:D-054(看板)、D-058(写操作)、D-051~053(状态库)。

## 2026-06-04 · crater plan:terraform 式变更预演(D-100)

### D-100 `crater plan`:连真机只跑只读探针,报告 ok / would-change / unknown / skip

- **背景**:`apply --dry-run` 是纯静态(不连目标机,OS=Unknown、不跑探针),只回答"会做什么",不回答"哪些会真的变"。生产前想要 terraform plan 式的差异预览。用户点名贴 terraform 习惯,独立子命令。
- **决策**:`crater plan <source>`(apply 的五种 source 形态全支持,`--set` 同 D-093 gate):连目标机、探 OS/arch、lower,然后 `engine::plan_check_task` **只执行每步的只读探针**——Shell(Install)跑 `check:`;WriteFile/PushFile 远端 sha256 比对;unarchive 探 `creates`;os_package 探 check;load_image 固定 would-change(无镜像探针的既有语义);**preflight/verify 的 shell 跳过**(它们是"检查"不是"状态",plan 的契约=除探针外零执行)。四态:`✓ ok / ~ would-change / ? unknown(无探针)/ - skip`,颜色随 TTY 门控。实现:RunOpts 加 `plan_check`,run_task_on_host 在 lower 后、执行前短路(不写 marker、不跑 register);project(在线/离线 bundle)逐 play 各出摘要。
- **验证**:68 tests 绿;真机 73.11:空机 plan rustfs `3 会变更`→ apply 后 `1 会变更`(只剩 load_image)→ 手动杀容器注入漂移 → plan 精确翻出 `container ~ would-change` 其余仍 ok;project bundle `offline plan project: 2 play(s)` 零执行。
- **边界**:不跑 register → 依赖跨主机 fact 的步骤探针含未解析 `{{}}`,HA 类 task 结论可能失真;探针粒度即预测粒度(`test -s` 类验"在不在"不验内容);不展示 diff 内容;teardown 方向用 `delete --dry-run`。
- **关联**:D-023(幂等探针,被复用)、D-024(dry-run)、D-093(--set gate)。
- **UI 接线(同日补)**:看板部署表每行加 **plan** 按钮(只读无确认),走 D-099 任务流(`/api/plan/{dep}` spawn `crater plan <source> -i inventory.yaml`),逐主机摘要流进任务面板;curl 实测 n11/n12 各报 `0 会变更, 1 已就位`、关机的 n13 的 ssh 错误呈现在面板并标失败。

## 2026-06-04 · registry 闭包分发:project push/pull/apply 直连(D-101)

### D-101 项目制品经 registry 整体分发:闭包 push(re-prefix)/ 闭包 pull(retag 裸 lock)/ store 直连编排

- **背景**:D-098 的离线 project 只有 save/.oci 文件通道;push/apply 项目 ref 都是报错守卫。有私有 registry(zot/Harbor)做交付分发中心的场景,要"仓库拉"而不是"拷文件"。
- **决策**:
  - **push <project-ref>**:解析项目 recipe 的锁定 ref 列表(去重)→ 每个裸 lock(`crater/yq:4.44.3`)retag 成 `<registry-of-project-ref>/<lock>` 并 push → 最后 push 项目制品。registry blob 内容寻址,跨 task 共享层仓库侧自动去重。
  - **pull <project-ref>**:拉项目制品 → 闭包成员从同 registry 拉 `<registry>/<lock>` → **retag 回裸 lock**,使 recipe 的 `source:` 在本地库可解析(save/.oci/apply 全不用改)。
  - **apply/plan/delete <project-ref>**:原守卫换成 **store 直连编排**——按 play 顺序(delete 逆序)materialize 各锁定 task 制品走标准管线;本地缺的成员自动从项目所在 registry 补拉(`ensure_pulled`,thin/--offline 语义沿用 D-087/088);teardown-less play 优雅跳过(同 D-098)。重构:`apply_task_artifact` 抽出单制品路径,单 ref 与 project 循环共用。
  - **适配边界(诚实)**:闭包 ref 是裸 `crater/...` 路径,**私有 registry 任意 repo 路径可用**;docker.io 命名空间规则不接受 → 公网用 save/.oci。
- **验证**:68 tests 绿;zot 真机闭环(73.12,zot 用 crater 自家制品部署):tag + 闭包 push → catalog `[crater/rustfs, crater/yq, demo-stack]` → **全新 CRATER_HOME**(模拟另一台控制机)闭包 pull(三制品 + 裸 retag)→ `apply <project-ref> --offline` 清机部署 73.11(2 play,rustfs api/console 200)→ `plan <project-ref>` 逐 play 摘要 → `delete <project-ref>` 逆序拆净 + yq 优雅跳过。
- **关联**:D-098(离线 project)、D-087/088(thin/offline)、D-033(zot 闭环前例)。


## 2026-06-04 · D-078④ 收尾:并行镜像拉取(index 原子写 + 锁)

- **实现**:build 在指纹遍历时顺带收集渲染后的 image ref(同一有序遍历,`{{arch}}` 状态一致),fetch 前 `buffer_unordered(4)` 并发预拉;打包循环命中"已预拉"跳过二次 manifest round-trip(单镜像 task 不预拉,行为不变)。
- **并发地基(本来就欠的债)**:`write_index` 原 `fs::write` 是截断后写——并发读者读到**半截文件**(竞态单测真实抓到 `EOF while parsing`)→ 改 tmp+rename **原子替换**;读-改-写(tag/remove)加进程内 `index_lock` 防 lost update。10 路并发 retag 单测三连绿。
- **验证**:69 tests 绿;3 镜像 task 真机 build:`pre-pull 3 image material(s) (parallel)` → 全部"已预拉"打包,store gc 体检 0 孤儿、引用完整。
- **关联**:D-078(①增量④file 并发)、D-096(构建缓存)。至此 D-078 全部四项完结。


## 2026-06-04 · aarch64 musl 构建 + 首个 GitHub Release v0.1.0

- **定位(用户纠偏)**:不是"每份 crater 随身带全架构"——同构机群(绝大多数)由对应 arch 的控制端**推自己**(`select_agent_binary` 第三优先级,既有);要补的是**发布矩阵**:ARM 用户得有 ARM 版 crater 可拿。`dist/` 多 arch 只服务异构混群(一台控制机管 x86+ARM),可选。
- **交叉编译踩坑**:Ubuntu `gcc-aarch64-linux-gnu` 是 **glibc 头文件**配 musl 链接 → aws-lc 的 C 先炸 fortify `__*_chk`、关掉又炸 glibc 2.38+ 的 `__isoc23_*` 重定向——头/库不匹配打地鼠没完。正解:**musl.cc 真 musl 交叉工具链**(头和 libc 都是 musl),一次通过。`build-musl.sh` 支持 `x86_64|aarch64|all`。
- **产物**:`crater-linux-aarch64`(15M,`ELF ARM aarch64 statically linked`,qemu-aarch64 冒烟 `crater 0.1.0`)+ `crater-linux-x86_64`(19M,static-pie,真机久经验证)+ sha256sums。**诚实边界:aarch64 无 ARM 真机,端到端未验**(测试环境全 x86_64 克隆机)。
- **发布**:GitHub Release **v0.1.0**(仓库首个 release),双 arch 二进制 + 校验和。

## 2026-06-05 · requires 环境准入契约:distro/version/arch 三时点校验(D-102)

### D-102 task 声明 `requires:`;apply 全员预检零步骤拒绝;inspect/plan 提前可见

- **背景**(用户指出):`crater apply` 不是谁都能随便跑——os_package 闭包钉死 base OS×版本、镜像物料单 arch、厂商只认证特定发行版(openEuler/麒麟),但引擎只认 Debian/RHEL **两族**:发行版、版本、架构全无校验,`when_os` 还是静默跳过(全过滤时显示"成功"实际没干活)。追问二:「设了 requires 跑起来才知道,是不是太晚」——所以校验必须前移。
- **决策**:
  - **声明**:task 级 `requires: { os: [{distro, versions}], arch: [] }`——distro 对 `/etc/os-release` 的 `ID`(发行版,**不是族**),versions 精确或前缀("9" 纳 Rocky 9.4),纯数据无范围表达式(守 D-036);空 = 不限(向后兼容)。`skip_serializing_if` 保持无契约 recipe 干净。
  - **三时点**,全在执行之前:`inspect` 零连接显示契约(烤进制品);`plan` 连接零执行受同门;apply **全员预检(admission)**——并发探测所有目标 distro/version/arch,**一台不符整体拒绝且列出全部**不符主机,零步骤执行(消灭"跑到第 7 台才发现,前 6 台已动过")。teardown 豁免(已部署的永远可删);dry-run 不连机查不了。
  - **配套**:`os.rs` 探测升级为 `OsInfo{family, distro, version}`(ID + VERSION_ID);堵 when_os 静默坑——action 全被过滤时 warn「0 步可执行…可能不在适用范围」;`library/rustfs` 声明 `arch: [amd64]` 作库内示例。
- **验证**:70 tests 绿(check 矩阵:精确/前缀版本、distro≠族、arch 别名、空契约全放行);真机:契约匹配 → `准入通过` 执行;要求 22.04 对 24.04 目标 → 清晰拒绝;双主机 arch 不符 → 两台都列出零步骤;plan 同门;inspect 零连接出「环境要求」行。
- **关联**:D-036(纯数据)、D-062(os_package 钉 base)、D-048(arch 物料)、D-100(plan)。

## 2026-06-05 · D-103:`unzip:` 物料(控制端解包)+ rustfs 二进制化(单盘/多盘/多机)

- **背景**:RustFS 等上游只发 **zip** 包,而 zip 在目标机上是死路:GNU tar 解不了 zip,
  `unzip` 又不保证存在(air-gap 更装不了)。同时 rustfs task 要从容器化改为官方推荐的
  二进制 + systemd 路线。
- **决定**:`kind: file` 物料新增 `unzip: <成员路径>`——下载物是 zip、**该成员的字节才是
  物料**。解包发生在**控制端**(纯 Rust 极简 zip reader,`zip.rs`,stored/deflate +
  zip64,零新依赖,复用 flate2):
  - **build**:fetch(声明 sha256 验的是 zip 本体)→ 解出成员 → 成员字节打进 OCI 层。
    离线链路(save/apply/agent staging)看到的就是普通文件,目标机零依赖。
  - **在线 apply/plan**:lower 前预取——zip 进下载缓存(D-096,与 build 共键)、成员
    解到 `<ckey>.unzip.<hash>`,注册成 blob → `copy` 自然 lower 成 PushFile,目标机
    **永远见不到 zip**。`--dry-run` 只算确定性缓存路径不下载。并发主机预取由进程内
    锁串行,原子 tmp+rename 发布。
  - 防呆:`unzip` 只配 `url_tmpl`(src 直接放解开的文件);`unarchive` 引用 unzip 物料
    报错引导用 `copy`;构建指纹追加 `:unzip=<member>`(不声明不追加,存量指纹不动)。
- **rustfs 二进制化**:三种形态一份 yaml,`volumes` 一个 apply 参数驱动(与官方
  `RUSTFS_VOLUMES` 语法 1:1):`/data/rustfs0`(单机单盘,默认)/ `/data/rustfs{0...3}`
  (单机多盘)/ `http://node{1...4}:9000/data/rustfs{0...3}`(多机多盘)。本机数据目录
  从 volumes **推导**(POSIX sh 递归展开 `{a...b}` + 剥 `http://host:port` 前缀),
  preflight 查多机主机名可解析(纯 IP 跳过)+ **端口被外人占**(旧容器化部署冒名顶替
  答 200 的真坑,verify 同时核 `is-active` 身份);`bypass_disk_check` 参数对应上游
  `RUSTFS_UNSAFE_BYPASS_DISK_CHECK`(测试 VM 多目录同盘必需,生产保持 false)。
  双架构物料 + musl 静态 → **requires 整个删掉**(发行版/架构都不限)。
- **验证(真机 73.11/.12)**:单机单盘 apply→幂等→plan 全干净;单机多盘 4 目录 + 磁盘
  校验 bypass;多机多盘 2 节点×2 盘 IP 区间表达式组真集群(**n11 写桶/对象,n12 读回**,
  S3 sigv4 跨节点),幂等 changed=0 数据健在;主机名不可解析 → preflight 全员响亮拒绝;
  build 双架构 495MB 制品 → save → 离线 apply(`blob cached, reusing`:build 端解包与
  apply 端预取字节同 sha,内容寻址互证)→ delete 全清。73 tests 绿(+3 zip)。
- **已知边界**:分布式**首启**期间 config/unit 的 notify 会在 run 末重启服务,两台重启
  时刻不同步可能撞上格式化窗口(真机撞过一次 `inconsistent drive found`;全节点停服清
  数据目录同启即愈)。根治思路(后续):service 步骤本 run 刚 `started→changed` 时抑制
  同名 `restarted` handler 的冗余重启。

## 2026-06-05 · D-104:`wait_for` 模块(等端口/路径,对齐 ansible)

- **背景**:"等服务开门/旧进程退出/socket 出现"是部署刚需,此前全靠手搓
  `for i in $(seq 1 30); do …; sleep 1; done`(rustfs verify、k8s 等),重复、易错、
  超时报错不统一。Ansible 的 `wait_for` 正是这个原语。
- **决定**:新增 `action: wait_for`——`port`(TCP 连接探测,目标机上发起)或 `path`
  (`test -e`)二选一;`state` 四值两义(`started`/`present`=开/存在,`stopped`/`absent`
  =关/消失);`timeout`(默认 30s)到点**响亮失败**;`delay` 首探前置睡眠。
  守模块宪章:只读原语、lower 成带 `check:` 的 Shell op、零引擎产品知识。
- **实现要点**:
  - 单次探针**兼任 `check:`**:条件已成立 → 整步 ok 跳过;`crater plan` 复用同一探针
    (D-100 只读契约白得)。等到了报 changed("这步真等了")。
  - 步骤经 `sh -c`(常是 dash)跑 → `/dev/tcp` 不可直用:探测链 `nc -z -w 2`(含
    busybox)→ `bash -c 'exec 3<>/dev/tcp/…'` 兜底;两者皆无 → 一直失败 → 超时报错,
    不会误判成功。
  - `port`/`host` 过 `{{var}}` 渲染(端口常是 apply 参数);YAML 数字/字符串都收
    (`de_opt_string_or_num`)。
- **验证**:74 tests 绿(lowering:渲染/超时/负探针/port+path 互斥);rustfs verify 换用
  `wait_for port {{port}}`(真机首启 ok、幂等 ok、plan 干净);负面用例等 19999 端口
  3s 超时响亮失败。
- **关联**:D-067(模块对齐 ansible)、D-100(plan 探针)、module-charter。

### D-104 补:库内推广(zot 循环替换 + k8s-ha wait_lb)

- **zot**:verify 的 `for i in $(seq 1 15)` curl 循环 → `wait_for port 5000` + 单次
  `/v2/` 检查(真机 73.11:首启 ok、幂等 changed=0;73.12 的基础设施 zot 未动)。
- **k8s-ha**:补了一个**该等没等**的真空窗——`svc_keepalived` restart 后 VIP 漂移要
  一两秒,而 init/cp_join 都走 `--control-plane-endpoint`(VIP:8443)。新增 `wait_lb`
  (`wait_for port 8443 host {{vip}}`,haproxy 不论后端死活都先监听,TCP 通 = VIP 在位
  + haproxy 在跑),init/cp_join 改 needs 它。worker 不需要:`worker_join` 等的 join
  fact 在 init 完成后才发布,彼时 VIP 必在。**HA 全链路未真机重验**(73.13 关机,三节点
  不齐):dry-run 校验 plan 形态(渲染/排序/三台都有)+ 单测;73.13 回来后跑一遍即可。
- 其余 task(k8s-online/offline、mysql、docker)无手搓等待:kubeadm 自带阻塞重试,
  不叠加冗余 wait。

## 2026-06-05 · D-105:步骤级 `loop:`(ansible loop/with_items)

- **背景**:rustfs 查两个端口要写两个几乎相同的 wait_for 步骤——查 10 个呢?(用户点出)
  重复步骤是 ansible `loop` 的标准场景。
- **决定**:`ActionStep` 新增 `loop:`——**标量列表**(字符串/数字/布尔;纯数据,无表达式,
  守 D-036),plan 期**宏展开**:一步变 N 步,`{{item}}` 代入。
- **实现要点**:
  - **数据级替换**(`expand_loops`,task.rs):action 序列化 → 走遍 YAML 树替换所有字符串
    标量里的 `{{item}}` → 反序列化。对**所有模块所有字段**统一生效,不维护"哪些字段可循环"
    的白名单;代入后类型不符(如 `timeout: "{{item}}"` 配字符串项)在重解析处响亮报错。
  - 展开 id = `<原id>@<序号>`;**自动 id 以原始下标先行固化**(`action<i>` 引用不被展开
    挪位)。其它步骤 `needs:` 引用被循环的 id → 重映射为全部展开步(语义:等它们全部)。
  - **可组合**:loop 项本身可含 `{{var}}`(`loop: ["{{port}}", 9001]`)——`{{item}}` 代入
    在先、常规 vars 渲染在后,互不干扰。
  - **V1 边界**:项只许标量(map/list 报错);handler 不许 loop(notify 按单一 id 触发,
    展开会拆散引用;要多个写多个 handler)。
- **验证**:75 tests 绿(展开/代入/needs 扇出/`{{var}}` 穿透/非标量拒绝);rustfs 端口闸
  两步合一(`loop: ["{{port}}", 9001]`),真机:dry-run 展开成两闸、全新装两闸秒过、
  重跑被占即拒、delete 正常。
- **关联**:D-036(纯数据:列表不是表达式)、D-104(wait_for)、D-067(对齐 ansible)。

### D-105 补:loop 库内推广 + role×loop 组合修复

- **收口**:k8s-ha / k8s-offline 的 kubeadm/kubelet/kubectl 三连 copy → `kube_bins`
  一步(`loop: [kubeadm, kubelet, kubectl]`,material/dest 都派生自 item);docker 的
  containerd/docker 双 unit → `units` 一步;kube-upgrade role 的 place_kubelet/kubectl
  → `place_kube`。不收的留痕:ctd/cni/crictl 三个 unarchive(to/strip/creates 多处
  不相关差异,标量 item 表达不了)、mysql 双 svc(差在 when_os 步级字段)。
- **组合修复(真 bug)**:role 展开会给 role 私有 material 改名加前缀并按**精确名**重写
  action 引用——`material: "{{item}}"` 在那一刻不是具体名,错过重写 → apply 报
  unknown material。修:role 的 actions 在前缀重写**之前**先 `expand_loops`(loop 是
  纯宏,提前展开无害);task 顶层 loop 仍在 plan 期展开,不对称但各自正确。
- **验证**:75 tests 绿;四个 task dry-run plan 形态正确(k8s-upgrade 经 role 展开出
  `upgrade.kubelet/kubectl` 前缀物料并正确解析);docker 真机 apply 幂等 changed=0。
  k8s-ha/offline 全链路未真机重验(73.13 关机/离线构建重),dry-run 还揪出 k8s-offline
  一处 `needs: [kubeadm]` 旧 id 引用,已改 `kube_bins`。

## 2026-08-28 · D-106:方向重整——「环境的包管理器 + 对账引擎」(白纸推导定案)

- **决策**:项目方向按 [research/product-design.md](research/product-design.md)(产品白纸设计)
  + [research/next-gen.md](research/next-gen.md)(调研/技术选型/架构)定案。一句话:crater 不是
  「更好的 Ansible」,而是**环境的包管理器 + 对账引擎**——Helm 的作者/操作者分离 ×
  Nix 的密封闭包 × Terraform 的 plan × K8s 的对账循环,合并作用到 OS/裸机层,
  单二进制,AI(MCP)可全程操作。
- **核心裁定**:
  - 对象模型 = 七名词:Substrate / Resource / Blueprint / Stack / Environment / Deployment / Run;
    模块契约 = 五动词:observe / diff / apply / destroy / upgrade(observe 强制,plan/drift 是推论)。
  - 状态与过渡分离:资源声明期望态;procedure(升级 dance 等)内置于 Blueprint,被调用不被编写。
  - 作者/操作者分离:操作者体验(install/values/plan/approve)是产品的脸;
    ansible 形状 DSL(模块名即 key、隐式顺序、CEL `when:`)**降位为作者层前端**;IR 才是契约。
  - 表达式层锁 CEL(非图灵完备,承接并升级 D-036);`${}` 插值与 `when:` 同一求值器。
  - 交付单元 = 密封 OCI 闭包(离线是副产品,密封是本体);闭包靠「声明为本 + 渲染发现 +
    record 采集 + 运行时收口」收敛(Zarf 参照系,见 next-gen §5.1)。
  - 存储 = Store trait + sqlx 双后端:SQLite 默认,Postgres 一等公民(HA 多实例:
    SKIP LOCKED 队列 / advisory lock 选主 / LISTEN+NOTIFY,不引 Redis/etcd)。
  - 插件四层:Rust 内建(~70,五动词全实现)→ YAML 数据模块 → WASM(Extism)→
    协议桥(TF provider gRPC / ansible 模块垫片);不做 ansible 语法级运行时兼容,做 `import` 转换器。
- **资产处置**:engine/executor(自举 agent)/store/bundle(OCI)保留;task/component/project
  parse 层重写;现行 task DSL(`action:` 标签 + id/needs 显式 DAG + when_* 枚举字段)废止方向。
- **实施顺序**:P0 前置 = 先冻结七名词 + 五动词 IR(schema + 示例),再动 DSL——名词错了后面全错。
- **关联**:承接 D-017(引擎零产品知识)、D-036(YAML 纯数据→升级为 CEL)、D-046(单 task
  模型→拆回作者/操作者两层)、D-083(project→Stack)、D-102(requires→参数契约的一部分)。

## 2026-08-28 · D-107:P0 落地 —— `crater-ir` crate(IR 前端 + 五动词契约)

- **交付**:新增 `crates/crater-ir`(D-106 的 IR 冻结版实现),**不动**现有 crater-core/cli,
  两者并存直到新管线接上。53 个测试全绿,clippy 零警告。
- **模块**:
  - `expr.rs` —— CEL 表达式层。`when:` 与 `${}` 插值**同一个求值器**;编译期抽取
    根变量与函数名供 lint。选 `cel` 0.14(非 `cel-interpreter` 0.10:后者语法错误
    会在 antlr 里 `unreachable!` panic,做不了 linter)。
  - `selector.rs` —— `all | role.X | host.X | first(sel) | rest(sel) | sel where <cel>`。
    `first/rest` 取代 `run_once` 与"全组跑+check 守卫跳过首台"的诡计(D-106 裁定 A)。
  - `schema.rs` —— params 契约:`type`(含 ip/cidr/port/version/list/enum 语义类型)
    + `secret` + `stage: build|deploy`(`stage: apply` 显式报错并指路)。
  - `ir.rs` —— 七名词类型。**刻意不存在**:handler/notify、run_once、phase、teardown、
    offline 开关 —— 每个"没有"都是一次裁定,写在文件头注释里。
  - `verbs.rs` —— 五动词 trait。`observe` 强制 ⇒ plan/drift/destroy 是推论而非功能。
  - `types.rs` —— L1 内建类型目录(24 个),按**类型化层次**拟而非照抄 ansible 模块表:
    `systemd_unit`/`swap`/`kernel_modules`/`sysctl`/`hostname`/`image_present` 全是
    两块试金石里"仪式型 shell + 手写 check"升上来的。
  - `parse.rs` —— YAML 前端。规则:**若干步骤关键字 + 恰好一个模块 key**;未知字段
    带拼写建议直接报错。
  - `lint.rs` —— 零连接静态诊断。
- **诊断能力**(每条都是 ansible 里要连机器跑到才炸的错,`tests/diagnostics.rs` 22 例):
  模块名/参数名/参数引用拼写(带建议)、作用域外根变量、`item` 无 `each`、未声明物料、
  必填缺失与 one_of 互斥、default 不符声明类型(IP/CIDR 格式)、**探针函数封闭白名单**
  (拒 `exec()` 之类,守住非图灵完备)、fact 产销平衡、自定义类型指向不存在的 procedure、
  同名物料无 `when:` 区分。裸 shell 无 `check:` 只报 warn 并计入"模型化欠债"——接住不羞辱。
- **试金石回归**(`tests/touchstones.rs`):两块 blueprint 解析 + lint 零 error,并断言
  压缩比 ≥2×(实测 rustfs 143→57、k8s-ha 519→84)。IR 一旦退化立刻红。
- **构建注记**:仓库 `.cargo/config.toml` 钉了 mold;本机无 mold 时用 `RUSTFLAGS=""` 覆盖。
- **关联**:D-106(方向与裁定)、D-017(引擎零产品知识 —— `types:` 让 kubeadm 这类知识
  留在 blueprint)、D-036(YAML 纯数据 → CEL 升级版)。

### D-107 补:`crater lint` —— 静态检查的用户可见兑现

- **命令**:`crater lint [PATHS...] [--strict] [--json]`(默认扫当前目录)。零连接、
  毫秒级、退出码 1 表示有 error。这是新 DSL 第一个能给别人看的东西。
- **行号定位**(`loc.rs`):`serde_yaml` 的 Value 不带 span,而"报错必须指到行"是 lint
  好不好用的分水岭。用一趟轻量缩进扫描重建"某 section 的第 N 个列表项在第几行",
  唯一陷阱是**块标量**(`content: |` 里的 `- xxx` 是内容不是列表项),已单测覆盖。
  诊断输出 `file.yaml:13  error  …  (resources.servce)` —— 终端可点击。
- **目录扫描的宽容度**(实测踩到才补的):仓库里绝大多数 YAML 不是 blueprint
  (inventory / CI 配置 / k8s manifest)。规则:**点名的文件一律解析并报错;目录扫描
  发现的文件,不含 blueprint 特征段(resources/procedures/types/materials/preflight/
  health)则静默跳过**。否则 `crater lint .` 全是噪音。
- **旧格式识别**:顶层有 `actions:`/`plays:` → 报"旧版 task 格式,跳过"并计入汇总,
  不报错 —— 那是**迁移待办**,不是写错了。全仓库实测:24 个 YAML,22 个旧 task 跳过,
  2 个新 blueprint 通过,0 error。
- **warn 不阻断**,`--strict` 才阻断(CI 用)。裸 shell 无 `check:` 属此类。
- **测试**:`crates/crater-cli/tests/lint_cli.rs` 10 例覆盖命令行契约(退出码 / strict /
  目录宽容度 / 旧格式 / JSON / 缺路径)。其中 `whole_repo_scan_is_clean` 是长期护栏:
  仓库里任何新 blueprint 有 error 就红。合计 68 测试全绿,clippy 零警告。

## 2026-08-28 · D-108:五动词 L1 骨架 —— plan / converge / destroy 成为推论

- **交付**:`crater-ir` 补齐 `eval` / `ctx` / `builtins`(file、copy、service)/ `plan`,
  并把 `crater plan -f <blueprint>` 接到新管线(按文件格式分流,旧 task 仍走原管线)。
  142 测试全绿(crater-ir 114 + crater-cli 28),新代码 clippy 零警告。
- **核心兑现**:`plan` / `converge` / `destroy` **不是三个功能,是同一个 `observe` 的三个
  推论**。旧模型里 actions / teardown / plan 探针分头维护还会不同步;现在它们共用一份
  observe,不可能不一致。
  - `plan` = ∀resource: observe + diff,零写入;
  - `converge` = plan 之后对非 noop 项 apply;
  - `destroy` = **逆序**调 destroy(所以 IR 里没有 `teardown:` 段)。
- **求值层**(`eval.rs`)一条影响手感的规则:**整串恰好是一个表达式时保留原生类型**
  (`port: "${params.port}"` → 整数 9000),混着字面量才拼字符串。旧模型全是字符串替换,
  于是 `timeout: "{{n}}"` 一路以字符串流到执行层。`--set port=9443` 同样按 YAML 解析,
  得到整数而非字符串。
- **"observe 只读"被机器验证**:`FakeCtx` 记录每一条调用并区分探针/写入,
  测试直接断言 `plan` 期间 `writes()` 为空。这条纪律是 plan 可信度的全部押注,
  不能只靠代码评审守;CLI 侧再用真实文件系统复验一次(plan 跑完目录仍不存在)。
- **handler 删除的兑现点**在 `service.diff`:`DiffInput.upstream_changed` 为真且
  `state: started` ⇒ 判定重启。v0 用保守规则(**本轮任一在先的资源会变**即传播),
  方向是宁可多重启一次而非漏重启;精确到字段的传播留 P1。反向保险也有测试:
  上游没变时不得产生"天天重启"。
- **求值撞出的 schema 修正**:`each:` 字符串此前按模板解析,导致
  `each: params.dirs` 与 `each: "${params.dirs}"` 是两种写法。收敛为
  **字符串 = CEL 表达式(与 `when:` 一致,不写 `${}`),列表 = 字面量**,新增 `Each` 类型。
- **测试逼出的真 bug**:`File::destroy` 不看 `observed`,对**不存在**的路径也发 `rm -rf`
  (退役一台从没装过的机器会刷屏甚至报错)。已修,并留下钉死它的测试。
- **诚实的边界**:blueprint 自定义类型(L2)的执行器未落地,plan 里显示 `?` 并计入
  **模型化欠债**计数,绝不假装成功;`crater plan` 目前只支持本机目标(SSH `Ctx` 在 P1)。
- **关联**:D-106(五动词契约)、D-107(IR 前端 + lint)、D-092(指纹幂等 → 升为引擎通用机制)。

## 2026-08-28 · D-109:新管线接通真机 —— SSH `Ctx` + `crater apply`

- **交付**:`crater apply -f <blueprint> [--host H] [-i inv]` 走五动词管线并真正落地;
  `--dry-run` 等价于 plan。按**文件格式**分流(`blueprint_source()`):新 IR blueprint
  进新管线,旧 task / 镜像 ref / `.oci` / 命名 task 仍走原管线,两条并存到迁移完成。
  151 测试全绿,新代码 clippy 零警告。
- **同步 `Ctx` × 异步 `Executor` 的桥**(`RemoteCtx`):`Ctx` **保持同步**是刻意的 ——
  它是四层实现的共同契约(Rust 内建 / blueprint types / WASM / 协议桥),WASM 侧天然
  同步;让整条 trait 染上 async 会把 `crater-ir` 拖进 tokio 依赖,而这个 crate 刻意保持
  最小依赖面。桥接**只此一处**:`block_in_place` 让出 worker 线程再 `block_on` 那次 SSH
  往返。代价是**要求多线程 runtime**(`#[tokio::main]` 默认即是),已用
  `#[tokio::test(flavor = "multi_thread")]` 把这个前提钉成测试。
- **stderr 不许丢**:桥接把 stderr 折进输出 —— 丢了它,`run_ok` 的报错就成了空壳。
  有专门测试守。
- **诚实失败优于假成功**:物料(`material:`)的闭包解析尚未接入新管线,
  `place_material` 直接报错而**不是**悄悄写个空文件让 apply "成功";
  CLI 侧有测试断言失败后不留半成品。
- **本机端到端实测**:apply → `changed=2 ok=0`,磁盘上目录/文件/权限(0750、0600)
  逐项复核通过;立刻重跑 → `+0 ~0 -0 ✓2 无需变更`;只弄脏内容后重跑 → `~1 ✓1`,
  只修漂移那一项。这三步在 `tests/apply_cli.rs` 里固化。
- **未验证的部分(不含糊)**:**真实 SSH 目标尚未跑通** —— 本机 sshd 在跑,但密钥认证
  未配置,且不应擅自改用户的 `authorized_keys`。桥接机制已用 mock Executor 在多线程
  runtime 下验证,SSH 本身待用户在真机复验。多台目标当前**串行**,并发调度留 P1。
- **关联**:D-108(五动词 L1)、D-007(agentless)、D-019(自举 agent —— 新管线接 agent
  执行器同样只需再实现一个 `Ctx`)。

## 2026-08-28 · D-110:物料闭包接入新管线(+ 目标侧事实)

- **交付**:`copy: {material: …}` 真能用了。`facts.rs`(目标侧事实)+ `materials.rs`
  (变体解析)+ `material_ctx.rs`(取用)三件套。
- **裁定 C 第一次被真正需要**:多架构变体(`when: substrate.arch == 'arm64'`)必须由
  **目标机**判定,于是 `substrate.*` 从设计条目变成必需品。三条纪律:**封闭白名单**
  (7 项:arch/kernel/hostname/distro/version/family/init,不做 ansible 式全量 setup)、
  **惰性 + 缓存**(一次连接内每项最多一次往返)、**只读**(走 `probe`,与 plan 同源)。
  `uname -m` 的方言归一到 OCI 平台名(`x86_64`→`amd64`),作者不必记别名。
- **取用两条路,由制品类型决定,无 `--offline` 开关**(承接旧约):有本地 blob → 控制端
  推;没有 → **目标机自己拉**(curl → wget,agentless:控制端不当几十 MB 二进制的中转)。
- **不覆盖即拒绝**:只打了 amd64/arm64 却部署到 riscv → 响亮失败并报出"这台机器长什么样"
  (`arch=… distro=… version=…`),**拒绝装半套**。变体条件重叠也拒绝,不搞"先匹配先赢"。
- **摘要校验后置删除**:声明了 sha256 却对不上 → 删掉落地文件再报错。内容寻址是离线可信
  的根,留个未经校验的文件比没有更危险。
- **闭包清单可见**:plan 时打印这台机器实际会用到的物料 —— air-gap 场景下这就是
  "要带走什么"的答案(`closure()` = f(values × 目标事实))。
- **实测**:本机 plan 按真实架构选出 amd64 变体 → apply 下载 9.9MB yq → chmod 0755 →
  可执行 → 重跑幂等。测试用 `file://` URL 走同一条路径,不依赖外网。
- **未接入**:`unzip:` 物料(需控制端解包,D-103)与 `image_present`(需 OCI 闭包)
  一律**响亮报错**,不悄悄成功。

## 2026-08-28 · D-111:内建类型补齐(3 → 21)

- **交付**:`paths.rs`(template/lineinfile/unarchive)、`host.rs`(hostname/swap/
  kernel_modules/sysctl/user/group)、`pkg.rs`(package/image_present/shell/wait +
  4 个健康探针)、`service.rs` 增 `systemd_unit`。登记 24 个,实现 21 个。
- **差距必须可见**:`builtins::pending()` 列出"已登记但没实现"的类型,并有测试断言
  剩余欠债只能是 `container`/`mount`/`cron`(需额外基建)。实现了却没登记 → lint 会把
  合法写法报成"未知类型";登记了却没实现 → 用户 apply 时才撞上。两个方向都由测试守。
- **类型化的收益是可举证的**,不是审美:
  - `systemd_unit` 能说出"**只有 ExecStart 变了**",而 `copy` 一坨 INI 文本只能整文件 hash;
    且只比对我们写的那几项 —— 目标机上别人手加的 `MemoryMax=` 不算漂移。
  - `swap` 把"只 `swapoff -a` 不改 fstab、重启后 swap 自己回来"这个经典陷阱变成
    **plan 里看得见的一项**。
  - `sysctl` 逐键报 `net.ipv4.ip_forward=0→1`,而不是"跑一遍 `sysctl --system`"。
- **退役时的越权红线**(逐个想清楚,不是默认 `rm -rf`):`unarchive` 只删 `creates:` 指的
  产物(`to:` 常是 /usr/local,整个删会连累别人);`kernel_modules` 只撤持久化**不 rmmod**;
  `lineinfile` 只删那一行**不删文件**;`swap`/`hostname` 退役时**什么都不做**(原来什么样
  我们并不知道);`image_present` 不删镜像;`shell` 返回 `Warn`(没有逆操作)。
- **只读者不谎报 changed**:`wait` 成功返回 `Ok` 而非 `Changed` —— 它没改变任何东西。

## 2026-08-28 · D-112:state 记录 + drift —— 对账循环闭合

- **交付**:`state.rs`(记录 / `Store` trait / `FileStore` / 漂移判定)+ `crater verify`。
- **为什么必须有记录**:没有它,**"从没部署过"与"部署了但漂了"在 plan 里长得一模一样**
  (都是一堆 `+`)。verify 只能靠猜,于是要么天天误报、要么不敢报。有记录后三态分明:
  `NeverDeployed`(不报警)/ `InSync` / `Drifted`,退出码据此分流,可直接进 CI 与定时任务。
- **新增声明 ≠ 漂移**:blueprint 长出新资源时标为"新声明"而非"漂移" —— 归因不同,
  处置也不同。
- **说不清就不给绿灯**:有 `Unknown` 项时判定为 `Indeterminate` 并非零退出。宣称
  "一切正常"而其实有项无法核对,是**假的安心**,比报警更危险。
- **记的是现实,不是意图**:收敛后**重新 observe 一次**再落记录;首次部署时间不被重复
  apply 刷新。收敛后若仍有未达期望项,明确提示而不是静默成功。
- **存储选型的偏离与理由**:D-106 §7.1 定的是 sqlx 双后端。这里先落 `Store` **trait** +
  零依赖 `FileStore`(一条记录一个 YAML 文件,可读、可 diff、可进 git)。理由:记录量级是
  "部署过几个 blueprint",不是事件流;而 sqlx 是 async,塞进刻意保持最小依赖面的
  `crater-ir` 会与 D-109 的同步 `Ctx` 决定冲突。**SQLite/Postgres 实现属于 server 形态
  (P2),届时只需再实现这个 trait,上层不动**。原子写(临时文件 + rename)、单条损坏不
  影响 `list` 都已就位。
- **实测**:未部署 → 不报警(exit 0);apply → 记账;verify → in-sync;`chmod 777` 后
  verify → `漂移 file  mode: 777 → 0750`(exit 1)。7 个 CLI 测试固化,每个用独立 HOME。
- **关联**:D-106(§7.1 存储)、D-108(plan 是推论)、D-051/D-055(旧管线的漂移检测)。

## 2026-08-28 · D-113:修 `on:` selector 静默失效 + 机群视角(fleet)

- **背景是个真 bug**:`on:` 能解析、能 lint、有测试,但 `plan.rs::expand()` 只处理
  `when`/`each`,**从未应用过 `on:`** —— 写 `on: role.controlplane` 的资源会在**每一台**
  机器上跑。声明了却静默不生效,比不支持更危险。
- **根因是架构**:引擎逐台独立 plan,每台互不知情,于是"我属于哪些组""谁是首台"
  都无从判定。修法是引入 `crates/crater-ir/src/fleet.rs`:**静态成员信息**
  (名字 + 组,从 inventory 就能得到,不必连机器)进 `Scope`,与"连上才知道"的
  `substrate.*` 单机事实分层。
- **三者正交且顺序不能换**:`on:` 决定"这台要不要参与"(机群层)→ `when:` 决定
  "参与了要不要做"(单机条件)→ `each:` 决定"做几遍"。
- **未知组是错误,不是静默跳过**:拼错组名会让整段资源悄悄不执行、而 plan 看起来
  一切正常 —— 那是最难查的一类故障。现在当场报错并列出已知的组;单机无 inventory 时
  直接指出 `需要 -i inventory.yaml`。
- **`first()`/`rest()` 只吃静态信息**:它们需要跨主机的稳定序(**顺序 = inventory 声明序**),
  而 `where` 依赖单机事实,在机群层无法判定 → 嵌在 `first()` 里的 `where` 被拒绝并
  提示"移到最外层"。
- **顺带揪出的第二个 bug**:`host_label()` 对所有本地目标一律返回"本机",而部署记录 id
  正是由它生成 —— 三台机群会**共用一条记录、互相覆盖**。改为记录用 inventory 的
  `name`(地址变了仍是同一个成员),展示标签也逐台可区分。
- **顺带补的可用性**:plan 每行原本是 `+ file` / `+ file1`,看不出改的是哪个文件。
  现加 `PlanItem::label()` 取该类型的"主键"参数(path/dest/name/…);超长路径**居中**
  省略 —— 辨识度在两端,掐头去尾会让两个不同深路径看起来一样。
- **验证**:3 节点机群(n11/n12 = controlplane,w01 = worker)实测 n11 得 all+cp+init、
  n12 得 all+cp+join、w01 得 all+wk;三条独立记录。`selector_cli.rs` 6 例固化,
  其中 `selectors_actually_gate_execution_not_just_the_plan` 守住"plan 说不做、
  apply 就真的不能做"—— 否则 plan 的可信度是假的。合计 252 测试全绿。
- **关联**:D-106(裁定 A:Selector 升为一等语法)、D-108(plan 推导)、D-112(部署记录)。
  这一步也是 procedure 执行器的前置:`exports` 跨主机传 fact、`throttle` 逐台、
  `run_once` 全都依赖机群视角。

## 2026-08-28 · D-114:procedure 执行器 —— 「状态/过程分离」兑现

- **交付**:`procedure.rs`(`Targets` trait + `run()` + `observe_custom()`)、
  `crater procedure <name> -f bp.yaml`、自定义类型(L2)在 plan/apply 里真正生效。
  275 测试全绿,新代码 clippy 零警告。**D-106 最后一个未兑现的核心主张至此落地。**
- **procedure 是机群级的,这是它与 plan 的根本区别**:一支舞的步骤分布在不同主机上,
  还要把 fact 从一台传到另一台,所以它不能塞进"逐台独立跑"的循环。为此引入
  [`Targets`] —— 能拿到**任意成员**的 ctx 与 scope,而不是"一个 Ctx"。
- **自定义类型的弥合动作被推迟到机群层**:逐台 converge 只把"需要跳哪支舞"记进
  `RunReport.procedures_needed`,调用方收齐**去重**后在机群层跑一次。
  在循环里跑会把同一支舞跳 N 遍 —— `a_custom_type_triggers_its_dance_from_apply_exactly_once`
  就是钉这个的。顺序也不能反:先逐台收敛资源,再跳舞(舞往往依赖资源已就位)。
- **舞天然幂等**:步骤走的是**与资源同一套** observe→diff,有 `check:` 的步骤重跑即
  `ok`。实测第一次 `changed=3`,第二次 `changed=0 ok=3`。
- **fact 只能有一个来源**:导出 fact 的步骤若选中多台,同名 fact 会被各写一份、
  消费方拿到哪份取决于执行顺序 —— **拒绝**,并提示用 `first(...)` 收敛到一台。
  不做"最后一个赢"。
- **空选择与拼错组名的取舍**(与资源刻意不同):资源引用不存在的组是**错误**
  (某台会因此缺状态,属静默少装);而一支舞覆盖多种拓扑是常态 —— 单 master 时
  `rest(controlplane)` 与 `role.worker` 本就该为空,不该让整支舞失败。代价是拼错的
  组名不再致命,所以它必须**响亮留痕**:`skipped` 里带上"已知的组有哪些"的线索。
- **诚实的边界**:`throttle` 的语义是"同时最多几台",当前执行**串行**,因此任何
  throttle 都天然被满足(护 etcd 那类"必须逐台"的约束不会被违反),但它真正开始
  起作用要等并发调度(P1)。`ignore_errors` 继续执行但**必须留痕**,不静默吞掉。
- **L2 数据模块层正式可用**:用户写的是名词(`cluster_member: {role: control-plane}`),
  现实由作者声明的只读探针读出(`observe.cmd` + `parse` 标记映射),舞封装在类型里。
  **引擎依旧不懂 kubeadm**(D-017 守住)。plan 里它不再是 `?`,而是
  `+ cluster_member  via: procedure bootstrap`,也不再计入模型化欠债。
- **实测**:3 节点机群跑 `bootstrap` —— n11 产出 token 并 `exports`,n12(rest)与
  w01(worker)都拿到**真实值**写入各自文件;`crater apply` 里自定义类型自动触发该舞
  且只触发一次;重跑全幂等。`procedure_cli.rs` 7 例 + `procedure.rs` 14 例固化。
- **关联**:D-106(裁定 B/D:exports 就近声明、`types:` 进 schema)、D-113(机群视角,
  本功能的前置)、D-017(引擎零产品知识)。至此 k8s-ha 试金石从"纸上能表达"
  变成"真能部署"。

## 2026-08-28 · D-115:k8s-ha 迁移到新 DSL —— 一次能力审计

- **交付**:`library/k8s/k8s-ha.blueprint.yaml`(290 行)。对照物:旧 task 模型
  `k8s-ha.yaml` 519 行 **+** 独立的 `roles/kube-upgrade/role.yaml` 58 行 = 577 行,
  **压缩 50%,且升级 role 被吸收进 blueprint 的 `procedures.upgrade`**。
  lint 零 error;3 节点机群 plan 通过;闭包 24/24 项全部解析。旧文件保留作参照,
  两条管线按文件格式并存。
- **迁移的真正意义是审计,不是变短**。它一次性揪出**四个"声明了却不生效"的 bug** ——
  与 D-113 的 `on:` 同一类,即静默失效,比不支持更危险:
  1. **procedure 局部 params 不进 lint 作用域** → 每个带参数的 procedure(如
     `upgrade` 的 `to:`)都被误报"引用了未声明的参数"。修:lint 时把
     `bp.params ∪ proc.params` 并入作用域。
  2. **`env:` 从未生效** → `KUBECONFIG=…` 被静默丢弃,blueprint 里每个 kubectl
     都会失败。修:`with_env()` 给命令加前缀,且 **`check:` 与 `cmd:` 共用同一套 env**
     (否则探针"看不到集群",永远判定"没做过")。
  3. **`sysctl` 把 stderr 当成当前值** → 键不存在时(br_netfilter 未加载),错误文本
     被塞进 diff 并按空格切碎,刷出十几行乱码。修:探针失败即 `(未设置)`,
     多项用不可见分隔符拼接而非空格。
  4. **闭包漏掉 `each` 展开的物料** → `material: "${item}"` 配 `each: [kubeadm,
     kubelet, kubectl]` 静态看是模板,实则引用三项。**air-gap 场景下这是致命的:
     少打三个二进制,现场装不上**。修:`referenced_names()` 覆盖 each 字面列表、
     `dropins[]`、以及 **procedure 步骤**里的物料引用。
- **新增能力**:`substrate.name` / `substrate.roles`(机群身份进 CEL)。
  `substrate.name` 是 **inventory 里的名字**,与 `substrate.hostname`(探针读回的
  当前 OS 主机名)**刻意分开** —— kubeadm 拿前者当节点名,而后者往往还没设成期望值。
  `Scope::identify()` 保证它与 `scope.host` 同源。
- **迁移中发现的 blueprint 自身缺陷**:flannel 清单只在 procedure 里被 `kubectl apply`,
  却没有任何资源把它落到目标机。补一条 `copy` 资源 —— 这正是"闭包清单可见"
  能帮上忙的地方。
- **仍缺的能力(kubespray 规模之前必须补齐)**:
  - `image_present` 无 OCI 闭包 → plan 显示 `?`(诚实,但离线镜像导入还不能用);
  - `template` 只落原文,**minijinja 渲染未接** → haproxy 的 `{% for %}` backend 列表、
    keepalived 的 VIP 都还没真渲染;
  - keepalived 的网卡名需要 `substrate.iface_in(subnet)` 这类探针函数(白名单里还没有);
  - `package` 的 `material:`(os_package 离线闭包)未接;
  - `crater delete` 尚未接新管线 → `types.destroy` 声明了但无处调用。
- **对 kubespray 重写的判断**:执行模型(资源/舞分离、机群 selector、跨主机 fact、
  L2 类型)已经扛得住 k8s-ha 这种复杂度;**卡点全在物料与模板两处**,而非编排。
  下一步的优先级应由此决定。
- **关联**:D-113(同类静默失效 bug)、D-114(procedure 执行器)、D-110(物料闭包)。


## 2026-08-28 · D-116:区分收敛/审计语境 + 物料内容寻址(补记)

- 详见 commit c322b3c。要点:`plan::Intent{Converge,Audit}` —— `upstream_changed`
  只在收敛语境传播(宁可多重启),审计语境只报自己不符的项(要指出**哪里**漂了);
  `Ctx::material_digest` 三级答案(声明 sha256 > 控制端现算 > 如实 None);
  k8s-ha 七个远端二进制钉官方摘要。真机:手改 unit 后 verify 从 7 项假阳性 → 1 项真漂移。

## 2026-08-28 · D-117:作者层 DSL v1 规范定稿(设计面板产出)

- **背景**:用户质疑「这套 YAML 人来写、来读的难易与可读性」——命中要害:语法不难但
  生态是空的(inspect 不认 blueprint、模块文档全是旧的、想查参数只能读 Rust 源码),
  且我自己在 blueprint 里写出了 `${params.ha ? "--upload-certs" : ""}` —— 逻辑进字符串,
  正是本设计声称要消灭的东西。
- **方法**:9-agent 设计面板 —— 5 个互相隔离的白纸设计(Fable 5;ansible-empathy /
  reader-first / toolability-first / minimal-core / wildcard 五种哲学)→ 3 评委横向对比
  (可用性走查 / 语言严谨性敌意审查 / 3am 事故演练+工具链上限)→ 综合终稿。
  语义模型(IR)冻结,只设计语法。
- **裁定**:骨架 = toolability-first(评委 2:1;E 字符串逻辑杜绝与 F 工具化上限是本引擎
  存在性前提;加糖易拆骨难),融合各方案单点最优 22 条;评委全部致命伤逐条处置。
  规范全文:[research/authoring-dsl-v1.md](research/authoring-dsl-v1.md)。
- **核心设计**(相对现行语法的变化):
  - 条件从 CEL 换成**子句微文法**(`params.ha` / `x is set` / `==` / `in`,仅 `and`;
    or 只以结构化 `any:` 出现);插值 `${}` 只许名词、必须在双引号内、严格解析无静默透传;
  - **条件拼 flag 的结构化终稿**:`cmd: {argv: [...], flags: [{name, value?, when?}]}`,
    `flags[].name` 禁插值 ⇒ lint 可静态枚举命令全部展开;想写三元的人没有地方写;
  - 自由字符串 shell **从文法中删除**(逃生舱降为 `argv: [bash, -c, "…"]` 显式形态,
    plan 高亮)——与 product-design.md 逃生舱原则的张力见规范「审校记 R1」;
  - `cast` 选角表(`seed: controlplane.first`)、materials `source.by` 开关表变体、
    无 default 即 required、`crater: 1` 版本键、`crater lock` lockfile;
  - **可发现性三件套同源一份类型注册表**:`crater types <类型>` 字段卡(必选/可选/默认/
    传导语义)、自特化 JSON Schema(枚举你自己的物料名/角色名)、四要素报错
    (位置/判决/最近似修复/下一步命令,心智模型纠正优先于拼写);
  - **plan 的两条规范义务**:解释缺席(每个被条件裁掉的 flag/资源为什么没出现)、
    标注传导(`↳ 传导重启: service caddy`)。
- **被拒绝的路**(16 条,全部记录在规范 §6):shell 字符串、双轨引用记号、主语入键、
  正则 capture、虚无律、算术运算符、模板条件/循环、YAML anchor…… 拒绝理由与采纳的
  设计同等重要。
- **实施影响**:parse 层重写(语法换血),IR 零改动 —— 「IR 是契约、语法是前端」保险单
  的兑付。现行语法与新语法的并存/迁移策略待实施时定。
- **关联**:D-106(IR 契约)、D-107(lint)、D-115(能力审计暴露的生态空洞)。


### D-117 补:修订 A1(约定式分节)+ A2(Stack 层)

- **触发**:用户裁定「kubespray 全写在一个 yaml 里根本不可能」。
- **A1(v1)**:蓝图 = 根文件 + 显式 `parts:` 外置顶层节;文件名约定钉死
  (`<stem>.<节名>.yaml`)、零参数零条件、合并后与单文件逐字节等价、诊断跨文件定位、
  `fmt --join/--split` 双向机械转换。与被拒的 include 的本质区别:无自由路径、
  无嵌套、无条件 —— 拒绝的是三级跳读,不是多文件。
- **A2(v1.1 方向)**:`<name>.stack.yaml` 有序 `uses:` 组合多蓝图(承接旧 project/
  D-083 与 Stack 名词):条目级 params 并入"默认值层"(运行期优先级仍 5 层)、
  组名重映射、跨蓝图 export 不可见(自包含边界)、栈 bake 为闭包并集制品(承 D-101)。
- **kubespray 映射**:一个 stack × 6 个中等蓝图(os-baseline/containerd/etcd/
  k8s-core/cni/addons),而非巨文件 + 50 个 part。A1 管蓝图内篇幅,A2 管蓝图间组合,
  任何一把包办全部就退化回 include 树。

- **A3(v1,lint 强制)**:篇幅规范 —— 单节 > 80 内容行 → W430 建议 parts 外置;
  根文件 > 200 且未分节 → W431;蓝图合并 > 400 → W432 建议拆蓝图 + stack
  (职责问题非篇幅问题)。依据:80≈两屏一次读完、200≈90 秒理解上限、
  400≈职责边界气味(kubespray 六蓝图逐个 <400 反向校验)。警告不可文件内豁免,
  团队调整走 lint 配置 —— 豁免必须在 review 可见。

### D-117 补:A4 —— 表达式层用回 CEL,按位置限权(用户裁定)

- **用户裁定**:「重新发明一门语言风险太大,用回 CEL」。推翻面板 §3.1/§3.2 微文法。
- **关键认识**:面板的诊断只对了一半 —— 病灶不是"CEL 能写三元",而是**我们允许
  完整 CEL 出现在值位置**。条件属于 when,值位置只该有名词:这是**位置**问题,
  不是**语言**问题。
- **新裁定**:引擎留 CEL; 用完整 CEL; 插值**只允许纯引用**
  (标识符+点路径),出现运算符/三元/函数/下标一律 lint E310 并给出 flags 改写。
  校验在 lint 期、零连接。
- **保留面板成果**(与表达式引擎无关的部分才是主要价值):结构化 flags
  ( —— 条件成为条目属性,这才是条件拼 flag
  的真正解药,它从不依赖微文法)、cast、selector 微语法、fleet 组契约、
  无 default 即 required、materials source.by、可发现性三件套、plan 两条义务、
  A1/A2/A3。
- **废止**:自造插值文法、条件子句微文法(is set→has()、is not empty→size()>0、
  or→CEL 的 ||,§6 拒绝 or 的理由"无括号易误读"对 CEL 不成立)、join 构造器。
- **承认的净损失**:微文法本可做更强静态类型检查(== 两侧类型、枚举穷尽),
  且 `x is set` 比 `has(params.x)` 更接近自然语言。换回零自造语言维护成本、
  CEL 公开规范与生态、k8s 用户既有认知。记录在此以便将来重新评估。

### D-117 补:A5 —— `on:` 键名的 YAML 1.1 风险(schema 实测发现)

- **发现**:实现 `crater schema` 后用真实校验器(PyYAML + jsonschema)跑真实 blueprint,
  29 条错误里 26 条是 `{True: ...}` —— 键 `on` 在 **YAML 1.1** 里是布尔 true。
  crater 用 serde_yaml(1.2)解析无误,但 PyYAML / 许多 CI / 部分编辑器仍是 1.1,
  会把元字段 `on:` 读成 `true:`。属**生态兼容风险**而非解析 bug,
  且这个坑由我们的作者承担(他会看到"我的 YAML 明明对,为什么 CI 说键是 true")。
- **三条路,待裁定**:保持 `on:`(语义贴切、与 GitHub Actions 同名,但踩坑)/
  改 `hosts:`(无风险,但与 Ansible 同名而语义更强 —— 正是哲学第 9 条禁止的"假同源词")/
  改 `target:`(无风险、无假同源,但要全面改名)。倾向 `target:`;涉及规范、blueprint、
  schema、lint 多处,**应一次改完**,故留待用户裁定。
- **同时修掉两个 schema 真 bug**(同样由实测暴露,单测覆盖):
  material 位不接受插值(`each:` 展开时写插值是正当写法)、
  `cmd` 未出现在探针位(它是双位置类型,`health:` 段就用它)。
  修后真实 blueprint 校验 **0 错误**;注入两处错误(类型名拼错、mode 未加引号)准确抓到 2 条。

### D-117 补:A5 裁定 —— 元字段 `on:` 改名 `target:`(用户选 C)

- **裁定**:采用方案 C。`hosts:` 的假同源风险(看着像 Ansible 实则语义更强)正是
  哲学第 9 条明令要避免的;`on:` 的坑虽不在我们身上,却由我们的作者承担。
- **旧关键字教学式拒绝,不静默接受**:静默接受等于把 YAML 1.1 的坑留给作者。
  `on:` 与"已被 1.1 解析器转成布尔 `true` 的键"两种形态都拦下,给同一条解释,
  而不是报无从下手的"未知字段 true"。
- **一次改完**:parse 关键字表与取值、jsonschema 元字段、library 蓝图、
  测试内联 YAML、规范文档,共 12 个文件。
- **验证**:改名后用 PyYAML(YAML 1.1)+ jsonschema 直接跑真实 k8s 蓝图,
  **0 条错误、零修正**(改名前 26 条)。331 测试全绿。

### D-117 补:A3 撤销 —— 篇幅阈值降为信息(用户质疑成立)

- **用户质疑**"要硬限制篇幅大小吗,这不合适吧" —— 成立,撤销原 W430/431/432 方案。
- **撤销的三条理由**(都是我该认的):(1) 80/200/400 那些数字是编的 ——
  初稿称"evaluator 实测的认知预算",实际没有任何实测;(2) 行数不度量复杂度,
  48 行物料声明与 48 行嵌套条件成本差一个数量级;(3) 实测本仓库旗舰蓝图
  k8s-ha(procedures 109 行 / 全文 248 行)会立刻触发两条警告,而它并不臃肿 ——
  在健康代码上误报的规则会让整个 lint 输出被整体关掉。
- **改为**:`crater lint --stats` 只**报数**,对可外置的节附一句提示,
  不计入 error/warn、不影响退出码;规范里保留经验参考并显式写明"这不是规则"。
- **A1 `parts:` 照常实现**(机制,不含判断):根文件显式声明 → 合并同目录
  `<stem>.<节名>.yaml`;E120 声明但缺文件 / E121 内联与外置双定义 /
  E122 幽灵文件(存在却没声明,静默不生效是最难查的一类问题) / 禁嵌套 /
  只能外置整个顶层节。所有命令改走 path-aware 加载,parts 一视同仁生效。
- **验证**:把 k8s-ha 真拆成 root + procedures 两个文件,解析结果
  (26 资源 / 24 物料 / 3 procedure / 1 自定义类型)与单文件**完全一致**。

### D-117 补:crater fmt --split / --join(兑现 --stats 的提示)

- **背景**:lint --stats 已在提示"可 crater fmt --split <节> 外置",但那个命令
  当时并不存在 —— 一张空头支票。本次补上。
- **纪律**:转换必须**机械、可逆、语义等价**。每次写盘后重新解析,与转换前的
  结构指纹(name/各节计数/procedure 与 type 名)比对,对不上就响亮失败并回滚。
- **事务性**:join 必须在校验**之前**删 part 文件(留着会被 E122 幽灵检查拦下),
  所以先把原样存进内存,校验不过就整体回滚 —— 绝不留下"根文件已改、part 已删"
  的半合并状态。split 同理。写盘一律临时文件 + rename。
- **verify 的边界(写成说明性测试)**:它比对的是**本次转换**的前后,
  用户在两次 fmt 之间手改 part 文件是合法编辑,fmt 不该也无法把它当错误 ——
  这条边界曾被我写成一个错误的测试,已改正。
- **验证**:真实 k8s 蓝图 split → lint → join,前后均为
  "26 资源, 24 物料, 3 procedure, 1 自定义类型",往返语义一致。351 测试全绿。

### D-118:UI 五阶段落地(对账中心世界观的全部兑现)

- **背景**:docs/research/ui-design.md 定案后按五阶段实施,全部真机
  (5 台 VM,SSH 隧道)+ 浏览器闭环验证。三次提交:
  1c5e814(①②③)/ 60167e8(④)/ ad878b7(⑤)。
- **阶段①② 执行打通 + 对账供血**:落盘 job 系统(子进程直写日志,UI 死
  不卡部署;setsid 进程组;exit_code 落地;sweep+watcher 重启恢复),
  verify --json 拆库入对账快照,两轴卡片墙(Synced/OutOfDate/Progressing
  × 记录),OutOfDate 靠 apply 记账的 blueprint/inventory sha 指纹。
- **阶段③ 从零闭环**:app 文件 = 期望态绑定(带注释 YAML,零入库),
  跨文件 lint(param 对账 + 拼写建议),plan-gated apply(提交时钉指纹
  蓝图 sha+inventory sha+**参数快照**,plan 成功才生效,apply 对不上 409),
  编辑器新建向导(登记表驱动骨架),60s 调度器(verify.interval 巡检)。
- **阶段④ 执行呈现**:`CRATER_EVENTS=<path>` 打开 NDJSON 事件流(机器
  契约走环境变量,不占人用旗标);crater-ir `converge_with(observer)`
  逐步回调;UI 字节游标轮询画"机器×资源"实况矩阵(·→~/✓/⚠/✗);
  verify 非零按事件流分流 drifted(黄)/failed(红)——"现实不符"
  与"执行出错"措辞不混。
- **阶段⑤ 表单投影**:/api/context 光标字段卡 + /api/patch 单行 scalar
  定点补丁(保行尾注释;锚点/flow/块标量 409 降级只读;安全裸词裸写、
  歧义词加引号)。行级启发式而非 YAML 解析器 —— 半成品文本是编辑常态,
  坏输入降级成"不认识"。
- **原则重申**:UI 全程零外部依赖(htmx+原生 JS);所有字段知识来自
  26 类型登记表,UI 不硬编码任何类型名 —— 新类型登记即出现在骨架、
  向导、字段卡、lint 四处。
- **验证**:漂移全周期真机实况 —— 停 n3 chrony → verify 矩阵 ✗ 精确
  命中(time_sync×n3)→ plan 过闸 → apply 治愈实况 ·→~ 逐格定案;
  plan 闸门四格(无 plan/改蓝图/换参数被拒,plan→apply 放行);
  调度器 auto-verify 自动点火。564 tests 绿。

### D-119:存储分层参考(三分与窄接口)

- **起因**:"YAML 该存哪 —— git / rustfs / SQLite / PostgreSQL"。结论是这四个
  不是备选项,而是**三个不同位置**上的答案,写进 `docs/research/storage-design.md`。
- **三分**:期望态(蓝图/inventory/app)→ git;物料与闭包 → 窄接口后面的 blob
  存储;运行态(部署记录/job/对账快照)→ 保持本地文件。划分依据是三种根本不同
  的访问模式,不是分类学。
- **期望态不入库**再次确认:用 DB 存它等于把 git 重新实现一遍且更差 —— 而这条
  路 D-106 已经走过又退回来(旧管线 TursoStore 的 deployments/job_runs 两张表)。
- **数据库的真正临界点是并发控制,不是数据量**:现在两台控制机同时 apply 没有
  任何东西拦得住,`~/.crater/state` 会被后写的覆盖。单机用文件锁;多控制端需要
  共享租约 —— 那才是 PostgreSQL 唯一站得住的位置。
- **窄接口**:建议 `BlobSource`(contains/fetch/manifest/origin)与 `BlobSink`
  (put/finish),复用已有的 `crater_core::bundle::{Manifest, BlobEntry}`。现有
  tar 闭包作为第一个实现搬进去应是**纯重构**。刻意不预加 list/delete/signed_url
  —— 每个方法都要能说出"谁在调它"。
- **硬前提**:密钥外置排在一切之前。inventory 里现在是明文口令,进 git 就是永久
  泄漏(历史删不掉)。优先 SSH key 直接绕开口令。
- **旁证**:harness/harness 的实现分析(一个容器 = 一个 Go 静态二进制 + 一个
  SQLite + 一个数据目录,git 壳调二进制、OCI 借 CNCF distribution、npm/maven/
  python/nuget/cargo 等自己实现,blob.Store 只有五个方法)。全文在 mica
  `devops/harness/architecture`。

### D-120:执行策略 host / linear,以及零警告门禁

- **起因**:与 ansible 对比 —— 它在每个 TASK 跑完时就报出所有主机的结果,
  而 crater 逐台执行,要等最后一台跑完才知道第三步早在第二台上炸了。
  对滚动升级这是真差距,而且改不了输出形状就绕不过去:**组织轴由执行模型
  决定**,不是格式选择。
- **做法**:加 `--strategy`,而不是改默认行为。ansible 自己也有 strategy
  的概念,两种顺序各有其对的场景:
  - `host`(默认):一台跑完全部资源再下一台。排障时最顺手 —— 一台机器的
    来龙去脉连在一起。
  - `linear`:一个资源在全机群跑完再下一个。每个资源做完立刻给出
    `→ <资源>:changed=4 ok=0 failed=1,已摘除 1 台`。
- **语义只有一份**:把逐项收敛的判断(Unknown 要重新观察、上游动过要复查
  预测)从 `converge_with` 的循环体抽成 `plan::converge_item`,两条路径共用。
  否则两种顺序会在最微妙的地方分家 —— 而那正是"配置改了服务没重启"那类
  故障的温床。抽取本身是纯重构,297 个 IR 测试原样通过。
- **失败摘除**:某台失败后从后续资源里摘出去(与 ansible 一致)。在一台
  半坏的机器上继续往下做,只会把故障现场搅得更难查。
- **资源顺序取各台计划的并集**(按首现次序):某台可能没有其中几项
  (选择器/when 没选中),拿任意一台的计划当全集会漏。
- **`--serial N|N%`(D-120 补)**:分批滚动,一批整批做完再下一批,
  **任一批失败就停** —— 后面的机器一根手指都不碰。这才是滚动升级的意义:
  出事时还剩大半个机群是好的。两种策略都支持;百分比向上取整且至少 1 台
  (向下取整会得到 0 批,命令什么都不做还看不出为什么)。
  linear 下每批只连**本批**的机器,机群级的舞也每批跑一次 —— 若某支舞
  依赖别批机器的 exports,它会在这里明确失败,而不是拿半个机群的事实
  悄悄算错。
- **顺带**:零警告门禁进 CI(`RUSTFLAGS=-D warnings`)。此前"零警告"全靠
  人肉 grep 声称,而 grep 写窄了就会漏 —— `does not need to be mutable` 和
  测试代码里的 unused variable 都这么溜过去过。cargo 对 registry 依赖自动
  `--cap-lints allow`,所以这条只管得住本仓库,不会被上游误伤。
  `just check` 是同一条口径的本地版。

### D-121:凭据外置(inventory 可以进 git 而不含明文)

- **起因**:反复标记为"离生产最远的洞",而这一轮它真的咬人了 ——
  `library/_template/inventory.example.yaml` 的占位口令被 UI 写成了实验机
  真口令,**两次**,差一点提交。同时它是走 git 的硬前提:git 历史删不掉,
  一旦推上去,正确做法就变成"先换口令,再谈清理历史"。
- **三条外置路径**,优先级 密钥 > 口令文件 > 字面口令:
  - `key:`(本就支持)—— 从根上绕开口令这个概念
  - `password_file: <path>` —— 文件不进版本库;末尾换行自动剥掉
    (`echo secret > f` 的必然产物,不该算进口令)
  - `password: "${env:VAR}"` —— 覆盖 CI / 容器 / systemd 的注入方式
- **只在执行前解析**,不在 `from_yaml_file` 里做:UI 的读取路径也走那条,
  而那条不该看见明文。解析得越晚,明文在进程里活得越短。
- **只认 `${env:NAME}` 一种形式**,不做通用插值 —— 凭据字段上支持任意表达式,
  只会让"这个口令到底从哪来"变成一道谜题,而那正是出事时最不想面对的。
- **变量缺失是硬错误**:悄悄替成空串,表现出来是"认证失败",而真因
  (忘了 export)要查很久。`password` 与 `password_file` 同时给也直接拒绝。
- **告警**:inventory 已被 git 跟踪且含字面口令时提醒一次。刻意只在"已进
  git"时提醒(本地文件里写明文是本地的事),且**去重** —— 同一条命令里
  `hosts()` 会被调用两次,重复的告警是最快被无视的那种。
- UI 的主机表单改成三选一并说明优先级 —— 问题正是从那个只会写字面口令的
  表单开始的。读回时区分 key / password_file / env / password 四种。

### D-122:机群机制回归验证(执行引擎改完之后)

- **为什么现在做**:这一轮改了执行引擎(抽出 `converge_item`)、加了两种
  执行策略与分批、重写了整个输出层 —— 而最难的机制(procedure / exports /
  throttle / 自定义类型的舞)一次都没重跑过。加第七个功能之前,先证明前六个
  没把地基弄坏。
- **靠什么证伪**:`library/selftest` 正是为此而生 —— 它不装任何东西,只让
  **每台机器自己**记下动手的时间区间。报告只说"做完了",不说"什么时候做的";
  限流有没有真限住,唯一的证据是时间戳,而且必须带外取回。
- **判据全过**(5 台真机):
  - `first()` 只落一台 —— 只有 cp1 有 `role.seed`
  - `exports` 跨主机 —— 4 个消费方的 token **逐字节相同**
  - `throttle: 1` —— cp2 区间在 594.398 结束、cp3 在 594.406 才开始,
    即便 `--parallel 2` 仍严格错开
  - 不限流 + `--parallel 2` —— w1/w2 区间几乎完全重叠(并发真起来了)
- **两条新路径也验了**:host 与 linear 策略结果一致且幂等;把 redis 的复制
  打回 master 后跑 linear,`replica_of` 判为未达期望态 → 触发机群级 `attach`
  舞 → 复制恢复成 slave。**自定义类型的舞在新执行路径上没有走丢**。
- 顺带还一笔文档债:storage-design.md 的"期望态 → git"补上更正 ——
  **crater 本身不依赖 git**(二进制零处调用、依赖里没有 git 库、镜像不含
  git),那一节讲的是文件该怎么管,不是运行时要求。初稿把两件事写混了。

### D-123:包分发走 OCI 分层制品,搜索走索引文件(设计,未实现)

详见 `docs/research/pkg-design.md`。

- **起因**:「要不要做成 Helm chart 那种,YAML 打成 OCI,`crater search` /
  `crater install` 一键安装?crater 有两个可分发物(蓝图包小、物料闭包大)
  怎么办?」
- **核心判断**:"从 OCI 只取 YAML"不是待验证的想法 —— 旧 task 管线 D-087 已
  真机验证过(Docker Hub 瘦拉 16K,全量 9.6M)。它同时是协议原生能力(层独立
  内容寻址)和行业惯例(Helm 自己跳过 `.prov` 层;KitOps / Ollama / timoni /
  Flux `layerSelector` 都是"小配置层 + 大数据层按需拉")。**两个可分发物不是
  问题,是分层的理由**:同一 manifest,蓝图层 + 物料层按 mediaType 区分;架构
  用 image index `platform` 分变体,蓝图层跨架构同 digest。
- **不照抄 Helm 的两处**:Helm 只有一个可分发物;`helm search repo` 在 OCI 上
  根本不工作(OCI 无搜索、`_catalog` 不在规范里且 Docker Hub 禁用)。搜索走
  Helm 经典 repo 那种静态 `index.yaml`,能随闭包进 U 盘。
- **兼容性地板 = OCI 1.0 风格**:自定义 `config.mediaType` + 真实 config blob +
  自定义层 mediaType。不依赖 `artifactType` + 空描述符(ACR 拒收,与本仓库
  buildx provenance 撞的是同一堵墙)、referrers API(GHCR / Docker Hub 无)、
  `_catalog`。版本发现只用 `tags/list`。
- **config blob 就是 `crater inspect` 的契约**(参数 / 机群 / 物料清单):
  `pkg inspect` 与 UI 远端目录只拉 manifest + config,零层下载。
- **`install` 不省 plan 闸门**:pull → 契约 → fit → app 文件 → plan,停在闸门前,
  `--yes` 才 apply。命令面归 `crater pkg` 子命令,不新增 `crater ls`(撞名)。
- **分四阶段**:格式 + push/pull → 闭包层 + 多架构 → install → 索引与搜索;
  阶段 4 在包不到十几个之前不做。第一阶段必须实测 **ACR 个人版收不收自定义
  制品**(文档写的是企业版专属),不收就用 Docker Hub / zot。
- **调研的局限**:四条线并行完成,交叉核对因配额只做了一部分;BuildKit
  注解分变体的先例引用已更正为其 attestation storage 文档。

### D-124:`crater pkg` 落地 —— 蓝图包成 OCI 制品(D-123 第一阶段)

- **做了什么**:`crater pkg push/build/pull/inspect/tags/ls`。蓝图目录 →
  一个 manifest:config 是**契约本身**(参数/机群/物料/规模),一层是蓝图
  目录的 tar.gz。**不设 `artifactType`** —— 制品身份靠 `config.mediaType`,
  1.0 写法,ACR 那堵墙绕开。
- **不是新造语法,是接线**:自定义类型过线(D-033)、增量拉(D-078)、
  瘦拉(D-087)都是旧 task 管线验过的,新蓝图管线此前只有 `closure.tar`
  一个出口 —— 一个只能靠 U 盘走的文件。
- **契约收敛成一处**(`pkg::contract`):UI 目录卡片、`pkg inspect`、远端
  registry 上那份 config blob 现在是同一份数据。此前 UI 自己算一遍,
  三边迟早会漂。
- **凭据永不进包**:inventory 全数排除并逐个报出;余下文件扫字面口令,
  撞上**拒绝打包**(不是告警 —— 推上 registry 之后"先换口令再谈清理"是
  唯一补救)。扫描放行 `${env:}`/`{{ }}`/以 `{` 开头的映射:
  `secret_key: { default: "changeme" }` 是**参数声明**不是泄漏,漏了这条
  每份带敏感参数的蓝图都打不了包(第一次跑就撞上了)。
- **顺手修了一个真 bug**:`untar_gz_into` 不建父目录,于是只有文件条目、
  没有目录条目的 tar(合法,也正是 `tar_gz_files` 打出来的)会以 ENOENT
  失败,报错还只说"文件不存在"。现存唯一调用方就是新代码,影响面为零。
- **验收**(见 pkg-design.md「第一阶段验收记录」):zot 与 distribution
  两家 registry 自定义类型原样过线;inspect 零层下载(store 仍 118 B);
  往返逐字节一致;拉回的 yq 包 apply 到 sshd 容器装成并幂等。
- **两个缺口**:ACR / Docker Hub 未实测(本地无凭据);实验室 73.x 被本机
  Meta 代理截断,真机改用 sshd 容器代替。

### D-125:`crater install` —— 一键安装,但闸门一步不省(D-123 第三阶段)

- **顺序是全部的设计**:蓝图 lint → 必填参数 → 机群对账 → 落 app 文件 →
  plan → (`--yes` 才)apply。**前三道全在本地,连第一台机器之前关完。**
  组名写错时这里说"机群里没有 storage 组";连上之后才发现,则要从一堆
  SSH 报错里往回猜。
- **"一键"省的是找包、抄参数、比对组名,不是"先看 diff 再动手"。**
  默认停在计划上,并打印照抄即可的 `crater apply` 命令。
- **不交互追问缺参数**:部署工具在管道里挂住等输入是最难查的一类故障。
  缺什么就连同现成的 `--set` 一起报出来 —— Helm 也是这个选择。
- **敏感参数不写进 app 文件**,改写成一行"下次怎么给"的注释。
- **跳出来一个真缺陷**:`library/rustfs` 的 `secret_key` 漏标 `secret: true`
  (试金石 fixture 标了、库里那份没标),第一次跑就把真值写进了 app 文件。
  两层修法:补声明,再加**按名字**的兜底(password/secret/token/key…)。
  理由是 `install` 拉的是**别人做的包**,作者标没标不由我们决定,而 app
  文件是要进 git 的、git 历史删不掉 —— 宁可多扣一个(扣下来的会连同用法
  一起报出来),不可漏放一个。
- **先做 3 后做 2**:第二阶段(物料层 + 多架构)卡在"ACR 个人版收不收
  自定义制品"这条实测上,本地没有凭据;第三阶段完全无阻塞,而且它才是
  "像 helm 那样一键安装"这个问题的正身。设计文档本就写明"阶段之间没有
  强依赖,1 是其余的前提"。
- **没做**:UI 目录的远端源选项卡(数据已就位,与第四阶段索引一起做更顺)。

### D-126:兼容地板在 Docker Hub 上实测通过

- **为什么值一条记录**:D-123 把兼容地板定在 OCI 1.0 风格(自定义
  `config.mediaType` + 真实 config blob + 自定义层 mediaType,**不设**
  `artifactType`),依据是文档与先例。文档说的和 registry 真做的是两回事 ——
  Docker Hub 是 2022-10-31 才开始收 OCI artifact 的。
- **判据绕开自己的客户端**:读回 manifest 走 registry API + curl,不走
  crater。拿自己的解析去证明自己的写入,证明不了任何事。
- **两个方向都验**:蓝图包全绿;把一个普通 docker 镜像喂给同一套断言,
  两条 mediaType 都报 ✗ 并非零退出 —— 否则这只是个只会绿的摆设。
- **结果**:zot 与 Docker Hub 均原样过线。Docker Hub 上 config 535 B、
  蓝图层 572 B,即"零层下载读契约"的实测数字。
- **ACR 仍未测**:个人版收不收自定义制品没有答案,workflow 留了
  `-f registry=acr` 的入口,但设计上不假设它可用。
- **踩到的坑**:新 workflow 漏抄了 release.yml 早就写明的 `RUSTFLAGS: ""`
  ——仓库 `.cargo/config.toml` 为本地提速指定了 `-fuse-ld=mold`,runner
  上没装。同一个坑在本地会话里也撞过一次。

### D-127:物料层 + 多架构 image index(D-123 第二阶段)

- **`--arch` 而不是 `--for arch=`**:`bake_scope` 把多次 `--for` 合并成**一个**
  画像,同一个 key 给两次是后者覆盖前者 —— 多架构今天根本表达不了。新开
  `--arch` 一个维度,正好对上 OCI `platform` 字段(那是规范定义的变体选择
  机制,所有 registry 与运行时都懂);其余维度仍走 `--for`,与每个 `--arch`
  合并。
- **物料层一律 `fetch=dependency`**:新蓝图管线里 `templates/` 与 `files/` 已经
  在蓝图层里了,`materials:` 全是外部字节。于是瘦拉 = 蓝图包,全量 = 加闭包,
  正好是设计里的两个可分发物。
- **蓝图层与 config 跨架构同 digest**(实测:config 1 个、蓝图层 1 个、物料层
  2 个)。registry 内容寻址,多一个架构只多它自己的物料字节。
- **索引解析改成本机架构优先**(原来硬编码 amd64,D-061)。原来的行为在
  arm64 机器上**静默**装错架构:摘要对得上(那是 amd64 那份的摘要),直到
  目标机 exec 才报 `Exec format error`。
- **`--closure` 接受 `oci://<ref>`**:包里的物料层已在本地 store 按 sha256
  躺着,不解包不复制,直接交路径给 `BlobMap`。`install --full` 自动接上。
  这印证了 D-119 的接口够窄 —— 加第二个 blob 后端没让 `BlobSource` 多方法。
- **`push` 学会推 image index**:子 manifest 按 digest 先推,再推 index。
  index 走原始 PUT,不经 `OciImageManifest`(它会把 `manifests` 丢掉)。
- **验收**:瘦拉 20K vs 全量 9.6M(且只拉本机架构那份,不是两架构的 18.8M);
  真断网目标机(`iptables OUTPUT --ctstate NEW -j REJECT`)上 `install --full`
  装成;**反证**同一台机不带 `--full` 报 `Could not resolve host`,装不上。

### D-128:索引文件与搜索(D-123 第四阶段,四阶段收官)

- **为什么不是 registry API**:OCI 只定义了 `tags/list`(某仓库有哪些版本);
  "这个 registry 上有哪些包"根本问不出来 —— `_catalog` 不在规范里,Docker
  Hub 还有意禁用。Helm 撞的是同一堵墙,答案是静态 `index.yaml`。索引另有
  两处便宜:能随闭包进 U 盘;任何静态 HTTP 都能托管(含 rustfs 这类 S3)。
- **索引的 `version` 取 tag,不取蓝图的 `version:`。** 头一版反了,结果 yq
  的 4.44.3 与 4.40.5 双双报成蓝图修订号 `1`,后一条**静默**覆盖前一条。
  索引存在的意义是把"包名+版本"翻译成一条**能拉的引用**,而能拉的只有 tag。
  蓝图版本降为 `blueprint_version` 字段(同 Helm 的 version / appVersion),
  不一致时 `pkg push` 提醒一句 —— 不报错,两种约定都合理。
- **摊包时留 `.crater-pkg` 印记**:包目录按包名命名,`install yq:4.40.5` 会
  复用上次 `install yq` 摊下的 4.44.3 目录并**静默装错版本**。印记对不上就
  拒绝,并给出换目录的命令。点开头,所以不会被打进下一个包。
- **`search` 只查本地缓存,不连网**:搜索要能在断网机房用,而且"每次搜都
  往外发请求"是把使用习惯与网络状况绑死。要新的就 `repo update`,那是一个
  明确动作。坏索引先落临时文件再校验,不覆盖上一份好的 —— `repo update`
  最容易在断网时撞上,而那时旧索引正是救命的。
- **`pkg index` 没有"扫一个 registry"这种来源**:那正是 OCI 问不出来的东西,
  假装能问只会在别人的 registry 上失败。来源只有显式引用与 `--store`。
- **顺带写明一条既有限制**:物料没声明 `sha256:` 且没有闭包时判不出漂移
  (既有测试 `a_remote_material_without_a_digest_admits_it_cannot_tell`),
  换版本时目标机保留旧字节并报"无变更"。带 `--full` 或声明 `sha256:` 即正常
  ——实测 `--full` 降级 4.44.3 → 4.40.5 成功。

### D-129:ACR 个人版实测拒收;crater 认 docker 的登录

**ACR 实测**(推最小蓝图包到 `registry.cn-shenzhen.aliyuncs.com/willspace`):

```
403 DENIED — unknown manifest class for
application/vnd.crater.blueprint.config.v1+json
```

- **卡在 manifest,不是 blob**:config 与蓝图层都已上传成功,报错来自
  `/v2/<repo>/manifests/<tag>`。ACR 拦的是 manifest class 白名单。
- **不是换个写法能绕过的**:我们用的已经是最保守的 OCI 1.0 写法。唯一出路
  是把 config 伪装成 `vnd.oci.image.config.v1+json`、改用注解识别包 ——
  那会砸掉"制品身份靠 `config.mediaType`"这条与其它五家 registry 共同的
  约定,为一家最保守的 registry 让所有人降级。**不做。**
- **同一个 ACR 收普通镜像没问题**(release workflow 一直在推),所以问题
  精确地是"自定义制品",与凭据、网络、区域无关。
- **更正 D-123**:"以 ACR 为兼容地板设计"这句站不住 —— 它不在地板上,在
  地板**之下**。兼容地板是 Docker Hub(已实测通过)。

**crater 认 `~/.docker/config.json`**:

- 别的 OCI 工具都认(helm / oras / skopeo / buildah)。让已经 `docker login`
  过的人再跑一遍 `crater registry login`,等于要他把口令在第二个地方再写
  一遍 —— 而**口令被抄写的次数,就是它泄漏的机会次数**。crater 只读不写。
- 优先级:crater 自己的 `auth.json` → docker config → 匿名。
- 只认明文 `auth` 字段;凭据助手(`credsStore` / `credHelpers`)不认 ——
  那要 exec 一个外部程序,与静态单二进制的气质冲突;装了助手的人用
  `crater registry login` 一句话补上。**认不出就退匿名,不拿空口令去撞认证。**
- 实测:Docker Hub 与 ACR 两家都是靠它连上的(ACR 那个 403 正说明认证过了)。

### D-130:UI 远端源选项卡

- **那一页只做一件事:把包弄进工作区。** 弄进来之后它是一张普通的本地蓝图,
  参数表单、机群对账、建任务、plan 闸门一步不变。于是远端源没有第二套表单
  逻辑 —— 两套逻辑迟早跑偏,而跑偏的表现是"UI 上填的参数和实际装的不一样"。
- **不连网地浏览**:卡片全部来自本地缓存的索引。断网机房里这一页照样能开,
  而"要不要联网"是按「同步索引」时的一个明确决定,不是每次打开页面的副作用。
- **默认瘦拉**,弹窗把两个选项讲清楚(几十 KB 的蓝图 vs 可能几百兆的物料)。
- **已在工作区的包标出来并变淡**,点它不重拉,只提示切标签建任务 ——
  避免反复拉同一个包,也避免覆盖本地改动。
- 浏览器实测走通:远端卡片 → 拉进工作区 → 本地卡片 → 参数表单 → 机群对账
  `✓ storage:1 台(需要 ≥1)`。

### D-131:容器镜像进闭包(issue #1)

- **补的是什么**:`closure.rs` 此前只收 `MaterialKind::File`,镜像被 `skipped`
  掉。后果很具体:离线装 k8s / docker 这类编排,二进制能带走、镜像带不走,
  **现场装不上** —— 而"一个文件带走一切"是 crater 相对 ansible 的立身之本。
  D-115 点名的"卡点全在物料与模板两处",模板那半早接上了,物料这半空到现在。
- **共享 blob 池,不是一镜像一个 tar**:镜像的 config / 层 / manifest 按 sha256
  落进闭包同一个池子(复用旧管线 D-018 的 `BundleStage::pull_image`)。k8s 那
  十几个镜像共用基础层是常态,一镜像一个 tar 会把同一层存好几遍。
- **archive 在部署时现合成**,而且只对**真的要装**的镜像合成。代价是控制端多
  一次打包,换来闭包体积按内容去重。
- **新增 `Ctx::material_source`**(与 `material_digest` 同构):`image_present`
  的期望态是"这几个镜像在不在",而蓝图里写的是**物料名**;名字到 ref 的映射
  要靠作用域求值,资源类型自己解析不出来。没有这条,observe 只能数一数目标机
  上有几个镜像 —— 那回答不了"我要的那几个在不在"(旧实现正是如此)。
- **`pull_image` 改用 crater 自己的凭据与客户端**。早先写死 `RegistryAuth::
  Anonymous` + 默认配置,等于宣布"闭包只支持公开镜像";本地 HTTP registry 也
  连不上,连测试都做不了。
- **ref 比对按后缀归一**:docker 把 `docker.io/library/x:1` 显示成 `x:1`,ctr
  保留全名。逐字比会让每次 apply 都重新导入一遍,而且**看起来是成功的**。
- **三种运行时**:docker / ctr / nerdctl 各有各的 list 与 import 写法,
  `namespace` 只对后两者有意义。
- **验收(multipass 真虚机 w1,Ubuntu 24.04 + docker 29)**:
  - 把 w1 用 `iptables -A OUTPUT --ctstate NEW -j REJECT` 断网,
    `docker pull` 确认失败(`connection refused`)
  - `crater apply --closure` 装成,`docker images` 看到镜像(6.81MB)
  - plan 精确报出**缺哪个镜像**(`~ image_present / image: <ref>`),
    不再是笼统的"镜像清单需在执行期比对"
  - 重跑报"已是期望态,无需变更" —— 幂等
  - **反证**:同一台机删掉镜像后不带 `--closure`,报"镜像不在闭包里 ——
    断网现场装不上"并失败,不是静默跳过
- **`crater pkg` 的物料层暂不收镜像**:那需要镜像树对应的层形态,不在
  issue #1 范围内。收不了时如实跳过并报出来,不假装收了。

### D-132:系统包进闭包(issue #2)

- **补的是什么**:`package` 的 `material:` 字段在 26 类型登记表里声明了,而
  `packages_for` 从来没读过它 —— 声明与实现对不上。后果是 air-gap 下
  `package:` 这类最常见的声明直接不可用(内网连 apt 源都没有是常态)。
- **依赖解析交给发行版自己**,在**同族容器**里跑一次
  `apt-get install --download-only` / `dnf --downloadonly --resolve`。
  自己解析等于重写 apt 的求解器,而且解析结果与"在什么系统上解"强相关。
  实测:nginx 一个包带出 9 个依赖,共 1.9 MiB。
- **这给控制端加了一个依赖(docker 或 podman),目标机仍然零依赖**,且只在
  真要烤系统包时才需要。缺了就明说缺什么、怎么补,**不静默降级**成"只下一个
  包不带依赖" —— 那种闭包拿到现场才会发现装不上。
- **必须指定 `--for os_image=`**:依赖集与解它的系统强相关,拿 debian 的镜像
  解 rhel 的包名毫无意义。不给就报错并说明该加什么,与 `${substrate.arch}`
  那条错误提示同一个套路。
- **判定用名字、安装用字节,两者出自同一份声明**:`material_source` 对
  `os_package` 返回**本机家族**那一列包名(现场探家族,不在构建期定死 ——
  同一个闭包可能装到 debian 与 rhel 两种机器上)。observe 因此照常工作,
  不需要作者把包名写两遍。
- **家族探测抽成 `FAMILY_PROBE` 一处定义**:资源层与物料层各写一遍迟早会漂,
  而漂了之后的表现是"装了另一族的包名"。按**有哪个命令**判定而不是按
  `/etc/os-release` 的名字 —— 派生发行版(Kylin、UOS、Anolis)名字五花八门,
  "有没有 apt-get"是稳定事实。
- **有网时也用闭包更确定**:装的是烤闭包那一刻的版本,不是执行时上游恰好
  给出的版本。
- **验收(multipass 真虚机 w1,Ubuntu 24.04)**:
  - `apt-get install nginx` 确认失败(`Network is unreachable`)
  - `crater apply --closure` 装成,`nginx -v` 报 1.24.0,dpkg 里 2 个 nginx 包
  - 重跑"跳过执行(无变更)" —— 幂等
  - **反证**:卸掉后不带 `--closure`,报"系统包不在闭包里 —— 断网现场装不上"
    并失败,机器上确认没装

### D-133:preflight 断言从未被求值 —— 声明了却不存在的安全闸门(issue #13)

- **症状**:`preflight: assert "1 == 2"` ——一条恒假的准入断言——拦不住任何
  东西,目标机上该建的照建。而 `jsonschema.rs` 里写的是「只读准入断言:
  **任一失败则整个部署不开始**」。
- **为什么一直没被发现**:`preflight` 被解析进 IR、被 lint 检查、写进 JSON
  schema —— **唯独没有求值的地方**。整条链上只缺最后一环,所以从代码上看
  每一处单看都是对的。这类缺口只有**端到端跑一遍**才暴露得出来。
- **发现的过程值得记**:本来在做 issue #8(`iface_in`),顺手试了一条恒假断言。
  这是"顺手验一下自己以为成立的东西"的第四次收获(前三次:D-113 `on:`、
  D-115 `env:`、D-128 索引覆盖)。
- **修法**:`run_preflight` 在**碰任何机器之前**跑,三条纪律:
  - **零成本**:蓝图没声明 preflight 就一次连接都不发起;
  - **任一失败即整个停**,不是"跳过那一台" —— 准入的语义就是全体准入;
  - **plan 也跑它**:preflight 是只读的,而 plan 正是闸门;等到 apply 才发现
    不满足,那道闸门就白设了。
- **求值出错 ≠ 断言为假**:引用不存在的变量、调用没实现的探针,报的是
  "断言求值失败",不是"环境不满足"。混为一谈会让"写错的断言"看起来像
  "环境不满足",而这两者的修法完全不同。
- **验收(multipass 真虚机 w1)**:恒假 → 拦住并原样报出作者的 `msg`,
  目标机上确认没建;恒真 → 照常部署;plan 同样拦住。
- **顺带暴露 issue #14**:`PROBE_FUNCS` 里四个探针 lint 放行但求值上下文从未
  注册(`Undeclared reference to 'port_owner'`)。在 preflight 不求值的年代,
  "没实现"与"实现了"表现完全一样。**没有一并拿掉** —— rustfs 试金石正用着
  `port_owner`,那是裁定 A 的设计决定,该由人来定怎么补,不该我单方面抹掉。

### D-134:四个 CEL 探针函数落地(issue #14)

- **背景**:`port_owner` / `path_exists` / `cmd_ok` / `service_state` 在
  `PROBE_FUNCS` 白名单里,求值上下文却从未注册 —— lint 放行,运行期报
  `Undeclared reference`。在 preflight 不求值的年代(D-133 之前),
  "没实现"与"实现了"表现完全一样,所以这个洞一直看不见。
- **不能一拿了之**:rustfs 试金石(裁定 A)正用着 `port_owner`,试探性移出
  白名单会让 fixture 的 lint 立刻变红。试金石就是为拦住这种改动而存在的。
- **做法:共享句柄**。`Scope` 多一个
  `prober: Option<Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>>`。
  `cel::Context::add_function` 要求 `'static + Send + Sync`,借用引用进不去,
  `Arc` 正好满足。语义上也对:探测是「这台机器」的能力,而 `Scope` 是 per-host 的。
- **每台一条探测连接,全程复用**。初版写成"每次调用现连" —— 一份蓝图几条
  断言就是几次 SSH 握手,五台机器十几次,而它们探的是同一台机器。
- **没有 prober 时照样注册函数**,让它们返回"此上下文探不了"。留一个
  `Undeclared reference` 会看起来像"函数名写错了",而真相是"这里探不了"
  (lint / 构建期烘焙)。
- **顺带修掉一个既有 lint bug**:CEL 把**前缀**运算符也算函数(`!_` / `-_`),
  而过滤只挡了 `_` 与 `@` 开头 —— 于是 `!path_exists(x)` 被报成
  「未知探针函数 `!_()`」,**任何在 `when:` 里写 `!` 的蓝图都过不了 lint**。
  改成只保留合法标识符:运算符名里必然带占位符或符号,按形状判定即可,
  不用穷举算子表。加了回归测试。
- **验收(multipass 真虚机 w1)**:
  - 六条断言(四个探针 + 否定形式)全部在真机上求值通过
  - `port_owner(9999)` 在 `nc` 占用时读到 `"nc"`,在 `python3` 占用时读到 `"python3"`
  - **裁定 A 的语义完整成立**:端口空闲 → 通过;被"自己"占 → 通过;
    被别人占 → 失败
  - `service_state('ssh') == 'active'`、`path_exists('/etc/os-release')`、
    `cmd_ok('true')` / `!cmd_ok('false')` 均正确

### D-135:判不出 ≠ 一致(issue #7)

- **症状**:远端物料没声明 `sha256:` 且没有闭包时,`copy` 的 diff 报
  `Change::Ok` —— 与"确认一致"长得一模一样。换版本时的表现是 plan 说
  "无变更"、目标机上还是旧字节,而**一切看起来都成功了**。
- **裁定(用户拍板)**:plan 显示第三态 `?`;apply **报出来但不动**。
  不选"判不出就重推":每次 apply 都重推一遍 containerd/k8s 这类几百 MB 的
  物料,而 crater 恰恰主打这类场景。不选"lint 直接拒绝":现存蓝图要全改。
- **下游机制早就齐了**:`Change::Unknown` 本来就会在 plan 里显示 `?`、计入
  "模型化欠债"、在 apply 里返回 `Warn`。只需要 diff 如实返回它。
- **但撞上一条既有规则**:converge 对 `Unknown` 的处置是"重新观察,仍说不清
  就**照跑**"——那是为**没写 `check:` 的裸 shell** 设计的(不跑等于这一步
  永远不执行)。它让 copy 也被重推了,而且报 `changed=0`:**汇报与实际不符**。
- **判据**:「什么都观察不到」(裸 shell)→ 照跑;「观察得到、只是比不了」
  (物料无摘要)→ 不动。用**这次观察到了什么**判定,而不是类型名单 ——
  前者是事实,后者要维护一张会漏的名单。
- **lint 警告只对字面量 URL**。URL 里插值了 `${params.version}` 时,写死一个
  sha256 反而是错的:换版本必然校验失败。那种物料的答案是 `--closure`。
  对它们报警既是错的建议,又会让库里几乎每份蓝图都中 —— **一条几乎总在响的
  警告,很快就没人看了**。
- **改了一条既有测试**:`material_backed_copy_is_stable_until_upstream_moves`
  原本断言 `Change::Ok`。它的本意"上游没变就不该谎报变更"仍然成立;变的是
  另一半 —— 那同时也是**谎报一致**(期望摘要根本拿不到,凭什么说一致)。
- **验收(multipass 真虚机 w1)**:
  - 换版本 → plan 报 `? copy … 说不清(物料 X 没声明 sha256 且不在闭包里…)`
    并给出两条修法,不再是 `✓`
  - apply → `changed=0`,目标机上确实**没动**(汇报与实际一致)
  - 补上 `sha256:` → 恢复正常判定
  - **反证**:裸 shell(没 `check:`/`creates:`)仍然照跑,`/tmp` 里的标记文件
    确实被创建 —— 逃生舱没被误伤

### D-136:`facts:` 派生事实块 + `iface_in` 探针(issue #8)

- **问题**:`interface: "${iface_in(params.vip_cidr)}"` 会被 **E310**(D-117/A4)
  拒掉 —— 值位置只许名词。但"按网段找网卡"这类计算确实要做。
- **裁定(用户拍板)**:**换个位置做**。新增顶层 `facts:` 块,声明处允许完整
  CEL(与 `when:` 同一门语言、同一个封闭函数白名单),资源里写
  `${facts.vip_iface}` 保持名词。**E310 一字未改** —— 那扇门仍然关着。
  这与 `cast:` 对 selector 做过的事同构:一次声明、多处引用。
- **不需要新词汇**:D-134 刚把 CEL 探针函数做出来,派生事实直接写成一段
  CEL 即可,不必发明 `{ iface_in: ... }` 这样的"探针种类"语法。
- **`iface_in(cidr)` 的网段计算放在控制端**。目标机上只跑一句
  `ip -o -4 addr show`;在目标机上算需要 python3 或 ipcalc,而**目标零依赖
  是硬约束** —— 为一个网卡名去要求目标装 python,这笔交易不成立。
  纯函数因此可测:/29 与 /30 的边界、/32 精确匹配、`<< 32` 是溢出不是 0
  (/0 单独处理),这些不该靠真机试。
- **匹配不到返回空串,不编一个网卡名**:编出来的话 keepalived 会起来但不
  工作 —— 一切进程都在跑,只是 VIP 不漂,那是最难查的一类故障。
- **三条 lint 护栏**:作用域照常校验;**派生事实不能引用 `facts.*`**
  (那要求一个可见的求值顺序,而顺序一旦可见就会被依赖);与探测事实撞名
  出 warn(`facts.arch` 与 `substrate.arch` 是两个东西)。
- **踩到的坑值得记**:`scope` 在**两处**构造 —— 逐台 plan 一处、
  `connect_fleet` 一处。第一版只补了后者,于是 plan 期直接报
  `No such key: vip_iface`。抽成 `equip_scope()` 一处实现 ——
  同一件事在两个地方各写一遍,漏一处是迟早的(这一轮里第三次撞上这个模式:
  D-132 的 `FAMILY_PROBE`、D-135 的判据、这次)。
- **验收(multipass 真虚机 w1,现加一块 `vrrp0` 网卡做出专用 VRRP 网段)**:
  - `facts: { vip_iface: "iface_in(params.vip_cidr)" }` → 渲染出 **`vrrp0`**,
    而 `substrate.iface` 给的是 `ens3` —— **这正是这个功能存在的理由**
  - E310 仍然拦住值位置的函数调用(`${iface_in(...)}` 报 error)
  - `facts.b: "facts.a"` 报 error
  - `facts.arch` 与 `substrate.arch` 撞名报 warn

### D-137:包签名 spike —— 现在只"不关门",不做实现(issue #6)

设计见 `docs/research/pkg-signing.md`。

- **洞是真的**:`crater install` 从 registry 拉包然后在生产机上执行。谁能往那个
  registry 写,谁就能让你的机器执行任意东西。**但威胁模型只有一条**:registry
  被投毒。签名证明"是他发的",不证明"是好的";传输中被改由 TLS + 内容寻址管住。
- **实测推翻了一条调研结论**:D-123 §3.4 写的"Docker Hub 无 referrers API"是
  错的。实测:推带 `subject` 的 manifest → 201,查 referrers → **200 + 1 条**,
  注解原样带回,`?artifactType=` 过滤也生效。tag schema 同样可用(201,自定义
  层类型保留)。于是存放方案是**主用 referrers、回退 tag schema,两条都验过**。
- **自己犯的错值得记**:第一次测 tag schema 报 `MANIFEST_BLOB_UNKNOWN`,差点
  写成"Docker Hub 不支持"。真因是我用了单步 blob 上传,Docker Hub 返回 202
  开了会话却没提交;改成两步就是 201。**registry 说"不行"时,先怀疑自己的请求。**
- **设计取向**:照 cosign 的**布局**存(格式兼容白拿:别人用 cosign 也能验),
  但**不 exec cosign 二进制**(与静态单二进制冲突,而我们要的是格式不是进程);
  不做 keyless/Fulcio(要联公共服务,与 air-gap 冲突);不做 TUF/证书链。
  **签的是 manifest digest 不是 tag** —— tag 可变,签可变的东西等于没签。
- **现在不实现**:价值与"有多少人拉你的包"成正比,而现在是零;做早了要背着
  一套密钥管理走很久。**但两件"不关门"的事现在做完了**(实现时零成本、补救
  时很贵):
  1. 预留 `org.crater.signature.*` 注解前缀与 `application/vnd.crater.signature.*`
     两个 mediaType;
  2. `pkg push` / `pkg build` 报出 **manifest digest**(取自 `put_manifest` 的
     返回值,不重新序列化 —— 重算出来的字节未必逐字相同,那会印出一个**错的**
     digest)。实测与 zot 报的 `Docker-Content-Digest` 逐字一致。
- **触发条件**:有第二方开始拉包 / 放到公开 registry / 有合规要求。任一成立
  就启动实现,估计两天,**最大的不确定性(存放路径)已经被实测消掉**。

### D-138:`--merge` 读不到历史时不许当成"没有历史"(issue #4)

- **验 `pkg index --merge` 时挖出的真缺陷**:**截断恰好落在 `entries:` 之后的
  索引是合法 YAML** —— `entries` 成了 null,serde 收成空 map。于是 merge 从零
  开始、退出 0、把整份版本历史换成这一版,终端上**与一次成功的增量发布长得
  一模一样**,要等到有人装旧版本时才发现。真机上复现过。
- **挡它的判据是"空索引根本不该存在"**:`n == 0` 那道闸决定了 `pkg index`
  自己永远写不出零个包的索引,所以磁盘上出现一份,只可能是写坏了或放错了
  文件。这条**只管 merge 一处,不下沉进 `load_index_file`** —— 别人发布的空
  索引拉回来缓存着是合法的,`repo list` 不该因此报未同步。
- **索引落盘改成同目录临时文件 + rename**。`fs::write` 先截断再写,中途被打断
  留下的正是上面那种半份索引,而下一次 `--merge` 读的就是它。临时文件必须是
  **兄弟**:跨文件系统的 rename 会直接失败,而 CI 容器里天天走这条路径。
- **`--merge` 指着不存在的文件时吭一声**,但仍放行:首次发布必须能跑,否则
  每条流水线都要为"第一次"多写一个分支。后果与索引损坏相同,区别只是它更
  常见(CI 里 `-o` 写错、或取上一版索引那步没成)。
- **输出报索引里的总版本数**,不只是本次收到的 n。之前两种情况在终端上没有
  区别 —— 而"历史还在不在"恰恰只能从总数看出来。
- **这条是派 agent 做的**(见 D-139),它自己找出了这个缺陷:issue 只要求
  "验一下 `--merge`",而验的过程逼出了真问题。

### D-139:UI 写入自动 git commit(issue #10)

- **补的是什么**:在 UI 上改一台主机的口令,以前不留任何痕迹 —— 谁、什么时候、
  把哪台机器的什么改成了什么,全查不到。"期望态可 git"此前只是嘴上说的。
- **crater 本身仍不依赖 git**(D-122 不能破):git 不在 PATH、或工作区不是 git
  仓库 → 不记,启动横幅说一句;记录过程中 git 失败 → **写入已经成功**,请求
  照常返回,只留一条 warn。
- **只提交我们自己写的那几个路径**(`git add -- <paths>` + `git commit --only`),
  不是 `git commit -a`。工作区里常有人手工改了一半的文件、甚至已经 `git add`
  进暂存区的改动 —— UI 的自动提交把它们一起卷走,比"不记录"严重得多。
- **作者钉成 `crater`**,不跟当前 shell 用户走:起 `crater ui` 的那个 unix 账号
  既不是改动的发起人,把它写进 `git log` 是一条**看起来可信的假记录**。
- **`--no-verify`**:这是 HTTP handler 里的无人值守机器提交,一个交互式或耗时
  的 pre-commit 钩子会把请求线程挂住 —— "UI 卡住"比"少一条记录"严重。这条
  绕过了用户可能配置的钩子,注释里写明了原因与如何关掉。
- **进程内 `Mutex` 串行化**:git 索引是仓库级单锁,两个 handler 同时提交会撞
  `index.lock`,症状是随机一次写入不留记录 —— 正是最难查的那类静默失效。
- **落点收敛到一处**(`ui_git.rs` + `ui_edit::write_file/move_file`),而不是每个
  handler 各调一次 —— 本仓库这一轮已三次栽在"同一件事在两处各写一遍,漏一处"
  上(D-132/D-135/D-136)。
- **验收(agent 真跑 + 我独立复验反证)**:git 工作区 → `ui: 保存 …`,作者
  `crater <crater@localhost>`;非 git 工作区与 git 不在 PATH → UI 照常、横幅
  说明;**反证**:预置"已 add 进暂存区""未跟踪""手工改了一半"三种改动,
  UI 提交后 `git status --porcelain` 一个都没被带走,`.bak` 也没进库。

### D-140:抽出 `BlobSource`(issue #9,纯重构)

- **做了什么**:物料字节的来源抽成 `BlobSource` 四方法接口,`TarClosure` /
  `OciSource` / `TargetFetches` 三个实现各自成文件。`open_closure()` 里不再有
  判断来源类型的 `if`。
- **四个方法各有调用方**(这是不加第五个的判据):`manifest()` + `fetch()` 给
  `blob_map()`;`contains()` 是 `blob_map()` 那道闸("只有真取得到的才进表");
  `origin()` 给报告行。**没有预先加 `list` / `delete` / `gc`。**
- **`BlobSink` 没抽,是对的**:D-119 写的签名
  `put(&self, source, bytes: &Path, sha256)` 与两个写侧都不合 —— 它们把字节
  拿在 `Vec<u8>` 里,还要用返回的 `BlobEntry.size` 印进度行;`finish(manifest)`
  对 `pkg` 更不成立(它根本没有 `bundle::Manifest`)。硬凑要么改签名要么把每份
  烤好的 blob 落一次临时文件 —— **那就不是纯重构了**。当前只有一个可实现的
  sink、没有第二个调用方,所以不抽。
- **`material_ctx.rs` 没动**,也是对的:系统包要按 `os-pkg://` 前缀**枚举**
  BlobMap,那正是 issue 明令不加的 `list` 形状的方法。于是 `BlobSource` 停在
  "打开期"这一层,`MaterialCtx` 继续持有 `BlobMap`。
- **我否掉了 agent 的一处保真手段**:它用 `origin.ends_with(')')` 还原重构前
  两条报告行差的那个空格。那是拿"以右括号结尾"当代理判据,一个叫
  `k8s(1).tar` 的闭包会**静默少一个空格**。为保住一处纯装饰性的不一致背一颗
  地雷不划算 —— 改成两行统一,接受 oci 那行多一个空格。**"行为零变化"说的是
  行为,不是排版。**
- **验收**:15 个测试套 605 个测试全绿,与重构前逐项相同;两条报错文本用
  **新旧两个二进制对拍**,逐字相同(不存在的 tar / 不存在的 oci 引用)。
- **顺带一条环境事实**:`CLAUDE.md` 要求先用 code-review-graph / GitNexus 的
  MCP 工具再 Grep/Read,但**这两个 MCP server 在 agent 会话里没有暴露**。
  agent 如实报告了这一点并退回手工核对爆炸半径。这条约定要么让它在 agent
  会话里也可用,要么在文档里说明何时不适用。

### D-141:`install` 有升级路径了 —— 版本化目录 + 本地改动闸门(issue #5)

- **补的是什么**:换版本此前撞上 `.crater-pkg` 印记检查(D-128)并报错。那个
  拦截是对的 —— 它挡住了"静默装错版本"这个真事故 —— **但它只是拦住,没有
  给出路**,人得手工删目录、重装、再改 app 文件。
- **目录布局选 `<name>-<version>/`,version 取引用的 tag**。三条理由,第一条
  是决定性的:**契约里的 `version:` 不能用** —— `library/yq` 的 `version: "1"`
  在 4.44.3 与 4.40.5 两个包里是同一个值,拿它做目录名等于把 D-128 那次事故
  原样复制。另两条:升级在 git 里成为 app 文件的一行 diff(`<name>/` 布局下
  升级在版本库里毫无痕迹,而 `ui_app.rs` 正是按那一行找蓝图 —— 不改它就是
  "机器装了新版、UI 一点 apply 悄悄退回旧版");旧版本原样留着,回退不用连网。
- **`tag_of` 认得端口不是 tag**:`zot:5031/lib/yq` 里的 `:5031` 是端口,只认
  最后一段路径里的冒号。digest 引用取短 digest 顶上。
- **`--force` 与 `--yes` 是两道不同的闸**:上一版目录里有本地改动时,升级停下
  并报出来;`--yes` **跨不过**它 —— 那句话的意思是"计划我看过了",不是"我的
  改动随便丢"。旧目录一个字节都不删,只是那些改动不会跟到新版本。
- **对账是三态**(`Drift::{Same, Changed, Unknown}`),与 D-135 同一套诚实:
  包不在本地 store 时说"判不出",不拿"没比出差别"冒充"没有差别"。比的口径
  复用打包时那份 `collect()` —— 凭据、app 文件、闭包本来就不在包里,把它们
  当"改动"报出来只会天天误报,报多了人就不看了。
- **D-135 的前提照旧成立**:不给 `--full`/`--closure` 且物料无 `sha256:` 时,
  升级**判不出**,如实报出并给两条出路,计划里是 `?`,`changed=0`,目标机上
  二进制不动。**没有做出"看起来升了、实际没升"的路径。**
- **验收(真机 zot:5031 + multipass w1)**:4.44.3 → 4.40.5 连续两条 `--yes`
  无人工干预;目标机 `yq --version` 真换版本;plan 报 `~ 将修改`;app 文件只
  动 `blueprint:` 一行;**反证**——预置手工改动后升级退出 1 并报出改了什么,
  机器与 app 文件都没动,`--force` 才继续;离线升级(停掉 registry)走通;
  老 `<name>/` 布局同版原地用、换版本时被认出是上一版。

### D-142:并发 agent 共用 `CARGO_TARGET_DIR` 会让你测到别人的二进制

- **怎么发现的**:派 agent 收尾时,为避开"新 worktree 冷编译 789 个依赖超
  600 秒看门狗",我让它们共用主仓的 `target/`。#5 那个 agent 报告:另一个
  agent 的构建覆盖了 `target/debug/crater`,而它自己再 `cargo build` 时 cargo
  报 `Finished 0.16s` **不重新链接** —— 它头一轮真机测试跑的是**别人编出来的
  二进制**,看到的是旧行为。
- **这是我派活时埋的坑,不是 agent 的问题。** 两害相权:各自冷编译会被看门狗
  杀掉,共用则可能测错二进制 —— 后者更隐蔽,因为它**看起来是成功的**。
- **规程**:并发 agent 若共用 target 目录,每次真机测试前必须
  `touch 源文件 → build → 立刻把二进制快照到 worktree 内的私有路径`,
  并在跑之前**断言快照确实是自己那一版**(grep 新加的 flag 或改过的文案)。
  本次合并前我按这条复验了 #5(断言 `--force` 在 `--help` 里)。
- 更好的解法是给每个 agent 独立 target 目录 + 预热(例如从主仓 `cp -al` 硬
  链接一份),但那要先验证 cargo 对硬链接缓存的行为,留待需要时再做。

### D-143:多架构包 `save` 出来是个 6 KB 的空壳 —— 而每一步都退出 0(issue #3)

- **症状**:U 盘离线搬运这条路**端到端是断的,且每一步都成功**。
  `crater save` 一个多架构包(yq,amd64+arm64,18.8 M 物料)产出 **6144 字节**、
  只含顶层 index 一个 blob,日志写着 `saved`、退出 0;`load` 收下(退出 0、
  报 "loaded");`pkg ls` 说"本地还没有蓝图包";`pkg index --store` 说"一个包
  都没收进来";`install` 死在断网机上。
- **根因是一个假设写了四遍**:「制品 = `config` + `layers`」。而**多架构包的
  顶层是 image index:它既没有 `config` 也没有 `layers`,只有 `manifests`**。
  于是每一处照这个形状写的遍历都得到**空集合而不是错误** ——
  `export_oci_archive`、`import_entry`、`has_all_layers`(对一个空 store 回答
  `true`)、`artifact_sizes`。`gc` 是唯一走对了 `manifests` 的地方,这也是那
  18.8 M 字节一直安稳躺在 store 里的原因:**从来没有东西把它们带出去过**。
- **这是 D-127 我自己引入的**:多架构 index 只补了 push/pull 两侧,没有清扫
  消费方。比"两处各写一遍漏一处"更糟 —— 这里是**改了数据形状却没扫消费者**。
- **`crater pkg save` / `pkg load` 当时根本不存在**。issue 的验收命令抄自设计
  文档 §6 的命令面,我写 issue 时当它们已经实现了。设计写过 ≠ 代码有。
- **另外三个洞**:`pull_thin`/`pull` 即便闭包已在本地也仍去 registry(断网机
  上表现为一次长超时);`import` 把引用 tag 到 index,而在线 `pull` tag 到平台
  子清单 —— 同一个 store,在线能装、离线报 `manifest 没有 config`。
- **`Entry.urls` 从"定义了从不读"变成真的读**:`pkg index` 扫输出目录里的
  `*.pkg.tar`,**只在归档自报的引用与条目相符时**才记(绝不按文件名 —— 同名
  不同版会指向错的包,而你要装到断网机上之后才发现)。`repo::resolve` 在字节
  不在本地时点名那个文件,或者说"U 盘只拷了索引、漏了包?"。这把 U 盘场景最
  常见的失误从"install 够不着 registry"(会让人去查防火墙和证书)变成了真正
  的诊断。
- **验收(agent 真跑 + 我独立对拍)**:同一个包、同一个 store,**修复前 6144
  字节 2 个 blob,修复后 19.7 MB 8 个 blob**;断网容器(`--network none`)里
  `load` → `repo add file://` → `search` → 到 plan 闸门;registry `docker rm -f`
  之后 `install` 把 yq 4.44.3 真装到 multipass w1 上,并打印"闭包已在本地 ——
  不联系 registry";**反证**:删掉 tar 只留索引,**0 秒**报"字节不在本地",
  不是长超时。
- **agent 如实报了两条它没验成的**:`--network none` 的容器到不了 w1(它压根
  没有网),所以"从未联网的机器"与"真装到真机"是两次跑分别证的;以及无
  `ca-certificates` 的机器上 `reqwest::Client::new()` 会 **panic** 而不是报错
  —— 真实的 air-gap 隐患,不在本 issue 范围内,值得单开。

### D-144:clippy 清零并进闸门(issue #11)

- **真实条数是 45 不是 51**:issue 里那个数字是几天前的,期间合并了七个 issue。
  agent 以自己跑出来的为准并给出分布(17 条 `doc_lazy_continuation` 居首)。
- **零新增 `#[allow]`** —— 全部在源头改掉。糊一条就少一条真信号。
- **两个顺带发现**:一处文档注释**跨文件漂了** —— `known_systemd_units` 从
  `apply.rs` 挪到 `main.rs` 时注释留在了原地,于是 `apply.rs` 里两行孤儿挂在
  不相干的 `print_plan` 上,`main.rs` 只剩半句 `/// data, never hardcoded.`;
  另有三处 **rustdoc 输出本来就是坏的**(以 `+` 开头的行被当成 markdown 列表,
  后续行被吞进去),**用重排而不是缩进修** —— 缩进能让 lint 闭嘴,但渲染还是错的。
- **`too_many_arguments` 当真信号处理**:`run_linear` / `connect_fleet` /
  `run_preflight` 共用一个 `RunCtx`,五个调用点各自抄的那对参数收进函数里。
- **agent 拒绝给一个假的零**:树里还有 **12 处既有的
  `#[allow(clippy::too_many_arguments)]`,一处都没有说明理由**。它们早于本
  issue,拆开是另一场重构,所以留着并如实报出。**"零"指的是未被抑制的 lint,
  不是那 12 个函数没问题。**
- **反证是它自己做的,我又独立复现了一遍**:追加一个 `&Vec<String>` 参数的
  金丝雀函数,**旧闸门(`cargo build -D warnings`)退出 0,新闸门
  (`clippy -D warnings`)退出 101**。这证明了 issue 的前提 —— clippy 那一半
  确实是旧闸门看不见的。
- **`components: clippy` 显式传**:runner 预装的 stable **恰好**带 clippy,
  但那是运行环境的巧合而不是约定。这句话写进了 action 的注释里。
- 验收:16 个测试套 626 个测试全绿;dev 与 release 两个 profile 下 clippy 均
  退出 0(唯一剩下的一条是依赖 `proc-macro-error2` 的 future-incompat 提示,
  不受 `-D warnings` 管)。

### D-145:没有 CA 证书时 panic 而不是报错(issue #15)

- **agent 的报告方向对、归因不准,复验时纠正了**:它说的是
  `reqwest::Client::new()` panic。逐条查下来:`source.rs::client()` 用的是
  `builder().build()?`,**不 panic**;真正的链是
  `oci_client::Client::new(cfg)` → `try_from` 失败 → `unwrap_or_else` 退回
  `Default` → `reqwest::Client::new()` → **panic**。`ai.rs` 那处则是直接
  `Client::new()`(不在部署路径上,但同病)。
- **实测比 issue 写的更容易撞上**:`ubuntu:24.04` 基础镜像**本身就不带
  `ca-certificates`**,所以在它里面跑 `crater pkg inspect` 直接是一句
  Rust 堆栈 + 退出码 101。内网机器裁掉这个包更是常态 —— 正是 crater 的主场景。
- **修法**:`registry_client()` 改为返回 `Result`,用 `Client::try_from`。
  `oci_client::Client::new` 的"失败就退回默认客户端"是一种**静默降级**,而
  那个默认客户端的构造本身会 panic —— 两层加起来,一个可诊断的配置问题
  变成了不可诊断的崩溃。
- **真因在 source 链里**:reqwest 顶层 `Display` 只说 `builder error`,
  "No CA certificates were loaded from the system" 在下一层。初版只报顶层,
  等于什么都没说 —— 人会去查网络和代理。改成走完整条 source 链再拼提示。
- **提示要给出路,不只给诊断**:装 `ca-certificates`,**或者本来就该走离线**
  (`pkg pull --full` 备好包 → `pkg load` → `--closure` 部署)。后半句才是
  air-gap 场景下真正该做的事。
- **验收(`ubuntu:24.04` 容器,两个方向)**:无证书 → 报错并给出两条出路,
  不再是 Rust 堆栈;**反证** —— `apt-get install ca-certificates` 之后同一条
  命令正常读到契约。回归测试钉的是**类型**(改回 `Client::new()` 连编译都
  过不去),因为在有证书的开发机上跑不出那个失败。

### D-146:判不出 ≠ 没达成 —— D-135 的另一半

- **症状**:一次**成功**的部署会紧跟一句「注意:收敛后仍有 1 项未达期望态」,
  而目标机上那个文件明明已经正确放好。真机复现:`changed=1` 之后立刻这句,
  然后 `yq --version` 报的是对的版本。
- **根因**:收敛后复观察时,`changing()` 的判据是 `!is_noop()`,而
  `Change::Unknown` 不是 noop。于是**物料没声明 `sha256:` 又没有闭包**时
  (D-135 那条路),复观察照样说不清,被算进了"未达期望态"。
- **D-135 说清的是"判不出 ≠ 一致",这里漏的是它的另一半:判不出 ≠ 没达成。**
  同一个区分在反方向漏掉,而两边的代价相反:前者把失败说成成功,后者把成功
  说成失败。第二种不会造成事故,但会**训练人忽略这句警告** —— 而它本来是要
  在真出事时被看见的。
- **修法**:`Unknown` 从 `stuck` 里剔除,单独报一句
  「N 项收敛后仍判不出 —— 已按计划执行,只是无从复核」。既不混进失败,也不
  只字不提 —— 后者会让"其实没验过"变成隐形的。
- **发现的过程**:issue #3 的 agent 在报告末尾顺手提了一句"apply 打印这句话
  但文件是对的",归在"其它发现,未修"。它没有当成自己的任务范围 —— 这是
  对的,而把它记下来同样是对的。
- **验收(真机 w1,两个方向)**:判不出的物料 → 报"判不出",不报"未达期望
  态",目标机上二进制正确;**反证** —— 真的失败(往 `/proc` 里建目录)仍然
  照常报失败并以非零退出。

### D-148:D-126 的反证补上了 —— 去掉覆盖,CI 确实变红(issue #18)

- **欠的是什么**:D-126 把构建前置抽成 composite action,正向验过(CI 绿)。
  但**反证一直没验**:去掉那条 `RUSTFLAGS` 覆盖之后,CI 是不是真会红?
  不验它,就说不清"绿"是因为覆盖在起作用,还是**别处碰巧也把 mold 关掉了**。
- **结果**:临时分支 `probe/no-rustflags`,run `33615376098`,**14 秒变红**,
  停在 composite action 自己那步自检上:
  > `RUSTFLAGS 没设上 —— .cargo/config.toml 的 -fuse-ld=mold 会生效,而
  > runner 上没有 mold,构建必然以链接失败告终`
  后续步骤全部 skipped。属于两种预期红里的第一种 —— **自检有效**。
- **一条额外证据,正好回答了"是不是别处碰巧"**:自检步的 env dump 完整,
  只有 `CARGO_HOME` / `CARGO_INCREMENTAL` / `CARGO_TERM_COLOR` /
  `CACHE_ON_FAILURE` 四条,**`RUSTFLAGS` 一个字都没有**。去掉覆盖之后
  runner 上再没有第二处设它 —— 绿是那条覆盖挣来的,不是巧合。
- **第二种红(走到 cargo 那步以 `cannot find -fuse-ld=mold` 失败)没验**,
  因为自检先拦下了 —— 这是保留自检的必然结果,不是遗漏。要看那句原文得再
  推一条把自检也去掉的分支,代价是一次真冷编译。**不值**:env dump 已经
  排除了第二来源,而链接失败是 D-126 当初实际撞到过的既有事实。
- 临时分支已删,`main` 未被动过。**做反证不该在主干上留下痕迹。**

### D-147:rustfmt 一次跑齐并进闸门(issue #16)

- **为什么是"悬着比做掉更糟"**:这个仓库从没跑过 rustfmt,积了 82 个文件的
  漂移。它逼着每个人(和每个 agent)在每次改动前重新判断一次"要不要顺手
  格式化",而任何一次"顺手"都会把真正的改动埋进几百行排版 diff 里。
  D-144 那个 agent 就是因此**刻意绕开**它 —— 判断对,但绕开不等于解决。
- **拆成三个提交**:纯格式化 / `.git-blame-ignore-revs` / 闸门。第一个的 sha
  进忽略文件,`git blame --ignore-revs-file` 会跳过它 —— 否则每一行的"最后
  改动者"都变成一次全仓格式化,而那句话对任何人都没有用。
- **判据用行为,不用字节**。我一开始想证明"只有排版改动",做法是抹掉所有
  空白后逐文件比对 —— **失败了,而且失败是对的**:rustfmt 会做几类语义中性的
  token 改动(给发散块补尾分号、补尾逗号、给闭包体加花括号)。按字节相等去
  判,会得出"它改了语义"的错误结论。真正的判据是:**627 个测试逐个不差、
  16 套全绿、clippy 与 build 均零错误**。
- **rustfmt 在本仓库不是幂等的**:`crater-ir/src/parse.rs` 里一处带长中文
  字符串的 match 分支,**要跑两遍 `cargo fmt` 才收敛** —— 第一遍之后
  `--check` 仍报差异。所以"跑一次 fmt"这个说法本身不严谨,**判据只能是
  `--check` 退出 0**。
- **闸门放在最前**:fmt 最快,而且排版一乱,后面 clippy 与 build 的输出就难读。
  用 `--check`(只报不改)—— 闸门不该偷偷改你的代码然后说自己通过了。
- **CI 显式装 rustfmt 组件**(`components: clippy,rustfmt`),同 D-126:
  runner 恰好带什么是运行环境的巧合,不是约定。
- **反证**:追加一行 `let x   =  1;` 后 `--check` 退出 1,还原后退出 0。

### D-149:`too_many_arguments` 审计 —— 12 处 allow 降到 5 处,且每处都有理由(issue #17)

- **裁定(用户拍板)**:参数过多本身是设计信号,该拆。
- **最大的发现:12 处里有 5 处是死的。** 把它们全摘掉之后 clippy 只报 7 处
  —— `deployments.rs::task_list` **只有 2 个参数**还挂着 allow。这类东西是
  **一张不会过期的免死金牌**:函数继续长参数没有任何机制再提醒,而下一个人
  看到它会合理推断"这里评估过、是可接受的",事实是没人评估过。
- **两处抽了结构体**:
  - `pkg::install` 的 `yes/full/force` → `InstallOpts`。三个 `bool` 挨着传,
    调用点上是三个裸 `true`/`false`,谁是谁全靠位置记 —— 而记错的后果分别是
    "没看计划就执行"和"手工改动被丢掉"。
  - `lint_scope_of` 的 `args/when/on/has_each` → `Scoped`。两个调用点各自把
    同一条声明拆开传了一遍。
- **剩下 5 处留 allow,但每一处都写清理由并指向 #20** —— 这个 issue 要的是
  "每一处都有依据",不是"零个 allow"。理由是实的:它们在 D-106 降级的旧
  task 管线里,参数确实构成一个整体、而那个整体**已经有名字了**(`RunOpts`,
  全仓 7 处各自组装一遍);不现在抽是因为这五个函数**零直接测试覆盖**,
  穿 `RunOpts` 是一次没有安全网的重构。
- **反证时被 clippy 缓存骗了一次**:加一个 8 参数的金丝雀函数,第一次跑报
  **0 命中**,险些得出"反证失败"。`touch` 源文件强制重跑才拿到真结果。
  这与 D-142(共用 target 目录测到别人的二进制)是同一类:**构建缓存会让
  验证悄悄失效**,而失效的表现是"看起来通过了"。

### D-150:清残骸(issue #19)—— 顺带捡回两样"删掉就没了"的东西

- **`apply.rs` 的孤儿 banner 已删。** 判断依据三条独立证据:`grep "ai::"` 在
  `apply.rs` 里零命中;`git log -S` 追到它的来历(`5bbe6e4` 拆 main.rs 时,
  banner 底下原本跟着 `known_systemd_units` 与 `ai_generate` 两个函数,
  banner 跟着前者走了,而 D-144 又把前者挪回 main.rs);main.rs 里
  `ai_generate` 上方现在没有 banner,所以也不存在"该挪回去"的空位。
- **顺带发现同类的第二、三例**(→ #21):`apply.rs:1152` 有六行文档描述的是
  `target.rs::target_hosts()`,`target.rs:117` 有一行描述的是 `hosts()` ——
  两个被描述的函数**都没有文档**,而两段文档都粘在了下一个函数头上。
  **文档注释比代码更容易在搬家时掉队,因为编译器不检查它**;掉队之后它比
  没有更糟 —— 读的人会拿它当真。
- **`~/crater` 工作区清点出两样值得进库的东西**,都是"删掉就没了":
  - 工作区的 `yq.blueprint.yaml` **比库里新**,多出的全是解释性注释:
    `stage: build` 为什么在烤闭包时就定死、`${substrate.arch}` 为什么要
    `--for` 才渲染得出来。这两段是踩过坑之后写下的。**已捡回库里。**
  - `library/selftest` 的 README 要求"≥3 台 controlplane + ≥2 台 worker",
    却**没有配套 inventory 示例**(`_template`/`k8s`/`rustfs`/`middleware`
    都有)。**已补上**,并写清为什么少于这个数判据就区分不出来。口令用
    `${env:LAB_PASS}` 而非照抄真机配置 —— 这个文件要进版本库,而 git 历史
    删不掉。
- **另外查出一处过期副本**:`~/crater/rustfs/` 的蓝图丢了 `secret: true`
  (D-125 补上的那个标志)。库里那份是权威且更新。
- **`.crater-trash/` 不是垃圾**,是 UI 的回收站(`ui_edit.rs` 的设计安全网)。
- **agent 抓到了自己的一次测量错误**:第一遍数测试数时管道带了 `| tail -60`,
  只留了尾部,数出"5 套 40 个"的假数;它重跑并捕获完整输出才拿到 16/627。
  **这是第三次栽在"验证被工具悄悄截断/缓存"上**(D-142 共用 target、
  D-149 clippy 缓存、这次管道截断)。

### D-151:整块删掉旧 task 管线(issue #22,约 8500 行)

- **为什么现在删**:D-106 把它降级为作者前端,但它没走,而是留在树里**持续
  收租** —— #17 那五处 `allow` 全在它里面、#19 与 #21 的孤儿注释是它搬家留下
  的、每次 clippy/fmt/build 都要处理它的 8000 行。而它**零测试覆盖**,所以
  任何"顺手改一下"都没有安全网。
- **做法:先删文件,让编译器把断点报出来。** 29 个断点一个都跑不掉,比手工
  追依赖可靠。
- **顺手查出两件事**:`ai.rs` 生成的是**旧 task YAML**,随管线走;`agent.rs`
  用的是整套旧任务执行机器,而新管线走 `connect_executor` 直连 SSH、**根本
  不经过它** —— 它看起来像共享基础设施,其实不是。
- **差点砍过头一处**:`gc` 里目标机侧的暂存 blob 清理(D-095)**与管线无关**
  —— 新管线推物料同样落在 `/var/lib/crater/blobs`。我把整段 gc 尾巴一起删了,
  靠"未用参数"的警告才发现。删掉它会让"磁盘被 crater 吃满"变成一个没有出口
  的问题。**已恢复。**
- **旧命令给出路,不只给拒绝**:只说"不支持"会让人以为是自己写错了、然后去
  调参数,而真相是这个形状的输入整个不存在了。
- **两条测试改了断言方向**,因为它们钉的正是被删掉的行为:「旧 task 走旧管线」
  → 「旧 task 得到迁移提示」;「库里的旧 task 被标为待迁移」→ **「库里不该再有
  旧 task」**。后一条从记账变成了**护栏**:谁再往 `library/` 放一个 `actions:`
  文件,它就红。
- **验收**:四道闸门全绿;16 套 581 个测试(基线 627,少的 46 个**逐一核对
  全部来自被删文件**,其余 12 套逐个不变);真机三条 —— `plan`、`install` 装成
  yq 4.44.3、**断网 `apply --closure`** 装成。
- **统计口径又骗了我两次**:`cargo test` 遇到失败会**中止后续套**,于是
  `grep "test result: ok"` 数出"4 套 145 个"和"2 套 130 个"两个假数。真相是有
  测试失败了 —— 而那两次失败恰恰是有价值的信号。**这是本轮第四次栽在"验证被
  工具截断或缓存"上**(D-142 共用 target、D-149 clippy 缓存、D-150 管道 tail、
  这次 cargo test 中止)。

### D-152:孤儿文档归位,`cargo doc` 清零(issue #21)

- **#21 的一半已被 D-151 解决**:`apply.rs` 那六行随文件一起删了。删掉一整条
  管线,顺带清掉的债比预期多。
- **`target.rs` 那处已归位**:`Resolve to a concrete host list: inventory >
  --host > localhost` 原先粘在 `declared_groups` 头上,搬到它真正描述的
  `hosts()` 上;三层细则补给了 `target_hosts()`(它此前一行文档都没有)。
  **搬之前核对过规则仍然成立**,不是无脑搬位置。
- **扫出了第四处**,而且是靠换判据换出来的:第一版启发式(找"文档中间突然
  换主题")报了 **97 处**,绝大多数是正常分段 —— 噪音太大等于没扫。换成
  「**文档点名了一个本文件里不存在的函数**」之后只剩 9 处,逐个核实,其中
  `blueprint.rs` 的 `open_closure` 头上那两行「顺序即 inventory 声明序,
  `first()` 每次选中同一台」根本不是在讲闭包 —— 它属于 `build_fleet`
  (那个函数同样一行文档都没有)。已搬。
- **顺带发现两个 crate 的头部说明整段过期**:`crater-core/src/lib.rs` 的模块
  清单里还列着 `component` / `engine` / `ai`(D-151 已删),`crater-cli/src/main.rs`
  的命令面还在写 `crater task` / `crater ai` / `crater agent`。**这类"总览
  文档"最容易烂掉**:改代码时没人会想起它,而它恰恰是新人第一眼看的东西。
  两段都按现状重写了。
- **`cargo doc --no-deps` 从 21 条警告降到 0**(唯一剩的是依赖
  `proc-macro-error2` 的 future-incompat 提示)。
- 验收:四道闸门全绿,16 套 581 个测试不变。

### D-153:一键安装 + `crater update`,以及 Windows 的诚实答案

**动机**:装 crater 的门槛不该高于它本身的复杂度。它是一个静态二进制,装它
应该就是一行命令;升级也不该逼人回去翻安装文档。

**做了三件**:

1. **`scripts/install.sh`** —— `curl … | sh` 一行装好。
2. **`crater update`** —— 自更新,同一套规矩。
3. **`docs/install.md`** —— 把两者的行为、边界与"为什么这样设计"写清楚。

**四条刻意的设计,两边一致:**

- **摘要必须核对,没有关闭开关。** 这两个入口做的事都是"从网上取字节然后
  执行它"。`SHA256SUMS` 发版流程本来就产出,校验成本是零 —— 给一个
  `--no-verify` 只会让人在赶时间的时候用它。摘要不符 → 退出码 1,**什么都
  不装**。
- **默认不要 root。** 安装脚本默认写 `~/.local/bin`。一个用管道执行的脚本
  顺手要 sudo,是在训练用户对"管道 + sudo"脱敏。
- **原子替换 + 换完自检。** 先写临时文件再 `rename`;换完立刻跑一次
  `--version`。不自检的话,"换成了一个跑不起来的二进制"要等到下次用才发现,
  而那时现场已经没了。
- **装在哪就换哪。** `update` 换的是 `current_exe()`,不猜 `PATH`。猜错的
  表现是"更新成功了但版本没变" —— 一种很难查的成功。

**验的时候实际做了反证**:改一个字节的 tarball → 拒绝、退出码 1、目标目录
干净;`--version v9.9.9` → 404 且信息明确。只跑正例的话,这两条路径写没写
过都看不出来。

**Windows:探针说不行,所以不写 `install.ps1`。**

用户要的是 `irm … | iex` 那种一行。我先在 `windows-latest` 上真编了一次
(run 33630572863),失败在**我们自己的代码**上:`pre_exec` + `setsid()`、
`kill(-pid)`、`PermissionsExt::mode()`、`Command::new("sh")`。

编译错只有 4 处,看着像"加几个 `#[cfg]` 的事" —— 但**能编 ≠ 能用**:全仓
还有 88 处 `sh -c` / `/tmp` / `/etc` 假设。把 4 处堵上能得到一个编得过、
跑起来立刻炸的 Windows 二进制,再配一个 `install.ps1`,就是给人装一个坏
东西。所以:**文档写明不支持,给 WSL 这条真能用的路**,Windows 移植另开
issue(#23)。

这一条和 #3 的教训是同一条:**设计写过 ≠ 代码有**。区别只在于,这次是在
写安装器之前先去问了机器,而不是写完再发现。

**一处已知的债,写在文档里而不是藏着**:安装脚本(shell)与 `update.rs`
(Rust)实现同一套规矩。**不能合并** —— 安装脚本必须在没有 crater 的机器上
跑。所以规矩写了两遍、会漂,`docs/install.md` 末尾给维护者留了明确提示。

**同一种错我自己又犯了一次(第三次)**:D-153 的新文档里写"`update` 子命令
是 v0.2.0 才有的" —— 干净机上一验就穿帮:v0.2.0 敲它报
`unrecognized subcommand`,因为 `update` 是 v0.2.0 **发布之后**才写的。
**版本号写在文档里就会漂。** 修法不是把 v0.2.0 换成 v0.2.1(那只是把同一个
坑往后挪一格),而是让 `install.sh` **去问装好的二进制有没有这个命令**
(`crater update --help`),文档则只说"下一个版本起"。问一下不会漂。

**顺带修了一个真 bug**:README 的安装命令指向 `crater-linux-$(uname -m)`,
这个产物**根本不存在**(实际是 `crater-<target>.tar.gz`)。实测 **404**。
一条谁照着敲都会失败的命令,在 README 里挂了不知道多久 —— 文档里的命令
没人跑,就等于没写。

### D-154:CLI 帮助按 kubectl 体例重写,顺带拆出 7 个"会骗人"的标志

**起因**是一个很小的问题:用户问"现在跑一个任务的主命令是什么"。翻 `--help`
才发现它整个是旧的 —— D-151 把 task 管线删了,帮助文本没跟着走。

**但真正的发现不是文本过期,是接口在说谎。**

七个标志能传、不报错、**什么都不做**:

| 标志 | 实况 |
| --- | --- |
| `apply --shell` / `--offline` | 蓝图分支根本不读它们,`let _ = (...)` 吞掉 |
| `plan --offline` | 同上 |
| `build --tag` / `--arch` / `--no-cache` | 同上 |
| `inspect --gen-inventory` | 必然 `bail`,没有能成功的路径 |
| `doctor --ai` | AI 模块随 D-151 删了 |

反证做过:`apply -f x --dry-run` 与 `apply -f x --dry-run --shell --offline`
输出 **md5 完全一致**。

**空转标志比过期文档坏。** 过期文档骗你一次,你去试就发现了;空转标志让你
以为自己开了离线模式 —— 然后带着"已经是离线的"这个错误前提去气隙机房。
七个全删,现在传它们是明确的 `unexpected argument`。

**帮助本身按 kubectl 的体例重写**:

- **命令分组**。27 条平铺没人读得下去。clap 4 不支持子命令分组,所以手写 ——
  但**手写清单必然漂**,所以配了 `grouped_listing_covers_every_command`:
  少一条、多一条、改个名都会红,两个方向都反证过。
- **一句话摘要 + 详述分层**。原来整段挤成 `about`,列表在终端里被揉成一坨;
  加 `verbatim_doc_comment` 之后 `plan` 的 ✓/~/?/- 四态表才立得住。
- **每条命令给例子**(`after_help`)。

**例子这件事必须机器校验,不能靠人核。** 我自己在这次重写里就编错三条:
`fmt --extract`(真名 `--split`)、`cp` 写成位置参数(真的是 `--src/--dst`)、
`load -f`(其实是位置参数)。三条都**看着完全正常**。所以加了
`every_documented_example_actually_parses`:把 help 里每一行 `crater …` 都拿去
真解析一遍。反证过 —— 把 `load k8s.oci` 改回 `load -f k8s.oci`,立刻红。

这是同一课的第四次:**没人跑的文档会烂**。前三次的修法是"发现后改对",
这次是"让它烂不了"—— 差别在于前者需要下一个人再撞一遍。

四道闸门全绿,16 套 588 个用例(新增 4 个)。

**留了一个更大的坑没填**(另开 issue):`docs/features/` 下 plan/projects/
materials/inventory 四篇、以及 `architecture.md` 标着「现状」的若干条,整篇
在讲**已删除的 task 管线**(`命名 task`、`project bundle`、`.oci` 直连)。
那是 D-151 就欠下的,不是这次改坏的 —— 半修比不修更容易让人以为已经修过。

### D-155:`crater apply yq` —— 名字直接从仓库拉下来跑(helm 那种)

**要的东西一句话**:最短的命令就是 `crater apply yq`。

**解法不是新造一条路,是把三条已有的接起来。** `<source>` 现在按这个顺序解,
**先本地后远端**:

1. 一个存在的文件 → 蓝图或栈(按内容分辨,不看文件名)
2. `<名字>.app.yaml` → 已装的任务:蓝图、机群、参数全从它来
3. 都不是、且像个名字 → 去已配仓库的索引里找,拉下来再跑

第 2 条是关键,而它**本来就存在** —— `install` 早就把这三样写进
`<名字>.app.yaml` 了(D-141),那个文件就是"这次安装的正身"。此前只有 UI 在
读它,CLI 反倒要人每次重复一遍 `-f … -i … --set …`。

第 3 条整条复用 `pkg::install`:名字 → 索引 → 拉包 → 参数契约 → 机群契约 →
落 app 文件 → 出计划。**没有新写一条拉取路径**,所以那些闸门(必填参数、
机群组数、版本封条)一个都没绕过。

**动词决定收不收敛**,不是新开一个开关:`apply` 印完计划就执行,`plan` 停在
计划。`plan yq` 会把包拉下来 —— 拉取不是破坏性动作,而不拉就没法给出计划。

**`verify` / `destroy` 只认本地。** 对一个从没装过的名字谈"漂移"或"退役"是
没有意义的:漂移是拿现场比记录,退役是移除装过的东西,两者都以"装过"为前提。
为它去仓库拉一个包,拉到的也只是"包长什么样",不是"这台机器上有什么"。

**远端那条**照印计划再收敛。这一眼不能省的理由很具体:拉下来的字节是**别人
做的**,而下一步要改的是生产机。

**三条边界值得写下来**:

- **文件优先。** 同名的 `yq` 文件与 `yq.app.yaml` 并存时,写了路径的人显然
  指的是文件。反过来猜会让"我明明指定了文件"变成一件要 debug 的事。
- **给了 `-f` 就绝不联网。** `-f` 的意思是"用这份文件",不该在它不存在时
  悄悄跑去 registry 找个同名的。
- **像文件名的不当名字试。** `web.blueprint.yam`(手滑少个 l)带 `.`,
  当成任务名去查只会给出一个答非所问的错误,而真正的原因是文件名打错了。

**合并规则:命令行盖过 app 文件。** app 记的是"这次安装是什么样",命令行是
"这一次我要不一样" —— 后者更具体。实现上是把命令行的 `--set` 追加在后面,
因为 `plan::with_overrides` 用的是 `insert`,后来者胜。

**端到端验过**(本地 registry + 静态 HTTP 索引 + 换掉 `$HOME` 的干净环境):
一个仓库都没配 → 报错并说该敲什么;`repo add` 后空目录里 `crater apply yq`
→ 拉包、落 `yq.app.yaml`、印计划、收敛;**把 registry 和索引都停掉**之后
`crater plan yq` 照跑(本地那条不碰网络);没见过的名字 `nginx` → 明确报
"仓库里没有"。

**自己踩的一个小坑记一下**:核对 README 里的命令时我直接**执行**了它们,
于是 `crater repo add lab https://example.com/index.yaml` 真往 `~/.crater`
里写了一条假仓库(已清)。验证文档里的命令应当**只解析不执行** —— D-154 加的
`every_documented_example_actually_parses` 正是这么做的,而我当时绕过了它自己
手跑一遍。工具建好了不用,等于没建。

**补:`apply`/`plan` 也认 OCI 引用了(helm 3.8+ 那种)。** 此前只有 `install`
认引用,`apply <ref>` 报的却是"只接受蓝图" —— 一条**误导性**的错误:真相不是
"不支持引用",而是"这个动词没接过去"。三种写法现在都通:

    crater apply oci://reg/ns/yq:1.0    # 显式协议头
    crater apply reg/ns/yq:1.0          # 裸引用
    crater apply yq                     # 包名,走索引

判据:带 `/` 的是引用,直连 registry,**不需要配任何仓库**;不带的是包名,
走索引。索引只为回答"有哪些包" —— OCI 没有搜索端点(D-123),而版本发现
不需要索引,`tags/list` 就够。

**一条边界是踩出来的**:最初只判"带 `/` 就是引用",于是 `./web.yaml` 被当成
引用拿去连 registry。测试当场红。现在**长得像路径的一律不碰**(`./` `../`
`/` `~` 开头),哪怕那个文件此刻不在 —— 打错的路径被拿去查仓库,报出来是
"仓库里没有 ./web.yaml",会把人引向仓库配置,而真正的问题是路径写错了。

四道闸门全绿,16 套 596 个用例。

### D-156:registry 不可达时不再无声挂死(附:自己写了一条误导性提示)

**症状**:`crater pkg tags <某个到不了的 registry>` **永远不返回**。不报错、
不超时、不退出 —— 就那么吊着。查 helm #11000 的时候撞上的。

**根因**:`oci_client::ClientConfig::default()` 的 `connect_timeout` 与
`read_timeout` 都是 `None`,也就是永不超时;`crater_core::source` 那个 reqwest
客户端同样一个超时都没设。两处出网,两处都没有。

**部署工具挂死比报错难查得多**:报错至少告诉你去看网络,挂死只让你以为它
还在干活 —— 于是你等,然后开始怀疑是不是包太大。

**修法里有一个必须说清的区分:用空闲超时,不是总时长。**

`read_timeout` 掐的是"多久没有任何字节",不是"总共跑多久"。这个区别不是
细节:crater 要拉几百 MB 的离线闭包,给**总时长**设上限等于给"多大的包能拉"
设上限 —— 一条 1MB/s 的内网线上 300MB 要五分钟,而那是完全正常的一次拉取。
换成 `.timeout()` 的后果是大包在慢网上必失败,而症状是"随机断在中途"。
这条写进了 `source::READ_TIMEOUT` 的文档注释,并配了一个测试钉住量级。

取值:连接 10 秒,读 60 秒。

**验证与反证**:起一个"接受连接、然后什么都不回"的黑洞 —— 那正是不可达
registry 最难查的形态(TCP 握上了,所以"连不上"的报错永远不会出现)。
有读超时:0.5 秒失败。把 `.read_timeout(read)` 注释掉:**挂满 30 秒**,直到
黑洞线程自己撒手。

**连接超时没有测试,这是有意的。** 试过用 TEST-NET-1(192.0.2.1)测,实测
却要 5 秒 —— 因为开发/CI 的沙箱网络**拦截全部出站 TCP 并一律接受**,裸
socket 连 192.0.2.1:9 都能"连上"。连接阶段根本不会卡住,这条超时在这里
永远不触发。写一条在本环境测不到东西的测试,只会得到一个测别人的绿灯 ——
所以留的是一段说明,不是一个假绿灯。

**顺带把真因从 source 链里捞出来**(与 D-145 同一课):顶层 Display 只有
"error sending request for url (…)",而"operation timed out"在下一层。只报
顶层的后果很具体:人会去查凭据和权限,而该看的是网络。八处走网络的调用
换成了 `net_err`。

**然后我自己写了一条误导性提示,值得记下来。** 第一版把两种超时混成一句
"registry 连得上但不回话",而实测那条错误链里明明写着 `client error
(Connect)` —— 是**连接**超时,根本没握上手。一条自己就在误导的提示,比不给
提示更坏:它给的方向是错的,而人会信。现在两种分开,各给各的建议,并配了
测试。

**还踩了一个排版坑**:多行提示用 `\` 续行,rustfmt 把续行折成一行,行首的
缩进空格**留在了字符串里** —— 打印出来是一段错位的文字。测试里加了一条
"提示的每一行缩进不超过 2 格"来钉住。

四道闸门全绿,16 套 601 个用例(新增 5 个)。
