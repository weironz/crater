# Crater

> Deploy anything — 纯 Rust 单二进制、零运行时依赖的**声明式远程执行引擎**(Ansible 心智),
> 面向国内弱网 / 离线 / 政企环境。在线与离线同一套 task,自举 agent 默认执行。

```bash
crater apply install-yq.yaml                              # 本机
crater apply yq --host 10.0.0.5,10.0.0.6 --password <pw>  # 少量机器(共用凭据)
crater apply yq -i inventory.yaml                         # 大量机器(各自凭据)
crater apply docker.io/library/app:v1 --host 10.0.0.5     # 直接部署镜像/制品
```

## 核心理念

- **YAML 是数据,逻辑在引擎(D-036,不可妥协)**:task YAML 永远是纯声明数据——条件 / 循环 / 计算 / 排序 / 重试全在 Rust;模板渲染器"故意残废",写 `if/for`/表达式直接报错。要 Ansible 的能力,不要它把 YAML 变程序的覆辙(可静态分析是 dry-run / preflight / AI 审核的前提)。
- **引擎零产品知识(D-017)**:引擎只懂通用原语(`place`/`run_cmd`/`file`/`copy`/`service`…);"装什么"全是 task 数据。加一个可部署对象 = 写一个 task,绝不改 Rust。
- **task 模型(D-037)**:`crater apply <task>` —— 一组 action(原语 + 参数 + `needs` 依赖)在目标机要达成的状态。装软件只是其一;它能在任意机器做任意事。
- **自举 agent 默认(D-019/D-044)**:把 crater 二进制(按 sha256 缓存)+ 渲染好的 plan 推到目标,目标**本地执行**(少 SSH 往返);`--shell` 逃生到 agentless。
- **离线 = OCI artifact(D-033/D-045)**:`crater build` 把 task 打成 **B 类 OCI artifact**(recipe + 物料,内容寻址);registry `push/pull` 或文件 `save/load`;apply 时 recipe-replay。在线 / 离线同一 task。

## 命令

| 形态 | 命令 |
|---|---|
| 本机 | `crater apply <task>` |
| 少量机器(共用凭据) | `crater apply <task> --host a,b --password x`(或 `--key ~/.ssh/id_rsa`) |
| 大量机器(各自凭据) | `crater apply <task> -i inventory.yaml` |
| 命名 task | `crater apply yq`(裸名 → 在 `library/` 下递归找 `yq.yaml`) |
| 文件 / 镜像 / 离线包 | `crater apply x.yaml` / `crater apply docker.io/...` / `crater apply x.oci` |
| 离线打包 | `crater build -f task.yaml -t <ref>` → `crater save <ref> -o x.oci` |
| 镜像库 | `crater images` / `pull` / `push` / `tag` / `load` / `registry login` |
| AI 副驾 | `crater ai "<大白话>" -o task.yaml`(生成并校验 task) |
| 诊断 / 临时命令 | `crater doctor --file log` / `crater run --host H -- <cmd>` |
| 生成 inventory | `crater create inventory` |

## task 写法

```yaml
name: install-yq
hosts: all                       # 或 inventory 组名(支持嵌套 groups)
vars: { version: "4.53.2" }
materials:                       # 物料闭包(D-034);build 据此打离线包
  - { name: yq-bin, kind: binary, url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64" }
actions:
  - { id: place,  action: place,   material: yq-bin, dest: /usr/local/bin/yq, mode: "0755" }
  - { id: verify, action: run_cmd, phase: verify, cmd: "/usr/local/bin/yq --version", needs: [place] }
```

**内置原语(16)**:`pkg_install` `download` `extract` `render_template` `write_file` `systemd_unit` `run_cmd` `place` `load_image` `module` `file` `copy` `service` `lineinfile` `user` `group`。
**能力**:`needs`(排序)、`when_os`/`when_offline`(封闭枚举条件)、`retries`/`ignore_errors`、`notify`+`handlers`、`register`/`hostvars`(跨节点)、`hosts` 组过滤、嵌套 `groups`。

## 快速开始

```bash
cargo build && cargo test
crater apply yq --host 10.0.0.5 --password <pw>        # 命名 task,经自举 agent
crater apply yq --host 10.0.0.5 --password <pw> --dry-run
crater create inventory                                 # 生成示例 inventory.yaml

# 离线:打包 → 分发 → 零联网部署
crater build -f library/apps/yq.yaml -t myreg/yq:1.0    # → 本地库(B 类 artifact)
crater save myreg/yq:1.0 -o yq.oci                      # 导出文件,拷到离线机
crater apply yq.oci --host 10.0.0.5 --password <pw>     # recipe-replay
```

幂等:再跑一次只报 `ok/changed/warn`,已就绪的步骤自动跳过(`changed=0`)。

## 工程结构

```
crater/
├── crates/
│   ├── crater-core/   # 引擎:task / component(原语) / engine / executor / source / bundle / store / ai / diagnose
│   └── crater-cli/    # `crater` 二进制
├── library/           # 模板/示例库:apps/ k8s/ projects/ demos/(crater apply <name> 递归找)
├── roles/             # 可复用 role(action: role uses: X → roles/X.yaml 或 roles/X/role.yaml)
└── docs/              # 设计 / 决策 / 功能文档
```

## 文档

- [设计方向 design.md](docs/design.md) ｜ [action 层 action-layer.md](docs/action-layer.md)
- [功能文档 features/](docs/features/README.md)
- [决策记录 decisions.md](docs/decisions.md)(D-001~D-046)
- [文档索引 docs/README.md](docs/README.md)

## License

Apache-2.0
