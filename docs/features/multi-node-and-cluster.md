# 多节点 + 跨节点 fact + k3s 集群 + 并发

> ADR: D-030（register/hostvars）/ D-031（并发）｜ 设计: [design.md §7.4](../design.md)

## 这是什么

- **多主机 fan-out**：spec 列多台 host，`apply` 逐组件按 `roles` 过滤分发；逐主机独立幂等。
- **跨节点 fact（register/hostvars）**：组件 `register: [{name, cmd}]` 在某 host 装完后由控制端捕获 stdout → `hostvars[host][name]`；其它 host 用 `{{ hostvars.<host>.<name> }}` 引用。这是**真集群**的钥匙（如 k3s join token）。主机按 inventory 顺序处理（producer 在前）。
- **并发（F17）**：hosts 按 role-set 分组，**组间串行**（保 register→消费序）、**组内并行**（`CRATER_FORKS`，默认 10）。

## 基本 demo

**多节点 + 并行**（`examples/multi-node.yaml`，两台同 role `[yq]`）：
```bash
crater apply -f examples/multi-node.yaml
# ▷ group [yq] — 2 hosts in parallel (forks=10)；两台同时启动，总时长≈max(各主机)
```

**跨节点传值**（`examples/cross-node.yaml`，leader register → follower 消费）：
```yaml
components:
  - name: producer
    install: [ { action: run_cmd, cmd: "echo token-$(hostname) > /tmp/t", check: "test -f /tmp/t" } ]
    register: [ { name: token, cmd: "cat /tmp/t" } ]
  - name: consumer
    install: [ { action: run_cmd, cmd: "echo 'got {{ hostvars.leader.token }}' > /tmp/r" } ]
```

**k3s 两节点真集群**（`examples/k3s-cluster.yaml`）：
```bash
crater apply -f examples/k3s-cluster.yaml
# server 装 k3s → register node-token+url → agent 用 {{hostvars.server.*}} join
crater run --host <server> --password <pw> -- "k3s kubectl get nodes"
```

## 验证（两台真机 192.168.73.11/.12）

- 多节点 yq：两台并行、各自独立幂等。
- 跨节点：leader register `token-from-ubuntu` → follower 经 `{{hostvars.leader.token}}` 收到。
- k3s 集群：`kubectl get nodes` 两节点全 Ready（`ubuntu` control-plane + `agent-192-168-73-12` worker）。
  - 踩坑：克隆 VM hostname 同为 `ubuntu`，k3s 拒重名 → 组件加 `K3S_NODE_NAME=agent-<ip>` 唯一化（纯数据修复）。

## 边界 / 后续

- 跨组顺序靠 role 首次出现序（未显式声明 role 依赖）；register 未支持 `no_log`；组内 host 失败不停其对等但整体 apply 失败。
