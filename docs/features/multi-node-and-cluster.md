# 多节点 + 跨节点 register/hostvars + 并发

> ADR: D-030（register/hostvars）/ D-031（并发）/ D-041（task 模型下）｜ 设计: [design.md](../design.md)
>
> 注：task 模型下 register/hostvars 见 [action-tasks.md](action-tasks.md)；下文 demo 沿用同机制。

## 这是什么

- **多主机 fan-out**：inventory 列多台 host，`apply` 按 `roles`/`hosts:` 组过滤分发；逐主机独立幂等。
- **跨节点 fact（register/hostvars）**：task 顶层 `register: [{name, cmd}]` 在某 host 跑完后由控制端捕获 stdout → `hostvars[host][name]`；其它 host 用 `{{ hostvars.<host>.<name> }}` 引用。这是**真集群**(leader 注册 token、follower join)的钥匙。host 按 role 分组、组间串行（producer 组在前）。
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

> 真实集群形成(leader 注册 token → follower join)见 `examples/cross-node-task.yaml`(task 版,D-041)。

## 验证（两台真机 192.168.73.11/.12）

- 多节点 yq：两台并行、各自独立幂等。
- 跨节点：leader register IP/token → follower 经 `{{hostvars.<leader>.*}}` 收到（task 模型,D-041 真机验证）。

## 边界 / 后续

- 跨组顺序靠 role 首次出现序（未显式声明 role 依赖）；register 未支持 `no_log`；组内 host 失败不停其对等但整体 apply 失败。
