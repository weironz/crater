# rustfs —— S3 兼容对象存储(二进制 + systemd 交付)

按[官方 Linux 安装路线](https://docs.rustfs.com.cn/installation/)以 musl 静态二进制 +
systemd 托管部署 [RustFS](https://github.com/rustfs/rustfs)。**三种部署形态同一份 yaml**,
由两个 apply 参数驱动,环境配置进 inventory `vars:`(D-082),制品环境无关:

- `volumes` —— 官方 `RUSTFS_VOLUMES` 表达式,1:1 照抄文档
- `data_dirs` —— 本机数据目录列表(空格分隔),显式声明、不从 volumes 推导

```bash
# ① 单机单盘(默认:/data/rustfs0)
crater apply rustfs --host 10.0.0.5 --password <pw>

# ② 单机多盘(4 盘)
crater apply rustfs --host 10.0.0.5 --password <pw> \
  --set 'volumes=/data/rustfs{0...3}' \
  --set 'data_dirs=/data/rustfs0 /data/rustfs1 /data/rustfs2 /data/rustfs3'

# ③ 多机多盘(4 节点 × 4 盘):环境全在 inventory 里,命令零 --set
#    (volumes/data_dirs/凭据写进 inventory vars,见 inventory.example.yaml)
crater apply rustfs -i my-inventory.yaml

crater delete rustfs ... --set/-i <同部署时>   # 卸载(连数据目录一起删!)
just push                                      # 构建 OCI 制品并推 registry
```

要点:

- **zip 上游,目标机零依赖**:上游只发 zip,物料声明 `unzip: rustfs`(D-103)——
  控制端解包出二进制,build/在线/离线一路只见纯二进制,目标机不需要 unzip。
- **amd64 + arm64 双物料变体**、musl 静态 → 不需要 `requires:`(发行版/架构都不限);
  一个制品推 x86 机群和 ARM 机群都行,apply 按目标 `uname -m` 自动选。
- **显式优于推导**:`data_dirs` 直接声明本机目录,任务里没有解析 `{a...b}` 区间的
  shell 黑魔法;与 `volumes` 不一致时 rustfs 启动自会报错。多机形态每节点本地盘
  路径相同(官方 MNMD 约定),所以 `data_dirs` 放全局 vars 一处定义即可。
- preflight 端口闸(`wait_for state: stopped` + `loop: [9000, 9001]`,D-104/105):
  **被占就直接拒**,不管占用者是谁(严格语义)。这意味着**对在跑实例重复 apply 也会
  被自己占的端口拒掉**——要改配置/重铺,先 `crater delete`(teardown 不走此闸)或
  `systemctl stop rustfs` 再 apply。
  verify 先 `wait_for` 等端口开门,再核 `systemctl is-active` 身份、探 `/health`。
- 多机主机名(`volumes` 里的 node1..N)需在**每台节点**可解析(/etc/hosts 或 DNS),
  用 IP 区间表达式则免;时间同步、节点间 9000 互通(官方要求,见示例 inventory 注释)。
- 端口:S3 端点 `:9000`(`port` 参数),Web 控制台 `:9001`(上游默认,路径
  `/rustfs/console/`);日志 `/var/logs/rustfs/`(上游约定路径)。
- 凭据默认非上游默认值(`crater-admin`/`crater-changeme`),生产用 inventory `vars:`
  或 `--set` 覆盖(D-082)。
- `bypass_disk_check=true` 对应上游 `RUSTFS_UNSAFE_BYPASS_DISK_CHECK`:多盘目录落在
  **同一块物理盘**时(测试 VM)必需,生产保持 false——rustfs 会拒绝同盘多目录组纠删码。
- 版本:`version` 是 build 参数(GitHub release tag,默认 `1.0.0-beta.7`);
  换版本 `crater build --set version=X` 重建制品(D-093)。

已知边界:

- **多机首启竞态**:首次 apply 时 config/unit 必然 changed → run 末 handler 重启服务,
  多台重启时刻不完全同步,可能撞上集群初始格式化窗口报 `inconsistent drive found`。
  补救:全节点 `systemctl stop rustfs` → 清空数据目录 → 同时 start。

真机验证(2026-06-05,Ubuntu 24.04 ×2):三形态 apply/幂等/plan/delete 全过;
多机 2 节点×2 盘组真集群,S3 sigv4 **n11 写对象 → n12 读回**;build 495MB 双架构制品
→ save → 离线 apply(blob 与在线预取同 sha,内容寻址互证)。
