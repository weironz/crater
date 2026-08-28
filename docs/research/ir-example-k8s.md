# 试金石 ②:k8s-ha 重写为 IR(最难的一个)

> 承接 [ir-draft.md](ir-draft.md) 与 [ir-example-rustfs.md](ir-example-rustfs.md)。对照物:
> [library/k8s/k8s-ha.yaml](../../library/k8s/k8s-ha.yaml)(现行,**519 行**)+
> [roles/kube-upgrade](../../library/k8s/roles/kube-upgrade/role.yaml)(升级 role)。
> 这个案例存在的意义:它是**唯一能证伪"状态/过程分离"的案例**——kubeadm init/join
> 不是收敛,是一支必须按拍子跳的舞(首台 init → 传 token/certkey → 其余逐台 join)。

---

## 1. 核心裁定:哪些是资源,哪些是舞

拆开看,519 行里其实是**两类完全不同的东西**被平铺在一个 `actions:` 列表里:

| 内容 | 本质 | 归属 |
|---|---|---|
| 主机名/swap/sysctl/内核模块/目录 | 期望态 | **资源**(可 observe/diff/destroy) |
| containerd + runc + cni + crictl + 配置 + unit + 服务 | 期望态 | **资源** |
| kubeadm/kubelet/kubectl 二进制 + kubelet unit | 期望态 | **资源** |
| keepalived + haproxy + 配置 + 服务(VIP) | 期望态 | **资源** |
| 镜像导入 | 期望态(镜像在不在本地) | **资源** |
| **kubeadm init → upload-certs → token → cp_join(逐台)→ worker join** | **过程** | **procedure** |
| **kubeadm reset → 杀 shim 残留 → 清目录** | **过程** | **procedure(destroy)** |
| 升级(drain → 换二进制 → upgrade apply/node → uncordon) | **过程** | **procedure(upgrade)** |

现行写法把这三支舞拆成了 `run_once` + `throttle` + `check` 守卫 + 可等待 fact 四种机制
散在 actions 里(注释里那句"needs kubeadm_init: on the init node this orders cp_join AFTER
init so its check skips it"——一行代码要三行注释解释,就是模型不对位的信号)。

## 2. 重写结果

### 2.1 blueprint 主体(资源部分)

```yaml
name: k8s-ha
version: 1.36.1
params:
  version:  { default: "1.36.1", stage: build }
  vip:      { type: ip,   desc: 控制面 VIP(keepalived 浮动) }
  cp_endpoint: { default: "${params.vip}:8443" }
  subnet:   { type: cidr, desc: VIP 所在子网(选网卡用) }
  pod_cidr: { default: "10.244.0.0/16" }
  image_repo: { default: "registry.aliyuncs.com/google_containers" }
  # 组件版本(containerd/runc/cni/crictl/etcd/coredns/pause/flannel)略,同现行 vars,stage: build

materials:  # 同现行(二进制×7、清单、配置文件×9、镜像×9),url 里 {{x}} → ${params.x}
  ...

requires: { os: [{ distro: ubuntu, versions: ["24.04"] }], arch: [amd64] }

resources:
  # ---- 主机基线 ----
  - hostname: { name: "${substrate.name}" }
  - swap:     { state: disabled, persist: true }          # 取代 swapoff + sed /etc/fstab 两步
  - kernel_modules: { load: [overlay, br_netfilter], persist: true }
  - sysctl:   { from_material: cfg-sysctl }               # 取代 copy + `sysctl --system` + check
  - package:  { material: sysdeps }

  # ---- containerd 栈 ----
  - unarchive: { material: containerd, to: /usr/local, creates: /usr/local/bin/containerd }
  - copy:      { material: runc, dest: /usr/local/sbin/runc, mode: "0755" }
  - unarchive: { material: cni-plugins, to: /opt/cni/bin, creates: /opt/cni/bin/bridge }
  - file:      { path: /etc/cni/net.d, state: directory }  # 必须早于 containerd 启动(D-076)
  - unarchive: { material: crictl, to: /usr/local/bin, creates: /usr/local/bin/crictl }
  - copy:      { material: cfg-crictl,     dest: /etc/crictl.yaml }
  - copy:      { material: cfg-containerd, dest: /etc/containerd/config.toml }
  - systemd_unit: { name: containerd, from_material: unit-containerd }
  - service:   { name: containerd, state: started, enabled: true }   # 上游 changed 自动触发 restart

  # ---- kube 二进制 ----
  - copy: { material: "${item}", dest: "/usr/local/bin/${item}", mode: "0755" }
    each: [kubeadm, kubelet, kubectl]
  - systemd_unit: { name: kubelet, from_material: unit-kubelet, dropins: [dropin-kubeadm] }
  - service: { name: kubelet, state: started, enabled: true }

  - image_present: { materials: [img-apiserver, img-cm, img-scheduler, img-proxy,
                                 img-coredns, img-pause, img-etcd, img-flannel, img-flannel-cni],
                     namespace: k8s.io }

  # ---- 控制面 LB(VIP) ----
  - package:  { material: lb-pkgs }
    target: role.controlplane                                  # ← Selector 取代 when_role
  - template: { material: cfg-haproxy,   dest: /etc/haproxy/haproxy.cfg }
    target: role.controlplane
  - template: { material: cfg-keepalived, dest: /etc/keepalived/keepalived.conf }
    target: role.controlplane
    # 模板里直接写 ${substrate.iface_in(params.subnet)} —— 目标侧 fact,渲染时求值,
    # 取代现行"写 __IFACE__ 占位符 + 一步 sed 回填"(见 §3-C)
  - service: { name: haproxy,    state: started, enabled: true }
    target: role.controlplane
  - service: { name: keepalived, state: started, enabled: true }
    target: role.controlplane

  # ---- 集群成员资格:状态在这里,舞在 procedure 里 ----
  - cluster_member: { role: control-plane }
    target: role.controlplane
  - cluster_member: { role: worker }
    target: role.worker

health:
  - cmd: "kubectl get nodes -o wide"
    target: first(role.controlplane)                           # ← 取代 run_once
    env: { KUBECONFIG: /etc/kubernetes/admin.conf }
  - node_ready: "${substrate.name}"                        # 每台自查在册且 Ready
```

### 2.2 blueprint 自定义资源类型(舞被封装的地方)

```yaml
# 五动词由 procedure 实现 —— 这是 next-gen §6 的 L2「数据模块」层,
# 引擎不需要知道 kubeadm 是什么(D-017 守住)
types:
  - name: cluster_member
    args: { role: { type: enum[control-plane, worker] } }

    observe:                                   # 只读:我在册吗?
      cmd: "test -f /etc/kubernetes/kubelet.conf && echo joined || echo absent"
      parse: { joined: "joined" }

    apply: procedure bootstrap                 # 不在册 → 跳这支舞
    destroy: procedure reset

procedures:
  bootstrap:
    steps:
      # ① VIP 就绪才动 kubeadm(haproxy 恒听 8443 ⇒ TCP 通 = VIP 在位)
      - wait: { port: 8443, host: "${params.vip}", timeout: 30s }
        target: role.controlplane

      # ② 首台 control-plane:init + 发布两个 fact
      - shell: "kubeadm init --control-plane-endpoint ${params.cp_endpoint} --upload-certs
                --pod-network-cidr=${params.pod_cidr} --image-repository ${params.image_repo}
                --kubernetes-version v${params.version}
                --cri-socket=unix:///run/containerd/containerd.sock"
        check: "test -f /etc/kubernetes/admin.conf"
        target: first(role.controlplane)
        exports:                               # ← 取代顶层 register:,就近声明
          join:    "kubeadm token create --print-join-command"
          certkey: "kubeadm init phase upload-certs --upload-certs | tail -1"

      - shell: "kubectl apply -f /etc/kubernetes/kube-flannel.yml"     # CNI
        check: "kubectl -n kube-flannel get ds/kube-flannel-ds"
        target: first(role.controlplane)
        env: { KUBECONFIG: /etc/kubernetes/admin.conf }

      # ③ 其余 control-plane:逐台 join(护 etcd);facts.* 未就绪时引擎自动阻塞
      - shell: "${facts.join} --control-plane --certificate-key ${facts.certkey}
                --cri-socket=unix:///run/containerd/containerd.sock"
        target: rest(role.controlplane)            # ← 取代「全组跑 + check 守卫跳过首台」
        strategy: { throttle: 1 }

      # ④ worker:并行 join
      - shell: "${facts.join} --cri-socket=unix:///run/containerd/containerd.sock"
        target: role.worker

      - shell: "mkdir -p /root/.kube && cp -f /etc/kubernetes/admin.conf /root/.kube/config"
        check: "test -f /root/.kube/config"
        target: role.controlplane
      - shell: "kubectl taint nodes --all node-role.kubernetes.io/control-plane- || true"
        target: first(role.controlplane)
        env: { KUBECONFIG: /etc/kubernetes/admin.conf }

  reset:                                       # destroy 的舞(其余资源的 destroy 是推论)
    steps:
      - shell: "kubeadm reset -f --cri-socket=unix:///run/containerd/containerd.sock || true"
      - shell: "crictl rm -f $(crictl ps -aq) 2>/dev/null; pkill -9 containerd-shim; true"
        # shim 保活会留下 apiserver/etcd 占端口 → 下次 join 撞 "Port 6443 in use"
      - shell: "rm -rf /etc/kubernetes /var/lib/kubelet /var/lib/etcd /root/.kube"

  upgrade:                                     # ← 现行是**另一个 role 文件**,这里内置
    params: { to: { type: version } }
    steps:
      - copy: { material: kubeadm@${params.to}, dest: /usr/local/bin/kubeadm, mode: "0755" }
      - shell: "kubectl drain ${substrate.name} --ignore-daemonsets --delete-emptydir-data --timeout=300s || true"
        target: role.controlplane
        strategy: { throttle: 1 }
      - shell: "kubeadm upgrade apply -y v${params.to}"
        target: first(role.controlplane)
        check: "kubeadm version -o short | grep -q v${params.to}"
      - shell: "kubeadm upgrade node"
        target: rest(role.controlplane)
        strategy: { throttle: 1 }
      - shell: "kubeadm upgrade node"
        target: role.worker
        strategy: { throttle: 1 }
      - copy: { material: "${item}@${params.to}", dest: "/usr/local/bin/${item}", mode: "0755" }
        each: [kubelet, kubectl]
      - service: { name: kubelet, state: restarted }
        strategy: { throttle: 1 }
      - shell: "kubectl uncordon ${substrate.name} || true"
        target: role.controlplane
```

**行数**:资源部分 ~60 行 + types/procedures ~70 行 ≈ **130 行 vs 519+60 行(-77%)**,
且**升级 role 被吸收进 blueprint**(`x upgrade k8s-ha --to 1.37`,不再是"另找一个 task 跑")。

## 3. 撞出来的 schema 修正

### A. Selector 必须是一等语法,`run_once` 该死

现行 `when_role: [controlplane] + run_once: true` 是"过滤器 + 布尔开关"的组合,而
`cp_join` 那步要表达"除首台外的其余 master"时,只能用**全组跑 + check 守卫自动跳过首台**
的诡计(注释三行解释)。选择器语法直接说人话:

```
all | role.<name> | first(<sel>) | rest(<sel>) | <sel> where <CEL> | host.<name>
```

**裁定**:`on: Selector` 取代 `when_role`/`run_once`;`first()`/`rest()` 内建。
`when:` 保留但只管**条件纳入**(与"在谁身上"正交)。

### B. 跨主机 fact 要就近声明(`exports:`),不是顶层 `register:`

现行 fact 声明在文件顶部 `register:`(带 `when_role`/`run_once` 重复一遍角色信息),
消费在 300 行之后——两处强耦合却相隔甚远。**裁定**:fact 由**产出它的那一步** `exports:`
声明,作用域自动继承该步的 `on:`;消费方写 `${facts.<name>}`,引擎按依赖自动阻塞等待。
lint 期可查:导出未被消费(告警)、消费无导出(报错)。

### C. 需要"目标侧 fact"参与模板渲染(现行只能控制端渲染 + sed 回填)

keepalived 要填本机网卡名——plan 期在控制端不可知,现行写 `__IFACE__` 占位符再用一步
`sed` 回填(还得配 `check: ! grep -q __IFACE__`)。**裁定**:CEL 作用域里的
`substrate.*` 包含**目标侧探测事实**(os/arch/hostname/网卡/IP/内存…,≈ ansible facts),
模板渲染在**已知这些事实之后**发生;探测项是封闭白名单 + 按需惰性采集(不做 ansible
式全量 setup 的开销)。这一条也顺手解决了"多节点 haproxy backend 列表"这类渲染。

### D. 自定义资源类型的五动词由 procedure 实现(L2 数据模块层落地)

`cluster_member` 证明了关键一点:**引擎不必懂 kubeadm**(D-017 守住),blueprint 可以
自定义类型,用 `observe: <只读 cmd/parse>` + `apply: procedure X` + `destroy: procedure Y`
补齐五动词。这让"状态/过程分离"不是空话——**用户面对的是"这台机器应当是集群成员"
(名词),舞被封在类型里**;操作者永远不写 init/join。
**裁定**:`types:` 进 v0 Blueprint schema(L2 层的正式形态)。

### E. 一批"仪式型 shell"应升为类型(与 rustfs 裁定 C 同向)

`swapoff -a` + `sed /etc/fstab`(2 步 + 2 个 check)→ `swap: {state: disabled, persist: true}`;
`modprobe` + `/etc/modules-load.d/`(2 步)→ `kernel_modules:`;
`copy sysctl.conf` + `sysctl --system` + check(2 步)→ `sysctl:`;
`hostnamectl` + check → `hostname:`。**裁定**:L1 内建清单据此追加
`swap`/`kernel_modules`/`sysctl`/`hostname`/`image_present`,与 rustfs 撞出的 `systemd_unit`
一起,构成"内建模块按类型化层次拟"的第一批证据。

### F. teardown 从 12 步降到 1 支舞

现行 teardown 12 步里,**10 步是各资源 destroy 的手写版**(停服务/删 unit/删二进制/删目录/
purge 包)——五动词契约下全自动逆序。只剩 `kubeadm reset` + `杀 shim 残留` 是真过程知识
(shim 保活导致端口占用这种坑,恰恰是**专家知识**,值得被封装在 blueprint 里)。

### G. 未撞到问题(继续冻结)

`each`/`when`/CEL 插值/materials/health/`strategy.throttle`/`deps` 隐式顺序——顺畅。
`phase:` 概念确认可删(preflight=断言、verify=health、install=资源默认)。

## 4. 操作者视角

```console
$ x install k8s-ha --env prod --set vip=192.168.73.14 --set subnet=192.168.73.0/24
plan: 3 substrates(controlplane×3, worker×0)
      +47 create,materials 28 层 1.9GB(air-gap 完备 ✓)
      procedure bootstrap 将执行:init(n11)→ join×2(逐台)
approve? y

$ x upgrade k8s-ha --to 1.37.0 --env prod        # 舞在 blueprint 里,不用找 upgrade role
plan: procedure upgrade —— drain/upgrade/uncordon × 3 台(throttle 1)
      新增 materials:kubeadm/kubelet/kubectl@1.37.0(3 层 218MB)
approve? y
```

## 5. 结论

IR 扛住了最难的案例:**状态/过程分离成立**,且分离后 519 行降到 ~130 行(-77%),升级
role 被吸收。新增 7 处裁定(A–F 需改,G 冻结),其中 **A(Selector)、B(exports)、
C(目标侧 fact)、D(types)** 是结构性的,已回填 ir-draft.md。

剩下第三块试金石(demo-stack 组合)只考 Stack 层 values 传递,风险低——可与 P0 实现同步做。
**IR v0 至此建议冻结**,进入 P0:按此 schema 写 parser + lint + 五动词 trait 骨架。
