# 发布你自己的 crater 包

**先说结论:绝大多数人不需要建任何东西。**

crater 没有中心仓库,也不打算有。包就是 OCI 制品,推到**你自己的** registry
(ghcr / Harbor / zot / 内网哪台机器都行),别人用完整引用就能装。这和
`docker run someuser/image` 是同一个形状 —— 引用本身就是完整地址。

分三档,按你实际要干的事对号入座。

## 第一档:只想用别人的包

什么都不用建。

```bash
# 知道引用就直接装(`oci://` 前缀可省)
crater apply ghcr.io/someone/pkgs/redis:7.2 -i inventory.yaml

# 或者订阅别人的索引,之后用名字
crater repo add theirs https://example.com/index.yaml
crater search redis
crater apply redis -i inventory.yaml
```

可以同时订阅多个索引。同名包出现在多个仓库时 crater **不会替你猜** ——
它会列出是哪几个,让你用 `--repo <名>` 指明。

## 第二档:发一个包给别人用

**一条命令,不需要索引,不需要 CI。**

```bash
crater push ./my-redis ghcr.io/<你>/pkgs/redis:7.2
```

对方装它:

```bash
crater apply ghcr.io/<你>/pkgs/redis:7.2 -i inventory.yaml
```

`oci://` 前缀可省。写了也认 —— helm 3.8+ 是那个写法,从 helm 过来的人照着
敲不会撞墙。

版本发现也不需要索引 —— OCI 自带 `tags/list`:

```bash
crater tags ghcr.io/<你>/pkgs/redis      # 有哪些版本
crater apply 'ghcr.io/<你>/pkgs/redis:7.*'    # 范围解析
```

**这一档覆盖了绝大多数情况。** 内部团队之间、给客户交付、开源一个包 ——
都到这里为止。

### 一份蓝图发多个版本

`--set` 就是变量渲染,和 helm 一个意思 —— 只是 crater 的参数分**两个阶段**:

| 什么时候给 | 命令 | 覆盖的是 |
| --- | --- | --- |
| 打包时 | `crater push … --set k=v` | `stage: build` 的参数(定死在包里) |
| 部署时 | `crater apply … --set k=v` | `stage: apply` 的参数(每次部署可不同) |

分两段的理由很实在:`version` 决定**下载哪个 URL**,而物料要在打包时就抓下来
烤进闭包 —— 部署时才问就太晚了。

```bash
# 同一份蓝图,发两个版本;源文件一个字不改
crater push ./my-redis reg/ns/redis:7.2 --set version=7.2 --set sha_amd64=…
crater push ./my-redis reg/ns/redis:7.4 --set version=7.4 --set sha_amd64=…
```

覆盖值**烤进包里的那份蓝图**,不是另存一处 —— 解包出来的蓝图自己就说自己是
7.2,不会"写着一套装的是另一套"。注释、空行、键序都保留。

**摘要要和版本一起给。** 物料上钉的 `sha256:` 和 URL 是一对(某个版本的字节 +
那份字节的摘要),只换版本不换摘要,落地时会摘要不符 —— 那是**对的**,内容
寻址就该在这里拦住。把摘要也写成参数即可:

```yaml
params:
  version:   { default: "7.2", stage: build }
  sha_amd64: { default: "c5f0…", stage: build }
materials:
  - name: bin
    file: "https://…/v${params.version}/redis-amd64"
    sha256: "${params.sha_amd64}"
```

**不要**给同一份内容打不同的版本 tag —— 那样索引会声称存在一个装下去货不
对板的版本(我们自己踩过,见 D-159)。

## 第三档:让人能"按名字"搜你的一批包

只有这一档需要索引,而它**仍然只是一条命令**:

```bash
crater index oci://ghcr.io/<你>/pkgs/redis \
                 oci://ghcr.io/<你>/pkgs/nginx \
                 -o index.yaml
```

然后把 `index.yaml` 放到任何静态 HTTP 上 —— GitHub Pages、对象存储、
内网 nginx、甚至 U 盘。对方:

```bash
crater repo add yours https://<你的地址>/index.yaml
crater search redis
```

### 索引是什么、不是什么

**是**:一份"有哪些包"的清单。OCI 规范里**没有搜索端点**(`_catalog` 不在
规范里,Docker Hub 还禁用它),所以"有哪些包"这个问题只能靠一个索引文件
回答。它是普通静态文件,能随闭包一起进 U 盘 —— 这正是气隙场景要的。

**不是**:装包的必需品。第二档已经证明了这一点。也不是版本发现的必需品 ——
`tags/list` 就够。

### 索引要从 registry 生成,不要手写

`index` 去 registry 问"实际有哪些 tag",再逐个读它们的契约。手写、或者
"照着我记得发过什么"写,迟早会与 registry 不一致 —— 而不一致的表现是**装
下去不是你以为的东西**。

发完新版本重跑一次同一条命令即可;多个来源可以一次给全,或者用 `--merge`
并进现有索引。

## 要不要建 CI

**可选。** 如果你发包很频繁,可以把上面那两条命令(push + index)放进 CI,
省掉"发完忘了更新索引"。crater 自己的官方索引就是这么做的,可以照抄:

- [`.github/workflows/packages.yml`](../.github/workflows/packages.yml) ——
  推包 + 从 registry 重建索引 + 有变化才提交
- [`scripts/pkg-tag.py`](../scripts/pkg-tag.py) —— tag 由蓝图自己决定,
  不让人手填(手填就会填错,而填错的表现是货不对板)

但**这只是自动化,不是机制**。手工跑两条命令一样成立。

## 规模上的老实话

单个 `index.yaml` 到几百个包都没问题;上千之后文件会变大、`repo update`
会变慢。helm 的 `index.yaml` 是同一个形状,也有同一个上限。

真到那个规模时的出路是分片索引或者一个聚合服务(helm 那边是 Artifact Hub),
而不是把索引做大。**现在不做**:没有那个规模,而为它设计会把简单的东西
先复杂化。

## 相关

- [`library/yq/README.md`](../library/yq/README.md) —— **一个包的完整生命周期走查**,
  每条输出都是真跑出来的
- [`packages/README.md`](../packages/README.md) —— crater 官方索引怎么订阅
- D-123(包分发设计)、D-128(索引与搜索)、D-159(为什么索引必须由机器生成)
