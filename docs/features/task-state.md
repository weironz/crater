# 部署状态 `crater task`(marker + 控制端 Turso 库,D-051)

## 这是什么 / 两层状态

crater 本来**无状态/agentless**——跑完不留痕,不知道"什么装在哪"。要做状态监控 + Web UI,就得记状态。设计成**两层**,各司其职:

- **目标机 marker**(`/var/lib/crater/state/<task>.json`)= **真相源**。`apply` 成功写、`delete` 删。它**就是个文件**(executor 读写),目标机**仍然零驻留**。任何控制机都能读,抗控制机丢失。
- **控制端 Turso 库**(`~/.crater/state.db`)= 聚合/缓存 + job 历史。喂 `crater task list/history` 和将来的 Web UI,**不必每次 SSH 每台**。藏在 `StateStore` trait 后,可换实现(redb/rusqlite)。

> DB 选了 **Turso**(纯 Rust 重写的 SQLite,守 N1 免 C;`pure-rust-crypto`、关 mimalloc/sync)。详见 [D-051](../decisions.md)。

## 命令

| 命令 | 作用 |
|---|---|
| `crater task list` | 列部署——**默认读控制端 DB**(本机 apply 过的) |
| `crater task list --host H` / `-i inv.yaml` | 读**目标机权威 marker**(谁 apply 的都算,真实) |
| `crater task history [--limit N]` | 最近的 apply/delete 记录(控制端 DB) |

`apply`/`delete` 会自动维护状态:成功后目标机写/删 marker + 控制端 DB upsert/delete + 记一条 job_run。**都是 best-effort**——部署已成功,状态记录失败只 warn,不回滚(marker 才是真相,DB 只是缓存)。

## 基本 demo

```bash
crater apply yq                                  # 本机:装 + 写 marker + DB
crater apply yq --host 192.168.73.12 --password 123456

crater task list                                 # 控制 DB 聚合视图
#  HOST            TASK   VERSION  APPLIED (UTC)        SOURCE
#  192.168.73.12   yq     4.53.2   2026-06-01 07:57:40  yq
#  localhost       yq     4.53.2   2026-06-01 07:57:13  yq

crater task list --host 192.168.73.12 --password 123456   # 读目标机权威 marker
crater task history
#  WHEN (UTC)           ACTION  TASK  HOST            RESULT
#  2026-06-01 07:57:40  apply   yq    192.168.73.12   ok

crater delete yq --host 192.168.73.12 --password 123456   # 删 marker + DB delete + 记 history
crater task list                                 # .12 消失,只剩 localhost
```

## 验证（真机 192.168.73.12）

- `apply yq` 本机 + `--host .12` → `task list` 两条、`history` 两条 apply。
- `task list --host .12` 读到 `/var/lib/crater/state/yq.json`(JSON 内容核对一致)。
- `delete yq --host .12` → marker 删、DB 删、list 中 .12 消失、history 多一条 `delete`。

## 边界 / 后续

- **Phase 1b**：`crater task status`——漂移检测(重跑 task 的 verify 阶段,比对声明态 vs 实际:有人手动删了就标 DRIFT)。
- **Phase 2**：`crater ui`——Axum + htmx 只读看板,读同一个 Turso 库。
- 控制端 DB 是 per-控制机的缓存;跨控制机的真相永远在目标机 marker(`--host` 读它)。

## 关联

- ADR：[D-051](../decisions.md)。相关：[action-tasks.md](action-tasks.md)、[delete-teardown.md](delete-teardown.md)、[idempotency-and-apply.md](idempotency-and-apply.md)。
