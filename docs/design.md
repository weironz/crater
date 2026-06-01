# Crater 设计方向（重新整理）

> 北极星文档。新会话读完 `requirements.md` 后读本文，建立「为什么这么设计」的整体心智。
> 最后更新：2026-06-01
>
> **D-046 起,component 模型已收敛为单一 task 模型**:`components/<name>/component.yaml`
> → `tasks/<name>.yaml`(顶层 `actions:` 列表);多组件 `crater.yaml` spec → 单 task +
> 分离的 `inventory.yaml`。下文出现的「component / 组件 / recipe」一律按 **task** 理解;
> 命令以当前 CLI 为准(`crater apply <task>`、`crater build -f task -t ref`、`crater save`)。
> 引擎原理(零产品知识、在线/离线单管线、自举 agent、OCI 离线、D-036 YAML 纯数据)不变。

---

## 0. 一句话

Crater 是一个**领域无关的、跑在 SSH 上的声明式远程执行引擎**：引擎只懂「怎么做」（通用原语），「做什么」（装 docker/mysql、改配置、起服务、跑巡检…）全是 **task 数据**（YAML）。它有**在线 / 离线**两种形态，共用同一套 task 与执行引擎，只在「制品从哪来」这一层分叉。

这跟 Ansible 是同一个哲学：ansible-core 不知道 nginx 是什么，知识在 playbook（数据）里。**「类似 Ansible」与「装万物」是同一个目标——且守住「YAML 是数据、逻辑在引擎」（D-036），绝不重蹈 Ansible 把 YAML 变程序的覆辙。**

---

## 1. 第一性原理：引擎零产品知识（D-017）

> **引擎（Rust）只能有「通用原语」，不能有任何「具体产品」。**

| 允许在代码里 | 禁止在代码里（必须是数据） |
|---|---|
| 通用原语：`place`/`extract`/`render_template`/`write_file`/`systemd_unit`/`run_cmd`/`pkg_install`/`load_image`… | 产品名、服务名、别名、镜像源、诊断规则、依赖关系 |
| 幂等契约、DAG 排序、SSH 执行、OCI 打包/解包、镜像导入机制 | 「es 其实是 elasticsearch」「docker 的服务叫 docker」「装 mysql 要哪些 OS 包」 |

**判据**：新增一个可部署对象 = 写一个 `tasks/<name>.yaml`（task：原语 + 物料），**绝不改 Rust 重编译**。

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
┌── 数据（YAML，领域知识，放目录即生效）─────────────────────┐
│  tasks/<name>.yaml      ：actions(原语 + needs) + materials     │
│                           + hosts + handlers + register          │
│  inventory.yaml（-i）   ：hosts + 嵌套 groups（targeting）       │
│  mirrors.yaml（可选覆盖）：镜像/代理源                          │
└──────────────────────────────────────────────────────────┘
```

> **recipe 与 targeting 分离**：`tasks/<name>.yaml` 是 **recipe**（要达成什么，可复用/打成 OCI artifact）；`inventory.yaml` 是 **targeting**（装到哪/哪些，含密钥）。命名 task（`crater apply yq`）从 `tasks/` 解析；也可直接 `crater apply ./x.yaml`（自带 `hosts:`）+ `--host`/`-i` 提供目标。

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
  源 fallback                          place→push-from-blob,
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
      ├── <digest>  material 层…     # 二进制/tarball（如 yq、zot 二进制），内容寻址
      └── <digest>  recipe 层…       # task YAML（recipe-replay 据此回放）
```

- **制品** 与 **容器镜像** 都以 OCI blob 内容寻址存放；`crater-manifest` 是 crater 在 OCI 之上的逻辑索引（哪个制品对应哪个 digest、落地到哪、用哪个原语）。
- **D-017 守则**：引擎只懂「如何打/解 OCI、如何把镜像导入运行时」；**哪些镜像/制品要打包，由组件数据声明**（组件新增 `images:` 字段，列出该产品离线所需镜像，可带 digest 锁定）。

### 4.1 build（在线控制机）`crater build -f task.yaml [-t ref]` → 本地库
1. 读 task → `materials`（binary：`fetch_best`，CN 镜像 fallback）。
2. recipe = task YAML 本身；写 blobs（内容寻址）→ artifactType 清单(`crater.component`) → 进本地库（`crater save <ref> -o x.oci` 导出文件）。

### 4.2 apply（隔离目标机）`crater apply <ref>|x.oci --host ...`
1. 从本地库 / registry / 文件取 artifact，分块上传到目标机（D-009）。
2. digest 自校验 → 识别 `artifactType` → **recipe-replay**：用 `plan_from_task` 离线回放（`place` 从包内 blob 推送），控制端 `execute_task`。
3. 与在线**同一套 task 引擎**，仅「制品从哪来」分叉（D-020）。

### 4.3 OCI 用法分 A/B：容器镜像 vs 物料包（D-032）

「用 OCI 封装依赖」有两条本质不同的路，**别混**：

| | A 类 · 容器镜像(image) | B 类 · 物料包(OCI artifact) |
|---|---|---|
| 是什么 | 被 containerd **run** 的镜像（ES 官方镜像、应用镜像） | 拿 OCI 当通用分发格式，装二进制/rpm/helm/recipe，**落地到宿主机**(copy+systemd) |
| 规范 | image-spec（config 有 entrypoint/env、rootfs 可运行） | image-spec 1.1 的 `artifactType`（ORAS 之道） |
| crater 怎么处理 | **只搬运**：pull blob 存 layout → 目标机 `ctr import`，**零自造 build** | **主力**：自造 artifact（自定义 mediaType 分层 + annotations 自描述） |

**铁律**：要跑容器走 A 类 image；落地宿主机的物料走 B 类 artifact，**不要把 yq 二进制伪装成一个永不被 run 的畸形 image**。crater 主力是 B 类。

> ⚠️ 现状缺口：`crater build --image` 目前把物料封成**伪 image**（image-manifest + rootfs config）——正是上面的反模式，标记为**过渡实现**，迁移到 B 类 artifact 是路线项。

**B 类 artifact 构建原则**：
1. **分层有边界**：binary / config / recipe / ospkg 各自成 layer（细粒度去重，升级只换变动层）。
2. **manifest 自描述**：`artifactType: application/vnd.crater.component.v1` + annotations（component 名/版本/`run-mode: process|container`），引擎靠它路由。
3. **recipe 进包**：物料 + 怎么装同 artifact，自包含。
4. **纯 Rust 构建**：`oci-spec` 类型 + 手写 blob（或 `ocipkg`），不 shell docker（守 N1/单二进制）。
5. **聚合用 image index**：多组件 bundle = 一个 index 引用各组件 artifact manifest → 跨 bundle 底层 blob（glibc rpm、containerd…）**全局去重**。

**crater 特有约束（对评审的补充）**：
- **zstd 非免费**：tar+zstd 解压快，但 zstd 编码 = `zstd-sys`（C 依赖），与 D-012/N1「纯 Rust/免 C/musl 静态」冲突 → 默认仍 **gzip(flate2)**，zstd 作**可选**；解码可先用 `ruzstd`(纯 Rust)。
- **生态验证**：需确认 `oci-client` 支持 artifact manifest（artifactType/referrers）+ Harbor/zot 的 1.1 支持；不行则**务实回退**＝带自定义 annotations 的 image-manifest（B 类语义、image-manifest 类型，最大兼容）。
- **A 类仍保留**：以容器跑的组件（其镜像）走 A 类搬运；crater 两类都要，按数据里的 `run-mode` 决定。
- **守 D-017**：A/B/run-mode/mediaType 全是**数据**，引擎只按 mediaType/artifactType 通用处理，不认识具体产品。

---

## 5. 执行模型：自举 agent（默认）+ agentless shell（逃生/引导）

crater 有两种把动作落到目标机的方式。**agent 是默认执行模型（D-027）**，shell 是它的引导层与逃生口——用户默认不用选。

### 5.1 agentless shell 推送（逃生口 / 引导层）
控制端经 SSH（russh）把每个 Op 翻成 shell 命令执行；写文件用分块 base64（D-009）。目标机**零安装**，只需 SSH + shell——这是最纯粹的 agentless，能跑在任何 Linux 上。`--shell` 强制走它（目标机无法运行 crater 二进制时的逃生口）；推 agent 二进制本身也走这个通道（引导层）。

### 5.2 自举 agent（`crater agent`）—— ✅ 默认执行模型（D-026/D-027）
把 **crater 二进制本身**推送到目标机，在目标机上以 `crater agent` 模式运行，由它在**本地**执行计划。F16 的核心能力，**现为默认**。
**现状**：控制端探 OS + 目标 arch（`uname -m`）、lower 出计划、**自动选二进制**（`--agent-bin` > 匹配 arch 的 bundled musl 静态 `dist/crater-linux-<arch>` > 同 arch 的 control 二进制）、推送（按 sha256 **缓存**在 `/var/lib/crater/crater` —— 就是同一个完整 crater 二进制，跑其 `agent` 子命令）、一条 exec 执行（LocalExecutor，幂等回显照旧）、清理瞬时 plan。无可用二进制则报错提示 `scripts/build-musl.sh <arch>` / `--agent-bin` / `--shell`。已验证 x86_64 自动选 musl 静态；aarch64 待装 target 验证。离线 `PushFile`（blob 随计划 ship）并入 OCI 离线时补。

**为什么需要它（不止是优化）**：
- **OCI 离线落地**：目标机解包 OCI Layout、digest 自校验、`image import`/推临时 registry —— 这些用一段段 shell 片段拼极脆弱；用推过去的同一个二进制在本地做，干净、可测、可复用引擎代码。**§4 的离线 deploy 第 2–4 步本质上就该由 agent 执行。**
- **减少 SSH 往返**：多步计划在本地连续跑，而非每步一个 exec 往返；弱网下尤其值。
- **更强能力**：本地可做 shell 表达不了的逻辑（结构化探测、幂等 check、回滚），与 §6 的幂等契约天然契合。

**形态**：控制端推送二进制（musl 静态单文件，N1）+ 一份计划/包 → 远端 `crater agent --plan <…>`（或 `--bundle x.oci`）→ agent 本地执行并回传结构化结果。控制端与 agent 复用**同一套引擎代码**，只是 Executor 换成 LocalExecutor。

> 设计要点：agent 不是常驻守护进程（仍 agentless 精神），是**一次性自举**——跑完即退，目标机无常驻进程/端口/服务（二进制按 sha256 缓存为惰性文件，供下次复用，D-027）。

### 5.3 控制端如何与 agent 通信（无常驻、无端口）
通信方式与二进制是否静态**无关**（静态只是链接方式）；相关的是进程模型——agent 是**一次性进程**，全程靠 **SSH 一发一收**：

```
控制机 ──SSH 写文件──▶ /var/lib/crater/agent(二进制) + /tmp/crater-plan.yaml(自包含输入)
控制机 ──SSH exec───▶ `agent --plan …`  →  目标机起一次性进程，本地执行
控制机 ◀─stdout/err─  顺同一 exec 通道流回进度/结果  →  进程退出，exec 结束，无残留
```

- **不存在"连到一个在跑的 agent"**：agent 不监听端口、不常驻。控制机是"推入参→触发一次→收输出"，本质同 ansible。
- **计划自包含**：agent 干活要的一切（步骤、将来离线 OCI 的 blob）都**事先作为文件推过去**，运行时无需中途回头要数据。
- **静态二进制照样这么通信**：它的 stdin/stdout/exit code 经 SSH 流转，与动态二进制无异。
- **将来更丰富（结构化结果/流式进度/多轮编排）仍守此模型**：控制机是大脑、agent 是无状态执行器，每"批"一次 SSH exec，结构化结果走 stdout（JSON/JSONL）；**绝不退化成常驻 agent + RPC 端口**（那会破坏 agentless、要开端口/防火墙/管生命周期）。

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

### 6.1 module 模块化：四层模型（D-029）

`action` 集若只是一个 Rust enum，会"每加一个 module 就改 Rust + 逼第三方 fork"。借鉴 ansible，按「复杂度/通用度」分四层定义 module；**复用已造好的零件**：B1 的 `StepStatus(ok/changed/failed)` 就是模块结果契约，自举 agent(D-027) 就是送达载体，D-017 数据驱动就是"放目录即加载"。

| 层 | 形态 | 何时用 | 改 Rust? | 类比 ansible |
|---|---|---|---|---|
| **1 内置类型化** | Rust enum 变体（place/pkg_install/systemd_unit/file/copy/service/user/lineinfile…），typed + 幂等 + OS 抽象，lower 成 shell | 通用高频、需幂等/OS 抽象的核心 | 是（刻意精选 ~15-20） | ansible-core 模块 |
| **2 数据定义** | `modules/<name>.yaml`：`params` + `check:` + `act:` 模板，引擎渲染后 lower 成 `Op::Shell{check,cmd}` | 简单可复用幂等操作，零代码 | 否（放目录即加载） | — (crater 特有，最轻) |
| **3 外部 module** | 脚本/静态二进制 + JSON 契约（收 params+check_mode，吐 `{status,changed,msg}`），agent 送到目标机跑 | 复杂逻辑、第三方生态 | 否（开放生态） | collection / Galaxy |
| **4 `run_cmd`+`check`** | 裸命令 + 幂等探针 | 一次性长尾、逃生口 | 否 | command/shell |

**统一契约**：所有层都遵循 `check → act → StepStatus`（B1）。
- 第 1/2 层 lower 成 `Op::Shell{check, cmd}`，**shell 模式也能用**（任何目标机）。
- 第 3 层需 agent（送二进制/脚本到目标机），优先**静态二进制 / 纯 shell 脚本**以守"目标机零依赖"（不像 ansible 依赖目标机 Python）——这也给"agent 作默认(D-027)"再添一条理由：**更丰富的 module 生态依赖 agent**。

**`module` action 语法**（第 2/3 层入口）：
```yaml
- action: module
  uses: lineinfile          # 解析顺序：内置 > modules/<uses>.yaml > 外部 module
  with: { path: /etc/hosts, line: "1.1.1.1 x" }
```
数据定义 module 示例 `modules/lineinfile.yaml`：
```yaml
params: [path, line]
check: "grep -qF {{line}} {{path}}"     # 命中→ok（跳过）
act:   "printf '%s\n' {{line}} >> {{path}}"
```

**落地顺序**：① 钉契约（`module` action + 数据定义 module 加载 → `Op::Shell{check,cmd}`，**本批做的地基**）；② 扩内置集（B3）；③ 外部 module JSON 协议（待生态需求，agent 已具送达能力）。

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

> **现状映射**：已实现 `apply`/`build`/`save`/`load`/`tag`/`images`/`pull`/`push`/`ai`/`doctor`/`run`/`cp`/`create`/`agent`；`bundle` = `build`+`save`(B 类 artifact)；`inspect`/`verify`/`rollback`/`remove` 仍为路线。

### 7.2 `<source>` vs `<release>`：两种第一类对象
| 概念 | 是什么 | 谁吃它 |
|---|---|---|
| **`<source>`** | task 的来源：命名 task（`yq`→`tasks/yq.yaml`）/ `./x.yaml` task 文件 / 镜像 `docker.io/…` / `./x.oci` artifact | plan / apply / bundle / inspect / verify |
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
| **bundle 时** | 依赖分析 | 分析 yaml 里声明的 `materials`/`images`，**推断隐性依赖**（内核参数、系统包、缺失镜像），建议补进包 | AI2 |
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
