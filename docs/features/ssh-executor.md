# SSH 执行器（russh）+ 临时命令 / 推文件

> ADR: D-008（russh 直连）/ D-009（分块写文件）｜ 代码: `crates/crater-core/src/executor.rs`

## 这是什么

crater 控制端 → 目标机的**唯一通道**，也是几乎所有功能的远程执行底座。一个 `Executor` trait，两实现：

- **`SshExecutor`**（agentless，纯 Rust **russh 0.45**，免 OpenSSL/C 工具链）：密码认证连接，`run()` 开 channel `exec` 收 stdout/stderr/exit-code，`write_file()` 经 SSH 写文件。目标机**零安装**，只需 SSH + shell。
- **`LocalExecutor`**：本地执行（开发态 / agent 在目标机上跑时用它）。引擎对两者无感。

两个关键技术点（血泪沉淀）：
- **D-008**：russh 0.45 API——`check_server_key(&russh::keys::key::PublicKey)`、`authenticate_password` 返 `Result<bool>`、`run` 走 `channel.wait()` 匹配 `ChannelMsg`。
- **D-009 分块写文件**：单条 exec 传大文件会无 exit-status（code -1）/ 触发 Linux `MAX_ARG_STRLEN`(~128KB)。故 `SshExecutor::write_file` 把 base64 **按 60KB 分块** append 到临时文件、再一次 `base64 -d` 解码。实测 10MB+ OK。（小文件 / Local 走 trait 默认的一次性 base64 写法。）

> ⚠️ 安全现状：`check_server_key` 暂为 accept-all（host key 校验 TODO，见 N4）。

## 基本 demo

**临时命令**（≈ `ansible -m shell`）：
```bash
crater run --host <host> --password <pw> -- "uname -a && uptime"
```

**拷文件**（分块 base64 over SSH，无需 scp）：
```bash
crater cp --host <host> --password <pw> --src ./local.bin --dst /usr/local/bin/tool --chmod 755
```
`cp` 推完会回显远端/本地 sha256 比对，确认一致。（原名 `crater push`，已更名为 `cp`——`push` 让给镜像推送，见 [images-registry.md](images-registry.md)。）

凭据也可用环境变量免传 `--password`：
```bash
export CRATER_SSH_PASSWORD=<pw>
crater run --host <host> -- "hostname"
```

## 验证（真机）

- `crater run`：贯穿全程用于探测/收尾（如 `k3s kubectl get nodes`、`yq --version`）。
- 分块写文件：离线制品（node_exporter 10.6MB）、自举 agent 二进制（musl 9.7MB）、OCI rootfs 层（13.7MB）均经 `write_file` 推送成功。

## 关联功能（都建立在本执行器上）

[idempotency-and-apply](idempotency-and-apply.md)（shell 模式逐 Op `run`）、[self-bootstrap-agent](self-bootstrap-agent.md)（`write_file` 推二进制 + `run` 跑 agent）、[multi-node-and-cluster](multi-node-and-cluster.md)（并发多连接 + register 捕获）、[offline-oci](offline-oci.md)（`write_file` 推 OCI 包 + `run` 解包）。

## 边界 / 后续

- host key 校验（known_hosts pin，替换 accept-all，N4）。
- 密钥认证（当前仅密码）、凭据安全存储。
- `write_file` 走 shell `printf`/`base64`；目标机需有 `base64`（coreutils，通用）。
