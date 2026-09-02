# crater 包分发设计:Helm 式 OCI 制品、两个可分发物、一键安装

> 起因:「要不要做成 Helm chart 那种——YAML 打成 OCI,`crater ls` / `crater search` /
> `crater install docker k8s mysql` 一键安装?crater 有**两个可分发物**怎么办?
> 之前考虑过从 OCI pull 只取部分内容,比如只取 YAML。」
>
> 调研基线:2026-09-02。四条线(Helm OCI 内幕、OCI 规范、Helm 之外的先例、
> registry 兼容性)由并行调研完成,关键论断已交叉核对;自家侧盘点了旧 task
> 管线的制品语法与真机数字。

## 一、结论速览

1. **"从 OCI pull 只取 YAML"不是待验证的想法,是 crater 已经做过并真机验证过的事。**
   旧 task 管线 D-087/D-088:每个物料是独立的层,带 `fetch=embedded|dependency`
   注解;`apply <ref>` 默认**瘦拉**,只拉 recipe 层与自建文件层。Docker Hub 实测:
   瘦拉 **16K**(recipe 733B + config 49B + manifest 863B,10MB 的 yq-bin 层留在
   registry),`--offline` 全量 **9.6M**。同一制品,一个 flag,registry 零特殊设施。
2. **它也是协议原生能力与行业惯例。** OCI manifest 的每一层是独立内容寻址的 blob,
   客户端只 GET manifest 再按 digest 选层;Helm 自己的 `pull` 就默认跳过 `.prov`
   层;KitOps、Ollama、timoni、Flux `layerSelector` 都是"小配置层 + 大数据层按需拉"
   的生产先例。
3. **所以"两个可分发物"不是问题,是分层的理由**:蓝图包(小)与物料闭包(大)
   住在**同一个 manifest 的不同层**里,按 mediaType 区分;瘦拉只取蓝图包,
   `--full` 才把物料层带走。多架构用 image index 的 `platform` 字段,蓝图层跨架构
   共享同一 digest(registry 自动去重)。
4. **Helm 的模型有两处不能照抄**:(a) Helm 只有一个可分发物,所以一层就够;
   (b) `helm search repo` 在 OCI 上**根本不工作** —— OCI 没有搜索、没有命名空间
   列举、`_catalog` 不在规范里且 Docker Hub 禁用。Helm 的答案是静态 `index.yaml`,
   crater 也该走这条:**registry 管存储与不可变 digest,索引文件管列举与搜索**,
   而且索引文件能随闭包进 U 盘。
5. **兼容性地板是 OCI 1.0 风格**:自定义 `config.mediaType` + 真实 config blob +
   自定义层 mediaType。**不要**依赖 OCI 1.1 的 `artifactType` + 空描述符(ACR 拒收,
   与本仓库 buildx 那次撞的是同一堵墙)、**不要**依赖 referrers API(GHCR / Docker
   Hub 没有)、**不要**依赖 `_catalog`。版本发现只用 `tags/list`(dist-spec 1.0 必选)。
6. **一键安装不绕过 plan 闸门。** `crater install` = 拉蓝图包 → 读契约 → 对账机群
   → 生成 app 文件 → plan → 停在闸门前;`--yes` 才 apply。"一键"省的是找包、填参、
   对账那几步,不是省"先看 diff 再动手"。
7. **注意你自己的 ACR**:阿里云 ACR 的自定义 OCI 制品是**企业版专属**,个人版
   能不能收 crater 包,要先实测;Docker Hub(2022-10 起)与 zot 是稳妥的验证场。

## 二、起点:crater 已经走完一半

这不是从零设计。旧 task 管线在 D-033 / D-046 / D-078 / D-087 / D-088 / D-098
累积了一整套**制品语法**,只是新蓝图管线没有接上:

| 已有机制 | 在哪 | 对新设计的意义 |
| --- | --- | --- |
| 自定义层 mediaType 过线保真(D-033) | `store.rs` 用 `pull_manifest_raw` + `pull_blob` 低层 API,不走高层 `pull`(它会把制品合成 image manifest,丢掉自定义类型) | 制品语法能原样到达任何 registry |
| `MT_RECIPE` / `MT_MATERIAL` 两类层 | `store.rs` 常量 | 就是"蓝图层 / 物料层"的原型 |
| `ANN_MATERIAL_FETCH = embedded \| dependency` | 层注解 | 自建文件随包走,外部依赖留在 registry —— 两个可分发物的边界已经画过 |
| `pull_thin` / `pull` / `has_all_layers`(D-087) | `store.rs` | 瘦拉 / 全量 / 本地完整性判定,三态齐全 |
| 增量拉取(D-078) | 按 digest 跳过已有 blob | 版本升级只拉变了的层 |
| 本地 store(`~/.crater/store`,OCI layout,index.json,GC) | `store.rs` | 本地缓存与 `pkg ls` 的数据源 |
| 多架构 index 解析 | `store.rs` 按 `platform.architecture` 选 manifest | 按架构分变体的读侧已在 |
| `push` / `pull` / `save` / `load` / `images` | `images.rs` | 命令骨架现成 |
| `crater inspect` 读参数契约 + 机群契约 | `inspect_bp.rs` | config blob 的内容来源;UI 目录已在用同一份数据 |

新蓝图管线目前只有一条出口:`crater build -f x.blueprint.yaml -o x.closure.tar`
——一个本地 tar。**要做的是把上面这套语法接给蓝图,不是再造一套。**

## 三、调研结论(按线)

### 3.1 Helm 的 OCI 内幕

- 布局极简且 v3/v4 不变:一个 image manifest;`config` =
  `application/vnd.cncf.helm.config.v1+json`,内容是 **Chart.yaml 元数据的 JSON**
  (不是空对象);chart `.tgz` 原样作一层
  `application/vnd.cncf.helm.chart.content.v1.tar+gzip`;`.prov` 是同一 manifest 的
  可选第二层。
- **`helm pull` 自己就做选择性拉取**:默认只拉 config + chart 层,`.prov` 层跳过,
  实现是 ORAS 按 mediaType 白名单过滤 descriptor。
- **`helm search repo` 对 OCI 不工作**,`helm repo add oci://` 直接报错。官方博客原话:
  "OCI based registries don't provide standard APIs to facilitate searching"。
  版本发现唯一靠 `tags/list` + 客户端按 semver 降序;tag 里 `+` 写成 `_`(OCI tag
  禁止 `+`),读回时换回。tag 严格等于 chart version,仓库名严格等于 chart name。
- 经典 repo 的 `index.yaml`:`apiVersion: v1` + `entries`(名 → 版本条目数组,含
  version / digest / urls / description)+ `generated`;`helm repo index` 可离线生成、
  `--merge` 增量;**任何能应答 GET 的静态 HTTP 都能托管**。air-gap 只需把包和
  重新生成的 index 放进内网静态服务器。
- 兼容性坑真实存在,集中在"非标 mediaType 被白名单拒":Docker Hub 2022-10-31 前
  不收 OCI artifact;Quay 走白名单模型;旧 GCR 不支持;部分 registry 要预建仓库。

### 3.2 OCI 规范

- **部分拉取是协议原生能力**:manifest 的 `layers[]` 每项是 descriptor,
  `digest/size/mediaType` 必填,每层独立内容寻址。只 GET manifest(end-3)再按
  digest 取选定 blob(end-2)即可。但 blob **内部**的 Range 只是 SHOULD —— 字节级
  懒加载不能硬依赖;crater 不需要它(层粒度够用)。
- `artifactType` 是 OCI 1.1 的顶层可选字段;当 `config` 用空描述符
  `application/vnd.oci.empty.v1+json` 时**必须**设它。消费端在 `artifactType`
  缺席时回退用 `config.mediaType` 当类型 —— 后者是 1.0 时代的老约定,也是兼容地板。
- image index 的 `platform{architecture, os, variant}` 是"同一 tag 按架构分发变体"
  的**标准用法**;`annotations` 区分变体在数据模型上合法,但规范未定义选择语义,
  需要自研客户端逻辑(BuildKit 的 attestation manifest 即此先例)。
- referrers API(dist-spec 1.1,2024-02):registry 支持不均;规范**强制**客户端在
  404 时回退 tag schema(`sha256-<hex>` 指向自维护的 index)。
- **`_catalog` 不属于 OCI 规范**(被列为 reserved prior extension);内容发现只定义
  了 `tags/list`,且整个 Content Discovery 类别仅 SHOULD。Docker Hub 有意不提供
  `_catalog`。

### 3.3 Helm 之外的先例

| 项目 | 分层 | 元数据放哪 | 版本发现 | 搜索 |
| --- | --- | --- | --- | --- |
| **timoni**(CUE 模块) | config `vnd.timoni.config.v1+json` + 内容层 `vnd.timoni.content.v1.tar+gzip`;vendor 层与模块层同 mediaType、靠层注解 `sh.timoni.content.type` 区分,vendor 层本地缓存后跳过 | config + OCI 标准 annotations | 严格 semver 作 tag,`timoni mod list` 排序 | 无(ArtifactHub 集成是 open issue) |
| **Flux** `push artifact` | 单内容层 `vnd.cncf.flux.content.v1.tar+gzip` | config `vnd.cncf.flux.config.v1+json` | `ref.tag / ref.semver / digest` | 无 |
| **Flux OCIRepository** | 拉取端 `spec.layerSelector` **按 mediaType 挑层** | — | — | — |
| **KitOps ModelKit**(CNCF) | config = Kitfile(小);model / dataset / code / docs **各自独立层、各有 mediaType**;`kit unpack` 按类型选择性解包 —— 官方卖点"部署时不必拉 50GB 训练数据" | config | tag | 无 |
| **Ollama** | 同一 manifest 内几 KB 的 template/params 层与 GB 级权重层并存;客户端逐层查本地 blob 只拉缺的;派生模型共享权重 blob | 层 | tag | 无 |
| **Homebrew bottles** | 每 formula 一个 image index,每平台一个 manifest;bottle tarball 是唯一层;**构建元数据 JSON 放 manifest annotation**(`sh.brew.tab`)—— 读元数据零 blob 下载 | annotation | `ref.name = version.platform` | 无 |
| **ORAS CLI** 惯例 | 一文件一层,`org.opencontainers.image.title` 记文件名,目录自动 tar | `--config` 或 `--artifact-type` | — | — |

三个横向结论:(1) "自定义 config mediaType + 自定义内容层 mediaType"是行业收敛的
类型识别惯例;(2) "小配置层 + 大数据层按需拉"至少四个生产先例;(3) **搜索无人在
registry 内解决**,全部外包给 ArtifactHub、registry UI 或 Git。

### 3.4 registry 兼容性

| registry | 自定义制品 | referrers API | `_catalog` | 备注 |
| --- | --- | --- | --- | --- |
| **zot** | 无白名单,纯 OCI | 原生(dist-spec 1.1.1 参考实现) | 有 | 另有 GraphQL 搜索扩展 `/v2/_zot/ext/search`(专有,只能当锦上添花) |
| **Harbor** | 2.0+ 任意制品(按 `config.mediaType` 识别) | ≥2.9(保守按 ≥2.10) | 有 | |
| **Docker Hub** | 2022-10-31 起 | **无**(仍停在 dist-spec 1.0.1) | **有意禁用** | 仓库列举只能走私有 hub API |
| **GHCR** | 接受 | **无**(2025-10 社区讨论确认) | — | 收 subject 但查不到 |
| **阿里云 ACR** | **企业版专属**;官方示例本身是 1.0 写法(`--manifest-config /dev/null:<type>`) | 仅 2024-04 后新建企业版 | — | 拒收 `vnd.oci.empty.v1+json` 空描述符 —— 与本仓库 buildx provenance 撞的是同一堵墙 |

**跨 registry 的公分母**:(a) 制品身份放 `config.mediaType`,config 用真实 blob;
(b) 层用自定义 mediaType;(c) 制品关联不依赖 referrers,要关联就用 tag fallback;
(d) 仓库枚举不依赖 `_catalog`,用 `tags/list`;(e) zot 搜索只当增强。
**以 ACR 为兼容性地板设计,referrers / 搜索做成运行时探测 + 降级。**

## 四、两个可分发物:设计答案

Helm 只有一个可分发物(chart 就是全部,镜像由 k8s 去拉),所以一层就够。crater 有两个:

| | 内容 | 大小 | 何时需要 |
| --- | --- | --- | --- |
| **蓝图包** | 蓝图 YAML + `templates/` + `files/` + README | KB 级 | 永远 |
| **物料闭包** | 物料字节(二进制、镜像层) | 几十 MB ~ GB | 只有离线才需要 |

在线部署时目标机自己按 URL 拉物料,**根本不需要闭包**。照抄 Helm 把两者打成一包,
每次 `pull` 都拖几百 MB 而多数场景用不上;更糟的是闭包按 `--for arch=` 分变体,
一个 tag 装不下多个。

**答案:同一个 manifest,按 mediaType 分层;架构用 image index 分变体。**

```
oci://reg/ns/rustfs:1.0                    ← image index
├─ platform {linux/amd64}  → manifest
│    config  vnd.crater.blueprint.config.v1+json   (契约:参数/机群/物料清单)
│    layer   vnd.crater.blueprint.v1.tar+gzip       (蓝图包,digest 与 arm64 那份相同)
│    layer   vnd.crater.material.v1  fetch=dependency  name=rustfs-bin  (amd64 字节)
│    layer   vnd.crater.material.v1  fetch=embedded    name=rustfs.env.j2
└─ platform {linux/arm64}  → manifest
     config  (同一 digest)
     layer   蓝图包 (同一 digest —— registry 去重,只存一份)
     layer   material rustfs-bin (arm64 字节)
     layer   material rustfs.env.j2 (同一 digest)
```

- **瘦拉**(默认):manifest + config + 蓝图包层 + `fetch=embedded` 的物料层。
  这正是 D-087 的 `pull_thin`。
- **全量**(`--full` / 离线):再拉 `fetch=dependency` 的物料层 —— 就是闭包。
- **只读契约**(`pkg ls` / `search` / UI 目录):只拉 manifest + config,零层下载
  ——Homebrew 的做法。
- **架构**:客户端按自身或 `--for arch=` 选 index 里的 manifest;蓝图层跨架构同
  digest,registry 只存一份,`--for arch=amd64,arm64` 一次推两份变体也只多物料层。
- **`--for` 的其他维度**(distro 等):`platform` 只覆盖 os/arch,其余维度放
  manifest 注解 `org.crater.profile`(BuildKit 先例),客户端按注解匹配。这是自研
  语义,文档里要写清"只有 crater 认得"。

**没有物料的蓝图**(纯 shell/service 之类)自然退化成 Helm 布局:config + 一层。

## 五、制品格式规范

### 5.1 mediaType 与 annotation

| 名称 | 值 | 说明 |
| --- | --- | --- |
| config | `application/vnd.crater.blueprint.config.v1+json` | 制品身份靠它(兼容地板);内容见 5.2 |
| 蓝图层 | `application/vnd.crater.blueprint.v1.tar+gzip` | 蓝图目录原样 tar.gz,一层 |
| 物料层 | `application/vnd.crater.material.v1` | 沿用旧管线常量;一物料一层 |
| 层注解 | `org.crater.material.fetch` = `embedded` \| `dependency` | 沿用旧管线 |
| 层注解 | `org.crater.material.name` / `org.crater.material.source` / `org.crater.material.sha256` | 与 `Manifest.blobs[]` 的 `BlobEntry` 一一对应 |
| 层注解 | `org.opencontainers.image.title` | ORAS 惯例,让 `oras pull` 能当逃生通道 |
| manifest 注解 | `org.opencontainers.image.{title,version,description,created,source,revision}` | 标准溯源(Helm/timoni/Flux 同款) |
| manifest 注解 | `org.crater.profile` | `--for` 的非 platform 维度,如 `distro=ubuntu` |
| `artifactType` | **不设** | 空描述符 + artifactType 是 1.1 写法,ACR 拒收 |

### 5.2 config blob 的内容

就是 `crater inspect` 已经算出的那份契约(UI 目录也在读它):

```json
{
  "name": "rustfs", "version": "1.0", "description": "...",
  "params":  [{ "name": "port", "type": {"kind":"port","min":1,"max":65535},
                "default": 9000, "required": false, "secret": false,
                "stage": "deploy", "desc": "S3 端点" }],
  "fleet":   [{ "name": "storage", "min": 1 }],
  "materials": [{ "name": "rustfs-bin", "source": "https://...", "sha256": "...",
                  "fetch": "dependency", "platforms": ["linux/amd64","linux/arm64"] }],
  "counts":  { "resources": 6, "procedures": 1, "health": 2, "custom_types": 0 },
  "crater":  { "min_version": "0.1.1" }
}
```

**读 config 不下载任何层**就能:列目录、生成参数表单、对账机群契约、判断本地
crater 版本够不够。Helm 的 config 是 Chart.yaml 元数据,timoni 的 config 是模块
元数据,都是这个思路。

### 5.3 引用、tag 与版本

- 引用形如 `oci://<registry>/<namespace>/<name>:<version>`,与 Helm 一致:仓库名 = 蓝图 `name`,
  tag = 蓝图 `version`(semver;`+` 写成 `_`,读回换回)。
- 版本发现只用 `tags/list`,客户端按 semver 降序;`latest` 是唯一可变 tag。
- 按 digest 安装(`@sha256:...`)必须支持 —— 这是"期望态可复现"的基础。
- 本地 store 沿用 `~/.crater/store`(OCI layout),`pkg ls` 列的就是它的 index.json。

## 六、命令面

```
crater pkg push   <蓝图目录|蓝图文件> oci://reg/ns/name:ver
                  [--with-closure] [--for arch=amd64 --for arch=arm64]   # 不带即只推蓝图包
crater pkg pull   oci://reg/ns/name[:ver|@digest] [--full] [--into <dir>]  # 默认瘦拉进工作区
crater pkg ls                                     # 本地 store + 工作区里有什么
crater pkg tags   oci://reg/ns/name               # 远端版本(tags/list + semver)
crater pkg inspect oci://reg/ns/name:ver          # 只拉 config,零层下载
crater pkg save   name:ver -o x.pkg.tar           # 离线搬运(= 现有 save)
crater pkg load   x.pkg.tar                       # (= 现有 load)

crater repo add   <名> <index-url>                # 见第七节
crater repo update
crater search     <关键词>                        # 查本地缓存的索引

crater install    <name>[:ver] -i inventory [--set k=v ...] [--repo 名] [--yes]
```

`install` 的语义是**串起已有的门**,不是新的执行路径:

1. 从 repo 索引或直接引用解析出 `oci://...`;瘦拉进工作区 `<name>/`
2. 读 config 契约;缺必填参数就问(或 `--set`);敏感参数不回显
3. `fit`:机群契约对账(组名、台数),不满足就停,不连机器
4. 生成 `<name>.app.yaml`(只写改过的参数)
5. **plan**,打印矩阵与汇总,**停在闸门前**
6. `--yes` 才 apply;否则提示 `crater apply` 的命令

"一键"省的是找包、复制目录、抄参数、对账机群那几步;不省"先看 diff 再动手"。
UI 侧对应:目录页多一个"远端源"选项卡,数据来自 `pkg inspect`(只拉 config),
其余复用已有的参数表单 → fit → 建任务 → plan 闸门。

`crater ls` 不再新增(已有 `images` 与 `task list`,再加一个会撞名);包相关全部归
`crater pkg` 子命令。

## 七、发现与搜索:索引文件,不是 registry API

调研的四条线在这一点上完全一致:**OCI 里没有搜索**。Helm 把 `search` 留在经典
repo 的 `index.yaml`;timoni / Flux / KitOps 干脆不做。

crater 走索引文件,理由有三:

1. 不依赖 registry 的可选 API(`_catalog` 不在规范里,Docker Hub 禁用)。
2. **air-gap 友好**:`index.yaml` 可以和 `pkg save` 的 tar 一起塞进 U 盘;registry API
   在断网机房里根本调不到。
3. 任何静态 HTTP 都能托管,包括 rustfs 这类 S3 —— 这与 storage-design 的分层一致:
   registry 管不可变字节,索引管列举。

```yaml
# index.yaml —— 由 `crater pkg index <dir|oci-prefix>` 生成,--merge 增量
apiVersion: crater.pkg/v1
generated: 2026-09-02T00:00:00Z
entries:
  rustfs:
    - version: "1.0"
      ref: oci://reg/willspace/rustfs:1.0
      digest: sha256:...
      description: RustFS S3 对象存储
      fleet: [{ name: storage, min: 1 }]
      params: 7
      platforms: [linux/amd64, linux/arm64]
      urls: [rustfs-1.0.pkg.tar]          # 可选:相对路径,离线镜像时指向本地 tar
```

`crater search` 查本地缓存的索引;`repo update` 拉新。zot 的 GraphQL 搜索只作为
"有则更好"的增强,不作依赖。

## 八、兼容性地板与降级

| 依赖 | 做法 | 为什么 |
| --- | --- | --- |
| 制品类型识别 | `config.mediaType`,config 为真实 blob | 五家 registry 全通;ACR 官方示例就是这写法 |
| `artifactType` / 空描述符 | **不用** | ACR 拒收;本仓库 buildx provenance 撞过同一堵墙 |
| referrers API | **不用作核心**;签名/关联(将来)走 tag fallback | GHCR、Docker Hub 没有 |
| `_catalog` | **不用** | 不在 OCI 规范;Docker Hub 禁用 |
| 版本发现 | `tags/list` | dist-spec 1.0 必选项 |
| 多架构 | image index + `platform` | 标准,所有 registry 与运行时都懂 |
| 高层 `pull` API | **不用**,继续用 `pull_manifest_raw` + `pull_blob` | 高层 API 会把制品合成 image manifest,丢掉自定义类型(D-032/D-087 踩过) |

### 实测结果(2026-09-02)

`.github/workflows/pkg-compat.yml` 手动触发,读回 manifest 那一步**绕开 crater
自己的客户端**走 registry API —— 拿自己的解析去证明自己的写入,证明不了任何事。

| registry | config.mediaType | 层 mediaType | artifactType | 结论 |
| --- | --- | --- | --- | --- |
| **zot**(本地) | ✓ 原样 | ✓ 原样 | 未设 ✓ | 过 |
| **Docker Hub** | ✓ 原样 | ✓ 原样 | 未设 ✓ | **过** |
| **阿里云 ACR** | — | — | — | 未测(见下) |

Docker Hub 上 `config` 535 B、蓝图层 572 B —— `pkg inspect` 的全部下载量就是
前者,这是"零层下载读契约"的实测数字。判据本身也验了反向:把一个普通 docker
镜像喂给同一套断言,两条 mediaType 都报 ✗ 并非零退出,所以这不是一个只会绿的
摆设。

**顺带确认了 D-033 仍然成立**:自定义层类型能穿过 Docker Hub 的白名单原样回来
(2022-10-31 之后才具备这个能力),前提是继续用 `pull_manifest_raw` + `pull_blob`
低层 API,不走高层 `pull`。

**关于你自己的 ACR**:自定义 OCI 制品是**企业版专属**能力,个人版的行为文档未提及。
落地第一步必须实测 `registry.cn-shenzhen.aliyuncs.com/willspace` 收不收 crater 包;
不收就用 Docker Hub 或自建 zot(library 里已有蓝图)。**不要在设计上假设 ACR 可用。**

## 九、与已有设计的关系

- **D-119 storage-design 的 `BlobSource` / `BlobSink`**:`OciSource` 就是这里的读侧
  (瘦拉 / 全量 / 按 digest 取物料层),`OciSink` 是 `pkg push`。本设计给出了
  "第二个后端"的具体形态,反过来验证那两个接口够不够窄:`contains / fetch /
  manifest / origin` 四个方法正好覆盖,不需要加。
- **D-106 方向定案**:「环境的包管理器 + 对账引擎 —— Helm 的作者/操作者分离」。
  本设计是把这句话落到命令面上,不是转向。
- **UI 目录(D-118 之后)**:目录页已经在读同一份契约(`/api/catalog`);远端源
  只是把数据来源从"工作区文件"扩到"registry 的 config blob"。
- **plan 闸门**:`install` 停在闸门前,与 UI 的 plan-gated apply 同一条纪律。

## 十、分阶段落地与验收

| 阶段 | 内容 | 验收(全部真机) |
| --- | --- | --- |
| **1. 制品格式 + push/pull** ✅ 已落地 | 蓝图目录 → manifest(config + 蓝图层);`pkg push/build/pull/ls/inspect/tags`;瘦拉;复用 `store.rs` | 见下方"第一阶段验收记录"。**ACR 个人版仍未实测**(本地无凭据) |
| **2. 闭包层 + 多架构** ✅ 已落地 | `pkg push --arch amd64 --arch arm64`,物料层 `fetch=dependency`;image index 按 platform;`pull --full`;`--closure oci://<ref>` | 见下方"第二阶段验收记录" |
| **3. install** ✅ 已落地(UI 远端源除外) | `crater install` 串起 pull → 契约 → fit → app → plan 闸门 | 见下方"第三阶段验收记录"。**UI 远端源未做** |
| **4. 索引与搜索** ✅ 已落地 | `pkg index` 生成 `index.yaml`;`repo add/update/list/remove`;`search`;`install <包名>` | 见下方"第四阶段验收记录" |

每阶段之间没有强依赖;1 是其余的前提。阶段 4 的价值取决于包的数量 —— 库里现在
8 个,`ls` 就够,**不要在阶段 1 之前做它**。

### 第一阶段验收记录(2026-09-02,本机 + 容器目标)

| 判据 | 结果 |
| --- | --- |
| 自定义类型过线保真 | zot 与 distribution(`registry:2`,Docker Hub 的参考实现)读回的 manifest 里 `config.mediaType` 与层 mediaType **原样**,`artifactType` 未设 |
| `pkg inspect` 零层下载 | 干净 `CRATER_HOME` 下 inspect 完,本地 store 仍是 118 B(只有 `oci-layout` + 空 index),契约完整印出 |
| 传输量 | manifest 810 B + config 2323 B = **3133 B**;蓝图层另 3109 B(rustfs 包) |
| 往返保真 | 空工作区 `pkg pull` 摊出的目录与 `library/rustfs` **逐字节一致**,只少了刻意排除的 `inventory.example.yaml` |
| 凭据不进包 | inventory 全数排除并逐个报出;字面口令扫描拒绝打包(**参数声明例外** —— `secret_key: { default: … }` 是登记不是泄漏) |
| 拉下来能跑 | 拉回的 yq 包在空工作区 `lint` 通过 → `apply` 到 sshd 容器 → `yq --version` 报 v4.44.3;二次 apply `+0 ~0 -0 ✓1` 幂等 |
| 内容寻址去重 | 同内容的两个 tag,第二次 pull 只多落一份 manifest(747 B),层零下载 |
| semver 排序 | `1.10.0 > 1.1 > 1.1.0-rc1 > 1.0` |

**没验到的**:阿里云 ACR 与 Docker Hub —— 凭据在 GitHub secrets 里,本地没有。
ACR 个人版收不收自定义制品仍是**第二阶段开工前必须实测**的那一条。

**真机没用上实验室**:192.168.73.x 的路由被本机的 Meta 代理截住(TCP 连得上、
SSH 握手即断),隧道也没起。改用一个 sshd 容器当目标 —— 对 crater 而言它就是
一台开着 22 端口、要口令登录的机器,验的东西一样。

### 第四阶段验收记录(2026-09-02)

命令面:`crater pkg index` 生成、`crater repo add/update/list/remove` 订阅、
`crater search` 查、`crater install <包名>` 装。

| 判据 | 结果 |
| --- | --- |
| 生成索引 | 4 个版本 / 3 个包,只读 manifest + config,零层下载 |
| 托管 | `python3 -m http.server` 就够 —— 任何静态 HTTP,`file://` 与裸路径同样认 |
| 订阅 | `repo add lab http://…/index.yaml` 当场同步并报"3 个包" |
| 搜索 | 匹配包名与描述;**机群契约直接印在结果里**(`rustfs 1.0 lab/rustfs [storage×1]`)—— "要几台机器"是决定装不装的第一个问题 |
| 按名字装 | `crater install yq` → 解析成 `…/lib/yq:4.44.3` → 装成 |
| 指定版本 | `crater install yq:4.40.5` 解析到对应引用 |
| 离线 | `search` 只查本地缓存,不连网;坏索引不覆盖上一份好的 |

**两个真缺陷是被这一阶段的测试逼出来的:**

1. **索引按蓝图 `version:` 组织会静默丢版本。** 头一版这么写,结果 yq 的
   4.44.3 与 4.40.5 双双报成 `1`(蓝图自己的修订号),后一条覆盖了前一条。
   改成 **`version` 取 tag** —— 索引存在的意义是把"包名 + 版本"翻译成一条能
   拉的引用,而能拉的只有 tag。蓝图的 `version:` 降为 `blueprint_version`
   字段(与 Helm 的 chart version / appVersion 同构),两者不一致时
   `pkg push` 提醒一句。
2. **`install <包名>:<版本>` 会撞上同名目录并静默装错版本。** 包目录按包名
   命名,`install yq:4.40.5` 复用了上次 `install yq` 摊下的 4.44.3 目录 ——
   引用解析对了、日志也对,装上去的却是另一版。修法是摊包时留一个
   `.crater-pkg` 印记,来源对不上就**拒绝**并给出换目录的命令。

**一条既有限制值得写明**:物料没有声明 `sha256:` 且没有闭包时,crater
**判不出漂移**(既有测试 `a_remote_material_without_a_digest_admits_it_cannot_tell`
就是它的封条)—— 换版本时目标机会保留旧字节并报"无变更"。带 `--full`
(字节在本地,能算摘要)或在蓝图里声明 `sha256:` 就正常:实测
`install …yq:4.40.5 --full` 判出 `~ 将修改`,降级成功,`yq --version`
从 4.44.3 变成 4.40.5。

### 第二阶段验收记录(2026-09-02)

`crater pkg push <目录> <ref> --arch amd64 --arch arm64` —— 物料随包,一个 tag
装下两个架构。全部在 zot 与一台**真断网**的目标机上验:

| 判据 | 结果 |
| --- | --- |
| 多架构 index | 顶层 `application/vnd.oci.image.index.v1+json`,两条 `platform` 条目 |
| **蓝图层跨架构共享 digest** | config 1 个 digest、蓝图层 1 个 digest、物料层 2 个 —— 设计里那句话是真的,registry 只存一份 |
| 物料层类型与注解 | `vnd.crater.material.v1` + `fetch=dependency` + `source=<渲染后的 URL>` |
| 瘦拉 vs 全量 | **20K vs 9.6M**。且全量只拉本机架构那一份(9.5M),不是两架构的 18.8M |
| 断网安装 | 目标机 `iptables -A OUTPUT --ctstate NEW -j REJECT`,`install --full --yes` 装成,`yq --version` 报 v4.44.3 |
| **反证** | 同一台断网机不带 `--full` → `curl: (6) Could not resolve host`,装不上。断网是真的,不是摆设 |

**`--for` 表达不了多架构,这是个真缺口。** `bake_scope` 把多次 `--for` 合并成
**一个**画像,所以 `--for arch=amd64 --for arch=arm64` 是后者覆盖前者 ——
错误提示里那句"(可给多次)"指的是可以给多个不同的 key,不是同一个 key 给多个
值。新的 `--arch` 单开一个维度,正好对上 OCI `platform` 字段;其余维度仍走
`--for`,与每个 `--arch` 合并。

**索引解析改成"本机架构优先"**(原来硬编码 amd64,D-061)。原来的行为在
arm64 机器上会**静默**装错架构的字节 —— 静默,是因为摘要对得上(那是 amd64
那份的摘要),直到目标机上 exec 才报 `Exec format error`。

**`--closure` 接受 `oci://<ref>`**:包里的物料层已经在本地 store 里按 sha256
躺着,不解包、不复制,直接把路径交给 `BlobMap`。这印证了 D-119 那两个接口
够窄 —— 加第二个 blob 后端没让 `BlobSource` 多一个方法。

### 第三阶段验收记录(2026-09-02)

`crater install <引用|目录> -i <机群> [--set …] [--yes]` —— 三道门按顺序在
**连第一台机器之前**关完:

| 判据 | 结果 |
| --- | --- |
| 空工作区一条引用装成 | `install …/yq:4.44.3 -i lab.inventory.yaml --yes` → 目标机 `yq --version` 报 v4.44.3 |
| 闸门 | 不带 `--yes` 时打印计划后停住,并给出照抄即可的 `crater apply` 命令 |
| 机群契约 | rustfs 要 `storage` 组,inventory 没有 → 打印对账表后报错,**一台机器都没碰** |
| 必填参数 | 缺 `endpoint` → 报出现成的 `--set endpoint=…`,不追问(管道里挂住等输入是最难查的一类故障) |
| 顺序 | 蓝图 lint → 必填参数 → 机群对账 → 落 app → plan;前三道全在本地 |
| 敏感参数不落盘 | `--set secret_key=…` **不写进** app 文件,改写成一行"下次怎么给"的注释 |

**过程中揪出一个真缺陷**:`library/rustfs` 的 `secret_key` **漏标了
`secret: true`**(试金石 fixture 标了、库里那份没标),于是第一次跑就把真值
写进了 app 文件。修法是两层:补上声明,再加一道**按名字**的兜底 ——
`install` 拉的是别人做的包,作者标没标不由我们决定,而 app 文件是要进 git 的。
宁可多扣一个(连同"下次怎么给"一起报出来),不可漏放一个。

**没做**:UI 目录的"远端源"选项卡。它要的数据(`pkg inspect` 的契约)已经
就位,是纯前端接线,与第四阶段的索引一起做更顺。

## 十一、不做什么(边界)

- **不做通用 OCI 客户端**。协议层继续全交给 `oci-client`(D-087),crater 只写制品
  语义;不引入 oras CLI。
- **不在 registry 里做搜索**。没有可抄的先例,行业全部外包;索引文件够用。
- **不做 OCI 1.1 专属能力**(artifactType、referrers、subject)的硬依赖;将来要做
  签名时走 cosign 式 tag fallback。
- **不把闭包塞进蓝图层**。两者分层是这份设计的全部意义。
- **不省 plan 闸门**。`install --yes` 是"我看过了",不是"别给我看"。

## 十二、参考

- 自家:D-033(自定义类型过线)、D-078(增量拉取)、D-087/D-088(thin pull 三态,
  真机数字)、D-098(project artifact)、D-106(方向定案)、D-119(存储三分与窄接口)、
  `docs/architecture.md §5.1`
- Helm:`helm.sh/docs/topics/registries`、`helm.sh/blog/storing-charts-in-oci`、
  `helm/helm#9983`(search 对 OCI 不支持,关闭为 Stale)、`pkg/registry/{constants,client}.go`
- OCI:image-spec `manifest.md` / `image-index.md` / `descriptor.md`;distribution-spec
  `spec.md`(referrers 与 tag schema;`_catalog` 为 reserved prior extension)
- 先例:timoni.sh/module、fluxcd.io OCI artifacts 与 OCIRepository `layerSelector`、
  kitops.org ModelKit spec、Homebrew discussions #4335、oras.land pushing_and_pulling
- registry:zotregistry.dev(architecture / GraphQL)、goharbor.io 2.0/2.9/2.10 博客、
  Docker Hub OCI artifacts 公告(2022-10-31)、GitHub community #163029(GHCR 无 referrers)、
  阿里云 ACR 自定义制品文档与 v1.1.0 支持说明
