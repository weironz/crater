# k8s —— Kubernetes 交付

主蓝图是 `k8s-ha.blueprint.yaml`，同时覆盖单节点、单控制面加 worker，以及多控制面 HA。

单节点只需一台 `controlplane`：

```bash
crater apply docker.io/willdockerhub/k8s:<tag> -i inventory.yaml
```

HA 默认使用每台控制面的本机 Nginx stream 代理。Nginx 仅监听 `127.0.0.1:8443`，把
控制面请求转发到 inventory 内所有 master；首台 init、其余 master 逐台经本机代理
join。无需 VIP、Keepalived、HAProxy 或 `cp_endpoint` 参数：

```bash
crater apply docker.io/willdockerhub/k8s:<tag> -i inventory.yaml --set ha=true
```

`controlplane` 至少应有 3 台以获得 etcd 仲裁。worker 不需要 Nginx；它们自动使用
首台控制面的真实地址 join。旧 VIP 方案仅为兼容保留：

```bash
crater apply docker.io/willdockerhub/k8s:<tag> -i inventory.yaml \
  --set ha=true --set ha_mode=vip \
  --set vip=192.168.1.100 --set cp_endpoint=192.168.1.100:8443
```
