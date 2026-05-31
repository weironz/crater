# module 模块化（四层模型 + 数据定义 module）

> ADR: D-029 ｜ 设计: [design.md §6.1](../design.md)

## 这是什么

为避免"每加一个 module 就改 Rust + 逼第三方 fork"，crater 借鉴 ansible 把 module 分四层（统一契约 = `check→act→StepStatus`）：

| 层 | 形态 | 改 Rust |
|---|---|---|
| 1 内置类型化 | Rust enum（download/pkg/systemd…） | 是（精选） |
| **2 数据定义** | `modules/<name>.yaml`（params+check+act 模板） | **否**（已实现的契约地基） |
| 3 外部 module | 脚本/静态二进制 + JSON 契约（agent 送达） | 否（后续） |
| 4 `run_cmd`+`check` | 裸命令 + 探针 | 否 |

本文聚焦**第 2 层（数据定义 module）**：放 `modules/<name>.yaml` 即用，零 Rust。

## 基本 demo

`modules/lineinfile.yaml`：
```yaml
params: [path, line]
check: 'grep -qxF "{{line}}" "{{path}}"'     # 命中→ok（跳过）
act: 'printf "%s\n" "{{line}}" >> "{{path}}"'
```
组件里调用（`examples/module-demo.yaml`）：
```yaml
components:
  - name: hosts-entry
    install:
      - action: module
        uses: lineinfile
        with: { path: /tmp/crater-demo.txt, line: "hello from a crater module" }
```
```bash
crater apply -f examples/module-demo.yaml          # 首跑 module → changed
crater apply -f examples/module-demo.yaml          # 再跑 module → ok（grep 命中）
```

## 验证（真机）

`[1/2] module lineinfile → changed` → 再跑 `→ ok`。module 渲染 check/act 后 lower 成 `Op::Shell{check,cmd}`，直接吃幂等回显（[idempotency-and-apply.md](idempotency-and-apply.md)）。

## 边界 / 后续

- 解析顺序（设计）：内置 > `modules/<uses>.yaml` > 外部 module。
- 第 1/2 层 lower 成 shell（shell 模式也能用）；第 3 层外部 module 需 agent 送达，优先静态二进制/纯 shell（守目标机零依赖）。
