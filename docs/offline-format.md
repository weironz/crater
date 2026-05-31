# Crater 离线包格式（OCI）设计

> 定型：离线包基于 **OCI 镜像 / OCI Image Layout**，取代 M2 的 tar.gz（[decisions.md D-018](decisions.md)）。
> 配套阅读：[design.md §3–§4](design.md)。
> 最后更新：2026-05-31

---

## 1. 选型结论

**离线包 = 一个 OCI Image Layout，序列化为单个 `oci-archive` tar 传输。**

对比（详见 design.md §3.3）：tar.gz 要手写 manifest+sha256、不去重、装不下容器镜像；OCI 内容寻址自带校验、分层去重、原生承载容器镜像、生态工具（registry / `ctr` / `docker load` / oras）通吃。参考实现：sealos clusterimage。

---

## 2. 包结构

```
x.oci  (tar 包，展开后是标准 OCI Image Layout)
├── oci-layout                       # {"imageLayoutVersion":"1.0.0"}
├── index.json                       # 顶层 image index（指向下面的 manifest）
└── blobs/
    └── sha256/
        ├── <d0>   image manifest     # 本离线包自身的 OCI manifest
        ├── <d1>   image config       # OCI config（含 crater 注解）
        ├── <d2>   crater-manifest     # mediaType: application/vnd.crater.manifest.v1+json
        ├── <d3>   layer: components   # 所有 component.yaml + templates 打成一层
        ├── <d4>   layer: artifacts    # 二进制/tarball（node_exporter、k3s 二进制…）
        └── <dN>   nested image blobs  # 容器镜像（pause/coredns/mysql/es…）的 manifest+config+layers
```

### 2.1 crater-manifest（crater 在 OCI 之上的逻辑索引）

OCI 只管「字节按 digest 存好」；crater-manifest 管「这些字节是什么、怎么落地」：

```jsonc
{
  "schema": "crater.manifest/v1",
  "spec": { /* 内联的 crater.yaml：inventory(去敏) + components + 版本/参数 */ },
  "artifacts": [
    { "logical": "node_exporter-1.8.2.tar.gz",
      "digest":  "sha256:…",
      "place":   "/tmp/crater-dl",          // 对应组件 download.dest
      "via":     "download" }               // 离线时 download→push-from-blob
  ],
  "images": [
    { "ref":    "docker.io/rancher/mirrored-pause:3.6",
      "digest": "sha256:…",                 // 锁定
      "load":   "runtime-import" }          // ctr image import / docker load / push 临时 registry
  ]
}
```

> **D-017 守则**：`crater-manifest` 由 `crater build` 从**组件数据**生成；引擎不内置任何镜像名/制品名。`artifacts` 来自组件的 `download:` 原语，`images` 来自组件新增的 `images:` 声明。

---

## 3. 组件数据扩展：`images:`（数据，非代码）

容器型组件离线时需要把镜像一起带走。镜像清单是**数据**，写在 component.yaml：

```yaml
name: k3s
aliases: [k8s, kubernetes]
# 离线时需打包的容器镜像（在线时由 k3s 自身/运行时拉取，不读此字段）
images:
  - docker.io/rancher/mirrored-pause:3.6
  - docker.io/rancher/mirrored-coredns-coredns:1.10.1
install:
  - run_cmd: "...INSTALL_K3S_MIRROR=cn... k3s-install.sh"
```

引擎只做「把 `images:` 里列的镜像拉下来塞进 OCI 包、在目标机导入」——**不知道这些镜像属于谁**。

---

## 4. build 流程（在线控制机）

`crater build -f spec.yaml -o x.oci`

1. 解析 spec → 组件 → DAG。
2. 逐组件收集：
   - **制品**：`collect_downloads` 得到 (url, dest)；`fetch_best` 拉取（直连→CN 镜像 fallback，复用现有逻辑）。
   - **镜像**：读组件 `images:`；用 OCI distribution 客户端从 registry 拉 manifest+layers（纯 Rust，见 §6）。
3. 写入 OCI Layout：每个 blob 内容寻址落 `blobs/sha256/`；组件目录打成 components 层；制品打成 artifacts 层；镜像作为嵌套 OCI blob 直接并入。
4. 生成 crater-manifest（§2.1）→ 写 image manifest / config / index.json。
5. 导出为 oci-archive tar（可选 zstd 压缩层；现有 flate2 先用 gzip，zstd 后续）。

---

## 5. deploy 流程（隔离目标机，零网络）

`crater deploy --bundle x.oci --host <ip> --password <pw> --apply`

1. **上传**：分块 base64 over SSH 推送 `x.oci`（D-009，已验证 10MB+ 可行）。
2. **解包**：目标机侧 crater（agent 模式）展开 OCI Layout；按 digest **自校验**（内容寻址，无需额外 sha256 步骤）。
3. **制品落地**：
   - 文件制品 → 按 crater-manifest `place` 放置（等价于在线的 download 产物）。
   - 容器镜像 → 探测本地运行时（`nerdctl/docker/podman/ctr`，复用 `LoadImage` 的探测，D-017）`image import` / `load`；**或**推送到目标网内的**临时 registry**（F13），多节点指过去，避免每台塞一份。
4. **执行计划**：跑同一套引擎，离线模式下 `download` Op → push-from-blob Op（现状 `engine.rs offline_blobs` 的自然延伸，把 blob 来源从 tar.gz 换成 OCI Layout）。

---

## 6. Rust 选型（守 N1：纯 Rust / 免 C / musl）

| 用途 | crate | 备注 |
|---|---|---|
| OCI 类型（manifest/config/index/layout） | `oci-spec` | 纯 Rust 类型定义 |
| 从 registry 拉镜像 | `oci-client`（原 `oci-distribution`） | rustls，无 openssl C 依赖 |
| OCI Layout 读写 / 打包 | `oci-spec` + 手写 blob writer，或 `ocipkg` | 需验证 musl 可编 |
| 压缩 | `flate2`（现用）→ `zstd`（后续） | gzip 先行 |
| 校验 | `sha2`（现用） | 内容寻址即 digest |

> ⚠️ 待验证：上述 OCI crate 在 `*-unknown-linux-musl` 下纯 Rust 可编、无 C 工具链。先在控制端（制包）跑通；目标端解包/导入逻辑尽量只用文件 IO + 运行时 CLI，降低对 OCI crate 在目标端的依赖。

---

## 7. 迁移：从 tar.gz 到 OCI（增量推进）

1. ✅ **增量 1（已落地，D-018 实现）**：`bundle.rs` 直接重写为 OCI Image Layout（制品/文件型），打包为 oci-archive（纯 tar）。`BundleStage` API 不变 → `build`/`deploy` 零改动；`serde_json` 手写 manifest/config/index（未引 `oci-spec`/`oci-client`，纯 Rust）。真机：node_exporter 离线部署 `:9100` OK，包结构经校验。**直接替换 tar.gz**（FORMAT_VERSION=2），未保留旧路径（无需要）。
2. ⏳ **增量 2**：镜像支持——组件 `images:` → `oci-client` 拉镜像 blob 并入 layout → 目标机 `ctr image import`/`docker load`，解锁 k3s/mysql/es air-gap。
3. ⏳ **增量 3**：临时 registry（F13）多节点分发。
4. ⏳ **增量 4**：agent 解 OCI（D-019 接力，目标机本地解包/导入）。

---

## 8. 开放问题
- [ ] OCI crate 的 musl/aarch64 纯 Rust 可编性实测（N2）。
- [ ] 临时 registry 形态：内嵌一个最小 registry（纯 Rust）还是带一个 distribution 二进制？
- [ ] 镜像 digest 锁定与版本漂移：`images:` 是否强制带 digest。
- [ ] 包签名（N4）：OCI 注解承载签名 / 对接 cosign 离线校验。
- [ ] 多组件共享层的去重粒度与体积实测对比 tar.gz。
