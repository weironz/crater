# yq —— 单文件二进制交付(最简示例)

把 `yq` 二进制放到目标 `/usr/local/bin`,演示 `material: binary` + `place` + `verify`。

```bash
crater inspect yq                         # 看参数(version)
crater apply  yq --host 10.0.0.5 --password <pw>
crater build  -f library/yq/yq.yaml -t myreg/yq:4.53.2   # 离线 OCI
```
单体交付,无 role/inventory.example——参数全在 `yq.yaml` 的 `params`。
