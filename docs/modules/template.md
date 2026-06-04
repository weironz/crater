# template —— 渲染模板物料写到目标

用 **minijinja** 渲染一个 `kind: file` 模板物料(如 `.j2`),支持 `{% for %}`/`{{ }}`(D-075)。
与 `copy`/`shell` 的"只许字面替换"不同,模板文件是**独立物料**,允许真模板语法——因为它
不藏在 task YAML 里,可独立审阅。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `material` | ✔ | `kind: file` 的物料名,其内容是模板 |
| `dst` | ✔ | 目标机路径 |

## 语义 / 幂等

- 渲染发生在**控制机**(拿得到 inventory 上下文),lower 成 `WriteFile`(sha256 幂等)→ agent/离线都能跑。
- 上下文:标量 vars + 结构化 `groups.<role>` = `[{name, ip}]`(如 haproxy backend `{% for h in groups.controlplane %}`)。
- 模板字节打进 OCI(物料层),离线自洽;在线读 material 的 `src` 本地文件。

## 示例

```yaml
materials:
  - name: haproxy-cfg
    kind: file
    src: templates/haproxy.cfg.j2
actions:
  - action: template
    material: haproxy-cfg
    dst: /etc/haproxy/haproxy.cfg
    notify: [restart_haproxy]
```

## 关联

ADR:D-075(minijinja 模板层)、D-036(为何只有 template 配真模板)。相关:[copy](copy.md)(不渲染的放置)。
