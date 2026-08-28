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
  公共步骤(containerd/kubelet/…)不写 when_role → 全跑;`kubeadm join` 标 `[worker]`。
  角色由 inventory 组成员推导(见 [inventory.md](inventory.md))。闭合枚举,守 D-036。
- **`run_once: true`**(`ActionStep` 与 `register`,D-077):只在「匹配 `when_role` 的**第一台**
  目标(inventory 顺序)」跑——即隐式 init 节点。仿 kubekey:**无独立 `bootstrap` 角色**,
  `controlplane` 组首台即 init。`kubeadm init`/flannel/去污点标 `when_role:[controlplane]+run_once`;
  其余 master 的 `cp_join` 标 `when_role:[controlplane]`(非 run_once,靠 `check:` 让 init 节点跳过)。
- **`throttle: N`**(`ActionStep`,D-077,仿 kubespray `throttle`):跑这一步的主机里同时最多 N 台
  (`1` = 逐台)。**通用并发原语,引擎不知 etcd 为何物**——`cp_join` 写 `throttle: 1` 即可让
  control-plane join 逐台(护 etcd learner 同步),而前置(传输/装 containerd)**全并行**。
  取代了过去整组 `serial_roles` 的粗粒度串行。
- **可等待 fact(cross-host)**:某步的命令插了 `{{ hostvars.<role>.<fact> }}` 而该 fact 由别的
  主机 register,引擎在执行这步前**阻塞等到生产者发布**(`HostCoord`,通用数据依赖,零产品知识)。
  于是 init 节点和其余 master **同组并行**跑前置,`cp_join` 自动等 init 的 token 出来再 join。
  超时(`CRATER_FACT_TIMEOUT`,默认 1800s)报错而非死等。一台**不会等自己产出的 fact**(防自锁)。
- **`{{ groups.<role> }}`**:inventory 里该角色所有主机地址(空格连接),注入渲染——
  如 haproxy backend = `{{ groups.controlplane }}`(自动列出所有 master IP)。
- **角色键 hostvars + 组内渐进式传递**:某主机 register 的 fact 额外发布 `hostvars.<role>.<name>`;
  serial 组内**每台跑完即合并供下一台**(D-077),故同一 `controlplane` 组里 init 节点注册的
  `hostvars.controlplane.join` 能被随后的 master/worker 直接用,无需拆角色制造组间 barrier。
- **`serial_roles: [..]`**(task 顶层,**遗留/粗粒度**):角色集命中的组整组逐台执行。优先用
  步骤级 `throttle` —— 它只串需要的那一步、前置仍并行。k8s-ha 已从 `serial_roles` 改用 `throttle`。
- **`{{ inventory_hostname }}` / `{{ inventory_addr }}`**:目标机自身 inventory 名/地址。

完整例子:[`library/k8s/k8s-ha.blueprint.yaml`](../../library/k8s/k8s-ha.blueprint.yaml)(新 IR blueprint;旧 task 版已删,见 git 历史)。

## 验证

- 多节点 yq(两台 .11/.12):并行、各自独立幂等;跨节点 register/hostvars 真机过(D-041)。
- **3-master HA k8s(.11/.12/.13 + VIP .14,全离线,D-071)**:`crater apply -i inventory.yaml`
  → 3 节点 Ready、etcd 3 成员 quorum、apiserver 经 VIP:8443 readyz passed。when_role
  (n11=45 步 / n12·n13=41 步)、groups(haproxy 后端)、角色键 hostvars(cp_join)、serial 全生效。

## 边界 / 后续

- 跨组顺序靠 role 首次出现序(未显式声明 role 依赖);register 未支持 `no_log`;组内 host 失败不停其对等但整体 apply 失败。
- 可等待 fact / `throttle` 只在**控制端 execute**(离线或 `--shell` 路径)生效;在线多节点走 agent 路径时不协调(HA 一律离线 build+apply,落在控制端 execute)。
- HA 入口 keepalived+haproxy 见 `tasks/k8s-ha.yaml`(co-located systemd,非 static pod)。
- 运行时:containerd 不一定动态加载 flannel 的 CNI 配置,脏状态节点需 `restart containerd`(干净首装无此问题)。
