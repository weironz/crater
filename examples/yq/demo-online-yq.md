# Demo：在线部署 yq（最小可复现）

> 用最简单的可部署对象 **yq**（单文件、无依赖的静态二进制）走通 crater 的在线部署主路径。
> 目标机：任意可 SSH 的 x86_64 Linux（本 demo 在真机 Ubuntu 24.04 @ 192.168.73.11 验证）。
> 配套：[design.md](design.md)（设计方向）、[decisions.md](decisions.md)（D-017/023/025/026）。

---

## 0. 前提

- 控制端：`cargo build`（或 `cargo build --release`）得到 `crater` 二进制；目标机只需 **SSH + shell**，不装任何东西（agentless）。
- 目标机能联网拉 GitHub（在线形态下由**目标机自己** curl 制品）。弱网/内网回退见文末。
- 下文用占位符 `<host>` / `<pw>`，替换成你的目标机 IP 与 root 密码（或设 `CRATER_SSH_PASSWORD` 环境变量免传 `--password`）。

> 约定：`apply` 默认**执行**；加 `--dry-run` 只打印计划不执行（D-024）。

---

## 1. yq 的「配方」是纯数据（D-017）

加一个可部署对象 = 丢一个 YAML，**零 Rust 改动、零重编译**。`components/yq/component.yaml`：

```yaml
name: yq
version_default: "4.53.2"
supported_os: [ubuntu, debian, rhel, centos, rocky]
install:
  - action: download                       # 裸二进制，直接下到 PATH 位置
    url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64"
    dest: /usr/local/bin/yq
  - action: run_cmd
    cmd: "chmod +x /usr/local/bin/yq"
    check: "test -x /usr/local/bin/yq"      # 幂等探针：已可执行则跳过
verify:
  - action: run_cmd
    cmd: "/usr/local/bin/yq --version"
```

引擎只认 `download`/`run_cmd` 这些**通用原语**，不认识"yq"是什么——这正是「装万物」的前提。

---

## 2. 三种用法（按场景选）

### A. 快捷式（临时、单机，零 spec）
```bash
crater yq --host <host> --password <pw>
```
配方来自 `components/yq/`，主机走 flag。最省事。

### B. 声明式 spec，引用可复用 component（`examples/yq.yaml`）
```bash
crater apply -f examples/yq.yaml
```
```yaml
inventory:
  hosts:
    - { name: test, address: <host>, user: root, password: "<pw>", roles: [yq] }
components:
  - { name: yq, version: "4.53.2" }
```
多机/可入库；`roles` 过滤哪台装哪些。

### C. 单文件内联配方（`examples/yq-inline.yaml`，D-025）
```bash
crater apply -f examples/yq-inline.yaml
```
inventory + 配方写在**一个文件**里，免 `components/` 目录（`components/` 退化为可选复用库）：
```yaml
inventory:
  hosts:
    - { name: test, address: <host>, user: root, password: "<pw>", roles: [yq] }
components:
  - name: yq
    version: "4.53.2"
    install:
      - { action: download, url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64", dest: /usr/local/bin/yq }
      - { action: run_cmd, cmd: "chmod +x /usr/local/bin/yq", check: "test -x /usr/local/bin/yq" }
    verify:
      - { action: run_cmd, cmd: "/usr/local/bin/yq --version" }
```

> 三者最终都被 `apply` 收敛处理；按 [D-020](decisions.md) 未来统一成 `crater apply <source>`。

---

## 3. 先预览（dry-run）

```bash
crater yq --host <host> --password <pw> --dry-run
```
```
Component : yq
Version   : 4.53.2
Mode      : DRY-RUN
Steps     : 3
 1. [Install] download https://github.com/mikefarah/yq/releases/download/v4.53.2/yq_linux_amd64
 2. [Install] run: chmod +x /usr/local/bin/yq
 3. [Verify] run: /usr/local/bin/yq --version
Dry-run only (--dry-run). Omit it to execute.
```

---

## 4. 执行 + 幂等回显（B1，D-023）

每步报 `ok` / `changed` / `warn`：读类步骤只读、安装类先 check 再决定是否动手。

**首次**（目标机干净）：
```
[1/3] Install download ... changed
[2/3] Install run: chmod +x /usr/local/bin/yq ... changed
[3/3] Verify  run: /usr/local/bin/yq --version ...  | yq ... version v4.53.2
                                                    ok
Done on root@<host>:22: changed=2 ok=1 warn=0 (3 step(s)).
```

**再次**（已就绪 → 全部跳过，重跑安全）：
```
[1/3] Install download ... ok        # test -s 命中，跳过下载
[2/3] Install run: chmod +x ... ok   # test -x 命中，跳过
[3/3] Verify  run: yq --version ... ok
Done on root@<host>:22: changed=0 ok=3 warn=0 (3 step(s)).
```

---

## 5. 执行模型：agent 是默认（D-027）

§4 那些命令**默认就走自举 agent**：控制端把 crater 二进制（按 sha256 **缓存**在目标机 `/var/lib/crater/agent`，推一次/版本）+ 计划推过去，由 `crater agent` 在目标机**本地执行**（少 SSH 往返）。无需任何 flag。

```bash
crater yq --host <host> --password <pw>            # 默认 = agent
```
```
Mode      : APPLY (agent)
Agent on root@<host>:22: pushing binary (9.9MB ...) ...     # 首次推；再次为 "binary cached (sha256 match), reusing"
--- agent output (executing locally on target) ---
[agent] executing 3 step(s) locally
[1/3] Install download ... changed
[2/3] Install run: chmod +x ... changed
[3/3] Verify  run: yq --version ... ok
Done on local: changed=2 ok=1 warn=0 (3 step(s)).
```
`Done on local` 表示目标机本地（LocalExecutor）执行；幂等照旧。

**逃生口 `--shell`**：目标机跑不了 crater 二进制（架构/libc 不符），或你就想要最纯 agentless 时，强制走 shell（每步经 SSH，目标机零 crater）：
```bash
crater yq --host <host> --password <pw> --shell    # Mode: APPLY (shell)，Done on root@<host>
```
异构目标也可 `--agent-bin <musl静态构建>` 指一个能在目标机跑的二进制。

> 注意：crater 是**控制端工具**，在控制机上跑、经 SSH 操作目标机。**不要在目标机上敲 `crater`**——目标机不需要它（agent 模式只是临时缓存一份二进制供本地执行）。

---

## 6. 验证 / 收尾

```bash
crater run --host <host> --password <pw> -- "yq --version && which yq"   # 临时命令(≈ ansible -m shell)
```

---

## 7. 这个 demo 证明了什么

| 设计点 | 在 demo 里的体现 |
|---|---|
| **引擎零产品知识**（D-017）| 加 yq 只丢了一个 13 行 YAML，零 Rust 改动 |
| **agentless** | 目标机只用 SSH；制品由目标机自己 curl |
| **幂等契约 + 回显**（D-023）| 首跑 `changed=2 ok=1` → 再跑 `changed=0 ok=3` |
| **CLI 默认执行**（D-024）| 无 `--apply`；预览用 `--dry-run` |
| **配方/实例可分可合**（D-025）| 快捷式 / spec 引用 / 单文件内联 三种皆可 |
| **自举 agent 作默认**（D-019/D-027）| 默认推二进制(缓存)+计划，目标机本地执行；`--shell` 逃生 |

---

## 8. 弱网 / 内网说明

本 demo 目标机直连 GitHub。若目标机连不上 GitHub：
- 在线 CN 镜像回退（让目标机 curl 走 `mirrors.default.yaml` 的镜像）—— 计划中。
- 或走**离线形态**：控制端 `crater bundle` 制 OCI 包 → 目标机零联网 `crater apply <bundle>`（OCI 离线 D-018，进行中）。
