# apply 三层目标 + SSH 认证（D-035）

> ADR: D-035 ｜ 命令: `crater apply [<name>] <source> [目标参数]`

## 这是什么

`crater apply` 按部署**规模**自然分三层，同一条命令、同一套引擎，只有"打哪些机器"不同：

| 层 | 命令 | 目标 |
|---|---|---|
| **本机单机** | `crater apply app01 <source>` | 不指定主机 → 装到**控制机自己**（`LocalExecutor`，零 SSH） |
| **少量机器** | `crater apply app01 <source> --host 10.0.0.5,10.0.0.6 --user root --password xxx` | `--host` 逗号分隔多主机，**共用一套**凭据 |
| **大量机器** | `crater apply app01 <source> -i inventory.yaml` | inventory 文件，**每主机各自**凭据/角色 |

`<source>` 仍是统一的：镜像地址（registry/本地库）、`.oci` 离线包、`spec.yaml`、或组件名。
`<name>` 是可选的部署 label（两个位置参数时首个为 name）；省略则首个即 source（向后兼容）。

## 认证：密码 / 私钥

```bash
--password xxx              # 密码认证
--key ~/.ssh/id_rsa         # 私钥认证（key 优先于 password；适合禁用密码登录的机群）
```

- `--host` 形态：所有主机**共用** `--user` + `--password|--key`（同构小机群）。
- **异构凭据**（每台密码/key 不同）：用 `-i inventory.yaml`，每个 host 各带 `password:` 或 `key:`：

```yaml
inventory:
  hosts:
    - { name: n1, address: 10.0.0.5, user: root, password: "p1" }
    - { name: n2, address: 10.0.0.6, user: ubuntu, key: ~/.ssh/n2_rsa }
```

> 即"`--host` 只能一套密码"的解法——少量同构机器用 `--host` 共用，异构就上 inventory（每主机独立）。

## 基本 demo（以 zot 上的 yq B 类 artifact 为例）

```bash
export CRATER_INSECURE_REGISTRIES=192.168.73.5:5000

# 层1 本机
crater apply app01 192.168.73.5:5000/yq:m
#  ▶ host localhost (local) → place (offline) yq-bin -> /usr/local/bin/yq → changed

# 层2 少量机器（逗号多主机，共用密码）
crater apply app01 192.168.73.5:5000/yq:m --host 192.168.73.11,192.168.73.12 --user root --password 123456
#  两台并行 → 各 changed=1

# 层2 私钥认证
crater apply app01 192.168.73.5:5000/yq:m --host 192.168.73.12 --user root --key /tmp/crater_key

# 层3 大量机器
crater apply app01 192.168.73.5:5000/yq:m -i inventory.yaml
```

## 真机验证（2026-05-31）

- 层1 本机：`apply app01 <zot>/yq:m`（无主机）→ 控制机 `/usr/local/bin/yq` v4.53.2。
- 层2 csv+密码：`--host .11,.12 --password 123456` → 两台并行装好。
- 层2 `--key`：`ssh-keygen` 装公钥到 n12 → `--key /tmp/crater_key`（无密码）→ n12 装好。
- 层3 inventory：`-i inv.yaml`（n11/n12）→ 两台装好。

## 边界 / 后续

- 本机执行强制走 shell 引擎（不 bootstrap agent）；`LocalExecutor` 直写文件系统（避免大二进制撑爆 `MAX_ARG_STRLEN`）。
- 私钥 passphrase 暂走无口令（`SshAuth::Key.passphrase` 已留位，CLI 未透传）。
- host-key 校验（known_hosts）仍 accept-all（N4 待办）。
- `<name>` 目前是 label；未来用于同一 source 多实例区分。

## 关联

- ADR：[D-035](../decisions.md)。引擎单管线 [D-020]，自举 agent [D-019/027]。
