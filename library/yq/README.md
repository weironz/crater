# yq —— 单文件二进制交付(最简示例)

把 `yq` 二进制放到目标 `/usr/local/bin`,演示 `material: binary` + `place` + `verify`。

```bash
crater inspect yq                         # 看参数(version)
crater apply  yq --host 10.0.0.5 --password <pw>
crater build  -f library/yq/yq.yaml -t myreg/yq:4.53.2   # 离线 OCI
```
单体交付,无 role/inventory.example——参数全在 `yq.yaml` 的 `params`。

## 构建制品(justfile)

本目录带一个 `justfile`(需 [just](https://github.com/casey/just)),把 build/push/save 串好,
版本从 `yq.yaml` 的 `params.version` 自动取(单一真相):

```bash
cd library/yq
just                                    # 列出命令
just build                              # 构建到本地库 ~/.crater/store
just push                               # 构建并推送到 registry
just save                               # 构建并导出离线 .oci
just registry=192.168.1.5:5000 push     # 覆盖 registry(私有/离线库)
```

默认 `registry := "willdockerhub"`,改 `justfile` 顶部变量即可换成你的命名空间。
