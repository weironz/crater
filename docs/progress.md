# Crater 进展日志（自主开发）

> 「外部记忆」。新会话/唤醒后先读本文件 + requirements.md + decisions.md 恢复上下文。

## ⚠️ 工具链/操作纪律（最重要，必须遵守）

1. **每条消息只发一个工具调用。** 夜间最大教训:`crater run` 会把远端退出码当进程退出码,远端命令非零 → 该工具调用"报错" → **同一消息里并行的其它所有调用被连带取消**。多次导致整批编辑/提交丢失。**严格串行,一次一个。**
2. 大输出会被渲染故障污染(出现 "the the the…" 噪声并截断,非注入)。命令输出一律 `Out-File` 到文件再用 Read 读;只取前面干净行。
3. 工具回显有时整批延迟到下一轮才回来;耐心,必要时多等一轮,**不要重试同一命令**。
4. 不擅自中断、不找用户确认(已授权);阻塞项记此并继续能做的。

## 真机环境事实（已确认）
- 测试机 `192.168.73.11`:Ubuntu **22.04 jammy**,root/`123456`。docker **未装**。
- apt 源已是 **aliyun 镜像 + universe 启用**(`deb http://mirrors.aliyun.com/ubuntu/ jammy main restricted universe multiverse`)。→ **docker.io 可装**,前提是先 `apt-get update`(crater 的 install_cmd 已内置 `apt-get update && install`)。此机**无需换源**;换源逻辑留给其它环境(D-011,未做)。
- 开发机 Windows;cargo 1.94.1;有 OpenSSH;ping 通测试机。
- 模型:控制端 Windows 跑 crater.exe,经 SSH 连 Ubuntu 执行(agentless),无需交叉编译。✔ SSH 连通 + OS 探测已实测 OK。

## 里程碑（真实状态）
- [x] **M1-骨架**:workspace + 声明式引擎(内部标签)+ docker 组件 + dry-run/本地 apply(commit `1b7749e`)。
- [x] **M1-SSH/引擎**:编译通过(commit `2a2e661`,EXIT=0,4.30s)。SSH 执行器(russh 0.45,API 已从源码确认)、Op 引擎(Shell/WriteFile)+ execute() 跨 Local/SSH、SSH 探测 OS、模板渲染、base64 写远端文件、CLI --host/run/apply。
  - **分支注意**:当前在 `master`(非 `main`)。HEAD = `2a2e661`。M1 代码都在这。
- [ ] **M1-收尾(待验证)**:`crater docker --host 192.168.73.11 --password 123456 --apply` 端到端装通 docker + verify。**夜间多次尝试都被批次取消,尚未确认装通**。下次:单独一条命令跑 apply,Out-File 后读。
- [ ] **M2**:离线 bundle(tar.gz + manifest + sha256;build/deploy)。**代码写过但被取消,需重做**(设计见下,照着重写即可)。
- [ ] M3:k8s 组件 + 多节点 DAG。
- [ ] M4:AI 制包侧(provider 抽象 + NL→spec)。
- [ ] M5:AI 离线侧(固化诊断 / 内网 endpoint)。

## SSH 库结论（已定，勿再动摇）
russh 直接依赖 `russh = "0.45"`(实际 0.45.0,已在本机 cargo 缓存,离线可编)。从源码确认:
- `check_server_key` 形参 `&russh::keys::key::PublicKey`(中间是 `::key::`)。
- `authenticate_password(user,pass)` 返回 `Result<bool>`(直接 `if !authed {bail}`)。
- exec:`channel_open_session()` → `channel.exec(true,cmd)` → 循环 `channel.wait()` 匹配 `ChannelMsg::{Data, ExtendedData{ext==1→stderr}, ExitStatus}`。
不要用 async-ssh2-tokio(需联网下载且 API 仍要猜)。

## M2 设计（重做时照此实现）
目标:`crater build -f spec.yaml -o x.bundle`(在线机制包)→ `crater deploy --bundle x.bundle --host .. --apply`(目标机零联网)。
- **bundle 格式**:单文件 tar.gz(纯 Rust:`tar` + `flate2 rust_backend`,免 C 工具链)。内含:
  - `manifest.yaml`:format_version、name、components[{name,version}]、blobs[{source_url, sha256, size}]
  - `components/<name>/`:组件描述 + templates(从 components/ 拷入)
  - `blobs/<sha256>`:内容寻址的已下载产物
- **build**:遍历 spec 组件 → `collect_downloads()` 取每个 download 的**原始 url**(离线模式不做镜像改写,raw url 作 manifest 键)→ 控制端 `reqwest`(走镜像改写)拉取 → sha256 → 存 blob → 写 manifest → verify → pack。
- **deploy**:unpack → 读 manifest → **先 verify 所有 blob 校验和** → 构造 `PlanContext.with_offline(url→本地blob路径)` → build_plan 时 `download` 动作变成 `Op::PushFile`(把本地 blob 经 base64 推到目标 /tmp/crater-dl 或 dest)→ execute。
- **引擎改动**:`Op` 加 `PushFile{local_path,dest}`;`PlanContext` 加 `offline_blobs: Option<Map<url,PathBuf>>` + `with_offline()` + `rendered_url()`(离线返 raw,在线返 rewrite);`collect_downloads()`;`Extract` 支持 `from`(默认 /tmp/crater-dl)。
- **新组件 node_exporter**:单个静态二进制(GitHub release tar.gz),最适合演示离线路径(download/extract/写 systemd unit/启服务)。`examples/node_exporter.yaml`。
- **依赖**(已想好,加到 workspace):`reqwest`(default-features=false, rustls-tls)、`tar="0.4"`、`flate2`(rust_backend)、`sha2="0.10"`。
- **bundle.rs 单测**:sha256 已知向量("abc")、pack/unpack roundtrip。

## 决策补充（夜间）
- D-007 在线下载交给目标机(curl/apt),控制端 reqwest 只在 M2 制包用。
- D-008 SSH 用 russh 0.45 直连(API 已确认)。
- D-009 写远端文件用 base64 over exec(Local/SSH 通用,Executor 默认方法)。
- D-010 docker 用发行版包路径(最稳);静态二进制路径留给 node_exporter 演示 + 后续 profile。
- D-011(未做) 通用换源加速(apt/dnf 国内镜像 + 启用 universe):本测试机已是 aliyun 故不阻塞;其它环境需要时再做。
- D-012 离线 bundle = tar.gz 纯 Rust(免 C);OCI/zstd 留后续。

## 工作日志（倒序）
### 2026-05-31 夜(第2轮唤醒后)
- 确认真机 apt = aliyun + universe,docker 可装(无需换源);M1 收尾只差实际 apply 验证。
- 第一次写的 M2(bundle.rs/source fetch/engine PushFile/CLI build&deploy/node_exporter)被 `crater run` 退出码触发的批次取消而丢失 → 改为严格单调用重做。
- 纠正了 progress.md 里"已合并 main/docker 已验证"的错误声明(那些来自被取消的批次,并未真正发生)。
### 2026-05-31 夜(第1轮)
- M1 骨架→SSH 执行器→Op 引擎重构。russh API 反复后查源码锁定 0.45。编译通过 2a2e661。
- 工具回显大面积异常,采取写文件+单步策略。
