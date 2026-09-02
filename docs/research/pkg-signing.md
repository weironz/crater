# crater 包签名:要不要做、怎么做

> spike(issue #6)。产出是判断和一页设计,不是代码。
>
> 实测基线:2026-09-02,Docker Hub(`willdockerhub/crater-pkg-compat`)。
> 测试制品已删除。

## 一、结论速览

1. **洞是真的**:`crater install` 从 registry 拉一个包,然后在生产机上执行它。
   中间没有任何东西能回答"这个包是不是发布者发的"。**谁能往那个 registry 写,
   谁就能让你的机器执行任意东西。**
2. **Docker Hub 支持 referrers API —— 实测推翻了调研笔记。** D-123 §3.4 写的是
   「Docker Hub 无 referrers,仍停在 dist-spec 1.0.1」。实测:推一个带 `subject`
   的 manifest,`GET /v2/<repo>/referrers/<digest>` 返回 **200 + 1 条**,注解原样
   带回,`?artifactType=` 过滤也生效。这条使签名的存放问题从"要设计回退"变成
   "两条路都通"。
3. **tag schema 同样可用**(`sha256-<hex>` 作 tag,201,自定义层类型原样保留)。
   所以设计上**主用 referrers、回退 tag schema**,而回退这条已经验过、不是纸面
   承诺。
4. **不自建签名格式,走 cosign 的布局**。它是事实标准,而"布局"是免费的 ——
   我们照它的形状存,别人的 cosign 就能验我们的包,我们也不必写密码学代码。
5. **验签失败必须硬失败**,且**默认开启**。可关,但要显式关。
6. **现在做的部分:只做"不关门",不做实现。** 见第六节的分期与触发条件。

## 二、威胁模型:到底防谁

诚实地把范围划出来,否则会滑向"签名能解决一切"的错觉。

| 威胁 | 签名管不管 |
| --- | --- |
| registry 被入侵 / 被投毒,包被替换 | **管** —— 这是唯一的核心场景 |
| 中间人改传输中的字节 | 不需要签名:TLS + 内容寻址(digest)已经管住 |
| 发布者自己发了个坏包 | **不管**。签名证明"是他发的",不证明"是好的" |
| 目标机被入侵 | 不管 |
| 拉包的人拿错了包名 | 不管(那是索引与命名的事) |

所以签名要回答的问题**只有一个**:这份字节是不是那个私钥的持有者发的。

## 三、实测记录(2026-09-02,Docker Hub)

三条路各推一次、各读一次,凭据来自 `~/.docker/config.json`(D-129):

| 判据 | 结果 |
| --- | --- |
| `GET /v2/<repo>/referrers/<digest>`(无关联时) | **200** + 空 index(不是 404) |
| 推带 `subject` 的 manifest | **201** |
| 推完再查 referrers | **200,1 条**;`annotations` 原样带回;`artifactType` 正确 |
| 按 `?artifactType=` 过滤 | **200,1 条** |
| tag schema:推到 `sha256-<hex>` | **201** |
| tag schema:读回来 | **200**,自定义层 mediaType 原样保留 |

**过程中自己犯的错值得记**:第一次测 tag schema 报
`MANIFEST_BLOB_UNKNOWN`,我差点写成"Docker Hub 不支持"。真因是我用了单步
blob 上传(`POST .../uploads/?digest=`),Docker Hub 返回 202 开了会话却没提交。
改成两步(`POST` 拿 Location → `PUT ?digest=`)就是 201。**registry 说"不行"
的时候,先怀疑自己的请求。**

## 四、设计

### 4.1 布局:照 cosign 的形状

签名本身是一个**普通 OCI 制品**,与被签的包同仓库:

```
config  application/vnd.crater.signature.config.v1+json   (元数据:签名算法、密钥标识)
layer   application/vnd.crater.signature.v1+json          (签名载荷)
annotations:
  org.crater.signature.subject: sha256:<被签包的 manifest digest>
subject: { digest: sha256:<同上>, ... }                   ← referrers 关联靠它
```

**签的是 manifest digest,不是 tag。** tag 可变,digest 不可变 —— 签一个可变的
东西等于没签。这也意味着 `crater install name:1.0` 的验签路径是:先解析 tag →
digest,再验那个 digest 的签名。

### 4.2 存放:主用 referrers,回退 tag schema

```
1. GET /v2/<repo>/referrers/<digest>?artifactType=...
   ├─ 200 且有条目 → 用它
   ├─ 200 但空     → 没签名(不是"不支持")
   └─ 404          → registry 不支持,走 2
2. GET /v2/<repo>/manifests/sha256-<hex>
```

回退是**规范强制**的(dist-spec 1.1),而且实测两条都能用 —— 这不是一条写了
不跑的分支。

### 4.3 不自建密码学

签名与验签交给 **cosign 的布局 + 一个 Rust 签名库**(ed25519 / ECDSA-P256)。
crater 只负责:算 digest、组制品、推/拉、比对。

**不 exec cosign 二进制**:与"静态单二进制"冲突,而且没必要 —— 我们要的是
它的**格式**,不是它的进程。照它的形状存,别人拿 cosign 也能验我们的包,
这是格式兼容白拿的好处。

**keyless(Fulcio/Rekor)不做**:它要联网到 Sigstore 的公共服务,与 air-gap
直接冲突,而 air-gap 恰恰是 crater 的主场景。

### 4.4 验签失败:硬失败,默认开

- 有签名且验不过 → **失败**,不装。
- 有签名且验得过 → 继续。
- **没有签名** → 取决于策略:
  - 默认 `--verify=if-present`:有就验,没有就装(**过渡期**,不然存量包全废)
  - `--verify=require`:必须有签名,没有就失败(生产该用这个)
  - `--verify=off`:显式关掉

**过渡期默认宽松是有代价的**:攻击者只要**删掉签名**就能绕过。所以
`if-present` 只能是过渡默认,文档要写明它挡不住主动攻击,并给出
"仓库级要求签名"的配置位置。

### 4.5 离线怎么验

公钥随 `crater repo add` 一起配:

```yaml
# ~/.crater/repos.yaml
repos:
  lab:
    url: https://.../index.yaml
    pubkey: |
      -----BEGIN PUBLIC KEY-----
      ...
```

`pkg save` 时把签名制品一起导出;U 盘那头 `pkg load` 时一起进来。公钥不进包
(那等于自己给自己开证明),它属于**订阅关系**,与索引地址同级。

## 五、边界:不做什么

- **不做 keyless / 透明日志**:要联网到公共服务,与 air-gap 冲突。
- **不做证书链 / TUF**:那是发行版级别的信任基础设施,crater 现在的规模用不上,
  引进来就得长期维护。
- **不 exec cosign**:要它的格式,不要它的进程。
- **不签闭包物料的每一份字节**:物料已经有 `sha256:` 或闭包内容寻址,签 manifest
  就覆盖了整棵树(digest 是递归的)。

## 六、分期:现在做什么

**现在不做实现。** 理由是价值与"有多少人拉你的包"成正比,而现在是零;
而做早了要背着一套密钥管理走很久。

**但两件事现在就该定,因为它们只是"不关门"**(实现时零成本,补救时很贵):

1. **命名空间预留**:`org.crater.signature.*` 注解前缀与
   `application/vnd.crater.signature.*` 两个 mediaType,现在起不用于别的用途。
2. **digest 可见**:`pkg push` 的输出要给出 manifest digest,`pkg inspect`
   支持 `@sha256:` 引用 —— 将来"按 digest 钉住 + 验签"才有着落。

**触发条件**(满足任一条就启动实现):

- 有第二个人/团队开始拉这个 registry 上的 crater 包;
- 包被放到公开 registry 上供不特定人使用;
- 有合规要求(供应链证明)。

工作量估计:格式与推拉半天(基础设施都在),验签与密钥管理一天,
`--verify` 策略与文档半天。**实测已经把最大的不确定性消掉了** ——
存放路径两条都验过能用。

## 七、参考

- 自家:D-123(包分发设计)、D-129(ACR 拒收 + docker 凭据回退)、
  D-126(Docker Hub 兼容实测)
- 本次实测脚本思路:两步 blob 上传 → PUT manifest(带/不带 `subject`)→
  查 referrers → 按 artifactType 过滤
- OCI:distribution-spec 1.1 的 referrers API 与**强制**的 tag schema 回退
- cosign:签名制品布局(`subject` + 签名层),我们借形状不借进程
