# docker_container —— 管理目标机上的容器(指纹收敛)

community.docker.docker_container 的**刻意精简版**(D-092):字段只留高频闭集,
收敛靠引擎的 spec 指纹,不做 Ansible 的 `comparisons:` 逐项 diff。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `name` | ✔ | 容器名(也是收敛的身份键) |
| `image` | started 时必填 | 镜像引用 |
| `state` | | `started`(默认)/ `stopped` / `absent` |
| `restart_policy` | | docker `--restart`:no / on-failure / always / unless-stopped |
| `ports` | | `-p` 映射列表,`"宿主:容器"` |
| `volumes` | | `-v` 绑定列表,`"宿主:容器"` |
| `env` | | 环境变量映射 |
| `command` | | 追加在镜像后的命令 |
| `args` | | 额外裸 `run` flag 列表(逃逸口,数据不是逻辑;同样计入指纹) |
| `runtime` | | 驱动的 CLI,默认 `docker`(podman 兼容同款 flag) |

## 语义 / 幂等(指纹收敛)

- **started**:引擎把渲染后的 spec(image/ports/volumes/env/command/args/restart_policy)
  规范化后 sha256 取 12 位 → `--label crater.spec=<指纹>`。探针 = "存在 name 匹配、
  **running**、label 匹配的容器"。不符(没容器 / 崩溃重启循环 / **任何参数变了**)→
  `rm -f` + `run` 整体重建;符合 → `ok`。
  - 容器**不可变**:没有原地 update;改任何参数都是重建(数据卷在宿主机,不丢)。
  - 凭据等敏感 env **不进 label**(label 是哈希);但仍在容器 Env 里可见,与 docker 本身一致。
  - 日志 describe 带指纹:`container rustfs <- <image> (spec 0a6aa0a05e40)`,排查"为什么重建"可比对前后指纹。
- **stopped**:`docker stop`;探针 = 无 running 同名容器。
- **absent**:`docker rm -f`;探针 = 无同名容器(含已退出)。teardown 直接用。
- 刻意不做(Ansible 有):`comparisons` 逐项 diff、networks 细粒度、healthcheck/GPU/
  device 限速等 —— 要么 `args:` 逃逸,要么回 `shell`。准入逻辑见 [module-charter](../module-charter.md)。

## 示例

```yaml
- id: run
  action: docker_container
  name: rustfs
  image: "docker.io/rustfs/rustfs:{{version}}"
  restart_policy: unless-stopped
  ports:
    - "{{api_port}}:9000"
    - "{{console_port}}:9001"
  volumes:
    - "{{data_dir}}:/data"
  env:
    RUSTFS_ACCESS_KEY: "{{access_key}}"
    RUSTFS_SECRET_KEY: "{{secret_key}}"
teardown:
  - action: docker_container
    name: rustfs
    state: absent
```

镜像获取不归本模块管:声明 `kind: image` 物料 + [load_image](load_image.md)
(在线 pull / 离线 import),本模块只负责"容器以期望参数在跑"。

## 关联

ADR:D-092(指纹收敛/精简集)。相关:[load_image](load_image.md)、[shell](shell.md)(更细的容器操作)、library/rustfs(真实交付)。
