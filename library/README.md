# crater 模板/示例库

把可复用的 task / project / role 示例集中在这里。`crater apply <名>` 会在 `library/`
下递归找 `<名>.yaml`(命名快捷),也可 `crater apply -f library/<路径>.yaml`。

## 目录
| 目录 | 内容 |
|------|------|
| `apps/`     | 单体应用/中间件(无本地文件依赖):`yq` `docker` `mysql` `zot` |
| `k8s/`      | Kubernetes 系列(共享 `files/` + `templates/`):`k8s-ha`(HA 多主)、`k8s-offline`(离线)、`k8s-online`(在线单节点);`inventory.example.yaml` 示范清单 |
| `projects/` | project 编排示例(= playbook):`demo-platform`(yq → k8s 两 play) |
| `demos/`    | 引擎特性演示:`cross-node`(跨机 register/hostvars)、`group`、`hostfilter`、`lug`、`fcs`、`d037b`、`demo-roles` |

## 用法速查
```bash
crater inspect library/k8s/k8s-ha.yaml                 # 看契约:参数/所需角色/materials
crater inspect library/k8s/k8s-ha.yaml --gen-inventory > my-inv.yaml
crater apply k8s-ha -i my-inv.yaml                     # 在线部署(命名快捷,自动找 library/)
crater build  -f library/k8s/k8s-ha.yaml               # 打离线 OCI(→ 本地库)
crater apply  -f library/projects/demo-platform.yaml -i my-inv.yaml   # 跑 project
crater apply  -f library/apps/yq.yaml                  # 自包含单文件(用内嵌 inventory)
```

## 仓库根的两个约定(不在 library/ 内)
- `roles/` —— 可复用 role(`action: role uses: X` 解析 `./roles/X.yaml` 或 `./roles/X/role.yaml`)。
- `inventory.yaml` —— 你的真机清单(含明文密码,已 gitignore)。库内 `*.example.yaml` 用占位密码。
