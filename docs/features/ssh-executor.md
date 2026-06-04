# SSH 执行器（russh）+ 临时命令 / 推文件

> ADR: D-008（russh 直连）/ D-009（分块写文件）/ D-094（host key 校验）｜ 代码: `crates/crater-core/src/executor.rs`

## 这是什么

crater 控制端 → 目标机的**唯一通道**，也是几乎所有功能的远程执行底座。一个 `Executor` trait，两实现：

- **`SshExecutor`**（agentless，纯 Rust **russh 0.45**，免 OpenSSL/C 工具链）：密码认证连接，`run()` 开 channel `exec` 收 stdout/stderr/exit-code，`write_file()` 经 SSH 写文件。目标机**零安装**，只需 SSH + shell。
- **`LocalExecutor`**：本地执行（开发态 / agent 在目标机上跑时用它）。引擎对两者无感。

两个关键技术点（血泪沉淀）：
- **D-008**：russh 0.45 API——`check_server_key(&russh::keys::key::PublicKey)`、`authenticate_password` 返 `Result<bool>`、`run` 走 `channel.wait()` 匹配 `ChannelMsg`。
- **D-009 分块写文件**：单条 exec 传大文件会无 exit-status（code -1）/ 触发 Linux `MAX_ARG_STRLEN`(~128KB)。故 `SshExecutor::write_file` 把 base64 **按 60KB 分块** append 到临时文件、再一次 `base64 -d` 解码。实测 10MB+ OK。（小文件 / Local 走 trait 默认的一次性 base64 写法。）

## host key 校验（D-094，ansible 式 accept-new）

连接时校验目标机 host key,钉在 **`~/.crater/known_hosts`**(crater 自管,不碰
`~/.ssh/known_hosts`;`$CRATER_HOME` 可挪)：

- **首次连接** → 记录 key 并继续(TOFU),日志 `首次连接 <host>:<port>,已钉其 host key(SHA256:…)`;
- **再连且 key 匹配** → 静默通过;
- **key 变了** → **拒绝连接**(可能中间人/主机重装),ERROR 给出两侧指纹与该行行号——确认无误后删除该行重连;
- 跳过校验(临时环境/重装频繁)：`CRATER_HOST_KEY_CHECKING=0`。

```console
$ crater run --host <host> --password <pw> "hostname"   # 首连
INFO ssh: 首次连接 <host>:22,已钉其 host key(SHA256:KHbl…,记录于 /root/.crater/known_hosts)

# 目标机 key 变化后:
ERROR ssh: <host>:22 的 HOST KEY 已变化!…与 /root/.crater/known_hosts 第 2 行不符 ——
      可能是中间人攻击,也可能主机重装过。确认无误后删除该行重连
Error: ssh connect <host>:22 failed: Unknown server key
```

> ⚠️ 克隆 VM 注意:同一模板克隆出的机器 host key **完全相同**(实测 73.11/73.12),
> TOFU 钉的是同一把 key,互相冒充检测不出来——生产模板应 `rm /etc/ssh/ssh_host_*` 后
> 重新生成(`ssh-keygen -A` 或重装 openssh-server 触发)。

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

- `crater run`：贯穿全程用于探测/收尾（如 `docker version`、`yq --version`）。
- 分块写文件：离线制品（node_exporter 10.6MB）、自举 agent 二进制（musl 9.7MB）、OCI rootfs 层（13.7MB）均经 `write_file` 推送成功。

## 关联功能（都建立在本执行器上）

[idempotency-and-apply](idempotency-and-apply.md)（shell 模式逐 Op `run`）、[self-bootstrap-agent](self-bootstrap-agent.md)（`write_file` 推二进制 + `run` 跑 agent）、[multi-node-and-cluster](multi-node-and-cluster.md)（并发多连接 + register 捕获）、[offline-oci](offline-oci.md)（`write_file` 推 OCI 包 + `run` 解包）。

## 边界 / 后续

- 密钥认证已支持（`SshAuth::Key`，`--key`/`--passphrase`，见 [apply-targets.md](apply-targets.md)）；凭据安全存储后续。
- `write_file` 走 shell `printf`/`base64`；目标机需有 `base64`（coreutils，通用）。
