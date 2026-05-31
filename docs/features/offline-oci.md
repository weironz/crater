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

## 构建逻辑：vs Dockerfile（每个动作都有归宿，零遗漏）

crater 构建 OCI 镜像**不用 Dockerfile、不起容器**。"Dockerfile"就是组件的 `component.yaml`。**你只写一份 component.yaml，不用选模式、不用懂拆分**——`build --image` 自动按动作类型分流，并**打印**清楚什么烤进了镜像、什么会在目标机执行：

| 动作类型 | 例子 | 归宿 |
|---|---|---|
| **文件类**（`produces_files`） | download / **extract** / write_file / render_template | **真实物化进 staging rootfs**（extract 真解压、download 真落盘），tar 成一个层烤进镜像 |
| **命令式** | run_cmd / pkg_install / systemd_unit / module | 没法变成文件 diff → 作为**残留 recipe** 随镜像带走，load 时在目标机 replay |

build 输出示例（透明，**绝不静默丢**）：
```
node_exporter → image crater/node_exporter:1.8.2: baked 3 file action(s); will replay on target: [systemd_unit]
yq            → image crater/yq:4.53.2:            baked 1 file action(s); will replay on target: [run_cmd]
```

load（`crater deploy`）= 把 rootfs 层 `tar -xpf -C /` 展开 + **replay 残留的命令式动作** + verify。

| | Docker | crater |
|---|---|---|
| 构建配方 | Dockerfile（FROM/RUN/COPY） | component.yaml（声明式 actions，自动分流） |
| 怎么产生层 | `RUN` 在容器沙箱执行+快照 fs diff | 把文件类动作的**真实文件效果**物化进 staging rootfs → tar 成层 |
| 命令式步骤 | RUN 烤进层（靠容器） | 不烤；作残留在目标机 replay（无构建期容器） |
| 依赖 | dockerd/buildkit + 容器运行时 | 纯 Rust，两端零运行时 |

**为什么这样**：crater 不引入构建期容器沙箱（契合纯 Rust / CN / air-gap），所以命令式步骤无法快照成层——但它们不会被丢掉，而是 load 时 replay。两端零容器运行时是核心取舍；Docker 的 `RUN` 烤层本质是拿容器换的。

后续（无需容器 builder）：`from: <base>` + COPY 式纯层叠加（已能 `pull` 基镜像 blob，是 Dockerfile `FROM`+`COPY` 子集）；layer 体积裁剪（剔除下载 scratch 已做）。

## 基本 demo

**把 yq 封装成 OCI 镜像 → 离线安装**：
```bash
crater build --image -f examples/yq/yq.yaml -o /tmp/yq.oci   # build+save：crater/yq:4.53.2（rootfs 层）
# 分发 yq.oci 到离线机器，然后：
crater deploy --bundle /tmp/yq.oci --host <host> --password <pw>   # load+install（crater 自解包展开到 /）
```
期望：`load image crater/yq:4.53.2 → extracting rootfs to /` → `verify: yq --version → v4.53.2`。

**daemon 型 `--image`**（node_exporter：extract 烤进层、systemd 在目标 replay）：
```bash
crater build --image -f examples/node_exporter.yaml -o /tmp/ne.oci
# build 打印：baked 3 file action(s); will replay on target: [systemd_unit]
crater deploy --bundle /tmp/ne.oci --host <host> --password <pw>
# load: 展开 rootfs（binary+unit）→ replay systemd_unit → :9100 出 metrics
```
（也可用不带 `--image` 的 `crater build`：纯 recipe-replay 离线包，download→push-blob + 动作照常执行——两种离线形态并存。）

**查看包结构**（验证 OCI 合规）：
```bash
tar -xf /tmp/yq.oci -C /tmp/x && cat /tmp/x/oci-layout /tmp/x/index.json && ls /tmp/x/blobs/sha256/
```

## 验证（真机 192.168.73.12）

- yq `--image`：baked 1（download）+ replay [run_cmd chmod] → 展开 + chmod +x → `yq --version` v4.53.2。
- node_exporter `--image`：baked 3（download+**extract**+write_file）+ replay [systemd_unit] → 展开 binary+unit、replay systemd → `:9100` 出 metrics。**（extract 不再被漏）**
- 纯 recipe-replay 离线（增量①）：node_exporter `push(offline)` → `:9100`，`changed=4 ok=3`。
- 包结构经校验：`oci-layout`/`index.json`(带 `org.crater.manifest` 注解)/`blobs/sha256` 齐全，镜像 manifest 引用 config+layers。

## 边界 / 后续

- registry **push**；容器镜像 import 到运行时（如确需）；临时 registry 多节点分发；多 arch rootfs；镜像签名（N4）。
