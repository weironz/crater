# Inventory

`-i inventory.yaml` 告诉 crater **在哪些机器上跑**(task 自带 recipe,inventory 只管目标)。
仿 kubekey/Ansible:**host 只写连接信息,成员/角色集中在可嵌套的 `groups`**(D-077)。

## 结构

```yaml
inventory:
  hosts:                      # 目标机:每台 name + address,认证用 password 或 key(key 优先)
    - name: n11
      address: 192.168.73.11  # SSH 连接地址
      user: root              # 默认 root
      port: 22                # 默认 22
      password: "123456"      # 或 key: ~/.ssh/id_rsa
    - name: n12
      address: 192.168.73.12
      password: "123456"
    - name: n13
      address: 192.168.73.13
      password: "123456"

  groups:                     # 成员/角色:每组列 hosts:(主机名)和/或 groups:(子组),可嵌套
    k8s_cluster:
      groups: [controlplane, worker]   # 嵌套:聚合两个子组
    controlplane:
      hosts: [n11, n12, n13]
    worker:
      hosts: []
```

## 角色由组成员推导(无需在 host 上写 roles)

加载时 `derive_roles()` 按组成员(含嵌套**向上传播**)给每台算出 `roles`:

| 主机 | 推导出的角色 |
|---|---|
| n11 / n12 / n13 | `controlplane`、`k8s_cluster` |

- task 的 `hosts:`、`when_role:`、`{{ groups.<role> }}` 都按**组名**匹配(组名即角色名)。
- 嵌套向上传播:control-plane 主机自动也带父组 `k8s_cluster`,故 `hosts: k8s_cluster` 命中全体。
- 一台机可属于多个组,角色 = 所属组的并集。
- 仍兼容在 host 上内联 `roles: [...]`(与组推导取并集),`--host a,b` 临时清单不受影响。

## 与拓扑的关系

k8s-ha 不需要 `bootstrap`/`master` 角色:**`controlplane` 组第一台**(inventory 顺序)即隐式 init 节点
(仿 kubekey `init_kubernetes_node = kube_control_plane[0]`),由 task 里的 `run_once` 表达。

- 单节点:`controlplane: { hosts: [n11] }`
- 1 主 N 从:`controlplane: { hosts: [n11] }` + `worker: { hosts: [w1, w2] }`
- HA 多主多从:`controlplane` 多台 + `worker` 多台

详见 [multi-node-and-cluster.md](multi-node-and-cluster.md)。生成样例:`crater create inventory`。

## 三级 vars(D-082):环境配置出 OCI

inventory 可在三个层级提供变量,覆盖 task 的 `params` 默认 —— 这样**环境特定值
(vip/网段/端口)放 inventory,OCI 保持与环境无关**(同一 OCI 到处部署)。

```yaml
inventory:
  vars:                          # ① 全局:对所有主机
    vip: "192.168.73.14"
    pod_cidr: "10.244.0.0/16"
  hosts:
    - name: n11
      address: 192.168.73.11
      vars: { node_label: "edge" }   # ③ 主机:最高优先级
  groups:
    controlplane:
      hosts: [n11, n12, n13]
      vars: { apiserver_port: "6443" }   # ② 组:覆盖全局,被主机覆盖
```

**优先级:主机 > 组 > 全局 > task params 默认**。一台属多个组时,组 vars 按组名排序
合并(冲突用主机 vars 兜底)。加载时 `Inventory::resolve()` 把三层合并进每台 `host.vars`,
apply 时叠加到渲染上下文(`{{vip}}` 等)。

用 `crater inspect <task>` 看哪些是 apply 期参数(该放 inventory);`--gen-inventory`
直接吐带这些 vars 的骨架。
