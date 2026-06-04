# user —— 确保系统用户存在 / 不存在

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `name` | ✔ | 用户名 |
| `state` | | `present`(默认)/ `absent` |
| `system` | | 系统用户(`useradd -r`) |
| `shell` | | 登录 shell(`-s`) |
| `home` | | 家目录(`-d` + `-m` 创建) |
| `groups` | | 附加组列表(`-G`) |

## 语义 / 幂等

- present:`useradd`,探针 `id <name>`(已存在 → ok,**不会**改既有用户的 shell/groups——要改用 shell 模块显式 `usermod`)。
- absent:`userdel -r`(失败回退不带 `-r`),探针 `! id`。

## 示例

```yaml
- action: user
  name: zot
  system: true
  shell: /usr/sbin/nologin
```

## 关联

ADR:D-037-b。相关:[group](group.md)(先建组再建用户,用 needs 排序)。
