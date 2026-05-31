# 离线 OCI（build / save / load / pull）

> ADR: D-018 ｜ 详细设计: [offline-format.md](../offline-format.md)

## 这是什么

离线包 = **合规 OCI Image Layout**（`oci-layout` + `index.json` + `blobs/sha256/<digest>`，内容寻址，digest 即校验）。crater **自备** build/save/load——目标机**零容器运行时**（不靠 ctr/docker）。

| 能力 | 命令 | 说明 |
|---|---|---|
| **build** | `crater build [--image] -f spec -o x.oci` | 制离线包；`--image` 把组件文件产物封装成 rootfs OCI 镜像 `crater/<name>:<ver>` |
| **save** | （build 产物） | oci-archive（纯 tar），skopeo/oras 可读 |
| **load+install** | `crater deploy --bundle x.oci --host <host>` | crater 自己解包：制品 push-from-blob / rootfs 镜像 `tar -xpf -C /` |
| **pull** | （build 时，组件 `images:`） | `oci-client` 从 registry 拉容器镜像 blob 进包（rustls 纯 Rust） |

两条离线落地路径，按组件性质选：
- **制品/文件型**（yq、二进制）：`--image` 封装 rootfs 镜像 → load 时展开到 `/`。
- **daemon 型**（node_exporter 等需 systemd）：recipe-replay（download→push-blob、systemd_unit 等照常执行）。

## 基本 demo

**把 yq 封装成 OCI 镜像 → 离线安装**：
```bash
crater build --image -f examples/yq/yq.yaml -o /tmp/yq.oci   # build+save：crater/yq:4.53.2（rootfs 层）
# 分发 yq.oci 到离线机器，然后：
crater deploy --bundle /tmp/yq.oci --host <host> --password <pw>   # load+install（crater 自解包展开到 /）
```
期望：`load image crater/yq:4.53.2 → extracting rootfs to /` → `verify: yq --version → v4.53.2`。

**daemon 型离线**（node_exporter，recipe-replay）：
```bash
crater build -f examples/node_exporter.yaml -o /tmp/ne.oci
crater deploy --bundle /tmp/ne.oci --host <host> --password <pw>  # push(offline) 制品 + systemd 起服务
```

**查看包结构**（验证 OCI 合规）：
```bash
tar -xf /tmp/yq.oci -C /tmp/x && cat /tmp/x/oci-layout /tmp/x/index.json && ls /tmp/x/blobs/sha256/
```

## 验证（真机 192.168.73.12）

- node_exporter 离线（增量①）：`push(offline)` → `:9100` 出 metrics，`changed=4 ok=3`。
- yq 封装为 OCI 镜像离线（增量②）：rootfs 层展开到 `/`、`/usr/local/bin/yq` -rwxr-xr-x、`yq --version` v4.53.2。
- 包结构经校验：`oci-layout`/`index.json`(带 `org.crater.manifest` 注解)/`blobs/sha256` 齐全，镜像 manifest 引用 config+layers。

## 边界 / 后续

- registry **push**；容器镜像 import 到运行时（如确需）；临时 registry 多节点分发；多 arch rootfs；镜像签名（N4）。
