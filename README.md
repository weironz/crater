# Crater

> Deploy anything — 纯 Rust 单二进制、零运行时依赖的「万物部署器」，支持**在线**与**离线**两种形态，面向国内弱网 / 离线 / 政企环境。

```
crater docker
crater k8s        # 别名 → k3s
crater mysql
crater es         # 别名 → elasticsearch
```

## 状态

🚀 **M1–M5 已完成，并在真机（Ubuntu @ 192.168.73.11）端到端验证通过**。`cargo build` 0 错 0 警，`cargo test` 18 passed。

- **M1 在线部署**：`crater docker --host <ip> --password <pw> --apply` 经 SSH 装好 docker（已验证 active，v29.5.2）。
- **M2 离线部署**：`crater build` 制离线包（tar.gz + manifest + sha256），`crater deploy` 零联网部署（已验证 node_exporter active，:9100 serving；10.6MB 制品分块 over SSH 推送）。
- **M3 集群/依赖**：组件 `requires` 依赖 DAG + k3s 组件（已验证节点 Ready，v1.30.5+k3s1 control-plane）。
- **M4 AI 制包侧**：`crater ai "<大白话>"` → 校验后的 crater.yaml（OpenAI 兼容；模型只产候选，crater 确定性校验，副驾不司机）。
- **M5 AI 离线侧**：`crater doctor` 离线规则诊断（零网络零模型，10 规则；`--ai` 可叠加内网大模型）。

## 设计要点

- **Deploy anything**：一切可部署对象都是统一生命周期的 **Component**。
- **声明式组件**：组件用 YAML 描述文件定义（内部标签 `check:` / `action:`），内置与第三方同构，第三方放目录即加载。
- **双形态**：在线（目标机现场拉依赖，失败回退国内镜像）/ 离线（在线机制包，目标机零联网一键部署）。
- **Agentless**：目标机只需 SSH；控制端经 russh 推送命令、用分块 base64 写文件，无需在目标机装任何东西。
- **AI 副驾**：在线制包用 AI 生成/校验 spec，离线现场用固化规则诊断；可完全关闭，永不成为硬依赖。

## 快速开始

```bash
# 构建 + 测试
cargo build
cargo test

# 在线部署一个组件到远端（SSH，agentless）
crater docker --host 10.0.0.5 --password <pw> --apply
crater k8s   --host 10.0.0.5 --password <pw> --apply     # 别名 → k3s
crater mysql --host 10.0.0.5 --password <pw> --apply

# 声明式 spec（按依赖 DAG 排序，逐主机/按 role）
crater apply -f examples/crater.yaml            # dry-run
crater apply -f examples/crater.yaml --apply

# 离线：在线机制包 → 目标机零联网部署
crater build  -f examples/node_exporter.yaml -o ne.bundle
crater deploy --bundle ne.bundle --host 10.0.0.5 --password <pw> --apply

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
│   └── crater-cli/       # `crater` 二进制（CLI）
├── components/           # 声明式组件描述文件
│   ├── docker/           # 在线安装(发行版包)
│   ├── mysql/            # 单机数据库(crater mysql)
│   ├── elasticsearch/    # 单机搜索(crater es)
│   ├── node_exporter/    # 静态二进制(离线包演示)
│   └── k3s/              # 轻量 k8s(crater k8s)
├── examples/             # 示例 spec
└── docs/                 # 需求与决策文档（多部分，持续增补）
```

## 文档

- [需求基线 v0.2](docs/requirements.md)
- [决策 / 沟通记录](docs/decisions.md)（D-001~D-016）
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
| 后续 | es live、kubeadm 多节点、k3s air-gap、临时 registry、musl/aarch64、host-key 校验 | 计划中 |

## License

Apache-2.0
