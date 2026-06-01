# 部署状态 `crater task`(marker + 控制端 Turso 库,D-051)

## 这是什么 / 两层状态

crater 本来**无状态/agentless**——跑完不留痕,不知道"什么装在哪"。要做状态监控 + Web UI,就得记状态。设计成**两层**,各司其职:

- **目标机 marker**(`/var/lib/crater/state/<task>.json`)= **真相源**。`apply` 成功写、`delete` 删。它**就是个文件**(executor 读写),目标机**仍然零驻留**。任何控制机都能读,抗控制机丢失。
- **控制端 Turso 库**(`~/.crater/state.db`)= 聚合/缓存 + job 历史。喂 `crater task list/history` 和将来的 Web UI,**不必每次 SSH 每台**。藏在 `StateStore` trait 后,可换实现(redb/rusqlite)。

> DB 选了 **Turso**(纯 Rust 重写的 SQLite,守 N1 免 C;`pure-rust-crypto`、关 mimalloc/sync)。详见 [D-051](../decisions.md)。

## "task" 的语义(D-052)

一次 `crater apply yq -i inv` 是**一个 task**(逻辑部署单元),落在 N 台机器上——**hosts 是它的属性**,不是把每台摊成一行(对标 `helm list` 的 release 维度)。所以:

- `crater task list` = **task 维度**,一行一个 task,hosts 聚合。
- `crater task show <name>` = 下钻看该 task 的 per-host 实例。
- per-host marker 仍是真相源,只是默认不平铺展示。

## 命令

| 命令 | 作用 |
|---|---|
| `crater task list` | **一行一个 task**(hosts 聚合);默认读控制端 DB |
| `crater task list --host H` / `-i inv.yaml` | 同上,但读**目标机权威 marker** |
| `crater task show <name> [--host/-i]` | 该 task 的 per-host 明细(version/applied/source) |
| `crater task history [--limit N]` | 最近的 apply/delete 记录(控制端 DB) |

`apply`/`delete` 自动维护状态:成功后目标机写/删 marker + 控制端 DB upsert/delete + 记一条 job_run。**都是 best-effort**——部署已成功,状态记录失败只 warn,不回滚(marker 才是真相,DB 只是缓存)。

## 基本 demo

```bash
crater apply yq -i inventory.yaml                # 一个 task,部署到 inventory 各机
crater task list                                 # task 维度
#  TASK   VERSION   HOSTS           LAST APPLIED (UTC)
#  yq     4.53.2    n11,n12 (2)     2026-06-01 08:10:56
crater task show yq                              # 下钻 per-host
#  HOST   VERSION   APPLIED (UTC)        SOURCE
#  n11    4.53.2    2026-06-01 08:10:55  yq
#  n12    4.53.2    2026-06-01 08:10:56  yq

crater task list -i inventory.yaml               # 读目标机权威 marker(同样 task 维度)
crater task history
#  WHEN (UTC)           ACTION  TASK  HOST   RESULT
#  2026-06-01 08:10:56  apply   yq    n12    ok

crater delete yq -i inventory.yaml               # 删 marker + DB delete + 记 history
```
版本各主机不一致时 `list` 显示 `4.53.2,4.54.0 (mixed)`。

## 验证（真机 192.168.73.12）

- `apply yq` 本机 + `--host .12` → `task list` 两条、`history` 两条 apply。
- `task list --host .12` 读到 `/var/lib/crater/state/yq.json`(JSON 内容核对一致)。
- `delete yq --host .12` → marker 删、DB 删、list 中 .12 消失、history 多一条 `delete`。

## 边界 / 后续

- **Phase 1b**：`crater task list --verify` / `task show <name> --verify`——漂移检测(重跑 task 的 verify 阶段,比对声明态 vs 实际:有人手动删了就标 DRIFT/MISSING)。两级都给 STATUS(task 级聚合 ok/部分漂移,host 级 ok/DRIFT/MISSING)。**不单开 `status` 动词**,做成 list/show 的 `--verify` 开关。
- **Phase 2**：`crater ui`——Axum + htmx 只读看板,读同一个 Turso 库。
- 控制端 DB 是 per-控制机的缓存;跨控制机的真相永远在目标机 marker(`--host` 读它)。

## 关联

- ADR：[D-051](../decisions.md)。相关：[action-tasks.md](action-tasks.md)、[delete-teardown.md](delete-teardown.md)、[idempotency-and-apply.md](idempotency-and-apply.md)。
