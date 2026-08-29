# docker —— 容器运行时(blueprint)

官方静态 tarball → `/usr/local/bin` → containerd/docker 两个 unit → 起服务。
amd64/arm64 双物料变体;daemon.json 变更经上游传导自动重启(无需 notify)。

```bash
crater apply library/docker/docker.blueprint.yaml -i inventory.yaml
```
