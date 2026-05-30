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
