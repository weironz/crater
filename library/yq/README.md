# yq(blueprint)

单二进制工具下发 —— 最小蓝图长这样:一个物料变体(URL 按 `${substrate.arch}`
插值)+ 一条 copy + 一条 health。

```bash
crater apply library/yq/yq.blueprint.yaml -i inventory.yaml
```
