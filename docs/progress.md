# Crater 进展日志（自主开发）

> 「外部记忆」。新会话先读本文件 + requirements.md + decisions.md 恢复上下文。

## 🌅 早上总结（已逐项亲眼验证，可信）

连夜完成 **M1–M5**，并在真机 192.168.73.11(Ubuntu 24.04 noble，root/123456)**逐项端到端验证通过**。代码在 `master`，`cargo build` 0 错 0 警，`cargo test` **21 passed / 0 failed**(原 18 + A 批新增)。下面每条都附亲眼所见的证据。

> **设计方向重整 + B1（2026-05-31 续）**：确立「引擎零产品知识」铁律(D-017)、完成**还债 A 批**(别名/doctor/LoadImage/镜像表数据驱动)、**B1 幂等回显**(D-023)；定向**离线转 OCI**(D-018)与**自举 agent**(D-019)。北极星见 [design.md](design.md)。下面 M1–M5 证据为历史记录，仍有效。
>
> **最小 demo + B1 真机验证（192.168.73.11）**：新增 `components/yq/component.yaml`(单文件二进制，**零 Rust 改动**坐实 D-017) + `examples/yq.yaml`。在线部署 yq v4.53.2 通过。幂等:清空后首跑 `changed=2 ok=1`，再跑 `changed=0 ok=3`(download/chmod 跳过、verify ok)。**CLI 改为默认执行**——去掉 `--apply`，预览用 `--dry-run`(D-024)。`cargo test` **23 passed**。

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
- **别名（A 批已改数据驱动）**:组件描述文件 `aliases:` 字段(k3s 声明 `[k8s, kubernetes]`、elasticsearch 声明 `[es]`)，`resolve_component` 扫 `components/` 从数据建表。原 `resolve_alias` 焊死映射已删(D-016 作废，D-017)。`crater k8s` dry-run 验证经数据别名解析到 k3s。

测试:`cargo test` **21 passed**(原 18 + A 批 mirrors 数据解析等)。

## 📁 代码结构
```
crates/crater-core/src/
  spec.rs       crater.yaml(inventory+components,host.password,预留 ai/offline)
  component.rs  声明式组件(内部标签 check:/action:,requires 依赖)
  os.rs         OsFamily(Debian/RHEL 探测+包管理,detect_via SSH)
  executor.rs   Executor + LocalExecutor + SshExecutor(russh 0.45)+ 分块 base64 写文件
  source.rs     镜像改写 + fetch_best(直连→CN 镜像回退)
  engine.rs     Op(Shell{+check}/WriteFile/PushFile)+ build_plan + execute(幂等 check→act→report, StepStatus ok/changed/warn)+ 离线模式
  bundle.rs     Manifest/BlobEntry/BundleStage/pack/unpack/verify(tar.gz+sha256)
  dag.rs        组件依赖拓扑排序
  ai.rs         AiProvider + OpenAiCompatProvider + nl_to_spec(护栏)
  diagnose.rs   离线规则诊断引擎(10 规则)
  mirrors.default.yaml  镜像源数据(include_str! 烤进二进制;$CRATER_MIRRORS/./mirrors.yaml 可覆盖)
crates/crater-cli/src/main.rs  apply/build/deploy/ai/doctor/run/agent/<component>
                               (resolve_component 经组件 aliases 数据解析;doctor 探针从组件 SystemdUnit 推导)
components/  docker mysql elasticsearch node_exporter k3s  (产品知识只在这里;含 aliases/images 数据)
examples/    crater.yaml node_exporter.yaml
```

## 🔧 关键技术结论（已验证，勿动摇）
- **SSH = russh 0.45 直连**:check_server_key 形参 `&russh::keys::key::PublicKey`;authenticate_password 返 `Result<bool>`;exec 走 channel.wait() 匹配 ChannelMsg。
- **大文件写远端 = 60KB 分块 base64**:单条 exec 传大文件先 code -1(channel 无 exit-status),再 200KB 触发 Linux `MAX_ARG_STRLEN`("Argument list too long");60KB/块 append→一次解码,已验证 10.6MB OK。
- **离线包 = tar.gz 纯 Rust**(tar + flate2 rust_backend),免 C 工具链。**⚠️ 已定向迁移到 OCI**(D-018);tar.gz 在 OCI 跑通前保留，渐进替换，见 [offline-format.md](offline-format.md)。
- **GitHub 拉取 = fetch_best**:先直连(控制端在线时通),再 CN 镜像(ghfast.top 等)回退。**ghproxy.net 已死**。
- **k3s**:安装脚本自身 enable+start,**勿**再加 systemd restart(race);verify 用 `|| true`。

## 🚧 后续（按 [design.md](design.md) 重整后的方向）
**主线（设计已定，待实现）**：
- **B 批 ansible 化**：①幂等契约 + `changed/ok/skipped/failed` 回显(地基)；②crater.yaml 轻量 task/play 层(module + when/loop/notify)；③module 库扩(file/copy/service/user/lineinfile/cron/git)。
- **离线转 OCI（D-018）**：抽 ArtifactSource/BundleFormat trait → 制品走 OCI(等价替换 tar.gz) → 加容器镜像打包(k3s air-gap 首个) → 临时 registry(F13)。组件加 `images:` 数据字段。选型 `oci-spec`+`oci-client`，musl 可编性待验证。
- **自举 agent（D-019）**：`crater agent` 现为 TODO 桩；实现本地执行计划/解包 OCI/导入镜像，复用引擎(Executor 换 Local)。一次性自举，用完即走。

**非阻塞**：
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
### 2026-05-31 续4（自举 agent D-019/D-026 落地）
- **实现**：`Op`/`Phase` 可序列化 + `plan_to_yaml`/`plan_from_yaml`；`crater agent --plan` 目标机本地执行（LocalExecutor，复用 `engine::execute`）；控制端 `--agent`：探 OS→lower 计划→推二进制(`/tmp/crater-agent`)+计划→一条 exec 跑 agent→流式回显→清理。`--agent-bin` 可指定二进制。
- **真机验证**：推 9.8MB glibc release 到 192.168.73.11，本地执行；清空后 `changed=2 ok=1`、再跑 `changed=0 ok=3`（`Done on local` 证实 LocalExecutor 在目标机跑，幂等贯穿）。+1 round-trip 单测（共 **26**）。
- **边界**：仅在线计划；离线 `PushFile`（blob ship）并入 OCI D-018 时做；二进制异构靠 `--agent-bin` 指 musl。
- **未动 git**。

### 2026-05-31 续3（Path B：spec 内联 recipe）
- **D-025**：`ComponentRef` 加内联 `preflight/install/verify/requires/supported_os`；`is_inline()` 真则直接构 descriptor，不读 `components/<name>/`。`resolve_descriptor` 统一加载（apply/order_components/build_bundle 共用）；bundle 把内联 recipe 序列化进包；`ComponentDescriptor::to_yaml`。
- **真机验证**：`examples/yq-inline.yaml`（单文件含 inventory + 内联 recipe）在线部署到 192.168.73.11，dry-run 计划正确、apply idempotent `ok=3`，全程未碰 `components/yq/`。
- `components/` 现为**可选复用库**；三种用法并存（零 spec / 单文件内联 / 分离）。+2 单测（共 **25**）。
- **未动 git**。

### 2026-05-31 续2（最小 demo + B1 幂等 + CLI 默认执行）
- **最小 demo**：加 `components/yq/component.yaml`（单文件无依赖二进制）+ `examples/yq.yaml`，**零 Rust 改动**在线部署到 192.168.73.11，verify 见 `yq version v4.53.2`。坐实 D-017「加东西=丢 YAML」。
- **B1 幂等回显（D-023）**：`Op::Shell` 加 `check` 探针 + `StepStatus{Ok,Changed,Warn}`；`execute` 按相分流（读类 ok/warn、安装类 check→ok/changed、写文件 sha256 比对）。探针：download=`test -s`、pkg=`dpkg -s`/`rpm -q`、systemd=`is-enabled/is-active`、run_cmd 支持数据 `check:`。真机 yq：首跑 changed=2 ok=1 → 再跑 changed=0 ok=3。+2 单测（共 23）。
- **CLI 默认执行（D-024）**：去掉 `--apply`，改 `--dry-run` 预览（`apply`=执行本就是 D-020 语义）。Apply/Deploy 子命令 + 快捷式 + 文档全改。
- **未动 git**。

### 2026-05-31 续（设计方向重整 + 还债 A 批）
- **理念对齐**：用户重申「代码里不能有任何具体产品逻辑」。审计出 4 处违规并清掉，确立 D-017「引擎零产品知识」铁律。
- **还债 A 批（已落地，build 绿 + 21 tests）**：①别名→组件 `aliases:` 数据 + `resolve_component`（删 `resolve_alias`，D-016 作废）；②doctor 探针→从组件 `SystemdUnit` 推导 + 通用 `journalctl -p err`；③`LoadImage` 加 `runtime` + 探测 nerdctl/docker/podman/ctr；④镜像表→`mirrors.default.yaml`(include_str! + 外部可覆盖)。`crater k8s` dry-run 验证数据别名生效。
- **文档重整**：新增 [design.md](design.md)(北极星)、[offline-format.md](offline-format.md)(OCI 详设)；回写 README/requirements(v0.3)/decisions(D-017~D-019)/索引。定向**离线转 OCI**(D-018)、**自举 agent**(D-019)。
- **未动 git**（用户未要求提交）。

### 2026-05-31 夜（全部完成并验证）
- 逐项真机验证 + 提交对账:docker、node_exporter 离线、mysql、k3s 全部 live 通过;es dry-run;doctor/ai CLI 行为符合预期;18 tests 绿。
- 关键修复:M2 fetch_best(ghproxy.net 死)+ 分块 base64 上传(大文件);M3 k3s 去 race 的 systemd restart。
- 多次因"批次取消"丢失整批编辑(根因:crater 命令非零退出),最终改严格单命令执行后全部补回并验证。progress.md 已对账为真实状态(去掉此前未经验证的夸大表述)。
