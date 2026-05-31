# 通用 task:`crater apply <动作>`(D-037)

> ADR: [D-037](../decisions.md) ｜ 设计: [action-layer.md](../action-layer.md) ｜ 铁律: [D-036](../decisions.md)

## 这是什么

把 `crater apply` 从"装某产品"升级为"**在目标机达成一组状态**"——一个 **task** 描述要做的事(装软件、改配置、发文件、起服务、跑巡检……任意操作),crater 是通用声明式远程执行引擎(Ansible/kubectl 心智)。

**严守 D-036**:task 是纯数据。控制流——条件、排序、循环——**全部在 Rust 引擎**,YAML 只声明"用哪个原语 + 参数 + 取哪个值 + 依赖谁"。

## 命令形态(方案 A:后缀自识别)

```
crater apply yq                       # 裸名 → 命名 task tasks/yq.yaml(无则回退组件)
crater apply install-yq.yaml          # .yaml/.yml 后缀 → 文件(task 或 spec,引擎自辨)
crater apply -f ./x.yaml              # -f 显式文件
crater apply docker.io/lib/app:v1     # 含 / 或 : → 镜像
crater apply yq.tar                   # .tar/.oci → 离线包
```

目标(沿用 [D-035](../decisions.md) 三层):无 → 本机;`--host a,b` → 少量共用凭据;`-i inventory.yaml` → 大量各自凭据。

**命名 task 库**(D-043):`crater apply <name>` 裸名解析 `tasks/<name>.yaml`(actions 格式);不存在则回退组件 `components/<name>`。

**inventory 嵌套 `groups:`**(D-043):task 的 `hosts: <group>` 可指向 inventory 里的命名组,组成员是 role 名或其它组名(可嵌套),引擎递归展开为 role 集合后过滤主机:

```yaml
inventory:
  hosts:
    - { name: n1, address: .., roles: [control] }
    - { name: n2, address: .., roles: [worker] }
  groups:
    cluster: [control, worker]      # hosts: cluster → control ∪ worker
```

## task YAML 结构

```yaml
name: install-yq
hosts: all                     # targeting:组名/all(声明式,引擎解析)
vars:
  version: "4.53.2"
materials:                     # 物料闭包(D-034),crater build 据此打离线包
  - { name: yq-bin, kind: binary, url_tmpl: ".../v{{version}}/yq_linux_amd64" }
actions:
  - id: place_yq
    action: place
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"
  - id: verify
    action: run_cmd
    phase: verify
    cmd: "/usr/local/bin/yq --version"
    needs: [place_yq]          # 引擎拓扑排序,排在 place_yq 后
  - id: rhel_only
    action: run_cmd
    when_os: [rhel]            # 封闭枚举开关,非表达式;非 rhel 时引擎过滤掉这步
    cmd: "echo rhel"
```

action 项字段(全是引擎读得懂的封闭词汇,**无需执行即可静态分析**):
`id`、`action`(原语)+ 参数、`needs`(依赖)、`phase`(install 默认/verify/preflight)、`when_os`(封闭枚举)、`when_offline`(布尔)、`retries`(失败重试次数)、`ignore_errors`(失败转 warn 不中断)、`notify`(变更时触发的 handler id)。

## 重试 / 容错 / handlers / 组过滤(D-042)

- **`retries: N`**:该 step 失败时重试至多 N 次,仍失败才算失败。
- **`ignore_errors: true`**:失败(含重试用尽)转 `warn`,不中断后续。
- **`notify: [hid]`** + 顶层 **`handlers: [...]`**:step 报 `changed` 时排入对应 handler;所有 actions 跑完后,被触发的 handler **去重、按 notify 顺序执行一次**(ansible 语义)。step 为 `ok`(幂等命中)则不触发。
- **`hosts: <group>`**:只在 `roles` 含该组名的主机跑(`all` = 全部;CLI `--host`/本机给的无 roles 主机视为已选,总匹配)。
- **task 默认走自举 agent**(D-044,与 component 一致):控制端渲染+lower 出 task plan(steps+policy+handlers)推到目标,目标 `crater agent --task-plan` 本地跑 `execute_task`(retries/notify/handlers 都在目标内执行,输出转发回控制端)。`--shell`/本机则走控制端逐 step(agentless 逃生)。register/hostvars 仍由控制端组间串行采集。

## 控制流在引擎(D-036 落地)

| 能力 | YAML 怎么写 | 引擎怎么做 |
|---|---|---|
| 排序 | `needs: [id]` | 拓扑排序(复用 dag) |
| 条件 | `when_os`/`when_offline`(封闭枚举) | 过滤:不满足的步骤**根本不进 plan** |
| 取值 | `{{ version }}` | 残废渲染器纯代入,写表达式直接报错(D-036/#1) |
| 循环 | (不在 YAML) | action 收列表参数 / targeting 组内逐台 |

## 内置 action 原语(Rust 白盒)

`pkg_install` `download` `extract` `render_template` `write_file` `systemd_unit`
`run_cmd` `place` `load_image` `module`,以及 D-037-b 补齐的:

| 原语 | 参数 | 引擎语义(幂等) |
|---|---|---|
| `file` | `path` + `state: directory\|absent\|touch` + `mode/owner/group` | `mkdir -p`/`rm -rf`/`touch`;探针 `test -d`/`test ! -e`/`test -e` |
| `copy` | `src`(控制端,相对 task 目录) + `dest` + `mode` | 读控制端文件**内联进 plan**(agent 也能写),sha256 幂等 + chmod;文本 only(二进制走 `place`) |
| `service` | `name` + `state: started\|stopped\|restarted` + `enabled` | systemd start/stop/restart + enable/disable;started/stopped 探针 `is-active` |
| `lineinfile` | `path` + `line` + `regexp?` + `state` + `create` | present 时(有 regexp)删匹配行 + append(即替换);探针 `grep -qxF` |
| `user` | `name` + `state` + `system/shell/home/groups` | `useradd`/`userdel`;探针 `id` |
| `group` | `name` + `state` + `system` | `groupadd`/`groupdel`;探针 `getent group` |

`copy` 复用增强后的 `Op::WriteFile`(加 `mode` + sha256 幂等),`render_template`/`write_file` 也因此变幂等(内容不变报 ok)。

## register / hostvars(跨 host fact,D-030 机制)

task 顶层 `register: [{name, cmd}]` 在每个 host 跑完 actions 后采集 fact。host 按
role 分组(`group_hosts_by_role`):**组间串行、组内并行**;每组完成后 fact 合入
`hostvars`,供后续组的 actions 以 `{{ hostvars.<host>.<name> }}` 取值——这是集群
形成(leader 注册 token、follower join)的钥匙。

```yaml
hosts: all
register:
  - { name: ip, cmd: "hostname -I | awk '{print $1}'" }
actions:
  - { action: run_cmd, cmd: "echo peer={{ hostvars.n11.ip }}" }   # 后续组取前组的 fact
```

排序/分组/合并全在引擎(D-036);`{{ hostvars.* }}` 只是取值,尚无值时(更早的组、
dry-run)残废渲染器原样保留、不报错。

## demo(真机验证 2026-06-01)

```bash
crater apply examples/install-yq.yaml                                   # 层1 本机
crater apply examples/install-yq.yaml --host 192.168.73.11 --password x # 层2 agent
crater apply examples/install-yq.yaml -i inventory.yaml                 # 层3 inventory
```

- 三层全部装好 yq v4.53.2(本机直跑 / `--host` 经自举 agent / `-i` 两台并行)。
- 引擎条件过滤生效:声明 3 个 action,目标非 rhel → `rhel_only` 被滤,plan 实际 2 步(`[Install] place` → `[Verify] verify`)。
- `needs` 排序、`{{version}}` 取值正确。

**真实 daemon 服务**(`crater apply docker --host <h>` → `tasks/docker.yaml`):`pkg_install` 装 docker.io + `write_file` daemon.json(CN mirror,`notify` 重启 handler)+ `service` started/enabled + verify。真机 n11:docker v29 active、mirror `docker.m.daocloud.io` 生效、cgroup=systemd;再跑 `changed=0 ok=4`、daemon 未变 → handler **不**触发。整套经自举 agent 在目标本地执行。

## 边界 / 后续(D-037-b)

- 本期:`actions` + `needs` + `phase` + `when_os/when_offline` + materials + 三层 targeting。
- `retries`/`ignore_errors` 字段已解析,**运行时行为**后续。
- `hosts` 本期支持 `all`;**组过滤**后续。
- 原语已补 **file/copy/service**(D-039)、**lineinfile/user/group**(D-040)。
- register/hostvars(D-041)、handlers/notify + `retries/ignore_errors` 运行时 + `hosts` 组过滤(D-042)均已实现。
- 命名 task 库 + 嵌套 `groups:`(D-043)、task 默认走自举 agent + `--shell` 逃生(D-044)均已实现。
- **task 模型(D-037)功能完整**:actions/needs/phase/when、materials/place、register/hostvars、retries/ignore_errors、handlers/notify、hosts 组过滤、命名 task 库、嵌套 groups、自举 agent、16 原语。
- 命名 task 库(裸名 `crater apply <task>` 解析新 actions 格式)后续;现阶段裸名仍解析 `components/`(旧格式兼容)。

## 关联

ADR [D-037](../decisions.md)(形态+分期)、[D-036](../decisions.md)(YAML 不写逻辑)、[D-035](../decisions.md)(三层 targeting)、[D-034](../decisions.md)(materials/place)。
