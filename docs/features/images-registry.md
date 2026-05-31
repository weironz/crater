# 镜像管理：images / pull / push / login + apply &lt;镜像地址&gt;

> ADR: D-018 ｜ 代码: `crates/crater-core/src/store.rs`

## 这是什么

crater 自带一个**本地 OCI 镜像库** + **纯 Rust registry 客户端**（oci-client，rustls，无 Docker）。镜像是离线依赖的打包/分发载体；安装由 crater 自己解包（展开 rootfs），目标机零容器运行时。

- **本地库**：`~/.crater/store`（`$CRATER_HOME` 可改），一个累积的 OCI Image Layout（`oci-layout` + `index.json` 每个 tag 一条 + `blobs/sha256/`）。
- **凭据**：`~/.crater/auth.json`（按 registry 存 user/pass），pull/push 自动用。

| 命令 | 作用 |
|---|---|
| `crater images` | 列出本地库的镜像（ref / digest / size） |
| `crater pull <ref>` | 从 registry 拉镜像进本地库 |
| `crater push <ref>` | 把本地库的镜像推到 registry |
| `crater registry login <registry> -u U -p P` | 存该 registry 的凭据 |
| `crater apply <ref> --host/-i` | 库里有就用、没有就 pull，再把镜像 rootfs 层**展开到目标机**安装 |

> 注：原"SSH 拷文件"的 `crater push` 已更名为 **`crater cp`**（`push` 让给 registry 推送）。

## 基本 demo

```bash
crater registry login docker.io -u <user> -p <pass>     # 私有库才需要；公共匿名即可
crater pull docker.io/library/hello-world:latest        # → ~/.crater/store
crater images
#  REFERENCE                              DIGEST        SIZE
#  docker.io/library/hello-world:latest   3455a1c81403  402

# apply 直接吃镜像地址：本地库命中→用，否则自动 pull，再展开到目标机
crater apply docker.io/library/hello-world:latest --host <host> --password <pw>
#  → image …: 1 layer(s), 1 host(s)；▶ host …；extracted layer 1/1

# 多主机：crater apply <ref> -i examples/two-hosts.yaml
# 推送（需可写 registry）：crater push <your-registry>/yq:1.0
```

## 验证（真机 192.168.73.12）

- `login` 写入 `~/.crater/auth.json`（并实际改变 pull 的认证：填错凭据 → pull 401，删后匿名成功）。
- `pull` hello-world → 库内；`images` 正确列出。
- `apply docker.io/library/hello-world:latest --host .12` → 库命中、展开 rootfs → 目标机 `/hello`（ELF 可执行）落地。

## 边界 / 后续

- `push` 已实现（oci-client `client.push`），但本环境无可写 registry，**未 live 验证**；需带凭据的 registry 测一遍。
- `apply <ref>` 当前把镜像**所有层展开到 `/`**（rootfs 覆盖语义，适合 crater 构建的 rootfs 镜像 / sealos 式镜像）；任意容器镜像展开到 `/` 会铺满其容器根文件系统，按需使用。
- 多 arch manifest-list 的平台选择、镜像签名（N4）、库 GC 后续。
