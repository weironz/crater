# Crater

> Deploy anything — 纯 Rust 单二进制、零运行时依赖的「万物部署器」，支持**在线**与**离线**两种形态，面向国内弱网 / 离线 / 政企环境。

```
crater docker
crater k8s        # 别名 → k3s
crater mysql
crater es         # 别名 → elasticsearch
```

## 状态

🚀 **M1–M5 已完成，并在真机（Ubuntu @ 192.168.73.11）端到端验证通过**。`cargo build` 0 错 0 警，`cargo test` 31 passed。

> **进行中**：按 [docs/design.md](docs/design.md) 重整设计方向——已完成「引擎去产品化」（D-017）、**B1 幂等回显**（D-023，apply 默认执行、`--dry-run` 预览、`changed/ok/warn`）、**spec 内联 recipe**（D-025，单文件即可）、**自举 agent 作默认**（D-019/D-026/D-027）；均真机 yq 验证。下一步离线转 OCI（D-018）、ansible 化 task 层、在线 CN 镜像 fallback。

- **M1 在线部署**：`crater docker --host <ip> --password <pw> --apply` 经 SSH 装好 docker（已验证 active，v29.5.2）。
- **M2 离线部署**：`crater build` 制离线包（tar.gz + manifest + sha256），`crater deploy` 零联网部署（已验证 node_exporter active，:9100 serving；10.6MB 制品分块 over SSH 推送）。
- **M3 集群/依赖**：组件 `requires` 依赖 DAG + k3s 组件（已验证节点 Ready，v1.30.5+k3s1 control-plane）。
- **M4 AI 制包侧**：`crater ai "<大白话>"` → 校验后的 crater.yaml（OpenAI 兼容；模型只产候选，crater 确定性校验，副驾不司机）。
- **M5 AI 离线侧**：`crater doctor` 离线规则诊断（零网络零模型，10 规则；`--ai` 可叠加内网大模型）。

## 设计要点

> 整体设计方向见 [docs/design.md](docs/design.md)（北极星文档）。

- **引擎零产品知识（第一性原理，D-017）**：引擎（Rust）只懂「怎么做」（通用原语 download/extract/template/service/cmd…），「做什么」（docker/k3s/mysql 的名字/服务/别名/镜像源/诊断规则）**全是数据**。加一个可部署对象 = 丢一个 YAML，绝不改 Rust 重编译。这跟 Ansible 同源——「类似 Ansible」与「装万物」是同一目标。
- **Deploy anything**：一切可部署对象都是统一生命周期的 **Component**；引擎是领域无关的、跑在 SSH 上的声明式部署引擎。
- **声明式组件**：组件用 YAML 描述文件定义（内部标签 `check:` / `action:`，`aliases:` / `images:` 等均为数据），内置与第三方同构，第三方放目录即加载。
- **双形态，单管线**：在线（目标机现场拉依赖，失败回退国内镜像）/ 离线（在线机制包，目标机零联网部署）。两形态共用同一套组件与引擎，**只在「制品从哪来」这一层（ArtifactSource）分叉**。
- **离线包基于 OCI 镜像（定向，D-018）**：离线包 = OCI Image Layout，内容寻址自校验、分层去重、原生承载容器镜像（k8s/mysql/es 离线的前提）。取代早期 tar.gz，渐进迁移中。详见 [docs/offline-format.md](docs/offline-format.md)。
- **自举 agent 作默认（D-019/D-027）**：默认把 crater 二进制推到目标机（按 sha256 缓存，推一次/版本）+ 计划，由 `crater agent` 在目标机**本地执行**（少 SSH 往返）；`--shell` 逃生到纯 agentless shell（目标只需 SSH+shell，任何机器都行），`--agent-bin` 指异构架构的 musl 静态构建。
- **AI 副驾**：在线制包用 AI 生成/校验 spec，离线现场用固化规则诊断；可完全关闭，永不成为硬依赖。

## 快速开始

```bash
# 构建 + 测试
cargo build
cargo test

# 在线部署一个组件到远端（SSH，agentless）—— 默认执行；默认走自举 agent
crater yq    --host 10.0.0.5 --password <pw>             # 最小 demo：单文件二进制(已真机验证)
crater docker --host 10.0.0.5 --password <pw>
crater k8s   --host 10.0.0.5 --password <pw>             # 别名 → k3s（来自组件数据 aliases）
crater mysql --host 10.0.0.5 --password <pw>
crater k8s   --host 10.0.0.5 --password <pw> --dry-run   # 只打印计划，不执行
crater yq    --host 10.0.0.5 --password <pw> --shell     # 逃生口：强制 agentless shell（目标跑不了二进制时）

# 幂等：再跑一次只报 ok/changed/warn，已就绪的步骤自动跳过（changed=0）

# 声明式 spec（按依赖 DAG 排序，逐主机/按 role）
crater apply -f examples/yq.yaml                # 引用 components/yq/（可复用 recipe）
crater apply -f examples/yq-inline.yaml         # 单文件：recipe 内联进 spec（Path B，免 components/）
crater apply -f examples/yq.yaml --dry-run      # 只看计划

# 离线：在线机制包 → 目标机零联网部署
crater build  -f examples/node_exporter.yaml -o ne.bundle
crater deploy --bundle ne.bundle --host 10.0.0.5 --password <pw>

# AI 副驾（需 CRATER_AI_ENDPOINT/CRATER_AI_MODEL，OpenAI 兼容）
crater ai "在 10.0.0.5 上装单机 docker" -o crater.yaml

# 离线诊断（规则引擎，零网络；--ai 可叠加内网模型）
crater doctor --file install-error.log
crater doctor --host 10.0.0.5 --password <pw> --ai

# 临时命令（ansible -m shell 风格）
crater run --host 10.0.0.5 --password <pw> -- "uname -a"
```

## 工程结构

```
crater/
├── crates/
│   ├── crater-core/      # 引擎:spec/component/os/executor/source/engine/bundle/dag/ai/diagnose
│   │                     #   + mirrors.default.yaml(镜像源数据,include_str! 烤进二进制)
│   └── crater-cli/       # `crater` 二进制（CLI；别名经组件数据 resolve_component 解析）
├── components/           # 声明式组件描述文件(产品知识只在这里;含 aliases/images 等数据)
│   ├── docker/           # 在线安装(发行版包)
│   ├── mysql/            # 单机数据库(crater mysql)
│   ├── elasticsearch/    # 单机搜索(crater es ← aliases)
│   ├── node_exporter/    # 静态二进制(离线包演示)
│   └── k3s/              # 轻量 k8s(crater k8s/kubernetes ← aliases)
├── examples/             # 示例 spec
└── docs/                 # 需求/设计/决策文档（多部分，持续增补）
```

## 文档

- [设计方向 design.md](docs/design.md)（北极星：引擎铁律 + 在线/离线单管线 + OCI 离线 + 自举 agent + ansible 化路线）
- [离线包格式 offline-format.md](docs/offline-format.md)（OCI 镜像方案详细设计）
- [需求基线 v0.3](docs/requirements.md)
- [决策 / 沟通记录](docs/decisions.md)（D-001~D-030）
- [进展日志](docs/progress.md)（M1–M5 已验证；含工具链纪律）
- [文档索引](docs/README.md)

## 路线图

| 阶段 | 目标 | 状态 |
|------|------|------|
| M1 | 单组件在线部署（docker）+ SSH 执行器 + spec 解析 | ✅ 真机验证 |
| M2 | 离线 build/deploy（tar.gz + manifest + sha256）| ✅ 真机验证 |
| M3 | 组件依赖 DAG + k8s(k3s) | ✅ 真机验证 |
| M4 | AI 制包侧（NL→spec，确定性护栏）| ✅ 实现+单测 |
| M5 | AI 离线侧（固化规则诊断 + 内网 endpoint）| ✅ 实现+验证 |
| A（还债） | 引擎去产品化：别名/doctor/LoadImage/镜像表 全部数据驱动（D-017）| ✅ build 绿 + 31 tests |
| B1（幂等）| check→act→report，`changed/ok/warn` 回显；apply 默认执行、`--dry-run` 预览（D-023）| ✅ 真机 yq 验证 |
| module 契约（D-029）| 四层 module 模型；`action: module` + 数据定义 module（`modules/*.yaml`，零 Rust 扩展）| ✅ 契约地基 + 真机验证 |
| B2/B3（ansible 化）| task/play 层（when/loop/notify）、内置 module 库（file/copy/service…）、外部 module JSON 协议 | 设计中 |
| 离线 OCI（D-018）| 离线包转 OCI Layout、容器镜像打包、临时 registry 多节点分发 | 设计中 |
| 自举 agent（D-019/D-026/D-027）| **默认执行模型**：推二进制(按 sha256 缓存)+计划，目标机本地执行；`--shell` 逃生、`--agent-bin` 异构 | ✅ 在线真机验证（解包 OCI 待 D-018）|
| 多节点 + 跨节点 fact（D-030）| 多主机 fan-out + 按 role 过滤；`register`/`hostvars` 跨节点传值（真集群钥匙）| ✅ 两台真机验证 |
| 后续 | k3s 多节点 join（用 D-030）、并发(F17)、es live、kubeadm、musl/aarch64、host-key 校验、包签名 | 计划中 |

## License

Apache-2.0
