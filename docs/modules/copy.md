# copy —— 放一个文件到目标机路径

来源**三选一**(多给/不给都报错),目的地一个 `dest`。D-090 起原 `place` 并入此模块。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `dest` | ✔ | 目标机绝对路径 |
| `content` | 三选一 | 内联文本,写入前做 `{{var}}` 字面替换 |
| `src` | 三选一 | 控制机文件(task 目录相对路径)。读取后**内联进计划**(agent 可执行),文本 only(非 UTF-8 报错),**不渲染** |
| `material` | 三选一 | 按逻辑名引用 `materials:` 物料。二进制安全、arch 变体(D-048)、可打进 OCI |
| `mode` | | chmod(如 `"0755"`),折进同一步 |

## 语义 / 幂等

- `content`/`src` → `WriteFile`(sha256 比对,一致报 `ok` 不写)。
- `material` → 三态解析(D-087/D-088):
  1. **有 blob**(纯离线全量包 / 瘦在线的 embedded 层)→ 控制面 `PushFile` 原样推送(sha256 幂等);
  2. **无 blob + 严格离线** → 报错(气隙不允许现拉);
  3. **无 blob + 在线** → material 有 `src` 则从控制机推送;有 `url_tmpl` 则目标机自己 `curl`(探针 `test -s dest`)。
- material 的 arch 变体按目标机 `uname -m` 选;`{{arch}}` 注入 url 模板(D-064)。
- 计划含 `PushFile`(blob / material-src)时强制控制面执行(agent 读不到控制机路径)。
- `src` 与 `material` 的取舍:`src` 直读文本内联,**不进 BOM、不打包**;要离线分发/二进制 → 声明 material。

## 示例

```yaml
materials:
  - name: yq-bin
    kind: file
    arch: amd64
    url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_{{arch}}"
actions:
  - id: install
    action: copy
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"
  - action: copy
    src: files/daemon.json
    dest: /etc/docker/daemon.json
    notify: [restart_docker]
  - action: copy
    content: "PermitRootLogin no\n"
    dest: /etc/ssh/sshd_config.d/90-crater.conf
```

## 关联

ADR:D-068(copy 原语)、D-090(place 并入)、D-034(物料闭包)、D-048(arch 变体)、D-087/D-088(三态)。
相关:[template](template.md)(要渲染用它)、[unarchive](unarchive.md)、[materials](../features/materials.md)。
