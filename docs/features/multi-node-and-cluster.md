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

## 角色非对称拓扑(D-071)—— 一份 task 跑 单节点/1主N从/HA多主多从

D-030 的 fan-out + register/hostvars 之上,D-071 补齐了**按角色分流**,让一份 task 表达
不对称集群(k8s:control 跑 init、worker 跑 join、HA 还要额外 master control-plane join):

- **`when_role: [..]`**(`ActionStep` 与 `register` 都支持):步骤/fact 只在持该角色的主机跑。
  公共步骤(containerd/kubelet/…)不写 when_role → 全跑;`kubeadm init` 标 `[bootstrap]`、
  `kubeadm join` 标 `[worker]`。闭合枚举,守 D-036。
- **`{{ groups.<role> }}`**:inventory 里该角色所有主机地址(空格连接),注入渲染——
  如 haproxy backend = `{{ groups.controlplane }}`(自动列出所有 master IP)。
- **角色键 hostvars**:某主机 register 的 fact 额外发布 `hostvars.<role>.<name>`,
  单例角色(bootstrap)免写死主机名:master/worker 用 `{{ hostvars.bootstrap.join }}`。
- **`serial_roles: [..]`**(task 顶层):角色集命中的组 `forks=1` 逐台执行——
  control-plane join 必须串行(防 etcd quorum 抢)。
- **`{{ inventory_hostname }}` / `{{ inventory_addr }}`**:目标机自身 inventory 名/地址,
  给 kubeadm `--node-name` 等需要稳定唯一值的场景(防同名节点冲突)。

完整例子:[`tasks/k8s-ha.yaml`](../../tasks/k8s-ha.yaml)(全 material 离线、可 build OCI)。

## 验证

- 多节点 yq(两台 .11/.12):并行、各自独立幂等;跨节点 register/hostvars 真机过(D-041)。
- **3-master HA k8s(.11/.12/.13 + VIP .14,全离线,D-071)**:`crater apply -i inventory.yaml`
  → 3 节点 Ready、etcd 3 成员 quorum、apiserver 经 VIP:8443 readyz passed。when_role
  (n11=45 步 / n12·n13=41 步)、groups(haproxy 后端)、角色键 hostvars(cp_join)、serial 全生效。

## 边界 / 后续

- 跨组顺序靠 role 首次出现序(未显式声明 role 依赖);register 未支持 `no_log`;组内 host 失败不停其对等但整体 apply 失败。
- HA 入口 keepalived+haproxy 见 `tasks/k8s-ha.yaml`(co-located systemd,非 static pod)。
- 运行时:containerd 不一定动态加载 flannel 的 CNI 配置,脏状态节点需 `restart containerd`(干净首装无此问题)。
