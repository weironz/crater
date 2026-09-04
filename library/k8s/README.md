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

## 离线交付

inventory 必须静态声明目标画像；这不是 SSH 探测结果，而是选择离线依赖闭包的
键。示例见 `inventory.example.yaml`：

```yaml
inventory:
  platform: { os: ubuntu, version: "24.04", arch: amd64 }
```

发布方仍只推一个公开 tag；每个支持画像的完整闭包作为该 OCI 制品内部的 Site
Seed 层。构建多个画像时重复 `--seed-inventory`，不会产生面向用户的 tag 矩阵：

```bash
crater push library/k8s docker.io/willdockerhub/k8s:1.36.6 \
  --seed-inventory ubuntu-24-amd64.inventory.yaml
```

联网的准备机只下载该 inventory 命中的一份 Seed：

```bash
crater pull docker.io/willdockerhub/k8s:1.36.6 --offline -i inventory.yaml
crater save docker.io/willdockerhub/k8s:1.36.6 -o k8s-ubuntu24-amd64.pkg.tar
```

`save` 此时导出的就是这一份选择后的完整 OCI 包；拷到隔离环境后照常 `load`，
再部署。`apply --offline` 不访问 registry 或物料源，只在连接目标后只读校验
实际 `arch + distro + version` 是否与 inventory 平台声明一致：

```bash
crater load k8s-ubuntu24-amd64.pkg.tar
crater apply docker.io/willdockerhub/k8s:1.36.6 --offline -i inventory.yaml
```
