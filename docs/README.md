# Crater 文档索引

需求与设计文档**分多部分，持续增补**。每次关键沟通后，把结论沉淀到对应文件，避免上下文丢失。

## 文档清单

| 文件 | 内容 | 状态 |
|------|------|------|
| [design.md](design.md) | **设计方向（北极星）**：D-036 YAML 纯数据铁律 + D-017 引擎零产品知识 + task 模型 + 自举 agent + 离线 OCI artifact | 持续 |
| [action-layer.md](action-layer.md) | **task/action 层设计**（`crater apply <动作>`，Ansible 能力在 D-036 约束下落地） | 持续 |
| [features/](features/README.md) | **功能文档目录**：每个功能一份（介绍 + demo + 验证），见其 README 索引 | 持续 |
| [offline-format.md](offline-format.md) | 离线包格式（OCI artifact 方案）详细设计 | 持续 |
| [requirements.md](requirements.md) | 需求基线总览（定位、功能、AI、非功能、CLI、OS、路线） | 持续 |
| [decisions.md](decisions.md) | 关键决策 / 沟通记录（ADR 风格，D-001~D-046） | 持续 |
| [progress.md](progress.md) | 开发进展日志（含工具链纪律） | 持续 |

> 注：component 模型已于 **D-046** 收敛为单一 task 模型。`design.md` / `requirements.md` /
> `offline-format.md` / `progress.md` 仍含 component 时代历史描述，逐步刷新中。

## 维护约定

1. **关键沟通后必记**：每次会话有新决策或需求变化，追加到 `decisions.md`（带日期），必要时回写 `requirements.md`。
2. **每实现一个功能必配文档**：在 `features/` 下加一份（功能介绍 + 基本 demo + 验证结果 + 关联 ADR），与实现**同一提交**；篇幅大可在 `features/` 下建子文件夹分文档。改行为时同步更新。
3. **新主题独立成文**：某块需求展开到一定篇幅，从 `requirements.md` 拆出独立文件，在本索引登记。
4. **新会话先读文档**：上下文窗口可能丢失，新会话先读 `design.md`（设计方向）+ `requirements.md` + `decisions.md` 恢复全部背景。
