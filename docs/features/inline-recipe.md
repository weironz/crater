# spec 内联 recipe（单文件部署）

> ADR: D-025 ｜ 设计: [design.md §3.1](../design.md)

## 这是什么

spec 里的组件可以**内联写 recipe**（`install`/`verify`/`preflight`/`register`），不必引用 `components/<name>/`。一个 yaml 文件即可完整描述一次部署；`components/` 退化为**可选**的复用库。

三种用法并存，按场景选：
| 用法 | 命令 | 适合 |
|---|---|---|
| 零 spec | `crater <name> --host …` | 临时单机 |
| 单文件内联 | `crater apply -f x.yaml` | 一个文件讲清一次部署 |
| 分离复用 | `crater apply -f x.yaml`（引用 `components/<name>`） | recipe 要复用/打包 |

## 基本 demo

`examples/yq-inline.yaml`（inventory + recipe 同一文件）：
```yaml
inventory:
  hosts:
    - { name: test, address: <host>, user: root, password: "<pw>", roles: [yq] }
components:
  - name: yq
    version: "4.53.2"
    install:                          # ← 有 install/verify 即走内联，免 components/
      - { action: download, url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64", dest: /usr/local/bin/yq }
      - { action: run_cmd, cmd: "chmod +x /usr/local/bin/yq", check: "test -x /usr/local/bin/yq" }
    verify:
      - { action: run_cmd, cmd: "/usr/local/bin/yq --version" }
```
```bash
crater apply -f examples/yq-inline.yaml
```

## 验证（真机）

`crater apply -f examples/yq-inline.yaml` 部署 yq，全程**未读 `components/yq/`**，幂等 `ok=3`。

## 边界 / 后续

- 内联 = 不可复用；要复用/打成 OCI 镜像分发就放 `components/`。
- 统一由 `resolve_descriptor` 加载（内联 vs 磁盘），apply/build/deploy 共用。
