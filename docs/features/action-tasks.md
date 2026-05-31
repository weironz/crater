# 通用 task:`crater apply <动作>`(D-037)

> ADR: [D-037](../decisions.md) ｜ 设计: [action-layer.md](../action-layer.md) ｜ 铁律: [D-036](../decisions.md)

## 这是什么

把 `crater apply` 从"装某产品"升级为"**在目标机达成一组状态**"——一个 **task** 描述要做的事(装软件、改配置、发文件、起服务、跑巡检……任意操作),crater 是通用声明式远程执行引擎(Ansible/kubectl 心智)。

**严守 D-036**:task 是纯数据。控制流——条件、排序、循环——**全部在 Rust 引擎**,YAML 只声明"用哪个原语 + 参数 + 取哪个值 + 依赖谁"。

## 命令形态(方案 A:后缀自识别)

```
crater apply yq                       # 裸名(无后缀、无 / :)→ 命名 task / 组件
crater apply install-yq.yaml          # .yaml/.yml 后缀 → 文件(task 或 spec,引擎自辨)
crater apply -f ./x.yaml              # -f 显式文件
crater apply docker.io/lib/app:v1     # 含 / 或 : → 镜像
crater apply yq.tar                   # .tar/.oci → 离线包
```

目标(沿用 [D-035](../decisions.md) 三层):无 → 本机;`--host a,b` → 少量共用凭据;`-i inventory.yaml` → 大量各自凭据。

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
`id`、`action`(原语)+ 参数、`needs`(依赖)、`phase`(install 默认/verify/preflight)、`when_os`(封闭枚举)、`when_offline`(布尔)、`retries`/`ignore_errors`(数据,运行时见 D-037-b)。

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

## demo(真机验证 2026-06-01)

```bash
crater apply examples/install-yq.yaml                                   # 层1 本机
crater apply examples/install-yq.yaml --host 192.168.73.11 --password x # 层2 agent
crater apply examples/install-yq.yaml -i inventory.yaml                 # 层3 inventory
```

- 三层全部装好 yq v4.53.2(本机直跑 / `--host` 经自举 agent / `-i` 两台并行)。
- 引擎条件过滤生效:声明 3 个 action,目标非 rhel → `rhel_only` 被滤,plan 实际 2 步(`[Install] place` → `[Verify] verify`)。
- `needs` 排序、`{{version}}` 取值正确。

## 边界 / 后续(D-037-b)

- 本期:`actions` + `needs` + `phase` + `when_os/when_offline` + materials + 三层 targeting。
- `retries`/`ignore_errors` 字段已解析,**运行时行为**后续。
- `hosts` 本期支持 `all`;**组过滤**后续。
- 原语已补 **file/copy/service**(D-039)、**lineinfile/user/group**(D-040)。
- handlers/notify、register/hostvars 在 task 模型下、`retries/ignore_errors` 运行时、`hosts` 组过滤后续。
- 命名 task 库(裸名 `crater apply <task>` 解析新 actions 格式)后续;现阶段裸名仍解析 `components/`(旧格式兼容)。

## 关联

ADR [D-037](../decisions.md)(形态+分期)、[D-036](../decisions.md)(YAML 不写逻辑)、[D-035](../decisions.md)(三层 targeting)、[D-034](../decisions.md)(materials/place)。
