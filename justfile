# crater 常用入口。`just` 不带参数 = 列出全部配方。

# ~/.cargo/config.toml 配了 -fuse-ld=mold(D-079 提速),但没装 mold 的机器
# 会直接链接失败。空 RUSTFLAGS 覆盖掉它:装了 mold 想要提速,删这行即可。
export RUSTFLAGS := ""

# 换端口 `just ui 9000`,换工作区 `just ui 8899 127.0.0.1 /srv/crater`。
# (注意:UI 无认证,0.0.0.0 仅限可信内网)。
#
# **工作区默认在仓库之外**(~/crater):UI 在哪个目录起,点一下就在改那个
# 目录 —— 从仓库里起 UI,主机页上的一次编辑会直接落到随工具发行的
# library/ 文件里(真发生过:模板 inventory 被写进了实验机的真口令)。
# 想拿库里的蓝图,复制一份过去,而不是就地改。
#
# 一键拉起 UI(默认 127.0.0.1:8899,工作区 ~/crater)
ui port="8899" bind="127.0.0.1" workspace="~/crater":
    #!/usr/bin/env bash
    set -euo pipefail
    WS=$(eval echo {{workspace}})    # 展开 ~
    mkdir -p "$WS"
    cargo build --release -p crater-cli
    ./target/release/crater ui --bind {{bind}} --port {{port}} --workspace "$WS"

# release 构建
build:
    cargo build --release

# 全量测试
test:
    cargo test --release

# 提交前自检:零警告 + 全量测试。
#
# `-D warnings` 只管得住本仓库(cargo 对 registry 依赖自动 --cap-lints allow),
# 所以不会被上游警告误伤。CI 用的是同一条口径。
#
# clippy 是 `cargo build` 看不见的那一半 lint(D-144)。两处 `-D warnings`
# 不是重复:RUSTFLAGS 那份只到 rustc,而 clippy 自己的 lint
# (type_complexity / unnecessary_unwrap / doc_lazy_continuation …)只认 `--`
# 后面这份 —— 少写后面那份,闸门就只剩个名字。
#
# 跟着 build/test 一起用 --release:同一份依赖产物,不为 lint 再编一遍 dev。
check:
    RUSTFLAGS="" cargo clippy --workspace --all-targets --release -- -D warnings
    RUSTFLAGS="-D warnings" cargo build --release --all-targets
    RUSTFLAGS="-D warnings" cargo test --release

# 用 install 而不是 cp:它一步做完权限设置,且**先写再原子替换**,
# 不会像 cp 那样在覆盖正在运行的二进制时撞上 ETXTBSY。
#
# 构建 CLI 并装到 /usr/local/bin(需要 sudo)
app:
    cargo build --release -p crater-cli
    sudo install -m 0755 target/release/crater /usr/local/bin/crater
    @echo "已安装 → $(command -v crater)"
    @crater --version

# 不接 traefik、不要证书、不设 token —— dev 要的是"改完立刻能点"。
# 换端口:`just dev 9090`
#
# 本地 dev 容器:构建二进制 → 打镜像 → compose 起来(默认 :8080)
dev port="8080":
    #!/usr/bin/env bash
    # shebang 配方:整段跑在**同一个 shell** 里(普通配方是一行一个 shell,
    # ARCH 活不过下一行)。
    set -euo pipefail
    # 与 CI 用同一个目标 —— 本地跑通而 CI 产物不同,等于没验。
    # 需要 musl-tools(aws-lc-sys 点名要 <arch>-linux-musl-gcc)。
    ARCH=$(uname -m | sed 's/x86_64/amd64/;s/aarch64/arm64/')
    TARGET=$(uname -m)-unknown-linux-musl
    command -v "$(uname -m)-linux-musl-gcc" >/dev/null || {
      echo "缺 musl 工具链:sudo apt-get install -y musl-tools"; exit 1; }
    rustup target add "$TARGET" >/dev/null
    cargo build --release --target "$TARGET" -p crater-cli
    mkdir -p dist
    cp "target/$TARGET/release/crater" "dist/crater-$ARCH"
    # TARGETARCH 只有 buildx/BuildKit 会自动注入,经典 builder 下是空的
    # (COPY 会去找 `dist/crater-` 然后失败)—— 本地显式给。
    docker build --build-arg TARGETARCH="$ARCH" -f docker/Dockerfile -t crater:dev .
    CRATER_DEV_PORT={{port}} docker compose -f deploy/docker-compose.dev.yaml up -d
    docker compose -f deploy/docker-compose.dev.yaml ps
    echo
    echo "→ http://127.0.0.1:{{port}}/?token=${CRATER_DEV_TOKEN:-dev}"
    echo "  (首访带 ?token= 会换成 cookie,之后直接开 http://127.0.0.1:{{port}} 即可)"

# 停掉本地 dev 容器
dev-down:
    docker compose -f deploy/docker-compose.dev.yaml down
