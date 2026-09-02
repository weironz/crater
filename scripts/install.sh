#!/bin/sh
# crater 一键安装。
#
#   curl -fsSL https://raw.githubusercontent.com/weironz/crater/main/scripts/install.sh | sh
#
# 三条刻意的设计:
#
# - **校验摘要,不给关闭的开关。** 这个脚本从网上取一个二进制然后让你执行它,
#   跳过校验就等于把"信道没被动过"当成理所当然。发版流程本来就产出
#   SHA256SUMS,不用白不用。
# - **默认装到用户目录**(`~/.local/bin`),不 sudo。一个用管道执行的脚本
#   不该顺手要 root —— 要装到 /usr/local/bin,自己加 `--prefix` 并自己决定
#   要不要 sudo。
# - **失败就退,不"尽力而为"。** 半装成功的 CLI 比没装更难查。
set -eu

REPO="weironz/crater"
PREFIX="${CRATER_PREFIX:-$HOME/.local/bin}"
VERSION="${CRATER_VERSION:-latest}"

usage() {
    cat <<EOF
crater 安装脚本

  --prefix DIR      装到哪(默认 ~/.local/bin,也可用 \$CRATER_PREFIX)
  --version vX.Y.Z  装哪一版(默认最新,也可用 \$CRATER_VERSION)
  --help

例:
  curl -fsSL <本脚本> | sh
  curl -fsSL <本脚本> | sh -s -- --prefix /usr/local/bin
  curl -fsSL <本脚本> | CRATER_VERSION=v0.2.0 sh
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) PREFIX="$2"; shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        --help|-h) usage; exit 0 ;;
        *) echo "未知参数:$1" >&2; usage >&2; exit 2 ;;
    esac
done

die() { echo "crater 安装失败:$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ── 平台 ──────────────────────────────────────────────────────────────
# 只发 musl 静态包:一份二进制在 glibc 与 musl 的机器上都能跑,不必按发行版
# 分。代价是体积略大,换的是"拷过去就能用"。
os=$(uname -s)
[ "$os" = "Linux" ] || die "目前只发 Linux(检测到 $os)。别的平台请从源码构建:cargo install --path crates/crater-cli"

case "$(uname -m)" in
    x86_64|amd64)  target="x86_64-unknown-linux-musl" ;;
    aarch64|arm64) target="aarch64-unknown-linux-musl" ;;
    *) die "不支持的架构 $(uname -m)(有 x86_64 与 aarch64)" ;;
esac

have curl || have wget || die "需要 curl 或 wget"
have tar || die "需要 tar"
have sha256sum || have shasum || die "需要 sha256sum 或 shasum —— 摘要校验不能跳过"

fetch() { # fetch <url> <目标文件>
    if have curl; then
        curl -fsSL "$1" -o "$2" || die "下载失败:$1"
    else
        wget -qO "$2" "$1" || die "下载失败:$1"
    fi
}

# ── 版本 ──────────────────────────────────────────────────────────────
if [ "$VERSION" = "latest" ]; then
    tmpj=$(mktemp)
    fetch "https://api.github.com/repos/$REPO/releases/latest" "$tmpj"
    # 不引 jq:安装脚本的依赖越少越好。这里只需从 JSON 里抠一个 tag。
    VERSION=$(sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' "$tmpj" | head -1)
    rm -f "$tmpj"
    [ -n "$VERSION" ] || die "问不到最新版本(GitHub API 限流?可用 --version 指定)"
fi

base="https://github.com/$REPO/releases/download/$VERSION"
tarball="crater-$target.tar.gz"

echo "crater $VERSION ($target) → $PREFIX"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

fetch "$base/$tarball" "$tmp/$tarball"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS"

# ── 校验 ──────────────────────────────────────────────────────────────
# 只核对**我们要装的那一个**文件:SHA256SUMS 里还列着别的架构,整份校验会
# 因为"那些文件不在本地"而失败 —— 那种失败会被人当成噪音,然后学会忽略它。
want=$(grep "[ *]$tarball\$" "$tmp/SHA256SUMS" | awk '{print $1}' | head -1)
[ -n "$want" ] || die "SHA256SUMS 里没有 $tarball —— 这一版可能没发这个架构"

if have sha256sum; then
    got=$(sha256sum "$tmp/$tarball" | awk '{print $1}')
else
    got=$(shasum -a 256 "$tmp/$tarball" | awk '{print $1}')
fi

[ "$want" = "$got" ] || die "摘要不符!
  期望 $want
  实得 $got
下载物被动过,或者传输出错。**不要**绕过这一步。"

echo "  摘要校验通过"

# ── 安装 ──────────────────────────────────────────────────────────────
tar -xzf "$tmp/$tarball" -C "$tmp" || die "解包失败"
[ -f "$tmp/crater" ] || die "包里没有 crater 可执行文件"

mkdir -p "$PREFIX" || die "建不出 $PREFIX"
# install 一步做完权限设置,而且**先写再原子替换** —— 覆盖正在运行的二进制
# 时不会撞上 ETXTBSY(与 justfile 里的 `just app` 同一条理由)。
if have install; then
    install -m 0755 "$tmp/crater" "$PREFIX/crater" || die "写入 $PREFIX 失败(需要 sudo?)"
else
    cp "$tmp/crater" "$PREFIX/crater.new" && chmod 0755 "$PREFIX/crater.new" \
        && mv -f "$PREFIX/crater.new" "$PREFIX/crater" || die "写入 $PREFIX 失败(需要 sudo?)"
fi

# ── 收尾 ──────────────────────────────────────────────────────────────
echo "  已装到 $PREFIX/crater"
"$PREFIX/crater" --version || die "装上了但跑不起来"

# PATH 提醒放在最后:装完却敲不出 `crater` 是最常见的一种"装失败"。
case ":$PATH:" in
    *":$PREFIX:"*) ;;
    *)
        echo
        echo "注意:$PREFIX 不在 PATH 里。加这一行到你的 shell 配置:"
        echo "  export PATH=\"$PREFIX:\$PATH\""
        ;;
esac

echo
echo "下一步:"
echo "  crater types                     看能声明哪 26 种资源"
echo "  crater lint <蓝图>               静态检查,不连机器"
echo "  crater plan -f <蓝图> -i <机群>  零写入预演"
echo "  crater update                    以后升级用它"
