# rustfs —— S3 兼容对象存储(二进制 + systemd 交付)

按[官方 Linux 安装路线](https://docs.rustfs.com.cn/installation/)以 musl 静态二进制 +
systemd 托管部署 [RustFS](https://github.com/rustfs/rustfs)。**三种部署形态同一份 yaml**,
由 `volumes` 一个参数驱动(与官方 `RUSTFS_VOLUMES` 语法 1:1):

```bash
# ① 单机单盘(默认:/data/rustfs0)
crater apply rustfs --host 10.0.0.5 --password <pw>

# ② 单机多盘(4 盘;目录区间用官方 {a...b} 语法)
crater apply rustfs --host 10.0.0.5 --password <pw> \
  --set 'volumes=/data/rustfs{0...3}'

# ③ 多机多盘(4 节点 × 4 盘;volumes 里的每个节点都要在 inventory 里)
crater apply rustfs -i inventory.yaml \
  --set 'volumes=http://node{1...4}:9000/data/rustfs{0...3}'

crater delete rustfs ... --set 'volumes=<同部署时>'   # 卸载(连数据目录一起删!)
just push                                            # 构建 OCI 制品并推 registry
```

要点:

- **zip 上游,目标机零依赖**:上游只发 zip,物料声明 `unzip: rustfs`(D-103)——
  控制端解包出二进制,build/在线/离线一路只见纯二进制,目标机不需要 unzip。
- **amd64 + arm64 双物料变体**、musl 静态 → 不需要 `requires:`(发行版/架构都不限);
  一个制品推 x86 机群和 ARM 机群都行,apply 按目标 `uname -m` 自动选。
- 本机数据目录从 `volumes` **推导**(展开 `{a...b}`、剥 `http://host:port` 前缀),
  多机形态每节点本地盘路径相同(官方 MNMD 约定)。
- preflight 三道闸:systemd 可用;**端口未被外人占**(典型:旧容器化部署的 rustfs
  还在跑——它会冒名顶替答 200,必须先 `docker rm -f rustfs`);多机主机名全部可解析
  (纯 IP 跳过)。verify 先核 `systemctl is-active` 身份再探 `/health`。
- 端口:S3 端点 `:9000`(`port` 参数),Web 控制台 `:9001`(上游默认,路径
  `/rustfs/console/`);日志 `/var/logs/rustfs/`(上游约定路径)。
- 凭据默认非上游默认值(`crater-admin`/`crater-changeme`),生产用 inventory `vars:`
  或 `--set` 覆盖(D-082);改凭据/端口/volumes → notify 自动重启收敛。
- `bypass_disk_check=true` 对应上游 `RUSTFS_UNSAFE_BYPASS_DISK_CHECK`:多盘目录落在
  **同一块物理盘**时(测试 VM)必需,生产保持 false——rustfs 会拒绝同盘多目录组纠删码。
- 版本:`version` 是 build 参数(GitHub release tag,默认 `1.0.0-beta.7`);
  换版本 `crater build --set version=X` 重建制品(D-093)。

已知边界:

- **多机首启竞态**:首次 apply 时 config/unit 必然 changed → run 末 handler 重启服务,
  多台重启时刻不完全同步,可能撞上集群初始格式化窗口报 `inconsistent drive found`。
  补救:全节点 `systemctl stop rustfs` → 清空数据目录 → 同时 start(或重跑一遍 apply,
  配置无变更不再触发重启)。
- 多机形态要求时间同步(`timedatectl` 查)与节点间 9000 互通,官方建议 XFS、禁 NFS。

真机验证(2026-06-05,Ubuntu 24.04 ×2):三形态 apply/幂等/plan/delete 全过;
多机 2 节点×2 盘组真集群,S3 sigv4 **n11 写对象 → n12 读回**;build 495MB 双架构制品
→ save → 离线 apply(blob 与在线预取同 sha,内容寻址互证)。
