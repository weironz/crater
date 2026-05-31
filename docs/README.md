# Crater 文档索引

需求与设计文档**分多部分，持续增补**。每次关键沟通后，把结论沉淀到对应文件，避免上下文丢失。

## 文档清单

| 文件 | 内容 | 状态 |
|------|------|------|
| [design.md](design.md) | **设计方向（北极星）**：引擎铁律(D-017) + 在线/离线单管线 + OCI 离线 + 自举 agent + ansible 化路线 | 持续 |
| [offline-format.md](offline-format.md) | 离线包格式（OCI 镜像方案）详细设计 | 持续 |
| [../examples/yq/demo-online-yq.md](../examples/yq/demo-online-yq.md) | 在线部署 demo（以 yq 为例，最小可复现） | 持续 |
| [requirements.md](requirements.md) | 需求基线总览（定位、功能、AI、非功能、CLI、OS、路线） | v0.3 |
| [decisions.md](decisions.md) | 关键决策 / 沟通记录（ADR 风格，D-001~D-027） | 持续 |
| [progress.md](progress.md) | 开发进展日志（M1–M5 真机验证 + 还债 A 批；含工具链纪律） | 持续 |

## 规划中的文档（按需新增）

| 文件 | 内容 |
|------|------|
| `component-schema.md` | 组件描述文件完整 schema（动作原语全集、参数规范、aliases/images 字段） |
| `spec-schema.md` | `crater.yaml` 顶层 schema（inventory + components + tasks + 全局 + AI/offline 字段） |
| `ai-design.md` | AI 能力详细设计（provider 降级、知识固化、离线 RAG） |

## 维护约定

1. **关键沟通后必记**：每次会话有新决策或需求变化，追加到 `decisions.md`（带日期），必要时回写 `requirements.md`。
2. **新主题独立成文**：某块需求展开到一定篇幅，从 `requirements.md` 拆出独立文件，在本索引登记。
3. **新会话先读文档**：上下文窗口可能丢失，新会话先读 `design.md`（设计方向）+ `requirements.md` + `decisions.md` 恢复全部背景。
