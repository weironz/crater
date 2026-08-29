# k8s-ha 真机验证记录

## 环境

VMware Workstation 上 5 台 Ubuntu 24.04(2 vCPU / 4G),控制端经 SSH 隧道接入
(inventory 的 `address` 是 `127.0.0.1:22xx`,`vars.ip` 才是同伴地址 —— 这个
差异本身就验到了一条机制,见下)。

```
crater build -f library/k8s/k8s-ha.blueprint.yaml -o k8s.closure.tar \
  --for arch=amd64 --for distro=ubuntu --for version=24.04      # 299.5 MiB / 39s

crater apply library/k8s/k8s-ha.blueprint.yaml -i inventory.yaml \
  --closure k8s.closure.tar \
  --set ha=true --set vip=192.168.73.100 --set cp_endpoint=192.168.73.100:8443
```

## 结果

| 判据 | 结果 |
| --- | --- |
| 节点 | 3 control-plane + 2 worker,**全 Ready**,v1.36.1 |
| etcd 成员 | **3 个 started,无 learner 残留**,peer 地址均为真实 `192.168.73.x` |
| 控制面 | 3×apiserver / 3×etcd / 3×controller-manager / 3×scheduler,19/19 Pod Running |
| VIP | `192.168.73.100` 仅在 cp1(VRRP 正确),网卡 **`ens33`** |
| HA 前端 | 从 cp3 `curl -k https://192.168.73.100:8443/healthz` → `ok` |
| haproxy 后端 | 三台真实 IP,由 `fleet.controlplane` 渲染 |
| 工作负载 | 4 副本 Deployment 全 Running,跨 4 个节点调度 |
| 幂等 | 重跑 `apply`:5 台全 `+0 ~0 -0`,126 项资源全绿 |
| verify | 干净集群 5 台全绿,**零假阳性** |
| 漂移检测 | 注入 3 处漂移 → **恰好报 3 项**,其余 4 台不受影响 |
| 自愈 | `apply` 修复 3 项 + 传导重启 3 个下游服务,verify 复归全绿 |

### `throttle: 1` 在真 kubeadm 上的证据

```
cp2 join  15:31:43
cp3 join  15:32:40    ← 相隔 57s,严格逐台
w1  join  15:32:56
w2  join  15:32:57
```

这比自检里的 `sleep 4` 有分量:真 join 会写 etcd,两台同时 join 会撞上 raft
配置变更。间隔说明限流确实把它们排开了。

### 上游传导替代 handler

自愈那次,除了被改动的 3 项,`containerd` / `kubelet` / `haproxy` 也被重启 ——
sysctl 与模板变了,下游服务自然需要重启。蓝图里**没有一个 `notify`**。

## 这次部署打回来的六个缺陷

真机验证的价值全在这里。六个都是单测和假目标覆盖不到的:

1. **模板拿不到 `fleet`** —— 实现渲染时刻意排除了它("fleet 是定址用的,渲染里
   没有意义"),haproxy 后端列表当场证伪:任何"这一台要知道其余台是谁"的配置
   只能从机群视角渲染。
2. **连接地址 ≠ 同伴地址** —— `Member` 此前连地址都没有。走隧道时把
   `127.0.0.1` 写进 apiserver 后端,会得到一个谁都连不上的集群。
3. **`package` 不刷新索引** —— apt 索引比仓库旧时抓已归档的 .deb,得到 404。
   改为失败后刷新重试一次。
4. **报错指向了已经改好的文件** —— 改模板没重烤闭包时渲染的是旧内容,而错误
   却指向磁盘上正确的那份。现在报错写明字节来自闭包还是本地文件。
5. **`kubelet` 声明了不可满足的期望态** —— 它在 kubeadm 写配置前起不来,
   `state: started` 让 plan 长期挂着一条注定为红的项。改为只声明 `enabled`。
6. **网卡名不能写死** —— keepalived 绑错网卡**不报错,只是永远不接管**。
   新增 `substrate.iface` 事实(本机实测是 `ens33`,不是 `eth0`)。

另有一条来自机群自检:跨主机比时间戳的精度下限是**节点间时钟偏差**(实测
124ms),判据必须带容差。由此新增 `timezone` / `time_sync` 两个类型,
`time_sync` 分别观察"启用了同步"与"已经同步上"—— 后者才是 etcd 仲裁吃的。
