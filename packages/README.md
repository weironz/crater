# 官方包索引

> 这份索引只管 crater **自己发的**那几个包。
> 想发你自己的包?看 [发布你自己的 crater 包](../docs/publishing.md) ——
> 绝大多数情况下**一条 `crater pkg push` 就够,不需要索引、不需要 CI**。

`packages/index.yaml` 是 crater 的官方包索引。订阅它:

```bash
crater repo add crater https://raw.githubusercontent.com/weironz/crater/main/packages/index.yaml
crater search yq
crater apply yq -i inventory.yaml
```

## 索引是什么、不是什么

**是**:一份"有哪些包"的清单。OCI 规范里没有搜索端点(`_catalog` 不在规范
里,Docker Hub 还禁用它),所以"有哪些包"这个问题只能靠一个索引文件回答。
它是普通静态文件,能随闭包一起进 U 盘 —— 这正是气隙场景要的。

**不是**:版本发现的必需品。远端有哪些版本,`tags/list` 就能问出来:

```bash
crater pkg tags ghcr.io/weironz/crater/yq       # 不需要索引
crater apply 'oci://ghcr.io/weironz/crater/yq:4.*'   # 范围解析也不需要
```

## 怎么更新

推完新版本之后重新生成:

```bash
crater pkg index oci://ghcr.io/weironz/crater/yq -o packages/index.yaml
```

多个包就多给几个来源,或者加 `--merge` 并进现有索引。

**`pkg index` 会重写整个文件**,包括抹掉注释 —— 所以说明写在这份 README 里,
不写进索引本身。
