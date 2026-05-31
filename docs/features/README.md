# Crater 功能文档

> 每实现一个功能就加一份文档：**功能介绍 + 基本 demo（可复现命令）+ 验证结果 + 关联 ADR**。
> 篇幅大的功能可在本目录下再建子文件夹分多文档。命令用占位符 `<host>`/`<pw>`。

## 索引

| 功能 | 文档 | ADR |
|------|------|-----|
| SSH 执行器（russh）+ run/push | [ssh-executor.md](ssh-executor.md) | D-008/D-009 |
| 引擎零产品知识（装万物地基） | [engine-zero-product-knowledge.md](engine-zero-product-knowledge.md) | D-017 |
| 幂等回显 + apply 默认执行 | [idempotency-and-apply.md](idempotency-and-apply.md) | D-023/D-024 |
| spec 内联 recipe（单文件） | [inline-recipe.md](inline-recipe.md) | D-025 |
| module 模块化（数据定义） | [modules.md](modules.md) | D-029 |
| 自举 agent（默认执行模型） | [self-bootstrap-agent.md](self-bootstrap-agent.md) | D-019/026/027 |
| 多节点 + 跨节点 fact + k3s 集群 + 并发 | [multi-node-and-cluster.md](multi-node-and-cluster.md) | D-030/D-031 |
| 离线 OCI（build/save/load/pull） | [offline-oci.md](offline-oci.md) | D-018 |
| 镜像管理（images/pull/push/login + apply &lt;ref&gt;） | [images-registry.md](images-registry.md) | D-018 |
| 在线部署（以 yq 为例，最小可复现） | [../../examples/yq/demo-online-yq.md](../../examples/yq/demo-online-yq.md) | — |

## 约定

- 新功能落地后**同一提交**补文档；改行为时同步更新对应文档。
- 每份文档结构：① 这是什么/解决什么 ② 基本 demo（命令 + 期望输出）③ 真机验证结果（如有）④ 边界/后续 ⑤ 关联 ADR/设计文档。
- 设计层面的"为什么"放 [decisions.md](../decisions.md)（ADR）与 [design.md](../design.md)；本目录是"怎么用 + 实测"。
