# Crater 设计方向（重新整理）

> 北极星文档。新会话读完 `requirements.md` 后读本文，建立「为什么这么设计」的整体心智。
> 最后更新：2026-05-31

---

## 0. 一句话

Crater 是一个**领域无关的、跑在 SSH 上的声明式部署引擎**：引擎只懂「怎么做」（通用原语），「做什么」（docker/k3s/mysql…）全是数据（YAML 描述文件）。它有**在线 / 离线**两种形态，共用同一套组件、同一套执行引擎，只在「制品从哪来」这一层分叉。

这跟 Ansible 是同一个哲学：ansible-core 不知道 nginx 是什么，nginx 的知识在 playbook/role（数据）里。**「类似 Ansible」与「装万物」是同一个目标。**

---

## 1. 第一性原理：引擎零产品知识（D-017）

> **引擎（Rust）只能有「通用原语」，不能有任何「具体产品」。**

| 允许在代码里 | 禁止在代码里（必须是数据） |
|---|---|
| 通用原语：`download`/`extract`/`render_template`/`write_file`/`systemd_unit`/`run_cmd`/`pkg_install`/`load_image`… | 产品名、服务名、别名、镜像源、诊断规则、依赖关系 |
| 幂等契约、DAG 排序、SSH 执行、OCI 打包/解包、镜像导入机制 | 「k8s 其实是 k3s」「docker 的服务叫 docker」「k3s 要拉哪些镜像」 |

**判据**：新增一个可部署对象 = 丢一个 `component.yaml`（必要时加模板/制品清单），**绝不改 Rust 重编译**。

这条铁律是「装万物」可信的唯一保证。已在「还债 A 批」清掉历史违规（`resolve_alias`、`doctor` 写死服务名、`LoadImage` 写死 docker、`source.rs` 写死镜像表）——详见 [decisions.md D-017](decisions.md)。

---

## 2. 两层结构

```
┌── 引擎（Rust，领域无关，编译进二进制）────────────────────┐
│  通用 module / 动作原语   幂等契约(check→act→report)       │
│  组件 DAG 编排            SSH 执行器(russh, agentless)       │
│  ArtifactSource 抽象      OS 探测(Debian/RHEL)               │
│  OCI 打包 / 解包 / 镜像导入（离线机制，不含具体镜像名）       │
└──────────────────────────────────────────────────────────┘
              ▲ 只 parse / 执行，不命名产品
┌── 数据（YAML，领域知识，放目录即生效，可热加载）──────────┐
│  components/<name>/component.yaml   ：原语序列 + requires      │
│                       + aliases + images(离线要打包的镜像)     │
│  crater.yaml                        ：inventory + 组件/任务     │
│  mirrors.yaml（可选覆盖）           ：镜像/代理源              │
└──────────────────────────────────────────────────────────┘
```

> **recipe 与 instance 可分可合（D-025）**：`components/<name>/` 是 **recipe**（怎么装，可复用/可签名/可进 OCI 镜像）；`crater.yaml` 是 **instance**（装到哪/哪些，含 inventory 与密钥）。分离是为复用与密钥隔离，但**不强制**——`ComponentRef` 可**内联** `install/verify/…`（Path B），让一个 spec 文件即完整描述部署；`components/` 退化为可选复用库。三种用法并存：零 spec（`crater apply yq --host`）／单文件内联／分离复用。

---

## 3. 在线 / 离线：同一管线，单点分叉

两种形态**不是两套代码**，而是同一条管线在 **ArtifactSource** 这一个点上切换。组件描述、DAG、计划、幂等、SSH 执行全部共用。

```
            crater.yaml + components/（完全相同）
                          │
                   Planner（DAG → Op 序列，完全相同）
                          │
          ┌───────────────┴────────────────┐
   ArtifactSource = Online        ArtifactSource = OciBundle
          │                                │
  目标机自己拉(curl/apt/                目标机零联网：制品来自
  runtime pull)，CN 镜像               推送过来的 OCI 包，
  源 fallback                          download→push-from-blob,
          │                            镜像→本地导入/临时 registry
          └───────────────┬────────────────┘
                   同一个 Executor（SSH/Local）执行
```

### 3.1 在线形态
- 控制端只编排；**依赖由目标机自身拉取**（curl/apt/容器运行时 pull）。符合 agentless（D-007）。
- 走 `mirrors.yaml` 的 CN 镜像改写 + 多源 fallback（F8）。
- 适合：能联网（哪怕弱网）的环境。

### 3.2 离线形态（基于 OCI 镜像 —— 本次定向）
- **在线控制机制包**：把所有制品（二进制/tarball + **容器镜像**）打成一个 **OCI 包**（见 §4）。
- **目标机零联网部署**：推送 OCI 包过去（复用 60KB 分块 base64 上传，D-009），现场解包、校验、导入镜像、执行计划。
- 适合：物理隔离 / 政企内网 / 真离线。

### 3.3 为什么离线选 OCI（取代 tar.gz，D-018）
| 维度 | tar.gz（M2 现状） | OCI 镜像/Layout（定向） |
|---|---|---|
| 完整性校验 | 手写 manifest + sha256 | **内容寻址**，digest 即校验，免手写 |
| 去重 | 无 | **分层**，多组件共享 base 只存一份 |
| 容器镜像 | 不支持（k8s/mysql 镜像没法离线带走） | **原生**：镜像本就是 OCI，可直接打包/导入 |
| 生态 | 自造 | registry / `ctr image import` / `docker load` / oras 通吃 |
| 多节点分发 | 每台塞一份 | 配合临时 registry（F13）一处推、多处指 |

这正是 sealos 的 clusterimage 思路（OCI 镜像即集群镜像），air-gap k8s 已验证可行。

---

## 4. OCI 离线包格式（概要，详见 [offline-format.md](offline-format.md)）

**离线包 = 一个 OCI Image Layout，打成单个 oci-archive tar 传输。**

```
bundle.oci  (tar of an OCI layout)
├── oci-layout                     # {"imageLayoutVersion":"1.0.0"}
├── index.json                     # 顶层 manifest 索引
└── blobs/sha256/
      ├── <digest>  crater-manifest # crater 自定义 mediaType：spec + 制品名→digest→落地动作
      ├── <digest>  components 层    # 组件描述文件打包
      ├── <digest>  artifact 层…     # 二进制/tarball（如 node_exporter、k3s 二进制），内容寻址
      └── <digest>  image blob…      # 容器镜像（pause/coredns/mysql/es…）的 manifest+layers 嵌套其中
```

- **制品** 与 **容器镜像** 都以 OCI blob 内容寻址存放；`crater-manifest` 是 crater 在 OCI 之上的逻辑索引（哪个制品对应哪个 digest、落地到哪、用哪个原语）。
- **D-017 守则**：引擎只懂「如何打/解 OCI、如何把镜像导入运行时」；**哪些镜像/制品要打包，由组件数据声明**（组件新增 `images:` 字段，列出该产品离线所需镜像，可带 digest 锁定）。

### 4.1 build（在线控制机）`crater build -f spec.yaml -o x.oci`
1. spec → 组件（DAG）。
2. 收集每个组件：`download` 制品（`fetch_best`，CN 镜像 fallback）+ `images:` 声明的容器镜像（OCI distribution 拉取，纯 Rust + rustls）。
3. 写 blobs（内容寻址）→ 构建 manifest/index + crater 注解 → 导出 oci-archive（可选 zstd）。

### 4.2 deploy（隔离目标机）`crater deploy --bundle x.oci --host ...`
1. 分块上传 OCI 包到目标机（D-009）。
2. 目标机解包 OCI Layout（零网络），digest 自校验。
3. 制品落地：文件→按描述放置；容器镜像→`ctr image import`/`docker load` 导入本地运行时，或推到**临时 registry**（F13）供多节点共享。
4. 跑同一套计划引擎（离线模式：`download` Op → push-from-blob Op，与现状同形）。

---

## 5. 执行模型：agentless shell + 自举 agent

crater 有两种把动作落到目标机的方式，**互补共存**：

### 5.1 agentless shell 推送（现状，已验证）
控制端经 SSH（russh）把每个 Op 翻成 shell 命令执行；写文件用分块 base64（D-009）。目标机**零安装**，只需 SSH + shell。优点：极致 agentless，适合在线、适合简单步骤。

### 5.2 自举 agent（`crater agent`）—— ✅ 在线已实现（D-026）
把 **crater 二进制本身**推送到目标机，在目标机上以 `crater agent` 模式运行，由它在**本地**执行计划。F16 的核心能力。
**现状**：`--agent` 模式已实现并真机验证——控制端探 OS、lower 出计划、推二进制+计划、一条 exec 跑 `crater agent --plan`（目标机用 LocalExecutor 本地执行，幂等回显照旧）、清理临时文件。离线 `PushFile`（blob 随计划 ship）并入 OCI 离线时补；异构目标用 `--agent-bin` 指 musl 静态构建。

**为什么需要它（不止是优化）**：
- **OCI 离线落地**：目标机解包 OCI Layout、digest 自校验、`image import`/推临时 registry —— 这些用一段段 shell 片段拼极脆弱；用推过去的同一个二进制在本地做，干净、可测、可复用引擎代码。**§4 的离线 deploy 第 2–4 步本质上就该由 agent 执行。**
- **减少 SSH 往返**：多步计划在本地连续跑，而非每步一个 exec 往返；弱网下尤其值。
- **更强能力**：本地可做 shell 表达不了的逻辑（结构化探测、幂等 check、回滚），与 §6 的幂等契约天然契合。

**形态**：控制端推送二进制（musl 静态单文件，N1）+ 一份计划/包 → 远端 `crater agent --plan <…>`（或 `--bundle x.oci`）→ agent 本地执行并回传结构化结果。控制端与 agent 复用**同一套引擎代码**，只是 Executor 换成 LocalExecutor。

> 设计要点：agent 不是常驻守护进程（仍 agentless 精神），是**一次性自举**——用完即走，目标机不留常驻服务。

---

## 6. ansible 化的执行模型（路线，B 批）

地基（A 批：引擎去产品化）已完成。接下来在干净地基上补 Ansible 内核：

1. **幂等契约 + `changed/ok/warn` 回显**（B1，地基）—— ✅ **已实现 + 真机验证**（D-023）
   每个 step 先探测当前态（check）→ 只对差异动手（act）→ 回报 `ok`/`changed`/`warn`。读类相（preflight/verify）只读不算 changed；安装类相 check 命中则跳过报 `ok`；写文件按 sha256 比对。`apply` 默认执行、`--dry-run` 预览（D-024）。真机 yq：首跑 `changed=2 ok=1` → 再跑 `changed=0 ok=3`。`skipped`（`when:` 条件跳过）归 B2。让重跑安全，是 crater 从「装东西脚本」升级成「配置管理工具」的关键一跃，**component 和 task 层都受益**。
2. **crater.yaml 轻量 task/play 层**（B2）
   ```yaml
   tasks:
     - copy:    { src: nginx.conf, dst: /etc/nginx/nginx.conf }
       when:    "os == debian"
       notify:  [reload nginx]
     - service: { name: nginx, state: started }
   ```
   覆盖「装任何东西」的长尾——不值得写完整 component 的随手活（改配置、加 cron、起服务、拉 git）。
3. **module 库扩到 Ansible 习惯**（B3）：`file/copy/template/service/user/group/lineinfile/cron/git` 等。仍是**通用原语**（D-017 允许），具体产品照旧是数据。

> task 层与 component 的分工：**component** = 精选、可复用、带 DAG、能离线的「已知物」；**task** = 一次性、长尾的「随便什么」。两者共用幂等契约与同一批 module。

---

## 7. CLI 终极形态、生命周期与状态

### 7.1 动词全集
```
crater plan     <source>     # 只生成执行计划（= apply --dry-run 的一等公民）
crater apply    <source>     # 执行部署：收敛到期望态(含 install+upgrade)，幂等，记一次 release 修订
crater bundle   <source>     # 打离线 OCI 包（= 现 build；build 保留为别名）
crater inspect  <source>     # 查看包内容（crater-manifest：组件/制品/镜像/digest/签名），只读
crater verify   <source>     # 校验依赖完整性 + 签名（F14/N4），只读，不需目标机
crater rollback <release>    # 回滚到上一修订（--to rev 指定）
crater remove   <release>    # 卸载（--purge 连系统包）
crater ai diagnose <release> # AI/规则诊断（= 现 doctor；见 §7.5）
```
所有破坏性动词（apply/rollback/remove）支持 `--dry-run`；`plan` 是 apply 预览的快捷式。

### 7.2 `<source>` vs `<release>`：两种第一类对象
| 概念 | 是什么 | 谁吃它 |
|---|---|---|
| **`<source>`** | recipe+制品的**来源**：组件名 / `./x.yaml` playbook / `oci://…` 镜像 / `./x.crater` 归档 / `-f spec.yaml` | plan / apply / bundle / inspect / verify |
| **`<release>`** | 已部署的**实例**：名字 + 修订历史 + 落在哪些主机 | rollback / remove / ai diagnose |

`apply` = 把一个 `<source>` 收敛成/更新一个 `<release>`。release 名默认取组件名，`--release <name>` 可区分（如 `docker-prod` / `docker-staging`）。

### 7.3 部署目标与覆盖（inventory 永远在镜像外，见 §4 与 D-018）
```
crater apply <source> --host 10.0.0.5 --password ***     # 单机命令式
crater apply <source> -i hosts.yaml                       # 多机 inventory
crater apply <source> --set version=24.0 --values prod.yaml  # 覆盖镜像内默认参数
crater apply -f crater.yaml                               # source=spec 文件(inventory 已内含，现状)
```

### 7.4 release 状态、回滚与卸载（D-021）
**状态记录放目标机**（不放控制端）——契合 agentless / air-gap / 政企：控制端可丢，主机自描述、可被任意控制端接管。

```
目标机 /var/lib/crater/releases/<release>/
  history.json     # [{rev, ts, source_ref, source_digest, params, status}]
  rev-<n>/
    plan.json      # 渲染后的 Op 序列（期望态快照）
    touched.json   # 动过的资源：写过的文件 / 启用的 unit / 装的包
```

- **apply**：解析 source → 计划 → 执行（幂等，逐步 `changed/ok/skipped`，B1）→ 写新修订 rev N（保留旧修订）。
- **rollback**：**声明式回滚**——默认重放 rev N-1 的期望态（吃幂等红利："装回旧版本"=apply 旧 desired-state）。OCI 下旧修订钉 `source_digest`，旧镜像在则直接重放；重放修不回的（删过的文件）用快照内容恢复；有副作用的 `run_cmd` **诚实标记不可自动回滚**，`plan rollback` 先列可逆/不可逆。
- **remove**：跑组件声明的 **`uninstall:` 阶段**（新数据字段，与 install/verify 对称，仍是数据→守 D-017）；未声明则按 `touched.json` 反向尽力清（默认不动系统包，`--purge` 才删，并明说可能不彻底）。成功后删 release 记录。

> 不靠"每个动作写逆操作"（脆、组合爆炸）。回滚=重放旧期望态（幂等）；卸载=声明式 `uninstall:` + 记录追踪兜底。两者都需 release 状态记录。

### 7.5 AI 接口（保留 namespace + 生命周期钩子，实现后置，D-022）
AI 始终副驾不司机（D-015）：只产候选/建议，确定性逻辑校验、人工确认才落地；可 `--ai off` 全关，永不成为硬依赖。AI 在 crater 里有**两种存在形式**：

**(a) 独立子命令**（统一收在 `crater ai <sub>`）：
```
crater ai diagnose <release>     # 诊断（规则引擎默认零网络零模型；--ai 叠加内网/云模型）— 现 doctor 迁入
crater ai generate "<大白话>"     # NL→spec（确定性护栏校验）— 现 crater ai 迁入
```

**(b) 贯穿生命周期的钩子**（在确定性主流程的关键点可选叠加 AI，全部默认关、`--ai` 开）：

| 阶段 | 钩子 | 干什么 | 对应需求 |
|---|---|---|---|
| **bundle 时** | 依赖分析 | 分析 yaml 里声明的 `requires`/`images`/`download`，**推断隐性依赖**（内核参数、系统包、缺失镜像），建议补进包 | AI2 |
| **apply 报错时** | 日志分析 | 部署中某步非零退出 → 抓该步 stderr/journal → 规则引擎先行，`--ai` 叠加模型给根因+修复命令 | AI5/AI6 |
| **apply 完成后** | doctor + 智能修复 | 部署后跑 `ai diagnose <release>` 健康检查；发现问题给修复建议（**仅建议，人工确认才执行**） | AI10 |

> 三个钩子共用一条**降级链**：固化规则（零网络，离线必可用）→ 内网 endpoint → 云端 OpenAI 兼容（D-015/AI9）。制包时还可把该 bundle 专属的诊断规则/runbook **固化进 OCI 包**（AI4），离线现场零模型也能诊断。
> 接口与钩子点先占位、契约稳定；规则侧已可用（M5），模型侧叠加后置实现。

---

## 8. 设计不变量（勿动摇）
- **引擎零产品知识**（D-017）——任何 PR 引入具体产品名/服务名到 Rust 代码即视为回归。
- **agentless**——目标机只需 SSH + shell；复杂逻辑由推送过去的同一二进制以**一次性自举 agent** 执行（§5，D-008/D-009），用完即走，不留常驻服务。
- **AI 副驾不司机**（D-015）——AI 只产候选，确定性逻辑校验；可 `--ai off` 完全关闭。
- **纯 Rust、免 C 工具链、musl 静态**（N1/D-012）——OCI 实现选型须守此线（`oci-spec`/`oci-client` + rustls，待验证）。
- **在线/离线共用一套组件与引擎**，只在 ArtifactSource 分叉。
