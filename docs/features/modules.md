# 内置模块(modules)—— 与 Ansible 对齐(D-067)

## 这是什么 / 解决什么

task 的每个动作(`action:`)调用一个**模块(module)**——和 Ansible 一样的叫法。模块是引擎内置的最小操作原语(跑命令、装包、写文件、起服务…)。

设计原则:**能和 Ansible 对齐的模块,就用 Ansible 的模块名**,让已经会 Ansible 的运维零学习成本迁移过来;只有 crater 特有(离线/物料模型)的能力才自造名字,并在文档里诚实标注。

> 术语:Ansible 把任务原语叫 **module**、把可复用的参数化子程序叫 **role**。crater 与此
> 对齐——原语 = 模块(本文),可复用子程序 = 角色([roles.md](roles.md))。

## 通用字段(任何模块都能带)

```yaml
- id: <步骤名>            # 可选;被 needs 引用时才需要
  action: <模块名>         # 见下
  <模块参数...>
  needs: [<id>...]        # 排序依赖(引擎拓扑排序)
  notify: [<handler id>]  # 变更时触发 handler
  when_os: [debian|rhel]  # 可选,按 OS 族分支
  phase: install|verify   # 可选,默认 install
```

## 一、与 Ansible 对齐的模块

| crater 模块 | 参数 | 作用 | Ansible 对应 |
|---|---|---|---|
| `shell` | `cmd`, `check` | 经 shell 跑命令(管道/`&&`/重定向/环境变量前缀都支持)。`check` 是幂等探针:退出 0 则跳过(报 `ok`) | `shell` |
| `package` | `packages: {debian:[..], rhel:[..]}`, `material` | 装系统包。在线走系统源;离线设 `material` 指向 `kind: os_package` 物料(D-062) | `package`/`apt`/`yum` |
| `unarchive` | `to`, `from`, `strip`, `creates` | 解压 tar/tgz 到 `to`。`creates` 已存在则跳过(幂等) | `unarchive` |
| `template` | `material`, `dst` | 用 **minijinja** 渲染模板物料(`kind: file` 的 `.j2`,打进 OCI 离线自洽)写到 `dst`。支持 `{% for %}`/`{{ }}`;上下文含标量 var + 结构化 `groups.<role>`=`[{name,ip}]`(D-075) | `template` |
| `copy` | `dest`, `content` / `src` / `material` 三选一, `mode` | 写文件到目标:`content` 内联(渲染 {{var}});`src` 拷控制端文本文件(内联进 plan);`material` 引用 `materials:` 物料(二进制安全、arch 变体,在线目标机下载 `url_tmpl`、离线推 OCI blob,D-034/D-090)。sha256 幂等 | Ansible 把 `get_url` 与 `copy` 分开;crater 用一个 `copy` 统一,来源由字段定 |
| `file` | `path`, `state`(directory/absent/touch), `mode`, `owner`, `group` | 管路径状态:建目录 / 删 / touch | `file` |
| `service` | `name`, `state`(started/stopped/restarted), `enabled` | 管 systemd 服务(自带 daemon-reload + enable + start/stop/restart,is-active/is-enabled 幂等) | `service`/`systemd` |
| `lineinfile` | `path`, `line`, `regexp`, `state`, `create` | 确保某行存在/不存在(grep 探针幂等) | `lineinfile` |
| `user` | `name`, `state`, `system`, `shell`, `home`, `groups` | 确保系统用户存在/不存在(`id` 探针) | `user` |
| `group` | `name`, `state`, `system` | 确保系统组存在/不存在(`getent` 探针) | `group` |

`shell` 对齐 Ansible 的 `shell` 而**不是 `command`**:crater 的命令默认经过 shell,管道、
`&&`、`2>/dev/null`、`KUBECONFIG=... cmd` 这些都能用(Ansible 的 `command` 不经 shell、不支持这些)。

## 二、crater 自有的模块(离线 / 物料模型)

这些没有 Ansible 1:1 对应——是 crater「一份描述,在线/离线通吃」模型的产物。

| crater 模块 | 参数 | 作用 | 为什么没对齐 |
|---|---|---|---|
| `load_image` | `material`, `namespace`, `runtime` | 导入 `kind: image` 物料:离线推 oci-archive 并 `ctr import`,在线运行时 pull(D-061) | Ansible 无内置镜像导入(社区模块) |

## 改名对照(旧名已废弃,不再解析)

D-070 起**彻底改名、不留别名**——下表旧名写进 task 会直接报错(`unknown variant`)。

| 旧名(已废弃) | 现名 |
|---|---|
| `run_cmd` / `command` | `shell` |
| `pkg_install` | `package` |
| `extract` | `unarchive` |
| `render_template` | `template` |
| `write_file`(`dst`+`content`) | `copy`(`dest`+`content`) |
| `systemd_unit`(`enable`/`start`) | `service`(`enabled`/`state: started`) |
| `module`(调用角色) | `role`(见 [roles.md](roles.md));`modules/` 目录 → `roles/` |
| `kind: binary`(material) | `kind: file` |

## demo / 验证

```bash
crater build -f tasks/k8s-offline.yaml   # 解析 shell/package/unarchive… 正常
cargo test -p crater-core action_names_are_ansible_module_names_only
# → 现名全部解析正确,旧名一律报错
```

## 关联

- ADR:[D-067](../decisions.md)(模块名对齐 Ansible)、[D-029](../decisions.md)(模块四层模型/角色)、[D-036](../decisions.md)(YAML 纯数据,逻辑在 Rust)。
- 相关:[roles.md](roles.md)(可复用角色)、[action-tasks.md](action-tasks.md)、[materials.md](materials.md)。
