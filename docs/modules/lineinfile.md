# lineinfile —— 确保某行存在 / 不存在

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `path` | ✔ | 目标文件 |
| `line` | ✔ | 期望的整行内容 |
| `regexp` | | 匹配要替换/删除的旧行(ERE);present 时先删匹配行再追加 |
| `state` | | `present`(默认)/ `absent` |
| `create` | | 文件不存在则先创建(含父目录) |

## 语义 / 幂等

- present 探针 `grep -qxF '<line>'`(整行精确匹配,命中 → ok 跳过);absent 反之。
- present + `regexp`:先 `sed -E '\|re|d'` 删旧行,再追加新行(实现"替换")。
- `line`/`regexp` 以单引号进 shell,内容**不要含单引号**。

## 示例

```yaml
- action: lineinfile
  path: /etc/sysctl.d/k8s.conf
  line: "net.ipv4.ip_forward = 1"
  regexp: "^net.ipv4.ip_forward"
  create: true
  notify: [reload_sysctl]
```

## 关联

ADR:D-037-b。相关:[copy](copy.md)(整文件管理优先;lineinfile 适合改别人拥有的文件)。
