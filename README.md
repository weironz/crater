<div align="center">

# 🛸 crater

**Deploy anything, anywhere — even air-gapped.**

纯 Rust · 单二进制 · 零运行时依赖的声明式远程执行引擎
在线与离线同一套 YAML,整套环境一个文件交付

[![Release](https://img.shields.io/github/v/release/weironz/crater?color=ea580c&label=release)](https://github.com/weironz/crater/releases)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](#license)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg?logo=rust)](https://www.rust-lang.org)
[![Platform](https://img.shields.io/badge/platform-linux%20x86__64%20%7C%20aarch64-lightgrey.svg)](https://github.com/weironz/crater/releases)

[安装](#-安装) · [快速开始](#-快速开始) · [离线交付](#-离线交付整套环境一个文件) · [功能文档](docs/features/README.md) · [模块参考](docs/modules/README.md) · [设计决策](docs/decisions.md)

</div>

---

```console
$ crater apply rustfs --host 10.0.0.5 --password ***
[1/5] dir /data/rustfs                              → changed
[2/5] preflight: docker ready                       → ok
[3/5] load image (blob) rustfs:1.0.0-beta.5         → changed
[4/5] container rustfs <- rustfs:1.0.0-beta.5       → changed
[5/5] verify: rustfs api /health -> 200             → ok
done: changed=3 ok=2 warn=0

$ crater apply rustfs --host 10.0.0.5 --password ***   # 再跑一次 → 幂等
done: changed=0 ok=5 warn=0
```

## 为什么是 crater

面向**弱网 / 离线 / 气隙 / 信创**环境的整套交付,Ansible 的心智、terraform 的预演、docker 的制品分发——压进一个 19MB 的静态二进制:

|   | crater | Ansible | 备注 |
|---|---|---|---|
| 运行时依赖 | **无**(musl 静态) | Python + 模块库 | 两端零安装,SSH 即可 |
| 离线交付 | **一等公民**(OCI 制品) | 自己想办法 | build → 一个文件 → 气隙部署 |
| YAML | **纯数据**(逻辑全在引擎) | Jinja2 图灵完备 | 可静态分析,模板写 `if/for` 直接报错 |
| 执行模型 | 自举 agent(本地执行) | SSH 逐任务往返 | 多节点 7min → 17s 的差距来源 |
| 变更预演 | `crater plan`(连真机探针) | check mode | terraform 习惯 |

## ✨ 特性

- 📦 **离线一包带走** — `build → save → apply`:task 打成 OCI 制品(recipe + 物料,内容寻址);**project 把整套环境(基线→docker→k8s→存储)装进一个 `.oci` 文件**,跨 task 共享 blob 自动去重
- 🚀 **自举 agent** — 把自己(musl 静态,不挑 glibc)推到目标机本地执行,二进制按 sha256 缓存、离线物料按内容寻址 staging,重复部署零传输
- 🔍 **`crater plan`** — terraform 式变更预演:连真机只跑只读探针,报告 `✓ ok / ~ would-change / ? unknown`,什么都不执行
- 🏪 **registry 闭包分发** — project `push/pull` 连同全部 task 制品走私有仓库(zot/Harbor),`apply <ref>` 直连编排
- ⚡ **构建缓存** — 源未变整体跳过(指纹);物料下载按声明 sha256/URL 寻址;重复 build 秒级
- 🧩 **声明式模块** — `copy` `template` `service` `package` `docker_container` 等 13 个内置模块,幂等回显 ansible 式 `ok/changed`;复杂交付用 role 复用,准入有[章程](docs/module-charter.md)
- 🌐 **多节点编排** — `when_role` / `register`+`hostvars` 跨节点传 fact / 组内并发组间串行 / `throttle`,真机验证过 3-master HA k8s
- 🖥️ **Web 看板** — `crater ui`:部署状态/漂移检测,verify / plan / heal / delete 后台任务流 + 日志面板,token 鉴权,htmx 内嵌**离线可用**
- 🔐 **安全默认** — SSH host key 校验(TOFU 钉 `~/.crater/known_hosts`)、build/apply 参数分治 gate(冻结闭包不可被 apply 篡改)
- 🤖 **AI 副驾不司机** — `crater ai "大白话"` 生成 task,引擎确定性校验;`crater doctor` 离线规则诊断

## 📥 安装

**一行装好**(musl 静态,任何 Linux 直接跑,默认装到 `~/.local/bin`,不要 sudo):

```bash
curl -fsSL https://raw.githubusercontent.com/weironz/crater/main/scripts/install.sh | sh
```

脚本会核对 `SHA256SUMS` 再装,**没有跳过校验的开关**。装完用 `crater update`
升级。装到系统目录、钉版本、手动装、以及 Windows/macOS 现状,见
**[安装文档](docs/install.md)**。

| 资产 | 架构 | 说明 |
|---|---|---|
| `crater-x86_64-unknown-linux-musl.tar.gz` | x86_64 | 真机久经验证 |
| `crater-aarch64-unknown-linux-musl.tar.gz` | ARM64(鲲鹏/飞腾/Graviton/树莓派) | qemu 冒烟通过,ARM 真机验证欢迎反馈 |

**从源码构建**:

```bash
git clone https://github.com/weironz/crater && cd crater
cargo build --release            # → target/release/crater
scripts/build-musl.sh all        # 双架构 musl 静态 → dist/
```

## 🚀 快速开始

**从仓库装一个包**(helm 那种用法):

```bash
crater repo add lab https://example.com/index.yaml   # 一次性:订阅一个索引
crater search yq                                     # 有什么
crater apply yq -i inventory.yaml                    # 拉下来 → 印计划 → 收敛
```

装过之后机群与参数都记在 `yq.app.yaml` 里,后面就是一个词:

```bash
crater apply yq        # 再收敛一次(幂等,已经对的不动)
crater plan yq         # 先看会变什么,零执行
crater verify yq       # 对账:还是我们部署的样子吗
crater destroy yq      # 退役(默认只预览,--yes 才动手)
```

**直接给蓝图文件**也一样:

```bash
crater apply -f yq.blueprint.yaml -i inventory.yaml         # 机群(各自凭据)
crater apply -f yq.blueprint.yaml --host 10.0.0.5,10.0.0.6  # 少量机器(共用凭据)
crater apply -f yq.blueprint.yaml --dry-run                 # 不连机器,只打印计划
```

蓝图长这样——**纯数据**,没有模板逻辑:

```yaml
name: yq
version: "1"
description: yq 命令行 YAML 处理器

params:
  # stage: build —— 这个参数在**烤闭包**时就要定死(它决定下载哪个 URL),
  # 不是部署时才问
  version: { default: "4.44.3", stage: build, desc: "上游 release tag" }

materials:
  # ${substrate.arch} 是**目标事实**:构建期没有机器可探,所以离线打包时
  # 要 `--for arch=amd64` 告诉它烤哪个变体
  - name: yq-bin
    file: "https://github.com/mikefarah/yq/releases/download/v${params.version}/yq_linux_${substrate.arch}"

resources:
  - copy: { material: yq-bin, dest: /usr/local/bin/yq, mode: "0755" }

health:
  - cmd: { run: "yq --version" }
```

`crater types` 列出全部 26 种资源类型及其字段;`crater lint <蓝图>` 不连机器就能查出类型名/字段名拼错、CEL 变量越界、物料没声明这一类错。

## 📦 离线交付:整套环境一个文件

```yaml
# demo-stack.yaml —— project:有序编排多个 task
name: demo-stack
plays:
  - { name: 装 yq,       source: yq,     hosts: all }
  - { name: 部署 rustfs, source: rustfs, hosts: all }
```

```bash
# 构建机(在线)
crater build -f demo-stack.yaml            # 逐 play 构建,play source 锁定为制品 ref
crater save crater/demo-stack:latest -o demo-stack.oci   # 项目 + 全部 task 闭包 → 一个文件

# 离线现场(零联网)
crater apply demo-stack.oci -i inventory.yaml            # 按 play 顺序 recipe-replay
crater delete demo-stack.oci -i inventory.yaml           # 逆序卸载

# 或者走私有 registry(zot / Harbor)
crater tag crater/demo-stack:latest reg:5000/demo-stack:1
crater push reg:5000/demo-stack:1          # 闭包 push:task 制品一起走
crater pull reg:5000/demo-stack:1          # 另一台控制机:闭包 pull
crater apply reg:5000/demo-stack:1 --offline -i inventory.yaml
```

## 🧰 命令总览

| 类别 | 命令 |
|---|---|
| 部署 | `apply` `plan` `delete`(source:task 文件 / 裸名 / project / `.oci` / 镜像 ref) |
| 离线打包 | `build`(`--set k=v` 覆盖、源指纹缓存、`--no-cache`)· `save` · `load` |
| 制品库 | `images` `pull` `push` `tag` `rmi` `gc` `inspect` `registry login` |
| 状态 | `task list/show/history`(`--verify` 漂移检测)· `ui`(看板,`--token` 鉴权) |
| 工具 | `run`(临时命令)· `cp`(推文件)· `doctor`(离线诊断)· `ai` · `create inventory` |

## 🏗️ 工程结构

```
crater/
├── crates/
│   ├── crater-core/   # 引擎:task / engine(模块 lowering)/ executor(SSH)/ bundle / store
│   └── crater-cli/    # `crater` 二进制(同一个二进制也是目标机上的 agent)
├── library/           # 交付库:每个子目录一个自闭环交付(yq/ docker/ rustfs/ zot/ k8s/ …)
├── docs/
│   ├── features/      # 功能文档(每功能一篇:介绍 + demo + 真机验证)
│   ├── modules/       # 模块参考(每模块一篇)
│   ├── decisions.md   # 架构决策记录(ADR,D-001 起持续追加)
│   └── module-charter.md  # 模块准入章程(shell → role → 模块的晋升路径)
└── scripts/build-musl.sh  # 静态二进制构建(x86_64 / aarch64 / all)
```

设计北极星见 [docs/design.md](docs/design.md):**YAML 是数据、逻辑在引擎**(D-036)、**引擎零产品知识**(D-017)——加一个可部署对象 = 写一个 task,绝不改 Rust。

## License

[Apache-2.0](LICENSE)
