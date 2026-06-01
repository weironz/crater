# 内置模块(modules)—— 与 Ansible 对齐(D-067)

## 这是什么 / 解决什么

task 的每个动作(`action:`)调用一个**模块(module)**——和 Ansible 一样的叫法。模块是
引擎内置的最小操作原语(跑命令、装包、写文件、起服务…)。

设计原则:**能和 Ansible 对齐的模块,就用 Ansible 的模块名**,让已经会 Ansible 的运维零
学习成本迁移过来;只有 crater 特有(离线/物料模型)的能力才自造名字,并在文档里诚实标注。

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
| `shell` | `cmd`, `check` | 经 shell 跑命令(管道/`&&`/重定向/环境变量前缀都支持)。`check` 是幂等探针:退出 0 则跳过(报 `ok`) | `shell`(`command` 也收作别名) |
| `package` | `packages: {debian:[..], rhel:[..]}`, `material` | 装系统包。在线走系统源;离线设 `material` 指向 `kind: os_package` 物料(D-062) | `package`/`apt`/`yum` |
| `unarchive` | `to`, `from`, `strip`, `creates` | 解压 tar/tgz 到 `to`。`creates` 已存在则跳过(幂等) | `unarchive` |
| `template` | `src`, `dst` | 渲染 `templates/<src>`({{var}} 替换)写到 `dst` | `template` |
| `copy` | `src`, `dest`, `mode` | 把控制端文件拷到目标(内容内联进 plan,sha256 幂等) | `copy`(src=) |
| `file` | `path`, `state`(directory/absent/touch), `mode`, `owner`, `group` | 管路径状态:建目录 / 删 / touch | `file` |
| `service` | `name`, `state`(started/stopped/restarted), `enabled` | 管 systemd 服务(自带 daemon-reload + enable + start,is-active 幂等) | `service`/`systemd` |
| `lineinfile` | `path`, `line`, `regexp`, `state`, `create` | 确保某行存在/不存在(grep 探针幂等) | `lineinfile` |
| `user` | `name`, `state`, `system`, `shell`, `home`, `groups` | 确保系统用户存在/不存在(`id` 探针) | `user` |
| `group` | `name`, `state`, `system` | 确保系统组存在/不存在(`getent` 探针) | `group` |

`shell` 等于 Ansible 的 `shell`,**不是 `command`**:crater 的命令默认经过 shell,管道、
`&&`、`2>/dev/null`、`KUBECONFIG=... cmd` 这些都能用。`command` 收作别名(行为同 `shell`)。

## 二、crater 自有的模块(离线 / 物料模型)

这些没有 Ansible 1:1 对应——是 crater「一份描述,在线/离线通吃」模型的产物。

| crater 模块 | 参数 | 作用 | 为什么没对齐 |
|---|---|---|---|
| `place` | `material`, `dest`, `mode` | 放置一个 `materials:` 声明的物料:在线目标机自己下载 `url_tmpl`,离线控制端推打进 OCI 的 blob(D-034) | Ansible 把"下载"`get_url` 与"拷贝"`copy` 分开;crater 用物料逻辑名统一,在线/离线由后端定 |
| `load_image` | `material`, `namespace`, `runtime` | 导入 `kind: image` 物料:离线推 oci-archive 并 `ctr import`,在线运行时 pull(D-061) | Ansible 无内置镜像导入(社区模块) |
| `write_file` | `dst`, `content` | 写内联内容文件(渲染 {{var}}) | ≈ Ansible `copy` 的 `content=`;crater 拆成独立模块 |
| `systemd_unit` | `name`, `enable`, `start` | 轻量地 enable/start 一个已存在的 unit | 多数场景用更全的 `service` 即可 |

## 别名对照(旧 crater 名 → 现规范名)

| 旧名(仍可用) | 现规范名 |
|---|---|
| `run_cmd` / `command` | `shell` |
| `pkg_install` | `package` |
| `extract` | `unarchive` |
| `render_template` | `template` |
| `module`(调用角色) | `role`(见 [roles.md](roles.md)) |

旧名全部保留为 serde 别名,既有 task 零改动;新 task / `crater ai` 生成的用规范名。

## demo / 验证

```bash
# 任一 task 用规范名书写,build/apply 行为与旧名完全一致(纯改名)
crater build -f tasks/k8s-offline.yaml   # 解析 shell/package/unarchive… 正常
cargo test -p crater-core action_names_align_with_ansible_and_keep_aliases
# → 11 个名字(规范名+别名)全部解析到正确变体
```

## 关联

- ADR:[D-067](../decisions.md)(模块名对齐 Ansible)、[D-029](../decisions.md)(模块四层模型/角色)、[D-036](../decisions.md)(YAML 纯数据,逻辑在 Rust)。
- 相关:[roles.md](roles.md)(可复用角色)、[action-tasks.md](action-tasks.md)、[materials-and-place.md](materials-and-place.md)。
