# group —— 确保系统组存在 / 不存在

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `name` | ✔ | 组名 |
| `state` | | `present`(默认)/ `absent` |
| `system` | | 系统组(`groupadd -r`) |

## 语义 / 幂等

- present:`groupadd`,探针 `getent group`;absent:`groupdel`,探针取反。

## 示例

```yaml
- id: grp
  action: group
  name: docker
  system: true
- action: user
  name: deploy
  groups: [docker]
  needs: [grp]
```

## 关联

ADR:D-037-b。相关:[user](user.md)。
