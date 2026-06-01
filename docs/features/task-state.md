# 部署状态 `crater task`(marker + 控制端 Turso 库,D-051)

## 这是什么 / 两层状态

crater 本来**无状态/agentless**——跑完不留痕,不知道"什么装在哪"。要做状态监控 + Web UI,就得记状态。设计成**两层**,各司其职:

- **目标机 marker**(`/var/lib/crater/state/<task>.json`)= **真相源**。`apply` 成功写、`delete` 删。它**就是个文件**(executor 读写),目标机**仍然零驻留**。任何控制机都能读,抗控制机丢失。
- **控制端 Turso 库**(`~/.crater/state.db`)= 聚合/缓存 + job 历史。喂 `crater task list/history` 和将来的 Web UI,**不必每次 SSH 每台**。藏在 `StateStore` trait 后,可换实现(redb/rusqlite)。

> DB 选了 **Turso**(纯 Rust 重写的 SQLite,守 N1 免 C;`pure-rust-crypto`、关 mimalloc/sync)。详见 [D-051](../decisions.md)。

## 身份与视图(D-052/D-053,受 AWX 启发)

关键认识:**`apply`/`delete` 是无状态收敛 `(配方, 主机)`,本身不需要任何身份**;唯一性是 `task list` 才逼出来的**分组**问题。参考 AWX(同样无状态的 Ansible):它是 **run/活动中心**(Jobs 主屏),没有 release 对象,身份 = 命名的 playbook+inventory 绑定(Job Template)。crater 照此:

- **`task history` 是主视图**(不可变活动流);**`task list` 是当前部署快照**(crater 因有 marker 才能给出,AWX 没有);`task show` 下钻。
- **可选 deployment 名(默认 = task 名)** 作纯分组标签:`crater apply <name> <source>`。**只影响 list 分组,apply/delete 行为不变**。区分"同一 task 在不同机器组的独立部署"(A/B)就靠它。

## 命令

| 命令 | 作用 |
|---|---|
| `crater task history [--limit N]` | **主视图**:最近 apply/delete 活动流(谁/何时/部署/task/主机/结果) |
| `crater task list [--host/-i]` | 当前部署快照——**一行一个 deployment**(HOSTS 计数);默认读控制 DB,`--host/-i` 读目标机权威 marker |
| `crater task show <name> [--host/-i]` | 该 deployment 的 per-host 明细 |

`apply`/`delete` 自动维护状态(best-effort):marker(目标机,真相)+ 控制 DB(聚合/历史)。

## 基本 demo

A/B:同一份 `tasks/yq.yaml`,两个独立部署:

```bash
crater apply yq-a yq --host 192.168.73.11 --password 123456   # deployment yq-a
crater apply yq-b yq --host 192.168.73.12 --password 123456   # deployment yq-b

crater task list
#  DEPLOYMENT  TASK  VERSION  HOSTS  LAST APPLIED (UTC)
#  yq-a        yq    4.53.2       1  2026-06-01 08:43:58
#  yq-b        yq    4.53.2       1  2026-06-01 08:44:38      ← 两个独立单元,主机只给计数

crater task show yq-a                            # 下钻(主机名在这看)
#  HOST            TASK  VERSION  APPLIED (UTC)        SOURCE
#  192.168.73.11   yq    4.53.2   2026-06-01 08:43:58  yq

crater task history                              # 主视图
#  WHEN (UTC)           ACTION  DEPLOYMENT  TASK  HOST            RESULT
#  2026-06-01 08:44:38  apply   yq-b        yq    192.168.73.12   ok

crater apply yq -i inventory.yaml                # 不给名 → deployment 默认 "yq"(合并视图)
```
HOSTS 只给**计数**(大批量不平铺,主机名去 `task show`);版本各机不一致显示 `… (mixed)`。

## 验证（真机 .11/.12）

- `apply yq-a yq --host .11` + `apply yq-b yq --host .12`(同一 task.yaml)→ `task list` 两行 yq-a/yq-b;`task show yq-a` → .11;`history` DEPLOYMENT 列区分两者。
- `apply yq`(无名)→ deployment 默认 `yq`。
- schema 变更过 → 升级后需清旧 `~/.crater/state.db`。

## 边界 / 后续

- **Phase 1b**：`crater task list --verify` / `task show <name> --verify`——漂移检测(重跑 task 的 verify 阶段,比对声明态 vs 实际:有人手动删了就标 DRIFT/MISSING)。两级都给 STATUS(task 级聚合 ok/部分漂移,host 级 ok/DRIFT/MISSING)。**不单开 `status` 动词**,做成 list/show 的 `--verify` 开关。
- **Phase 2**：`crater ui`——Axum + htmx 只读看板,读同一个 Turso 库。
- 控制端 DB 是 per-控制机的缓存;跨控制机的真相永远在目标机 marker(`--host` 读它)。

## 关联

- ADR：[D-051](../decisions.md)。相关：[action-tasks.md](action-tasks.md)、[delete-teardown.md](delete-teardown.md)、[idempotency-and-apply.md](idempotency-and-apply.md)。
