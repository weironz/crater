# crater 模板/示例库

每个子目录 = **一个自闭环交付包**(对齐 Ansible 的 playbook 仓库 + role 目录)。
`crater apply <名>` 在 `library/` 下递归找入口 `<名>.yaml`。

## 标准交付目录(见 `_template/`)
```
library/<交付>/
├── README.md
├── <交付>.yaml             # 入口:project(多 play)或 task(单体)= Ansible site.yml
├── inventory.example.yaml  # 示范清单(占位密码)= Ansible inventories/sample
└── roles/<role>/           # 本交付私有 role = Ansible role 目录
    ├── role.yaml           # params + materials + actions + handlers(crater 紧凑合一)
    ├── files/              # role 私有静态文件(src: 相对本 role 目录)
    └── templates/          # role 私有 .j2(minijinja)
```
唯一比 Ansible 多的:`role.yaml` 里的 `materials:`(离线闭包,烤进 OCI)—— Ansible 没有的概念。

## 现有交付
| 目录 | 内容 |
|------|------|
| `yq/` `docker/` `mysql/` `zot/` | 单体应用/中间件 |
| `k8s/` | Kubernetes:`k8s-ha`/`k8s-offline`/`k8s-online` 部署 + `k8s-upgrade`(滚动升级 project,用交付内 `roles/kube-upgrade`)+ 共享 files/templates + inventory.example |
| `_template/` | 标准交付骨架(复制起新交付)|
| `_examples/` | 非交付:跨交付编排(demo-platform)+ 引擎特性 demo |

## 仓库根
- `roles/`(根)— 全局共享 role(`action: role` 先找交付内 `roles/`,回退根 `./roles`)。
- `inventory.yaml`(根,gitignored)— 你的真机清单。
