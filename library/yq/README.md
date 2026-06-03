# yq —— 单文件二进制交付(最简示例)

把 `yq` 二进制放到目标 `/usr/local/bin`,演示 `material: binary` + `place` + `verify`。

```bash
crater inspect yq                         # 看 materials / 契约
crater apply  yq --host 10.0.0.5 --password <pw>
crater build  -f library/yq/yq.yaml -t myreg/yq:4.53.2   # 离线 OCI
```
单体交付,无 role/inventory.example。版本是 `yq.yaml` 的内部 `vars`(非对外契约):
默认 4.44.3,`crater build --set version=X` 或 `just version=X` 可覆盖。

## 构建制品(justfile)

本目录带一个 `justfile`(需 [just](https://github.com/casey/just)),把 build/push/save 串好,
版本默认从 `yq.yaml` 的 `params.version` 取(基线/单一真相):

```bash
cd library/yq
just                                    # 列出命令
just build                              # 构建到本地库 ~/.crater/store
just push                               # 构建并推送到 registry
just save                               # 构建并导出离线 .oci
just version=4.45.1 push                # 临时换版本(不改 yaml)
just registry=192.168.1.5:5000 namespace=lib push  # 覆盖私有/离线库
```

- `registry`(默认 `docker.io`)+ `namespace`(默认 `willdockerhub`)→ ref =
  `docker.io/willdockerhub/yq:<version>`。换私有库只改 `registry`。
- `version` 默认从 `yq.yaml` 取;`just version=X` 临时覆盖时,build recipe 用
  `crater build --set version=X` 把**实际拉取的版本和 tag 一起改**(D-089),二者
  永远一致——不会出现 tag 写 X、内容却是基线版本的情况。永久改版本就改 `yq.yaml`。
