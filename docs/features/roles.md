# 角色 role（可复用子程序 + 模块四层模型）

> ADR: D-029 ｜ 设计: [design.md §6.1](../design.md) ｜ 内置模块见 [modules.md](modules.md)

## 这是什么

为避免"每加一个能力就改 Rust + 逼第三方 fork"，crater 借鉴 ansible 把「操作的提供方式」分
四层(统一契约 = `check→act→StepStatus`):

| 层 | 形态 | 改 Rust |
|---|---|---|
| 1 内置模块 | Rust enum(shell/place/package/file…,见 [modules.md](modules.md)) | 是(精选) |
| **2 角色(role)** | `roles/<name>.yaml`(params+check+act 模板) | **否**(已实现的契约地基) |
| 3 外部模块 | 脚本/静态二进制 + JSON 契约(agent 送达) | 否(后续) |
| 4 `shell`+`check` | 裸命令 + 探针 | 否 |

本文聚焦**第 2 层「角色」**——就是 ansible 的 **role**:放 `roles/<name>.yaml` 即用,零 Rust。
(术语见 [modules.md](modules.md):原语=模块,可复用子程序=角色。)

## 基本 demo

`roles/lineinfile.yaml`:
```yaml
params: [path, line]
check: 'grep -qxF "{{line}}" "{{path}}"'     # 命中→ok(跳过)
act: 'printf "%s\n" "{{line}}" >> "{{path}}"'
```
task 里调用:
```yaml
actions:
  - action: role
    uses: lineinfile
    with:
      path: /tmp/crater-demo.txt
      line: "hello from a crater role"
```
```bash
crater apply -f task.yaml          # 首跑 role → changed
crater apply -f task.yaml          # 再跑 role → ok(grep 命中)
```

> 旧拼写 `action: module` + `modules/` 目录仍作 back-compat 兼容(D-067)。

## 验证(真机)

`role lineinfile → changed` → 再跑 `→ ok`。role 渲染 check/act 后 lower 成
`Op::Shell{check,cmd}`,直接吃幂等回显([idempotency-and-apply.md](idempotency-and-apply.md))。

## 边界 / 后续

- 解析顺序(设计):内置模块 > `roles/<uses>.yaml`(回退 `modules/`)> 外部模块。
- 第 1/2 层 lower 成 shell(shell 模式也能用);第 3 层外部模块需 agent 送达,优先静态二进制/纯 shell(守目标机零依赖)。
