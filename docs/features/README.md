# Crater 功能文档

> 每实现一个功能就加一份文档：**功能介绍 + 基本 demo（可复现命令）+ 验证结果 + 关联 ADR**。
> 篇幅大的功能可在本目录下再建子文件夹分多文档。命令用占位符 `<host>`/`<pw>`。
>
> **D-046 后单一 task 模型**：下列文档中「component / 组件」按 **task** 理解，命令以
> [README](../../README.md) / [action-tasks.md](action-tasks.md) 为准；早期文档（idempotency /
> engine-zero / modules / ssh-executor 等）的 component 措辞属历史，原理对 task 同样成立。

> **架构/设计理念**:整体定位、概念模型(对齐 Ansible)、离线打包设计、OCI artifact 结构、
> 与 Helm/kubespray 的关系,见 [../architecture.md](../architecture.md)(区分已实现/规划)。

## 索引

| 功能 | 文档 | ADR |
|------|------|-----|
| SSH 执行器（russh）+ run/push + host key 校验 | [ssh-executor.md](ssh-executor.md) | D-008/D-009/D-094 |
| 引擎零产品知识（装万物地基） | [engine-zero-product-knowledge.md](engine-zero-product-knowledge.md) | D-017 |
|  物料闭包 `materials:` + `copy material:` | [materials.md](materials.md) | D-034 |
| apply 三层目标（本机/--host/-i）+ SSH key 认证 | [apply-targets.md](apply-targets.md) | D-035 |
| 通用 task（`crater apply <动作>`，Ansible 能力；含离线打包/agent） | [action-tasks.md](action-tasks.md) | D-037~D-046 |
| 卸载/重置 `crater delete`（由 task `teardown:` 驱动，opt-in） | [delete-teardown.md](delete-teardown.md) | D-049 |
| 部署状态 `crater task list/show/history`（marker + 控制端 Turso 库） | [task-state.md](task-state.md) | D-051~053 |
| Web 看板 `crater ui`（Axum + htmx，只读，离线嵌入） | [web-ui.md](web-ui.md) | D-054 |
| YAML 纯数据铁律（残废模板渲染器） | （见 decisions） | D-036 |
| 幂等回显 + apply 默认执行 | [idempotency-and-apply.md](idempotency-and-apply.md) | D-023/D-024 |
| 内置模块（action 原语,与 Ansible 对齐） | [modules.md](modules.md) | D-067 |
| 角色 role（可复用子程序,数据定义） | [roles.md](roles.md) | D-029 |
| 自举 agent（默认执行模型，贯穿 task） | [self-bootstrap-agent.md](self-bootstrap-agent.md) | D-019/027/044 |
| 多节点 + 跨节点 register/hostvars + 并发 | [multi-node-and-cluster.md](multi-node-and-cluster.md) | D-030/D-031 |
| 离线 OCI（build/save/load/pull，recipe-replay） | [offline-oci.md](offline-oci.md) | D-018/033/045 |
| 镜像管理（images/pull/push/tag/login + apply &lt;ref&gt;） | [images-registry.md](images-registry.md) | D-018/033 |
| params 契约 + `--set` 覆盖（build/apply 分治 gate） | [params-and-set.md](params-and-set.md) | D-081/089/093 |

## 约定

- 新功能落地后**同一提交**补文档；改行为时同步更新对应文档。
- 每份文档结构：① 这是什么/解决什么 ② 基本 demo（命令 + 期望输出）③ 真机验证结果（如有）④ 边界/后续 ⑤ 关联 ADR/设计文档。
- 设计层面的"为什么"放 [decisions.md](../decisions.md)（ADR）与 [design.md](../design.md)；本目录是"怎么用 + 实测"。
