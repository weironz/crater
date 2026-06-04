# 模块参考(docs/modules/)

task 的每个动作(`action:`)调用一个**模块**——引擎内置的最小操作原语。本目录**每个模块一篇**,
是字段级参考;总览/设计原则(为何对齐 Ansible、改名史)见 [features/modules.md](../features/modules.md)。

> **维护约定:模块有任何变动(字段增删、语义/幂等探针变化),同一提交里更新对应文档。**
> 什么时候配新建模块:见 [module-charter](../module-charter.md)(准入四条 + 晋升路径)。

## 索引

| 模块 | 一句话 | Ansible 对应 |
|---|---|---|
| [shell](shell.md) | 经 shell 跑命令,`check` 探针幂等 | `shell` |
| [copy](copy.md) | 放文件到目标:content/src/material 三选一(D-090) | `copy`(+`get_url`) |
| [template](template.md) | minijinja 渲染模板物料写到目标 | `template` |
| [file](file.md) | 路径状态:建目录/删/touch | `file` |
| [package](package.md) | 装系统包,离线走 os_package 物料闭包 | `package`/`apt`/`yum` |
| [unarchive](unarchive.md) | 解压:物料直取或目标机已有文件 | `unarchive` |
| [service](service.md) | systemd 服务:enable/start/stop/restart | `service`/`systemd` |
| [lineinfile](lineinfile.md) | 确保某行存在/不存在 | `lineinfile` |
| [user](user.md) | 系统用户存在/不存在 | `user` |
| [group](group.md) | 系统组存在/不存在 | `group` |
| [docker_container](docker_container.md) | 容器以期望参数在跑(指纹收敛) | community.docker 精简版 |
| [load_image](load_image.md) | 导入容器镜像物料(在线 pull / 离线 import) | (无内置) |
| [role](role.md) | 调用可复用角色 | `include_role` |

## 通用字段(任何模块都能带)

```yaml
- id: <步骤名>            # 可选;被 needs/notify 引用时才需要,默认 action<i>
  action: <模块名>
  # ...模块参数...
  needs: [<id>...]        # 排序依赖(引擎拓扑排序,不靠 YAML 顺序)
  notify: [<handler id>]  # 本步 changed 时触发 handler
  when_os: [debian|rhel]  # 按 OS 族过滤
  when_role: [<role>...]  # 按 inventory 角色过滤(D-071)
  when_offline: true|false # 仅离线/仅在线跑
  run_once: true          # 只在首个匹配 when_role 的目标跑(D-077)
  throttle: 1             # 本步跨主机并发上限(D-077)
  retries: 2              # 失败重试次数
  ignore_errors: true     # 重试后仍失败只 warn 不中断
  phase: install|verify|preflight  # 默认 install
```

条件是**封闭枚举**(when_os/when_role/when_offline),不是自由表达式——逻辑在 Rust 引擎,
YAML 只有数据(D-036)。
