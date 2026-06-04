# params 契约 + `--set` 覆盖(build 期 / apply 期分治)

> 关联 ADR:D-081(params + inspect)、D-082(inventory vars)、D-089(build --set)、
> **D-093(apply --set gate)**。

## 这是什么

task 用 `params:` 声明输入契约,每个参数标 `stage:`:

- **`stage: build`** —— 影响物料的参数(典型:`version`)。build 时解析,物料按它取好、
  **冻结进 OCI 制品**。
- **`stage: apply`**(默认)—— 环境配置(典型:`vip` / `subnet` / 端口)。部署时按目标
  环境提供,不影响制品内容。

两端各有一个 `--set KEY=VAL`(可重复),**管辖范围互斥**:

| 命令 | 接受的参数 | 给错时 |
|------|-----------|--------|
| `crater build --set` | 任意(典型 build 期,如 `version`) | — |
| `crater apply --set` / `delete --set` | **只接 `stage: apply` 的声明参数** | build 参数 → 报错引导 `crater build --set` 重建;未声明 → 报错(typo 防护) |

**为什么 apply 不能改 build 参数(D-093)**:已 build 的 OCI 是 build 期参数的冻结闭包——
blob 按 material key 寻址、物料按 `version` 取好。apply 时改 `version` 要么无效、要么让
recipe 与 blob 失配,等于废掉制品。要换版本:`crater build --set version=X` 重建。

**变量优先级(低 → 高)**:

```
param default → task vars → inventory 全局/组/主机 vars → CLI --set
```

`--set` 最高:显式操作员意图盖过 inventory。`delete --set` 同 gate——teardown 渲染用同一套
vars,卸载要与部署时同值。

## demo

```yaml
# demo.yaml
name: demo
hosts: all
params:
  version: { default: "1.0", stage: build }
  vip:     { default: "10.0.0.1", stage: apply }
actions:
  - action: shell
    cmd: "echo version={{version}} vip={{vip}}"
```

```console
$ crater apply -f demo.yaml --set vip=192.168.73.14 --dry-run
 1. [Install] run: echo version={{version}} vip={{vip}}
      $ echo version=1.0 vip=192.168.73.14        # ← --set 渲染进 plan

$ crater apply -f demo.yaml --set version=2.0
Error: --set version: 是 build 期参数(stage: build)…请用 `crater build --set version=…` 重建制品

$ crater apply -f demo.yaml --set vipp=x
Error: --set vipp: 不是该 task 声明的参数。…(`crater inspect <source>` 看契约)
```

`crater inspect <source>` 列出每个参数的 stage / default / required,部署前先看契约。

## 验证

- 59 tests 绿,含 gate 三态单测(放行 apply 参数 / 拒 build 参数 / 拒未声明 key)。
- CLI 实测同上 demo:渲染覆盖生效、两类报错文案符合预期。

## 边界 / 后续

- gate 只认**声明过的** `params:`——裸 `vars:` 不可被 apply `--set` 覆盖(typo 防护,
  也是推动 task 把契约写出来的轻推)。
- 五个 apply 入口(task 文件 / named / project / OCI bundle / image ref)统一走 gate;
  project 的 `--set` 作用于其下每个 play。
