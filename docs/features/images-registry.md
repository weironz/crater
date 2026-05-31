# 镜像管理：images / pull / push / login + apply &lt;镜像地址&gt;

> ADR: D-018 ｜ 代码: `crates/crater-core/src/store.rs`

## 这是什么

crater 自带一个**本地 OCI 镜像库** + **纯 Rust registry 客户端**（oci-client，rustls，无 Docker）。它既分发 **crater B 类 artifact**（`build --image` 产物，apply 时 recipe-replay，D-033），也能搬运**普通容器镜像**（A 类，apply 时展开层）；目标机零容器运行时。

- **本地库**：`~/.crater/store`（`$CRATER_HOME` 可改），一个累积的 OCI Image Layout（`oci-layout` + `index.json` 每个 tag 一条 + `blobs/sha256/`）。
- **凭据**：`~/.crater/auth.json`（按 registry 存 user/pass），pull/push 自动用。

| 命令 | 作用 |
|---|---|
| `crater images` | 列出本地库的镜像（ref / digest / size） |
| `crater pull <ref>` | 从 registry 拉镜像进本地库 |
| `crater push <ref>` | 把本地库的镜像推到 registry |
| `crater load <file.oci> [--as <ref>]` | 把 oci-archive 导入本地库;省略 `--as` 用包内 ref.name（`build -t` 定的） |
| `crater tag <src> <dst>` | 给已有镜像加一个新引用（别名），内容寻址共享 blob、零拷贝（同 `docker tag`） |
| `crater registry login <registry> -u U -p P` | 存该 registry 的凭据 |
| `crater apply <ref> --host/-i` | 库里有就用、没有就 pull；crater artifact→**recipe-replay**，普通镜像→**展开层**到目标机 |

- HTTP（不带 TLS）的临时/内网 registry：设 `CRATER_INSECURE_REGISTRIES=host:port`（逗号分隔，通用，引擎不认识具体 registry）。
- 临时 registry 可用 crater 自己装：`crater zot`（`components/zot`，数据驱动，systemd 管理，:5000）。

> 注：原"SSH 拷文件"的 `crater push` 已更名为 **`crater cp`**（`push` 让给 registry 推送）。

## 基本 demo

```bash
crater registry login docker.io -u <user> -p <pass>     # 私有库才需要；公共匿名即可
crater pull docker.io/library/hello-world:latest        # → ~/.crater/store
crater images
#  REFERENCE                              DIGEST        SIZE
#  docker.io/library/hello-world:latest   3455a1c81403  402

# apply 直接吃镜像地址：本地库命中→用，否则自动 pull（hello-world 是普通镜像→展开层）
crater apply docker.io/library/hello-world:latest --host <host> --password <pw>
#  → image …: 1 layer(s), 1 host(s)；▶ host …；extracted layer 1/1

# 多主机：crater apply <ref> -i examples/two-hosts.yaml
```

## 完整闭环：build → push → 另一台 pull/apply（zot 临时仓库，真机验证）

```bash
crater zot                                            # 本机装 zot registry（systemd，:5000）
export CRATER_INSECURE_REGISTRIES=192.168.73.5:5000   # zot 走 http
crater build --image -f examples/yq/yq.yaml -o /tmp/yq.oci -t 192.168.73.5:5000/yq:4.53.2
crater load /tmp/yq.oci                                # 无 --as:用包内 ref.name(build -t 定的)
crater tag 192.168.73.5:5000/yq:4.53.2 192.168.73.5:5000/yq:stable   # 起别名（零拷贝，同 digest）
crater push 192.168.73.5:5000/yq:4.53.2               # → zot；curl .../v2/_catalog 见 {"repositories":["yq"]}
rm -rf ~/.crater/store                                # 清本地库，强制从 zot 拉
crater apply 192.168.73.5:5000/yq:4.53.2 --host 192.168.73.12 --password 123456
#  → "not in local store → pulling"（真从 zot 拉）→ "crater component artifact → recipe-replay"
#  → place (offline) yq-bin -> /usr/local/bin/yq → v4.53.2、可执行
```

## 验证（真机）

- `login` 写入 `~/.crater/auth.json`（并实际改变 pull 认证：错凭据→401，删后匿名成功）。
- `pull` hello-world → 库；`images` 列出；`apply docker.io/library/hello-world:latest --host .12` → `/hello`（ELF）落地。
- **闭环（zot）**：`build → load → push`（zot catalog 出现 yq）→ 清库 → `apply <zot地址>`（真 pull）→ n12 上 `yq --version` v4.53.2、可执行。

## 边界 / 后续

- `apply <ref>`：**crater B 类 artifact**（`artifactType` 命中）→ recipe-replay（取 recipe + material blob，走在线同一引擎，`place` 按名落地，D-033/D-034）；**普通容器镜像**（无 artifactType）→ 把所有层展开到 `/`（rootfs 覆盖语义，适合 crater/sealos 式镜像；任意镜像展开到 `/` 会铺满其容器根文件系统，按需使用）。
- 多 arch manifest-list 的平台选择、镜像签名（N4）、库 GC、registry TLS/认证 后续。
