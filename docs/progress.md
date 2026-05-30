# Crater 进展日志（自主开发）

> 「外部记忆」。新会话先读本文件 + requirements.md + decisions.md 恢复上下文。

## 🌅 早上总结（已逐项亲眼验证，可信）

连夜完成 **M1–M5**，并在真机 192.168.73.11(Ubuntu 24.04 noble，root/123456)**逐项端到端验证通过**。代码在 `master`，`cargo build` 0 错 0 警，`cargo test` **18 passed / 0 failed**。下面每条都附亲眼所见的证据。

## ✅ 里程碑（均真机验证，附证据）

- **M1 在线部署** — `crater docker --host .. --apply` → 见 `Docker version 29.5.2` + `active`。
- **M2 离线部署** — `crater build -f examples/node_exporter.yaml -o ne.bundle`(直连 GitHub 拉 10,676,343 B)→ `crater deploy --bundle ne.bundle --host .. --apply` rc=0：日志见 `pushed 10676343 bytes`、`node_exporter, version 1.8.2`、`# HELP go_gc_duration_seconds`、`Done: 7 steps`。纯 Rust tar+flate2，sha256 校验，**分块 base64 over SSH** 上传。
- **M3 集群/依赖** — `dag.rs` 拓扑排序(+4 单测);`crater k8s --host .. --apply`(别名→k3s)→ 见 `k3s version v1.30.5+k3s1`、`active`、`ubuntu Ready control-plane,master`。
- **M4 AI 制包侧** — `crater ai "..."`(无 endpoint)→ rc=1 友好报错(符合预期);护栏单测覆盖(好响应通过/幻觉组件被拒，含 tokio async)。**副驾不司机**:模型只产候选，crater 确定性校验(schema + 组件存在性)。
- **M5 AI 离线侧** — `crater doctor --file <log>` → `10 built-in rules, 3 finding(s)`：正确报出 apt / network / disk 并给修复建议。零网络零模型;`--ai` 可叠加内网模型。

## ✅ 组件覆盖（用户 4 个 named examples 全部到位）
- `crater docker` — 真机 active(29.5.2)
- `crater k8s`(别名→k3s) — 真机节点 Ready(v1.30.5+k3s1，control-plane)
- `crater mysql` — 真机 active(8.0.45，`mysqld is alive`)
- `crater es`(别名→elasticsearch) — dry-run 校验(11 步，SSH+OS 探测 OK)。**live 暂缓**:测试机已跑 docker+k3s+mysql，空闲内存不足 ES 所需 ~2GB，不冒险 OOM 已验证服务。
- 另:`crater node_exporter` — 离线 bundle 部署验证(:9100/metrics)。
- **别名**:`resolve_alias` 把 `k8s/kubernetes→k3s`、`es→elasticsearch`，让用户字面命令可用。

测试:`cargo test` 18 passed(bundle sha256/roundtrip、dag 4、ai extract/guard-rail 含 async、diagnose 4、os、source fetch_candidates 等)。

## 📁 代码结构
```
crates/crater-core/src/
  spec.rs       crater.yaml(inventory+components,host.password,预留 ai/offline)
  component.rs  声明式组件(内部标签 check:/action:,requires 依赖)
  os.rs         OsFamily(Debian/RHEL 探测+包管理,detect_via SSH)
  executor.rs   Executor + LocalExecutor + SshExecutor(russh 0.45)+ 分块 base64 写文件
  source.rs     镜像改写 + fetch_best(直连→CN 镜像回退)
  engine.rs     Op(Shell/WriteFile/PushFile)+ build_plan + execute + 离线模式
  bundle.rs     Manifest/BlobEntry/BundleStage/pack/unpack/verify(tar.gz+sha256)
  dag.rs        组件依赖拓扑排序
  ai.rs         AiProvider + OpenAiCompatProvider + nl_to_spec(护栏)
  diagnose.rs   离线规则诊断引擎(10 规则)
crates/crater-cli/src/main.rs  apply/build/deploy/ai/doctor/run/agent/<component>(+resolve_alias)
components/  docker mysql elasticsearch node_exporter k3s
examples/    crater.yaml node_exporter.yaml
```

## 🔧 关键技术结论（已验证，勿动摇）
- **SSH = russh 0.45 直连**:check_server_key 形参 `&russh::keys::key::PublicKey`;authenticate_password 返 `Result<bool>`;exec 走 channel.wait() 匹配 ChannelMsg。
- **大文件写远端 = 60KB 分块 base64**:单条 exec 传大文件先 code -1(channel 无 exit-status),再 200KB 触发 Linux `MAX_ARG_STRLEN`("Argument list too long");60KB/块 append→一次解码,已验证 10.6MB OK。
- **离线包 = tar.gz 纯 Rust**(tar + flate2 rust_backend),免 C 工具链。
- **GitHub 拉取 = fetch_best**:先直连(控制端在线时通),再 CN 镜像(ghfast.top 等)回退。**ghproxy.net 已死**。
- **k3s**:安装脚本自身 enable+start,**勿**再加 systemd restart(race);verify 用 `|| true`。

## 🚧 后续可做（非阻塞）
- 真机跑一次 `crater ai`(配 CRATER_AI_ENDPOINT/MODEL,如 DeepSeek 或内网)验证 NL→spec 全链路。
- es live 安装(需更大内存的机器);kubeadm 完整多节点;k3s 多节点 join(K3S_URL/K3S_TOKEN);k3s air-gap 离线(复用分块上传)。
- 离线镜像分发临时 registry(F13);docker 静态二进制 profile;通用 apt/dnf 换源(D-011,本机已 tuna/aliyun)。
- AI2 依赖补全、AI4 知识固化打进 bundle;本地小模型(candle)可选。
- musl 静态编译 + aarch64 交叉(N1/N2);安全:host key 校验(当前 accept-all)、凭据存储、包签名。

## ⚠️ 工具链纪律（本夜血泪教训，下个会话务必遵守）
1. **任何会非零退出的执行命令(crater --apply/run/build、远端命令)必须单独一条消息**;同批会**连带取消整批**(本夜反复中招,丢失大量编辑/验证)。纯 Write/Edit 可批量。
2. 命令输出 `Out-File` 到文件再 Read;通道会延迟/丢失/串字,耐心等一轮,**勿重试同命令**(会重复执行)。
3. 用窄 `grep -ac` 验证关键标志。
4. 不擅自中断、不找用户确认(已授权)。

## 工作日志（倒序）
### 2026-05-31 夜（全部完成并验证）
- 逐项真机验证 + 提交对账:docker、node_exporter 离线、mysql、k3s 全部 live 通过;es dry-run;doctor/ai CLI 行为符合预期;18 tests 绿。
- 关键修复:M2 fetch_best(ghproxy.net 死)+ 分块 base64 上传(大文件);M3 k3s 去 race 的 systemd restart。
- 多次因"批次取消"丢失整批编辑(根因:crater 命令非零退出),最终改严格单命令执行后全部补回并验证。progress.md 已对账为真实状态(去掉此前未经验证的夸大表述)。
