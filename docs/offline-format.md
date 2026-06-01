# Crater 离线包格式（OCI）设计

> 定型：离线包 = **B 类 OCI artifact**（OCI Image Layout，artifactType `crater.component`），取代 M2 的 tar.gz（[decisions.md D-018/D-033](decisions.md)）。
> 配套阅读：[design.md §3–§4](design.md)、[features/offline-oci.md](features/offline-oci.md)。
> 最后更新：2026-06-01
>
> **D-045/D-046 起**：离线包由 **task** 打成（`crater build -f task -t ref` → `crater save -o x.oci`），
> recipe = task YAML，apply 时按 task `plan_from_task` recipe-replay。下文「组件/component」按 task 理解。

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
        ├── <d4>   layer: artifacts    # 二进制/tarball（yq、zot 二进制…）
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

> **D-017 守则**：清单由 `crater build` 从 **task 数据**生成；引擎不内置任何镜像名/制品名。物料来自 task 的 `materials:` 段（`kind: file/image/os_package`），绝不扫描 actions。

---

## 3. task 物料扩展：`materials:`（数据，非代码）

离线时要带走的东西是**数据**，写在 task 的 `materials:`（D-034）：`kind: file`(二进制/tarball)、
`kind: image`(容器镜像,build 拉 blob 进包、目标 import)、`kind: os_package`(deb/rpm)。

```yaml
name: app
materials:
  - { name: app-bin, kind: file, url_tmpl: "https://.../app-{{version}}" }
  - { name: app-img, kind: image,  ref: "docker.io/library/app:{{version}}" }   # 待接线
actions:
  - { id: place, action: place, material: app-bin, dest: /usr/local/bin/app, mode: "0755" }
```

引擎只做「把 `materials:` 列的东西拉下来塞进 artifact、apply 时落地/import」——**不知道它们属于谁**。
(注:`kind: file` 已全链路;`image`/`os_package` 为待接线项,见下文与 D-034。)

---

## 4. build 流程（在线控制机）

`crater build -f task.yaml -t ref`（→ 本地库；`crater save -o x.oci` 导出文件）

1. 解析 task。
2. **只读 task 的 `materials:` 段**（D-034/D-047）收集物料——**绝不扫 actions**，藏在 `run_cmd`
   里的依赖不会进包也不会被误扫；`download` 动作已删，获取外部文件的唯一途径就是声明 `material`：
   - **二进制/tarball**（`kind: file`）：`fetch_best` 拉取（直连→CN 镜像 fallback）。
   - **镜像**（`kind: image`，待接线）：用 OCI distribution 客户端从 registry 拉 manifest+layers（纯 Rust，见 §6）。
3. 写入 OCI Layout：recipe = task YAML 打成 recipe 层；每个物料打成一层 `vnd.crater.material.v1`，
   按 **material 名**（`org.crater.material.name`）标注；镜像作为嵌套 OCI blob 直接并入。
4. 写 `artifactType` manifest / config / index.json（B 类 artifact，D-033）。
5. 导出为 oci-archive tar（可选 zstd 压缩层；现有 flate2 先用 gzip，zstd 后续）。

---

## 5. deploy 流程（隔离目标机，零网络）

`crater apply x.oci --host <ip> --password <pw>`（D-050:专用 `deploy` 子命令已删,`apply <x.oci>` 即离线部署）

1. **上传**：分块 base64 over SSH 推送 `x.oci`（D-009，已验证 10MB+ 可行）。
2. **解包**：目标机侧 crater（agent 模式）展开 OCI Layout；按 digest **自校验**（内容寻址，无需额外 sha256 步骤）。
3. **物料落地**：
   - 文件物料 → `place` 按 **material 名**从包内 blob 推到 `dest`（与在线 `place` 同一动作，仅来源不同）。
   - 容器镜像 → 探测本地运行时（`nerdctl/docker/podman/ctr`，复用 `LoadImage` 的探测，D-017）`image import` / `load`；**或**推送到目标网内的**临时 registry**（F13），多节点指过去，避免每台塞一份。
4. **执行计划**：跑同一套 task 引擎，离线模式下 `place` 解析为 push-from-blob（`engine.rs offline_blobs` 按 material 名索引），与在线唯一区别是字节来自包内而非 `curl`。

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
2. ⏳ **增量 2**：镜像支持——组件 `images:` → `oci-client` 拉镜像 blob 并入 layout → 目标机 `ctr image import`/`docker load`，解锁容器型组件 air-gap。
3. ⏳ **增量 3**：临时 registry（F13）多节点分发。
4. ⏳ **增量 4**：agent 解 OCI（D-019 接力，目标机本地解包/导入）。

---

## 8. 开放问题
- [ ] OCI crate 的 musl/aarch64 纯 Rust 可编性实测（N2）。
- [ ] 临时 registry 形态：内嵌一个最小 registry（纯 Rust）还是带一个 distribution 二进制？
- [ ] 镜像 digest 锁定与版本漂移：`images:` 是否强制带 digest。
- [ ] 包签名（N4）：OCI 注解承载签名 / 对接 cosign 离线校验。
- [ ] 多组件共享层的去重粒度与体积实测对比 tar.gz。
