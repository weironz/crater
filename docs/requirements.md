# Crater 需求文档 v0.3

> 状态：需求基线；设计方向见 [design.md](design.md)。
> 最后更新：2026-06-01
>
> **D-046 起单一 task 模型**：本文下文的「component / 组件」均按 **task** 理解；
> `crater <component>` 等旧命令现统一为 `crater apply <task>`（裸名解析 `tasks/<name>.yaml`）。
> 引擎原则（D-017 零产品知识、D-036 YAML 纯数据）不变。

---

## 0. 一句话定位

**Crater**：一个纯 Rust 单二进制、零运行时依赖、支持在线/离线两种形态的「万物部署器」，面向国内弱网 / 离线 / 政企环境。

终极目标：`crater <target>` 部署一切。

```
crater k8s
crater docker
crater es
crater mysql
crater redis / minio / nginx / etcd ...
```

---

## 1. 核心理念

- **引擎零产品知识（第一性原理，D-017）**：引擎（Rust）只持有「通用原语」（download/extract/template/service/cmd…，类比 ansible-core 的 module），**不得持有任何具体产品的名字/服务名/别名/镜像源/诊断规则/依赖**——后者一律是数据（YAML）。加一个可部署对象 = 丢一个描述文件，绝不改 Rust 重编译。这条铁律是「装万物」可信的唯一保证，也让「类似 Ansible」与「装万物」合一。
- **Deploy anything**：一切可部署对象都是一个 **Component（组件）**，遵循统一生命周期。
- **统一抽象**：所有组件共享 install / start / stop / status / upgrade / uninstall / scale 生命周期。
- **双形态，单管线**：在线/离线**共用同一套组件与执行引擎**，只在「制品从哪来」（ArtifactSource）这一层分叉。
  - 在线：现场按需拉依赖（走国内镜像源加速）。
  - 离线：先在在线环境制包（**基于 OCI 镜像**，D-018），现场零网络一键部署。
- **AI 副驾（后置）**：在线制包阶段重度用 AI，离线现场用固化产物 / 本地模型降级。AI 可完全关闭。
- **Agentless + 自举 agent（D-019）**：借鉴 Ansible，目标机只需 SSH；复杂逻辑/离线落地由推送过去的同一个二进制以**一次性自举 `crater agent`** 在本地执行（用完即走，不留常驻服务）。

> 整体设计方向（引擎铁律 + 在线/离线单管线 + OCI 离线 + 自举 agent + ansible 化路线）见 [design.md](design.md)。

---

## 2. 关键决策记录（拍板项）

| 决策点 | 结论 | 对架构的影响 |
|--------|------|------------|
| 目标 OS | **首批仅 Ubuntu/Debian 系 + RHEL 系**；国产化 OS（麒麟/UOS）暂不纳入 | 按 Debian/RHEL 系族抽象 `OsProvider`，国产 OS 留扩展位不堵死 |
| 组件定义方式 | **声明式描述文件（YAML）** | 核心是「确定性执行引擎 + 内置动作原语」，组件用 YAML 声明；内置与第三方组件同构 |
| AI 优先级 | **硬核部署优先，AI 后置（M4/M5）** | M1–M3 不依赖 AI；但 spec/数据结构预留 AI 字段，避免返工 |
| MVP 首个组件 | **docker / containerd** | M1 只打通这一个组件的在线安装全链路 |
| 项目命名 | **crater** | cluster + crate(Rust) 合成；CLI 主命令为 `crater` |

---

## 3. 总体架构

```
┌─────────────── 控制端 (crater 单二进制) ───────────────┐
│  CLI  →  解析 crater.yaml (inventory + spec)            │
│         ↓                                                │
│  Planner (期望态 vs 实际态 → DAG 任务图)                 │
│         ↓                                                │
│  Executor ── SSH(russh) ──→ 各目标节点并发执行           │
│              └─ 推送同一二进制，以 `crater agent` 执行    │
│         ↑                                                │
│  ArtifactSource (在线: 远程拉 / 离线: 本地包)            │
│  OsProvider (apt vs dnf / systemd / 防火墙 / 内核参数)   │
│  ComponentLoader (加载声明式组件描述文件)                │
└─────────────────────────────────────────────────────────┘
```

### 3.1 组件模型（声明式 + 执行原语）

两层结构：

1. **确定性执行引擎（Rust 核心）**：提供一组内置**动作原语**：
   `download`（断点续传 + sha256）、`extract`、`render_template`、`write_file`、`systemd_unit`、`run_cmd`、`pkg_install`、`preflight_check`、`load_image`（离线阶段）…
2. **组件描述文件（YAML）**：声明用哪些原语、什么顺序、什么参数。内置组件和第三方组件**同构**（吃自己狗粮验证表达力）。

组件描述示意（`components/docker/component.yaml`）：

```yaml
name: docker
version_default: "24.0"
supported_os: [ubuntu, debian, rhel, centos, rocky]
preflight:
  - check: port_free
    port: 2375
  - check: kernel_min
    version: "3.10"
install:
  - pkg_install:
      ubuntu: [ca-certificates]
      rhel: [device-mapper-persistent-data]
  - download:
      url_tmpl: "...{{version}}..."
      sha256: "..."
  - extract: { to: /usr/bin }
  - render_template: { src: daemon.json.tmpl, dst: /etc/docker/daemon.json }
  - systemd_unit: { name: docker, enable: true, start: true }
verify:
  - run_cmd: "docker info"
```

> ⚠️ 表达力边界：纯声明式遇到复杂分支（如 k8s 证书生成、集群 join）会吃力。预留 `run_cmd` + 脚本钩子作为逃生口；复杂组件后期再评估是否补 Rust 内建模块。docker 不会触到此边界，正好先验证声明式主路径。

---

## 4. 功能需求

### 4.1 组件部署（核心）
- **F1** 单机/集群两种拓扑：`crater mysql`（单机）、`crater k8s`（多节点集群）。
- **F2** 统一组件生命周期：install / start / stop / restart / status / upgrade / uninstall / scale。
- **F3** 声明式 spec（`crater.yaml`）：节点清单 + 组件 + 版本 + 参数；可重入、幂等。
- **F4** 命令式快捷方式：`crater mysql --version 8.0 --host 10.0.0.5` 无需写 yaml。
- **F5** 组件依赖编排：组件间依赖自动算 DAG 顺序。
- **F6** 内置组件库（首批）：k8s、docker/containerd、mysql、redis、elasticsearch、minio、nginx、etcd。
- **F7** 组件可扩展：声明式描述文件，第三方放目录即加载。

### 4.2 在线模式
- **F8** 国内镜像源加速：内置 registry.k8s.io→阿里云/中科大、github→ghproxy 等替换规则，多源 fallback。
- **F9** 目标机按需下载依赖（镜像/二进制/系统包），断点续传 + 校验和。

### 4.3 离线模式（基于 OCI 镜像，D-018；详见 [offline-format.md](offline-format.md)）
- **F10** 制包：`crater build -f xxx.yaml -o x.oci`，在线环境拉全依赖（制品 + **容器镜像**）打包。
- **F11** 离线包格式：**OCI Image Layout**（序列化为单个 oci-archive tar）。制品与容器镜像均**内容寻址**存放（digest 即校验，去重免费）；crater 在其上加 `crater-manifest` 做逻辑索引。可选 zstd（先 gzip）。**取代早期 tar.gz**，渐进迁移。
- **F12** 离线部署：`crater apply x.oci`（D-050:`deploy` 子命令已删,apply 统一入口），现场零网络；目标机解包 OCI、digest 自校验、导入容器镜像、跑同一引擎。
- **F13** 离线镜像分发：起临时 registry，多节点指过去，避免每台塞一份。
- **F14** 制品完整性：OCI 内容寻址 digest 自校验、缺包检测、版本兼容校验（确定性规则，不靠 AI）。哪些镜像/制品入包由组件数据（`images:` / `download:`）声明，引擎不内置（D-017）。

### 4.4 节点与执行
- **F15** Inventory 管理：节点清单（IP/SSH 凭据/角色/标签）。
- **F16** SSH agentless 执行 + 自举 agent：推送二进制到目标机，以 `crater agent` 模式执行步骤。
- **F17** 并发执行 + 进度可视化。
- **F18** 幂等与回滚：失败可重入；关键操作支持回滚（至少 uninstall 干净）。
- **F19** 预检（preflight）：OS 版本、内核参数、端口占用、磁盘、依赖。

---

## 5. AI 功能需求（M4/M5 后置）

**总原则**：AI 是副驾不是司机。离线包内容与实际部署动作走确定性逻辑；AI 输出必须经 schema 校验 / 人工确认。AI 可 `--ai off` 完全关闭，不成为硬依赖。

### 5.1 在线（制包/规划）—— 重 AI
- **AI1** 自然语言 → spec：大白话描述 → AI 生成 `crater.yaml` → 工具校验。
- **AI2** 依赖补全建议：AI 推断隐性依赖（内核参数、系统包），规则/人工确认后入包。
- **AI3** 版本兼容性分析：组件版本矩阵冲突提示。
- **AI4** 知识固化：制包时为该 bundle 生成专属排障 runbook / 诊断规则 / FAQ + 向量索引，打进离线包。

### 5.2 离线（部署/排障）—— 轻 AI，降级链
- **AI5** 优先用固化产物：错误 → 匹配预生成诊断规则 → 修复建议（零依赖）。
- **AI6** 可选内嵌本地小模型（candle / llama.cpp + 量化 Qwen 等），日志解读/问答；可选组件，开关控制。
- **AI7** 支持对接内网私有大模型 endpoint（政企内网常有自建模型）。
- **AI8** RAG 离线知识库：离线向量检索增强问答。

### 5.3 通用
- **AI9** Provider 可配：云端（OpenAI 兼容协议）/ 本地模型 / 内网 endpoint，优先级降级。
- **AI10** 自愈仅建议不自动执行：AI 给修复命令，人工确认后才 apply。

---

## 6. 非功能需求

- **N1** 零运行时依赖：musl 静态编译，单二进制，目标机不装额外东西。
- **N2** 跨架构：x86_64 + aarch64（国产 ARM 服务器常见）。
- **N3** 体积可控：核心二进制尽量小；本地模型作为可选下载，不进核心包。
- **N4** 安全：SSH 凭据安全存储、离线包签名校验、最小权限。
- **N5** 可观测：结构化日志、详细执行记录、失败现场可导出（便于离线排障）。
- **N6** 幂等可重入、操作可审计。

---

## 7. CLI 设计草案（终极契约见 [design.md §7](design.md)，D-020/021/022）

**主入口 `apply`（声明式幂等），动词分两组——吃 `<source>` / 吃 `<release>`：**
```
# —— 吃 <source>（recipe+制品来源：组件名 / ./x.yaml / oci://… / ./x.crater / -f spec.yaml）——
crater plan    <source>      # 只生成执行计划（= apply --dry-run）
crater apply   <source> --host X --password ***   # 部署：收敛到期望态(install+upgrade)，幂等
crater apply   -f crater.yaml                      # source=spec 文件(inventory 内含，现状)
crater bundle  <source> -o x.crater                # 打离线 OCI 包（= 现 build，别名保留）
crater bundle  <source> --push oci://reg/x:1.0     # 推 registry（在线分发）
crater inspect <source>      # 查看包内容（组件/制品/镜像/digest/签名），只读
crater verify  <source>      # 校验依赖完整性 + 签名，只读

# —— 吃 <release>（已部署实例：名字+修订历史+主机）——
crater rollback <release> [--to rev]   # 回滚（重放旧期望态）
crater remove   <release> [--purge]    # 卸载（声明式 uninstall: + 记录追踪兜底）
crater ai diagnose <release>           # AI/规则诊断 + 智能修复建议

# —— AI（保留接口，多数后置；副驾不司机，可 --ai off）——
crater ai generate "给我一个3主3从带MinIO的k8s集群"   # NL→spec（确定性护栏）
# 另：AI 还作为生命周期钩子（bundle 时析依赖 / apply 报错析日志 / apply 后 doctor+修复），见 §5、design.md §7.5

# —— 内部 ——
crater agent ...             # 目标机一次性自举执行（D-019）

# 快捷式（糖）：crater <component> == crater apply <component>
crater k8s --host X --password ***
```

> 关键约定：**inventory 永远在镜像外**（CLI `--host`/`-i` 或 spec），不入可分发的 OCI 镜像；镜像内 recipe 是默认参数，`--set`/`--values` 覆盖。破坏性动词均支持 `--dry-run`。

---

## 8. 目标 OS 适配（首批）

| 系族 | 具体发行版 | 包管理 | 备注 |
|------|-----------|--------|------|
| Debian/Ubuntu 系 | Ubuntu 20.04/22.04/24.04、Debian 11/12 | apt / deb | |
| RHEL 系 | RHEL/CentOS/Rocky/AlmaLinux 7/8/9 | yum/dnf / rpm | CentOS 7 老内核要注意 |

适配抽象：`OsProvider` trait 屏蔽差异（包管理、systemd、防火墙、SELinux、内核参数路径）。国产化 OS（麒麟/UOS）后续按系族归类扩展。

---

## 9. 技术选型（Rust crate）

| 需求 | crate |
|------|-------|
| SSH/SFTP（纯 Rust） | `russh` + `russh-sftp` |
| OCI 镜像操作 | `oci-client`、`oci-spec` |
| 异步运行时 | `tokio` |
| 配置/序列化 | `serde` + `serde_yaml` |
| 模板渲染 | `tera` / `minijinja` |
| 校验和 | `sha2` |
| 压缩 | `zstd` / `flate2` |
| CLI | `clap` |
| HTTP/下载 | `reqwest`（建议 `rustls` 避免 openssl C 依赖） |
| 进度条 | `indicatif` |
| 日志 | `tracing` |
| 静态编译 | target `x86_64-unknown-linux-musl` / `aarch64-unknown-linux-musl` |
| 本地推理（后置） | `candle` / `llama-cpp-2` |
| Embedding（后置） | `fastembed` |
| 向量检索（后置） | `hnsw_rs` / 内嵌 `qdrant` |

> musl 注意点：DNS/动态加载有坑，TLS 用 `rustls` 替代 openssl 规避 C 依赖。

---

## 10. 分期路线

| 阶段 | 目标 |
|------|------|
| **M1 MVP** | 单组件在线部署（docker）+ SSH 执行器 + spec 解析 + 国内源加速 |
| **M2** | 离线 build/deploy + OCI 包 + 临时 registry + 预检 |
| **M3** | k8s 集群部署（多节点编排、DAG）|
| **M4** | AI 制包侧（NL→spec、依赖补全、知识固化）|
| **M5** | AI 离线侧（固化诊断 / 本地模型 / 内网 endpoint）+ 组件插件化开放 |

---

## 11. M1 范围锁定（MVP）

**目标**：`crater docker` 在单台 Ubuntu/RHEL 机器上在线装好并验证。

**必须有**：
- Cargo workspace 骨架（core / cli / components 分层）
- `crater.yaml` + inventory 解析（serde_yaml）
- SSH 执行器（russh）+ 自举 agent 模式
- `OsProvider` 抽象（apt vs dnf）+ 系族探测
- 内置动作原语：download(断点续传+sha256)、extract、render_template(tera)、systemd_unit、pkg_install、run_cmd、preflight_check
- 声明式组件加载器 + docker 组件描述文件
- 国内镜像源加速规则（github→ghproxy 等）
- 进度/日志（indicatif + tracing）

**明确不做（后期）**：离线 build/deploy、OCI 包、k8s 多节点编排、所有 AI 能力、国产 OS。

**但要预留（避免返工）**：
- spec 结构里给 AI 字段留位（不实现）
- `ArtifactSource` 抽象（在线/离线）接口先定义，M1 只实现 OnlineSource
- 组件描述 schema 里给 offline 制品清单留字段

---

## 12. 待办 / 开放问题

- [x] 离线包格式定型：**选 OCI 方案**（参考 sealos clusterimage），D-018 / [offline-format.md](offline-format.md)。
- [ ] 组件描述 schema 正式定义（动作原语全集、参数规范；含 `aliases` / `images` 等数据字段）
- [ ] `crater.yaml` 顶层 schema 设计（inventory + components + 全局配置 + 预留 AI/offline 字段）
- [ ] 名称占用核查（crates.io / GitHub / npm）
- [ ] 复杂组件（k8s）声明式表达力的逃生口设计
- [ ] SSH 凭据安全存储方案
