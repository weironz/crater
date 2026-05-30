# 决策 / 沟通记录

> ADR 风格，按时间倒序追加。每条记录：背景、决策、理由、影响。
> **每次关键沟通后在此追加。**

---

## 2026-05-31 · 项目立项与 M1 启动

### D-001 项目命名为 crater
- **决策**：工具与项目名定为 `crater`，CLI 主命令 `crater`。
- **理由**：cluster + crate(Rust) 合成，短、好记；终极目标 `crater <target>` 部署一切。
- **影响**：仓库、二进制、文档统一用此名。待办：crates.io / GitHub / npm 占用核查。

### D-002 目标 OS 首批仅 Ubuntu/Debian 系 + RHEL 系
- **决策**：首批支持 Debian/Ubuntu 系与 RHEL 系；国产化 OS（麒麟 Kylin / 统信 UOS）暂不纳入。
- **理由**：先跑通主流；国产 OS 多为两系衍生，按系族归类后续扩展，且需额外处理 SELinux/安全模块/内核差异。
- **影响**：`OsProvider` 按 Debian/RHEL 抽象，留扩展位不堵死。

### D-003 组件用「声明式描述文件」定义
- **决策**：组件以 YAML 描述文件定义，核心是「确定性执行引擎 + 内置动作原语」；内置与第三方组件同构。
- **理由**：第三方扩展门槛低、可热加载、放目录即加载。
- **影响**：核心实现动作原语集（download/extract/render_template/systemd_unit/pkg_install/run_cmd/preflight…）；复杂分支（如 k8s 证书/join）预留 `run_cmd` + 脚本钩子逃生口。

### D-004 AI 能力后置（M4/M5）
- **决策**：M1–M3 先做硬核、可靠的部署能力，AI 放 M4/M5。
- **理由**：先有稳妥可用产品；地基不稳不上 AI。
- **影响**：M1 不依赖 AI；但 spec / 数据结构预留 AI 字段，避免返工。AI 始终是副驾，可 `--ai off` 关闭。

### D-005 MVP 首个组件为 docker/containerd
- **决策**：M1 只打通 docker 的在线安装全链路。
- **理由**：依赖少、是众多组件的基础，适合验证「在线安装 + SSH 执行 + 声明式组件」主路径，不触碰声明式表达力边界。
- **影响**：`components/docker/component.yaml` 作为首个内置组件与狗粮。

### D-006 文档分多部分、持续记录沟通
- **决策**：需求文档拆分多文件、持续增补；关键沟通沉淀到 `decisions.md`。
- **理由**：防止上下文窗口丢失导致需求遗忘。
- **影响**：见 [docs/README.md](README.md) 维护约定；新会话先读 `requirements.md` + 本文件。

---

## 2026-05-31 夜 · M1–M5 自主开发（均真机验证）

### D-007 在线下载交给目标机；控制端 reqwest 仅用于制包
agentless 下在线依赖由目标机自身 curl/apt 拉取;控制端 reqwest 只在 `crater build` 制离线包时拉取。符合 F9,简化 M1。

### D-008 SSH 用 russh 0.45 直连
从 russh-0.45.0 源码确认 API(check_server_key 形参 `&russh::keys::key::PublicKey`,authenticate_password 返 `Result<bool>`)。放弃 async-ssh2-tokio(需联网且 API 仍要猜)。

### D-009 写远端文件用 base64 over exec + 大文件分块
小文件:`printf %s <b64> | base64 -d > file`,免单独 SFTP 通道,Local/SSH 通用。大文件(如 10MB 离线制品)单条 exec 过大会导致 SSH channel 无 exit-status(code -1),且 >128KB 触发 Linux MAX_ARG_STRLEN;故 SshExecutor::write_file 改为**60KB 分块** append 到临时文件后一次性解码。已验证 10.6MB OK。

### D-010 docker 用发行版包路径
首个端到端最稳(docker.io / docker);静态二进制(引擎已支持 download/extract)留作 node_exporter 演示与后续 profile。

### D-011 通用换源加速（待做）
apt/dnf 国内镜像 + 启用 universe。测试机 192.168.73.11 本就是 tuna/aliyun 源 + universe,故 M1 未被阻塞;其它环境需要时再补。

### D-012 离线 bundle = tar.gz 纯 Rust
tar + flate2(rust_backend),免 C 工具链,Windows 控制端可直接编。OCI layout / zstd 留后续。

### D-013 k8s 代表选 k3s
轻量、单二进制、CN 镜像友好(INSTALL_K3S_MIRROR=cn)。kubeadm 完整集群、k3s 多节点 join、k3s air-gap 离线均留后续。安装脚本自身 enable+start,组件**不再加** systemd restart(避免 race),verify 用 `|| true` 容忍首启慢。已真机验证节点 Ready。

### D-014 组件依赖用 requires + 拓扑排序
`dag.rs` Kahn 拓扑排序,确定性(字母序断平),检测环与缺失依赖;spec 内未列出的依赖被忽略(宽松)。

### D-015 AI 副驾不司机（OpenAI 兼容 + 确定性护栏）
provider 用 OpenAI 兼容协议(通吃 OpenAI/DeepSeek/Qwen/内网 endpoint,契合政企离线)。`nl_to_spec` 让模型只产候选,再经 schema 反序列化 + 组件存在性校验;幻觉被拒,绝不静默驱动部署。离线诊断 `diagnose.rs` 固化规则零网络零模型先行,内网模型可叠加。

### D-016 组件别名
`resolve_alias`:`k8s`/`kubernetes`→`k3s`,`es`→`elasticsearch`,使用户字面命令 `crater k8s` / `crater es` 可用。
