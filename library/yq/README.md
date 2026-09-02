# yq —— 一个包的完整生命周期

这是库里**最小**的一个包:一条 `copy` 加一条健康探针。拿它走一遍完整流程 ——
构建、推送、拉取、使用、离线搬运、升降级、退役。

下面每一段的输出都是**真跑出来的**,不是照着帮助文档编的。

## 这份蓝图长什么样

```yaml
name: yq
version: "1"                          # 蓝图自己的修订号,不是 yq 的版本

params:
  version:   { default: "4.53.6", stage: build }   # 决定下载哪个 URL
  sha_amd64: { default: "c5f0…",  stage: build }   # 摘要与版本成对
  sha_arm64: { default: "88a1…",  stage: build }

materials:
  - name: yq-bin                       # 同名两条,靠 when: 分成变体
    when: "substrate.arch == 'amd64'"
    file: "https://…/v${params.version}/yq_linux_amd64"
    sha256: "${params.sha_amd64}"
  - name: yq-bin
    when: "substrate.arch == 'arm64'"
    file: "https://…/v${params.version}/yq_linux_arm64"
    sha256: "${params.sha_arm64}"

resources:
  - copy: { material: yq-bin, dest: /usr/local/bin/yq, mode: "0755" }

health:
  - cmd: { run: "yq --version" }
```

三个设计点,都是踩出来的:

- **`version` 是 `stage: build`。** 它决定下载哪个 URL,而物料要在打包时就抓
  下来 —— 部署时才问就太晚了。
- **摘要按架构分开。** 一条带 `${substrate.arch}` 的 URL 配一个 sha256 是错的:
  那个摘要只对一个架构成立,另一个架构上落地即失败。
- **摘要写成参数**(`${params.sha_amd64}`)。这样一份蓝图能发出任意版本的包 ——
  版本和摘要一起 `--set`。

## 零、先静态检查

不连任何机器:

```console
$ crater lint library/yq/yq.blueprint.yaml
library/yq/yq.blueprint.yaml: ✓ yq (1 资源, 2 物料)
检查 1 个文件:0 error, 0 warn
```

## 一、构建 —— `pkg build`

只组装进本地 store,**不推**。想先看看包成什么样时用:

```console
$ crater pkg build library/yq -t localhost:5000/demo/yq:4.53.6
包 localhost:5000/demo/yq:4.53.6 —— 2 个文件,1.5 K
  · tag `4.53.6` 与蓝图 version `1` 不同 —— 索引与 install 按 tag 走
已入本地 store(sha256:2bd636dc…)—— `crater pkg push …` 推上去
```

那句"tag 与蓝图 version 不同"是**提醒不是错误**:蓝图的 `version: "1"` 是它
自己的修订号,而包的 tag 该是 yq 的版本。索引与 install 都按 tag 走。

## 二、推送 —— `pkg push`

### 在线包(默认):只装蓝图,不装物料

```console
$ crater pkg push library/yq localhost:5000/demo/yq:4.53.6
包 localhost:5000/demo/yq:4.53.6 —— 2 个文件,1.5 K
推送完成 → localhost:5000/demo/yq:4.53.6
  digest  sha256:687dcd84…
```

1.5 K —— 因为**物料没进包**。部署时目标机自己按 URL 下载。适合能联网的场景:
registry 里不必躺着几百兆。

### 离线包:`--arch` 把物料也烤进去

```console
$ crater pkg push library/yq localhost:5000/demo/yq:4.53.6-full --arch amd64
── amd64 ──
烘焙 `yq` —— 1 个物料变体
  ✓ yq-bin                          13.5 M  c5f056448f97
闭包 13.5 M —— 一份 manifest
推送完成 → localhost:5000/demo/yq:4.53.6-full
```

`--arch` 可以给多次(`--arch amd64 --arch arm64`),产出一个 image index,
**一个 tag 装下所有架构**,目标机按自己的架构挑。

### 发别的版本:`--set`

`--set` 就是变量渲染,和 helm 一个意思。覆盖值**烤进包里的那份蓝图**,
源文件一个字不动:

```console
$ crater pkg push library/yq <ref>:4.44.3 \
    --set version=4.44.3 \
    --set sha_amd64=a2c09718… --set sha_arm64=0e7e1524…
  · --set version=4.44.3 已烤进包里的蓝图
推送完成 → <ref>:4.44.3
```

**摘要必须和版本一起给。** 只换版本不换摘要,落地时会摘要不符:

```
执行失败 —— copy: 物料 `yq-bin` 摘要不符 —— 期望 c5f0…,实得 a2c0…(已删除落地文件)
Error: 1/1 台目标执行失败
```

**这是对的。** 内容寻址就该在这里拦住 —— 而且它删掉了已落地的文件,机器上不会
留下一个"来路不明"的二进制。

## 三、看一眼 —— `pkg inspect` / `pkg tags`

拿到别人的包,先问"我要准备什么":

```console
$ crater pkg inspect localhost:5000/demo/yq:4.53.6
蓝图 yq  v1
yq 命令行 YAML 处理器

参数:
  sha_amd64  默认 c5f0…  yq_linux_amd64 的 sha256 [构建期]
  sha_arm64  默认 88a1…  yq_linux_arm64 的 sha256 [构建期]
  version    默认 4.53.6  上游 release tag [构建期]

资源 1 项 · 物料 2 份 · 自定义类型 0 个 · 健康探针 1 条
```

远端有哪些版本 —— 走 OCI 的 `tags/list`,**不需要索引**:

```console
$ crater pkg tags localhost:5000/demo/yq
  4.53.6
  4.53.6-full

2 个版本。
```

## 四、使用

### 直接装(最常用)

```bash
crater apply localhost:5000/demo/yq:4.53.6 -i inventory.yaml
```

它会拉包 → 摊成 `yq-4.53.6/` → 落一个 `yq.app.yaml` → **印出计划** → 收敛。

`oci://` 前缀可省。装过之后机群与参数都记在 `yq.app.yaml` 里,后面就一个词:

```bash
crater apply yq        # 再收敛(幂等)
crater plan yq         # 只看会变什么
crater verify yq       # 对账
crater destroy yq      # 退役(默认只预览)
```

### 版本范围

```bash
crater apply 'localhost:5000/demo/yq:4.*'    # 4 这条线上的最新
crater apply 'yq:^4.44'                      # >=4.44,不跨主版本
```

范围靠 `tags/list` 解析,同样不需要索引。引号别忘了,`*` 会被 shell 展开。

### 先看再动

```console
$ crater plan yq
localhost    + copy /usr/local/bin/yq                       将创建
localhost  计划 +1 ~0 -0 ✓0
```

四态:`+` 将创建 / `~` 将修改 / `✓` 已经对了 / `?` **判不出**。
`?` 与 `✓` 是两回事 —— 前者是"我们不知道",后者是"我们知道它对"。

## 五、离线:U 盘搬到断网机房

**联网这头**(包必须在本地 store 里,所以先 `pkg build`/`push` 或
`pkg pull --full`):

```console
$ crater pkg save localhost:5000/demo/yq:4.53.6-full -o /media/usb/yq.pkg.tar
索引也放同一个目录,对面就能搜:
  crater pkg index --store -o /media/usb/index.yaml
```

导出来 14 M —— 蓝图加 13.5 M 的 yq 二进制,全在里面。

**断网那头**:

```console
$ crater pkg load /media/usb/yq.pkg.tar
  -          物料 1/1 份在本地

闭包完整,不用连网:
  crater install localhost:5000/demo/yq:4.53.6-full --full -i <机群>
  (`--full` 是关键 —— 少了它会去下载物料,断网现场必失败)

$ crater install localhost:5000/demo/yq:4.53.6-full --full --yes
localhost    changed copy
localhost  执行 changed=1 ok=0

$ yq --version
yq (https://github.com/mikefarah/yq/) version v4.53.6
```

**这一段是在 `--network none` 的容器里跑的,而且那个容器连 curl 都没装** ——
也就是说"下载"这条路根本不存在,装上去的字节只能来自那个 tar。

`--full` 少不得:没有它 install 走瘦拉,物料仍然按 URL 去下载。

## 六、升级、降级、对账、退役

摘要钉住之后,换版本是**真的会动手**的:

```console
$ crater apply yq:4.44.3
localhost  计划 +0 ~1 -0 ✓0        # ~1 = 会改
$ yq --version
yq … version v4.44.3

$ crater apply yq:4.53.6
localhost  计划 +0 ~1 -0 ✓0
$ yq --version
yq … version v4.53.6

$ crater apply yq:4.53.6            # 再来一次
localhost  计划 +0 ~0 -0 ✓1        # ✓1 = 已经对了,不动

$ crater verify yq
localhost  ✓ 现实符合期望,上次部署于 9 秒前

$ crater destroy yq                 # 默认只预览,--yes 才动手
```

**没有摘要会怎样**:crater 判不出目标机上那份是不是这一版,于是计划报 `?` 而
**不动手** —— `apply yq:4.44.3` 跑完机器上还是旧版本,而它会明说自己判不出并
给出两条出路。那不是 bug,是三态诚实(见 D-135 / D-160)。

## 七、发布索引(只有想让人"按名字搜"时才需要)

装包不需要索引,版本发现也不需要。只有"有哪些包"这个问题需要 —— 因为 OCI
规范里没有搜索端点:

```bash
crater pkg index oci://localhost:5000/demo/yq -o index.yaml
# 扔到任意静态 HTTP,对方:
crater repo add demo https://…/index.yaml
crater search yq
crater apply yq -i inventory.yaml
```

索引**从 registry 生成**,不要手写 —— 手写迟早和 registry 不一致,而不一致的
表现是**装下去不是你以为的东西**(D-159 就是这么发生的)。

## 相关

- [发布你自己的包](../../docs/publishing.md) —— 三档路径,大多数人停在第二档
- [官方索引](../../packages/README.md) —— `ghcr.io/weironz/crater/yq` 就是这么发的
- D-135(三态诚实)、D-159(索引必须由机器生成)、D-160(摘要与版本成对)、
  D-161(`--set` 与摘要参数化)
