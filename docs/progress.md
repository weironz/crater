# Crater 进展日志（自主开发）

> 「外部记忆」。新会话先读本文件 + requirements.md + decisions.md 恢复上下文。

## 🌅 给早上的诚实总结

连夜推进了 M1–M5。**有的部分在干净通道里亲眼验证过(可信),有的验证命令在工具故障中被取消(存疑,需复核)。** 下面严格区分,不夸大。请按"立即复核"先确认存疑项。

工具故障说明:整夜 `crater run/--apply/build` 等会非零退出的命令一旦与其它调用同批,会**连带取消整批**;后段输出通道还大面积延迟/丢失。导致部分验证与提交未能确认。

## ✅ 确凿验证（在干净通道亲眼见到结果）

- **M1 在线 docker**:`crater docker --host 192.168.73.11 --password 123456 --apply` → 见到 `Docker version 29.5.2` + `active` + exit 0。**可信**。
- **M2 离线 bundle 全链路**:`crater build -f examples/node_exporter.yaml -o ne.bundle`(直连 GitHub 拉 10,676,343 B)→ `crater deploy --bundle ne.bundle --host .. --apply` rc=0,完整日志见到:`pushed 10676343 bytes`、`node_exporter, version 1.8.2`、`# HELP go_gc_duration_seconds...`、`Done: 7 step(s)`。**可信**。这是夜里最硬的成果(含两处关键修复:fetch_best 直连+CN 回退、SshExecutor 大文件 60KB 分块上传)。
- **cargo build 0 错 0 警 + cargo test 18 passed / 0 failed**:多次见到。**可信**(含 bundle/dag/ai/diagnose/os/source 单测)。

## ⚠️ 存疑（代码已写/已提交，但 live 验证被取消，需复核）

- **M3 k3s live 安装**:`dag.rs` 拓扑排序与单测可信(在 18 tests 内)。但 k3s **真机是否装成尚未确认**——最后一次探测 `which k3s` 返回 127(未找到),说明之前"k3s Ready"的说法来自被取消的批次,**不可信**。需复核:跑 `crater k8s --host .. --password 123456 --apply` 再 `crater run ... -- "systemctl is-active k3s; k3s kubectl get nodes"`。
- **M4 `crater ai`**:`ai.rs` provider + nl_to_spec + 护栏单测可信。CLI 无 endpoint 友好报错见过一次但被故障干扰,建议复跑。真实 endpoint 的 NL→spec 未跑(需配 CRATER_AI_ENDPOINT/MODEL)。
- **M5 `crater doctor`**:`diagnose.rs` 10 规则 + 4 单测可信。`crater doctor --file <log>` 的 CLI 输出未在干净通道确认,建议复跑(应报 apt/network/disk 3 类)。
- **mysql / elasticsearch 组件 + 别名(k8s→k3s, es→elasticsearch)**:component.yaml 与 main.rs 的 resolve_alias 代码可能已写入工作区,但**提交状态与 live 验证均未确认**。当前工作区有 3 个未提交文件(通道故障看不到文件名)。需复核:`git status`,按需 `cargo build` 后提交;mysql 可真机 apply(box 内存够);es 建议仅 dry-run(box 仅 ~1.2GB 空闲,ES 需 ~2GB,勿 OOM 已验证服务)。

## 🧭 立即复核（建议顺序，每条单独一命令）
```powershell
cd D:\codes\crater
git status                  # 看 3 个未提交文件是什么；cargo build 通过后决定是否提交
cargo build                 # 应 0 错
cargo test                  # 应全绿
.\target\debug\crater.exe run --host 192.168.73.11 --password 123456 -- "systemctl is-active docker; systemctl is-active k3s; systemctl is-active mysql; which k3s mysql"
# 据上结果决定补跑哪些 --apply（k3s 大概率需要补装）
```

## 📁 代码结构（已落盘）
```
crates/crater-core/src/  spec component os executor source engine bundle dag ai diagnose lib
crates/crater-cli/src/main.rs   apply/build/deploy/ai/doctor/run/agent/<component>(+resolve_alias)
components/  docker(✓live) node_exporter(✓live) k3s(存疑) mysql(存疑) elasticsearch(存疑)
examples/    crater.yaml node_exporter.yaml
```

## 🔧 关键技术结论（已被验证，勿动摇）
- **SSH = russh 0.45 直连**:check_server_key 形参 `&russh::keys::key::PublicKey`;authenticate_password 返 `Result<bool>`;exec 走 channel.wait() 匹配 ChannelMsg。
- **大文件写远端 = 60KB 分块 base64**:单条 exec 传大文件先 code -1,再 200KB 触发 Linux `MAX_ARG_STRLEN` "Argument list too long";60KB/块 append→一次解码,已验证 10.6MB OK。
- **离线包 = tar.gz 纯 Rust**(tar + flate2 rust_backend),免 C 工具链。
- **GitHub 拉取 = fetch_best**:先直连(控制端在线时通),再 CN 镜像(ghfast.top 等)回退。**ghproxy.net 已死**。
- **k3s**:安装脚本自身 enable+start,**勿**再加 systemd restart(race);verify 用 `|| true`。

## ⚠️ 工具链纪律（务必遵守）
1. **任何会非零退出的执行命令(crater --apply/run/build、远端命令)必须单独一条消息**;同批会连带取消其它调用(本夜反复中招)。纯 Write/Edit 可批量。
2. 命令输出 `Out-File` 到文件再 Read;通道会延迟/丢失,耐心等一轮,**勿重试同命令**(会重复执行)。
3. 不擅自中断、不找用户确认(已授权)。

## 工作日志（倒序）
### 2026-05-31 夜
- 干净验证:M1 docker、M2 离线 bundle 全链路(含 fetch_best + 分块上传两修复)。
- 存疑(验证被取消):k3s live、mysql/es 组件提交与 live、M4/M5 CLI live。代码大多已写。
- 后段工具通道大面积故障,停止盲改,转为诚实交接 + 安排复核。
