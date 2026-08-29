# crater 蓝图库

每个子目录 = 一个自闭环交付包。**新 DSL(blueprint)是正典**;标 Legacy 的目录
是旧 task 管线遗存,等旧管线退役时一并清理。

| 目录 | 格式 | 内容 |
|---|---|---|
| `k8s/` | blueprint | HA Kubernetes(单节点/1主N从/多主多从同一份),真机验证记录见 VALIDATION.md |
| `middleware/` | blueprint + stack | 六场景中间件(基线/postgres 主从/redis 哨兵/nginx/观测)+ platform 栈 |
| `rustfs/` | blueprint | S3 兼容对象存储,单机/分布式同一份蓝图(平台栈引用它做存储层) |
| `docker/` | blueprint | Docker + containerd,官方静态二进制 |
| `mysql/` | blueprint | MySQL(发行版包) |
| `zot/` | blueprint | OCI 镜像仓库 |
| `yq/` | blueprint | 单二进制工具下发 —— 最小蓝图示例 |
| `selftest/` | blueprint | 机群机制自检(选择器/exports/throttle/并发),附判据表 |
| `_examples/` `_template/` | **Legacy** | 旧 task 管线示例与模板,勿新增 |

常用:

```bash
crater lint library/                 # 全库静态检查
crater inspect <蓝图>                # 看输入契约:必填参数/需要的机群/可跳的舞
crater plan  <蓝图> -i inv.yaml      # 零写入预演
crater apply <蓝图> -i inv.yaml      # 收敛
crater build -f <蓝图或栈> -o x.tar  # 烤离线闭包
```
