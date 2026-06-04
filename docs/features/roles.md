# 角色 role（可复用子程序 + 模块四层模型）

> ADR: D-029 ｜ 设计: [design.md §6.1](../design.md) ｜ 内置模块见 [modules.md](modules.md)

## 这是什么

为避免"每加一个能力就改 Rust + 逼第三方 fork"，crater 借鉴 ansible 把「操作的提供方式」分
四层(统一契约 = `check→act→StepStatus`):

| 层 | 形态 | 改 Rust |
|---|---|---|
| 1 内置模块 | Rust enum(shell/copy/package/file…,见 [modules.md](modules.md)) | 是(精选) |
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

> D-070 起彻底改名:旧 `action: module` + `modules/` 目录已废弃,统一用 `action: role` + `roles/`。

## 完整 role 捆绑(自带 materials + 多 actions,D-080)

上面的「瘦角色」(单 `check→act`)适合无依赖的小操作。而一个**可交付单元**(containerd、
mysql……)需要带**自己的离线闭包 + 多步配方** —— 这就是对齐 ansible role 的「完整 role」:
`roles/<name>.yaml` = task 减去 `hosts:`(`materials` + `actions` + `handlers` + `params`)。

```yaml
# roles/yq.yaml
name: yq
params: [version]
materials:                                   # ← role 自己的离线闭包(D-080)
  - name: bin
    kind: file
    url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64"
actions:                                     # ← 多步配方(不止一条命令)
  - { id: place, action: copy, material: bin, dest: /usr/local/bin/yq, mode: "0755" }
```
```yaml
# task 引用
actions:
  - id: install_yq
    action: role
    uses: yq
    with: { version: "4.44.3" }
```

**机制 = plan/build 前的「展开扁平化」**([`TaskFile::expand_roles`]):每个 `action: role`
被替换成 role 的 actions(id 与 material 名按**调用步 id**(无则 role 名)前缀,如 `install_yq.bin`、
`install_yq.place`;`with` 参数渲染进去;内部 `needs` 同步前缀;入口动作继承调用步的 `needs`/
`when_role`/`when_os`),role 的 `materials`/`handlers` **上浮并入 task**。之后 build 收闭包、
planner 排序都按一份**扁平 task** 走,无需感知 role。

关键收益:
- **materials 随 role 走**:复用 role = 复用它的闭包;闭包在展开时自动上浮进 OCI。
- **OCI 自包含**:build 时已扁平化进 recipe,**离线 apply 不需要 role 文件**(真机验证:移走
  `roles/yq.yaml` 后 `crater apply crater/demo-roles:latest` 仍 `copy (blob) install_yq.bin`)。
- 同一 role 多次引用用不同步 id → 不同前缀,**不冲突**。
- 别的步 `needs: [install_yq]` → 自动指向 role 的**终端动作**(role 内无人依赖的)。

示范见 [`roles/yq.yaml`](../../roles/yq.yaml) + [`tasks/demo-roles.yaml`](../../tasks/demo-roles.yaml)。
单测 `expand_roles_flattens_bundle_and_hoists_materials`。

> 兼容:无 `actions:` 的瘦角色(只 `check`/`act`)仍按旧路 lower 成单 `Op::Shell`。
> 后续(规划):role 的 `params` 加默认值/类型(契约);`meta.dependencies`(role 依赖 role,
> 闭包沿图组合);见 [../architecture.md](../architecture.md)。

## 验证(真机)

`role lineinfile → changed` → 再跑 `→ ok`。role 渲染 check/act 后 lower 成
`Op::Shell{check,cmd}`,直接吃幂等回显([idempotency-and-apply.md](idempotency-and-apply.md))。

## 边界 / 后续

- 解析顺序(设计):内置模块 > `roles/<uses>.yaml`(回退 `modules/`)> 外部模块。
- 第 1/2 层 lower 成 shell(shell 模式也能用);第 3 层外部模块需 agent 送达,优先静态二进制/纯 shell(守目标机零依赖)。
