# file —— 管理路径状态

建目录 / 删路径 / touch 文件,带可选 mode/owner/group。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `path` | ✔ | 目标机路径 |
| `state` | ✔ | `directory` / `absent` / `touch` |
| `mode` | | chmod |
| `owner` / `group` | | chown/chgrp |

## 语义 / 幂等

| state | lower 成 | 探针 |
|---|---|---|
| `directory` | `mkdir -p` (+chmod/chown) | `test -d` |
| `absent` | `rm -rf` | `test ! -e` |
| `touch` | `touch` (+chmod/chown) | `test -e` |

teardown 的主力模块:删二进制/配置/数据目录(见 [delete-teardown](../features/delete-teardown.md))。

## 示例

```yaml
- action: file
  path: /etc/zot
  state: directory
  mode: "0755"
- action: file
  path: /var/lib/docker
  state: absent
```

## 关联

ADR:D-037-b(原语扩充)、D-049(teardown)。
