# unarchive —— 解压归档到目标目录

两种来源:`material`(取+解压一步,D-073)或 `from`(目标机已有文件)。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `to` | ✔ | 解压目的目录(自动 `mkdir -p`) |
| `material` | 二选一 | 归档物料名:在线下载 `url_tmpl` / 离线流式推 blob,然后解压——免去手动"先放 /tmp 再解" |
| `from` | 二选一 | 目标机上已存在的归档路径 |
| `strip` | | `tar --strip-components` 值,默认 0 |
| `creates` | | 幂等探针:此路径已存在 → 视为已解压,跳过(报 `ok`) |

## 语义 / 幂等

- `material` → `UnarchiveMaterial` op(blob 或 url);`from` → `tar -xf` shell。
- 不写 `creates` 则每次重解(覆盖安全);写上才幂等——声明解压产物里的关键文件(如 `dockerd`)。

## 示例

```yaml
materials:
  - name: docker-tgz
    kind: file
    arch: amd64
    url_tmpl: "https://download.docker.com/linux/static/stable/x86_64/docker-{{version}}.tgz"
actions:
  - id: extract
    action: unarchive
    material: docker-tgz
    to: /usr/local/bin
    strip: 1
    creates: /usr/local/bin/dockerd
```

## 关联

ADR:D-073(material 直取)、D-048(arch)。相关:[copy](copy.md)、[materials](../features/materials.md)。
