# Crater

> Deploy anything — 纯 Rust 单二进制、零运行时依赖的「万物部署器」，支持**在线**与**离线**两种形态，面向国内弱网 / 离线 / 政企环境。

```
crater k8s
crater docker
crater es
crater mysql
```

## 状态

🚧 早期开发中（M1 MVP）。当前可跑通：`crater docker` 的声明式计划生成与本地 dry-run。

## 设计要点

- **Deploy anything**：一切可部署对象都是统一生命周期的 **Component**。
- **声明式组件**：组件用 YAML 描述文件定义，内置与第三方同构，第三方放目录即加载。
- **双形态**：在线（现场拉依赖，走国内镜像源）/ 离线（先制包，现场零网络一键部署）。
- **Agentless**：目标机只需 SSH；复杂逻辑由推送过去的同一个二进制以 `crater agent` 模式执行。
- **AI 副驾（后置）**：在线制包重度用 AI，离线现场用固化产物 / 本地模型降级；可完全关闭。

## 快速开始（开发）

```bash
# 构建
cargo build

# 查看某组件的部署计划（dry-run，不执行）
cargo run -p crater-cli -- docker --os debian

# 在本机实际执行（仅 Linux 有意义）
cargo run -p crater-cli -- docker --os debian --apply

# 应用声明式 spec
cargo run -p crater-cli -- apply -f examples/crater.yaml
```

## 工程结构

```
crater/
├── crates/
│   ├── crater-core/      # 执行引擎、动作原语、OsProvider、ArtifactSource、Executor、组件加载
│   └── crater-cli/       # `crater` 二进制（CLI）
├── components/           # 声明式组件描述文件
│   └── docker/
│       ├── component.yaml
│       └── templates/
├── examples/             # 示例 spec
└── docs/                 # 需求与决策文档（多部分，持续增补）
```

## 文档

- [需求基线 v0.2](docs/requirements.md)
- [决策 / 沟通记录](docs/decisions.md)
- [文档索引](docs/README.md)

## 路线图

| 阶段 | 目标 |
|------|------|
| M1 | 单组件在线部署（docker）+ SSH 执行器 + spec 解析 + 国内源加速 |
| M2 | 离线 build/deploy + OCI 包 + 临时 registry |
| M3 | k8s 集群部署（多节点编排、DAG）|
| M4 | AI 制包侧（NL→spec、依赖补全、知识固化）|
| M5 | AI 离线侧（固化诊断 / 本地模型 / 内网 endpoint）+ 组件插件化开放 |

## License

Apache-2.0
