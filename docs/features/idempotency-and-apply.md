# 幂等回显 + apply 默认执行

> ADR: D-023（幂等）/ D-024（默认执行）｜ 设计: [design.md §6](../design.md)

## 这是什么

- **幂等契约**：每步 `check → act → report`。读类步骤（preflight/verify）只读、报 `ok`/`warn`；安装类步骤先跑幂等探针，命中则跳过、报 `ok`，否则执行、报 `changed`。写文件按 sha256 比对。重跑安全。
- **apply 默认执行**：`apply` 动词本身即执行；预览用 `--dry-run`（去掉了多余的 `--apply`）。

探针来源（数据/通用规则，守 D-017）：`place`=`test -s dest`、`pkg_install`=`dpkg -s`/`rpm -q`、`systemd_unit`=`is-enabled`/`is-active`、`run_cmd` 支持 YAML 里写 `check:`（ansible `creates:` 风格）。

## 基本 demo

```bash
crater yq --host <host> --password <pw> --dry-run   # 只看计划
crater yq --host <host> --password <pw>             # 执行（默认）
crater yq --host <host> --password <pw>             # 再执行 → 全 ok（幂等）
```

期望输出（第二次）：
```
[1/2] place yq-bin -> /usr/local/bin/yq → ok  # test -s 命中，跳过（mode 已折入 place）
[2/2] run: /usr/local/bin/yq --version → ok
done on local: changed=0 ok=2 warn=0 (2 step(s))
```

## 验证（真机 192.168.73.11）

清空后首跑 `changed=2 ok=1`；再跑 `changed=0 ok=3`。

## 边界 / 后续

- `skipped`（`when:` 条件跳过）属 B2，未做。
- 敏感值（如 token）不进日志：step 描述显示模板原文、实际执行渲染后命令。
