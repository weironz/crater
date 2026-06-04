# `crater plan`:terraform 式变更预演(check-only)

> ADR: D-100 ｜ 代码: `engine.rs plan_check_task`、`apply.rs`(RunOpts.plan_check 贯通全管线)

## 这是什么

回答「**在生产上跑这个 apply,会动什么?**」——连上目标机、探测 OS/arch、lower 出计划,
然后**只跑每步的只读幂等探针**(D-023 的 `check`),什么都不执行:

```console
$ crater plan crater/rustfs:1.0.0-beta.5 --host <h> --password <pw>
[1/5] dir /data/rustfs                              → ✓ ok
[2/5] run: docker info …                            → - skip(preflight/verify)
[3/5] load image (blob) …rustfs:1.0.0-beta.5        → ~ would-change
[4/5] container rustfs <- … (spec 0a6aa0a05e40)     → ✓ ok
[5/5] run: curl …/health …                          → - skip(preflight/verify)
[<h>] plan: 1 会变更, 2 已就位, 0 未知, 2 跳过
```

四种结论:

| 符号 | 含义 |
|---|---|
| `✓ ok` | 探针通过 → 已在期望状态,apply 会跳过 |
| `~ would-change` | 探针失败 → apply 会执行;导入类(load_image)固定此类(无镜像探针,见 [load_image](../modules/load_image.md)) |
| `? unknown` | 该步没有探针 → apply 会直接跑,plan 无法预测 |
| `- skip` | preflight/verify 的 shell——它们是"检查"不是"状态",且 plan 的契约是**除探针外零执行** |

与 `apply --dry-run` 的分工:dry-run **纯静态**(不连目标机,打印渲染后的步骤);
plan **连真机**,告诉你哪些会真的变。

## 用法

```bash
crater plan rustfs --host <h> --password <pw>      # 命名 task
crater plan -f task.yaml -i inventory.yaml          # 文件 + 机群
crater plan env.oci -i inventory.yaml               # 项目 bundle:逐 play 各出一份摘要
crater plan <image-ref> [--offline]                 # 制品引用
crater plan x --set vip=10.0.0.14 ...               # --set 同 apply 的 gate(D-093)
```

apply 的五种 source 形态全支持(task 文件 / named / project / .oci / image ref)。

## 验证(真机 192.168.73.11)

- 空机 plan rustfs → `3 会变更, 0 已就位`(dir/镜像/容器全要做)。
- apply 后 plan → `1 会变更`(只剩固定 would-change 的 load_image),容器/目录 ✓ ok。
- **漂移注入**:手动 `docker rm -f rustfs` → plan 精确翻出 `container … ~ would-change`,
  其余仍 ok —— 这就是部署前的"差异预览"。
- project bundle:`offline plan project 'demo-stack': 2 play(s)`,每 play 单独摘要,零执行。

## 边界 / 后续

- plan **不写 marker、不跑 register**:依赖跨主机 fact(`hostvars.*`)的步骤,其探针里的
  `{{ }}` 未解析,结论可能失真(多见于 HA 集群 task)。
- `would-change` 的粒度 = 探针粒度:探针只验"产物在不在"的步骤(如 `test -s`),
  内容变化可能漏报——关键文件类步骤(copy/template)用 sha256 探针,无此问题。
- 不算 diff 内容(只有结论不展示差异);teardown 方向无 plan(用 `delete --dry-run`)。
