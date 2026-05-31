# 离线 OCI（build / save / load / pull）

> ADR: D-018 ｜ 详细设计: [offline-format.md](../offline-format.md)

## 这是什么

离线包 = **合规 OCI Image Layout**（`oci-layout` + `index.json` + `blobs/sha256/<digest>`，内容寻址，digest 即校验）。crater **自备** build/save/load——目标机**零容器运行时**（不靠 ctr/docker）。

| 能力 | 命令 | 说明 |
|---|---|---|
| **build** | `crater build --image -f spec -o x.oci` | 制离线包；`--image` 把组件封成 **B 类 OCI artifact** `crater/<name>:<ver>`（D-033） |
| **save** | （build 产物） | oci-archive（纯 tar），skopeo/oras 可读 |
| **load+install** | `crater apply x.oci --host <host>`（= `crater deploy --bundle`） | crater 自己解包：识别 `artifactType` → **recipe-replay** |
| **pull** | （build 时，组件 `images:`） | `oci-client` 从 registry 拉容器镜像 blob 进包（rustls 纯 Rust） |

## 构建逻辑：B 类 OCI artifact（D-032/D-033），不是伪容器镜像

crater 构建 OCI **不用 Dockerfile、不起容器**。`build --image` 把一个组件封成一个
**B 类 OCI artifact**——一种"落地宿主机的物料包"，而**不是**被 containerd run 的容器镜像：

| 层 / 字段 | mediaType / 值 | 内容 |
|---|---|---|
| **manifest** | `artifactType: application/vnd.crater.component.v1` | 标记这是 crater 组件（非可运行 image），自描述 annotations（名/版本/run-mode） |
| **recipe 层** | `application/vnd.crater.recipe.v1+yaml` | 组件的 `component.yaml`（部署配方本身进包） |
| **material 层**（每物料一层） | `application/vnd.crater.material.v1` | `materials:` 段声明的物料 blob，按 **material 名** 标注（D-034） |
| **config** | `application/vnd.crater.component.config.v1+json` | `{name, version, runMode}` |

build 读组件的 **`materials:` 段**（D-034）知道抓什么打包——**不扫 install**，藏在 run_cmd
里的依赖不会漏。输出示例：
```
yq → artifact crater/yq:4.53.2: recipe + 1 material(s)
wrote /tmp/yq.oci (1 component artifact(s), … bytes) — OCI artifact (application/vnd.crater.component.v1)
```

**load = recipe-replay**：apply 识别 `artifactType` → 取出 recipe（写回 `components/<name>/`）
+ material blob（按名建 blobmap）→ 走**和在线完全相同的引擎**跑离线管线（D-020）：`place`
从包内 blob 推送、extract/write_file/systemd 照常 replay、verify 收尾。**没有伪 rootfs、没有
`tar -xpf -C /` 覆盖宿主根**。

| | Docker | crater B 类 artifact |
|---|---|---|
| 是什么 | 被运行时 run 的容器镜像 | 落地宿主机的物料 + 配方包，永不被 run |
| 构建配方 | Dockerfile（FROM/RUN/COPY） | component.yaml（`materials:` + 声明式 actions） |
| 安装方式 | 容器运行时拉起 | crater 解包 → recipe-replay（与在线同一引擎） |
| 依赖 | dockerd/buildkit + 容器运行时 | 纯 Rust，两端零运行时 |

**A/B 之分（D-032）**：要真正跑容器的镜像走 **A 类**（image-spec，crater 只 `pull` 搬运 blob，
不自造）；crater 自己分发的物料走 **B 类 artifact**（如上）。两者按 `run-mode` 数据路由，引擎
按 mediaType 通用处理，不认识任何具体产品（守 D-017）。

**artifactType 全程保真（D-033）**：oci-client 高层 `pull`/`push` 面向 image，会合成
image-manifest 丢掉 `artifactType` + 自定义层 mediaType；crater 改用 `pull_manifest_raw` +
`pull_blob`（拉）和 `OciImageManifest` + `push_blob` + `push_manifest`（推）原样保真，
artifactType 与 recipe/material 层在 registry 往返后仍在。

## 基本 demo

**把 yq 封装成 B 类 artifact → 离线安装**：
```bash
crater build --image -f examples/yq/yq.yaml -o /tmp/yq.oci   # → crater/yq:4.53.2（B 类 artifact）
# 分发 yq.oci 到离线机器，然后（在线/离线同一条 apply，D-020）：
crater apply /tmp/yq.oci --host <host> --password <pw>       # 识别 artifactType → recipe-replay
```
期望：`crater component artifact → recipe-replay` → `place (offline) yq-bin -> /usr/local/bin/yq` → `yq --version` v4.53.2。

**经 registry 分发**（build → push → 另一台 pull/apply，见 [images-registry.md](images-registry.md)）：
```bash
crater load /tmp/yq.oci --as <registry>/yq:4.53.2
CRATER_INSECURE_REGISTRIES=<registry> crater push <registry>/yq:4.53.2
CRATER_INSECURE_REGISTRIES=<registry> crater apply <registry>/yq:4.53.2 --host <host> --password <pw>
```

**查看包结构**（验证 OCI 合规 + artifactType）：
```bash
tar -xf /tmp/yq.oci -C /tmp/x && cat /tmp/x/oci-layout /tmp/x/index.json && ls /tmp/x/blobs/sha256/
# index.json 的 manifest 带 artifactType: application/vnd.crater.component.v1
```

## 验证（真机 192.168.73.11/.12）

- yq B 类 artifact：`build`（`recipe + 1 material(s)`）→ `load` → `push` 到 zot（manifest 带 `artifactType` + recipe/material 层）→ 清本地 store → `apply <zot>/yq --host .12`（`pull_blob` 取自定义层 → recipe-replay → `place (offline) yq-bin`）→ `yq --version` v4.53.2。
- 包结构经校验：`oci-layout`/`index.json`/`blobs/sha256` 齐全；组件 manifest 为 `artifactType` 制品，layer 为 recipe + material（**无伪 rootfs config / 假 image 层**）。

## 边界 / 后续

- `materials: kind: image / os_package` 接线（容器镜像 import、deb/rpm 离线装）；build 的 version×os 矩阵用 OCI image index 组织（D-034 下一阶段，待 mysql/docker）。
- 多 arch；镜像/制品签名（N4）；聚合包跨 bundle blob 全局去重。
