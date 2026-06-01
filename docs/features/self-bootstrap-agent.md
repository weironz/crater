# 自举 agent（默认执行模型）

> ADR: D-019/D-026/D-027 ｜ 设计: [design.md §5](../design.md)

## 这是什么

**默认**把 crater 二进制 + lower 出的计划推到目标机，由 `crater agent` 在目标机**本地执行**（少 SSH 往返、为离线/复杂逻辑铺路）。crater 是单一二进制多子命令——目标机上跑的就是同一个 crater（`crater agent --task-plan`，内部命令、对人隐藏），不是另一个程序。

- 二进制按 **sha256 缓存** 在目标机 `/var/lib/crater/crater`，推一次/版本。
- **`--shell` 逃生**：强制纯 agentless shell（每步 SSH，目标机零 crater，任何机器都行）。
- **按 arch 自动选二进制**：探测目标 `uname -m` → 优先用 `dist/crater-linux-<arch>` 的 musl 静态（不挑 glibc）→ 否则同 arch 回退 `current_exe`；`--agent-bin` 手动指定。
- **通信**：全靠 SSH 一发一收（写文件 + 一次性 exec + 收 stdout），**无常驻进程/端口/RPC**；静态与否不影响通信。

## 基本 demo

```bash
crater yq --host <host> --password <pw>            # 默认 = agent（推二进制+计划，本地执行）
crater yq --host <host> --password <pw> --shell    # 逃生：agentless shell
scripts/build-musl.sh                              # 产出 dist/crater-linux-x86_64（静态，可移植 agent）
```

期望（agent 模式）：
```
Mode      : apply via agent
[<host>] agent: pushing dist/crater-linux-x86_64 ... [bundled musl static for x86_64]
[<host>] agent: executing on target ↓
[1/3] ... → changed
done on local: changed=2 ok=1 warn=0
```
（`done on local` 表示在目标机本地执行；再跑显示 `binary cached, reusing`。）

## 验证（真机 192.168.73.11/.12）

首跑推 9.7MB musl 静态、`changed=2 ok=1`；再跑缓存命中、`changed=0 ok=3`；`--shell` → `Mode: apply via shell`、`done on root@<host>`。目标机 `/var/lib/crater/crater` 实测 `static-pie linked`、`--version` = crater 0.1.0。

## 边界 / 后续

- 离线 `PushFile`（blob 随计划 ship）并入 OCI 离线时补；agent 暂只流 stdout（结构化结果后续）。
- 异构全覆盖需多 arch musl 随发布 + 控制端按 arch 内置选择（现需 `--agent-bin` 或 `dist/` 就位）。
