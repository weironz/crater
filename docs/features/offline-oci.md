# 离线 OCI（build / save / load / pull）

> ADR: D-018 ｜ 详细设计: [offline-format.md](../offline-format.md)

## 这是什么

离线包 = **合规 OCI Image Layout**（`oci-layout` + `index.json` + `blobs/sha256/<digest>`，内容寻址，digest 即校验）。crater **自备** build/save/load——目标机**零容器运行时**（不靠 ctr/docker）。

crater 走 docker 式分工:**build → 本地库**,**save → 文件**,**load ← 文件**,**push/pull ↔ registry**。

| 能力 | 命令 | 说明 |
|---|---|---|
| **build** | `crater build -f spec [-t ref]` | 构建 **B 类 OCI artifact** 进**本地库** `~/.crater/store`(像 `docker build`);`-t/--tag` 指定引用(默认 `crater/<name>:<ver>`，D-033) |
| **save** | `crater save <ref> -o x.oci` | 从库导出 oci-archive 文件(离线分发;纯 tar,skopeo/oras 可读) |
| **load** | `crater load x.oci [--as ref]` | 导入文件到库(省略 `--as` 用包内 ref.name) |
| **apply** | `crater apply <ref> --host <host>` | 从库/registry 取 → 识别 `artifactType` → **recipe-replay** |
| **push/pull** | `crater push/pull <ref>` | 本地库 ⇄ registry(`oci-client`,rustls 纯 Rust) |

## 构建逻辑：B 类 OCI artifact（D-032/D-033），不是伪容器镜像

crater 构建 OCI **不用 Dockerfile、不起容器**。`crater build` 把一个组件封成一个
**B 类 OCI artifact**——一种"落地宿主机的物料包"，而**不是**被 containerd run 的容器镜像：

| 层 / 字段 | mediaType / 值 | 内容 |
|---|---|---|
| **manifest** | `artifactType: application/vnd.crater.component.v1` | 标记这是 crater task artifact（非可运行 image），自描述 annotations（名/版本/run-mode）。注:`crater.component` 是历史协议常量名,内容是 task |
| **recipe 层** | `application/vnd.crater.recipe.v1+yaml` | task 的 YAML 本身（部署配方进包） |
| **material 层**（每物料一层） | `application/vnd.crater.material.v1` | `materials:` 段声明的物料 blob，按 **material 名** 标注（D-034） |
| **config** | `application/vnd.crater.component.config.v1+json` | `{name, version, runMode}` |

build 读 task 的 **`materials:` 段**（D-034）知道抓什么打包——**不扫 actions**，藏在 run_cmd
里的依赖不会漏。输出示例：
```
yq → artifact crater/yq:4.53.2: recipe + 1 material(s)
wrote /tmp/yq.oci (1 component artifact(s), … bytes) — OCI artifact (application/vnd.crater.component.v1)
```

**load = recipe-replay**：apply 识别 `artifactType` → 取出 task recipe + material blob（按名建
blobmap）→ 走**和在线完全相同的 task 引擎**(`plan_from_task`)跑离线（D-020/D-045）：`place`
从包内 blob 推送、extract/write_file/systemd 照常 replay、verify 收尾。**没有伪 rootfs、没有
`tar -xpf -C /` 覆盖宿主根**。

| | Docker | crater B 类 artifact |
|---|---|---|
| 是什么 | 被运行时 run 的容器镜像 | 落地宿主机的物料 + 配方包，永不被 run |
| 构建配方 | Dockerfile（FROM/RUN/COPY） | task.yaml（`materials:` + 声明式 actions） |
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

**离线文件分发**（build → 库 → save → 文件 → 拷贝 → load → apply）：
```bash
crater build -f examples/yq/yq.yaml -t 192.168.73.5:5000/yq:1.0   # → 本地库
crater save 192.168.73.5:5000/yq:1.0 -o /tmp/yq.oci              # → 离线文件
# 把 /tmp/yq.oci 拷到离线机器,然后:
crater load /tmp/yq.oci                                          # → 库(用包内 ref.name)
crater apply 192.168.73.5:5000/yq:1.0 --host <host> --password <pw>
```
期望：`crater task artifact → recipe-replay` → `place (offline) yq-bin -> /usr/local/bin/yq` → `yq --version` v4.53.2。

**经 registry 分发**（build → push → 另一台 pull/apply，见 [images-registry.md](images-registry.md)）：
```bash
crater build -f examples/yq/yq.yaml -t <registry>/yq:1.0        # → 本地库
CRATER_INSECURE_REGISTRIES=<registry> crater push <registry>/yq:1.0
CRATER_INSECURE_REGISTRIES=<registry> crater apply <registry>/yq:1.0 --host <host> --password <pw>
```

**查看包结构**（验证 OCI 合规 + artifactType）：
```bash
tar -xf /tmp/yq.oci -C /tmp/x && cat /tmp/x/oci-layout /tmp/x/index.json && ls /tmp/x/blobs/sha256/
# index.json 的 manifest 带 artifactType: application/vnd.crater.component.v1
```

## 验证（真机 192.168.73.11/.12）

- yq B 类 artifact：`build`（`recipe + 1 material(s)`）→ `load` → `push` 到 zot（manifest 带 `artifactType` + recipe/material 层）→ 清本地 store → `apply <zot>/yq --host .12`（`pull_blob` 取自定义层 → recipe-replay → `place (offline) yq-bin`）→ `yq --version` v4.53.2。
- 包结构经校验：`oci-layout`/`index.json`/`blobs/sha256` 齐全；组件 manifest 为 `artifactType` 制品，layer 为 recipe + material（**无伪 rootfs config / 假 image 层**）。

## build 提速（D-078）

- **增量镜像拉取**：`store.pull` 拉每个 layer 前先看 `blobs/sha256/<digest>` 是否已存在——内容寻址,digest 命中即内容命中,跳过网络。重 build / 多镜像共用基础层只下一次。manifest 仍每次取,移动的 tag 照样拉新层。
- **并行 file 取材**：`kind: file` 的二进制/压缩包(build 字节大头)并发下载(`buffer_unordered(8)`)。`kind: image`(`index.json` 读改写非并发安全)与 `os_package`(buildah 重)仍串行。
- 真机:k8s-ha 全套 build 76s(9 镜像各 1~3s 命中缓存、8 个 file ~5s 内并发完成)。
- 未做(后续):file/os_package 下载缓存(`~/.crater/cache`)、整体构建缓存(源未变即跳过)、`--no-cache`、并行镜像拉取(需 `index.json` 锁)。

## 边界 / 后续

- `materials: kind: image / os_package` 接线（容器镜像 import、deb/rpm 离线装）；build 的 version×os 矩阵用 OCI image index 组织（D-034 下一阶段，待 mysql/docker）。
- 多 arch；镜像/制品签名（N4）；聚合包跨 bundle blob 全局去重。
