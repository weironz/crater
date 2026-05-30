# Crater 进展日志（自主开发）

> 这是我的「外部记忆」。每完成一步就更新,防止上下文窗口被压缩后丢失状态。
> 新会话/唤醒后:**先读本文件 + requirements.md + decisions.md 恢复全部上下文**,再继续。

## 当前总体状态

- 起始:2026-05-31 夜,用户授权连夜自主推进 M1→M5,次日早晨对进展。
- 测试机:Ubuntu @ `192.168.73.11`,root / `123456`(用户授权的测试机)。
- 开发机:Windows(本地 `cargo test` 原生可跑;crater.exe 作为控制端通过 SSH 连测试机执行)。
- 部署模型确认:**控制端在 Windows 跑 crater,经 russh SSH 到 Ubuntu 目标机执行**(agentless),无需交叉编译即可端到端测试。

## 里程碑勾选

- [x] M1-骨架:workspace + 声明式组件引擎 + docker 组件 + dry-run/本地 apply(commit 1b7749e)
- [ ] M1-完整:russh SSH 执行器 + reqwest 下载(+sha256) + 经 SSH 跑通 docker 安装,在 192.168.73.11 验证
- [ ] M2:离线 build/deploy(tar.zst + manifest + sha256;OCI 镜像后续)
- [ ] M3:k8s 组件 + 多节点 DAG 编排
- [ ] M4:AI 制包侧(provider 抽象 + NL→spec + 依赖补全)
- [ ] M5:AI 离线侧(固化诊断 / 内网 endpoint;本地模型 candle 视情况)

## 工作日志（倒序追加）

### 2026-05-31 夜 — 启动自主开发
- 读取 requirements/decisions,确认范围。
- 检查环境:交叉编译目标、ssh 客户端、到测试机连通性(见 _env.txt)。
- 决定 M1 用 russh(纯 Rust + rustls,无 C 依赖,跨平台),reqwest 用 rustls-tls。
- 取 russh 当前文档(context7)以确保 async handler API 正确。

## 已知风险 / 注意

- 本会话工具输出有延迟(结果常晚一轮返回);改为「命令输出写文件 → 读文件」模式规避。
- 国内网络:测试机可能拉不动 docker.com,下载步骤可能需镜像源;验证时注意。
- russh / reqwest API 版本敏感,优先 context7 核对。
- 不擅自中断、不找用户确认(用户明确授权)。出现阻塞性问题记录到本文件,继续做能做的部分。
