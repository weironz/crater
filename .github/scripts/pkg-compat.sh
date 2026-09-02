#!/usr/bin/env bash
# 一家 registry 的兼容性实测:推一个蓝图包上去,再把 manifest **绕开 crater**
# 原样读回来,断言三件事。任一条不成立就非零退出 —— 这是判据,不是巡视。
#
#   $1  登录主机(docker.io / registry.cn-shenzhen.aliyuncs.com)
#   $2  完整引用(user/repo:tag 或 host/ns/repo:tag)
#
# 需要 USER_NAME / TOKEN。读回来那一步刻意不用 crater 自己的客户端 ——
# 拿自己的解析去证明自己的写入,证明不了任何事。
set -euo pipefail

HOST="$1"; REF="$2"
# CI 里是 release 构建;本地验脚本本身时用 CRATER_BIN 指到 debug 那份。
CRATER="${CRATER_BIN:-$(pwd)/target/release/crater}"
CFG_MT="application/vnd.crater.blueprint.config.v1+json"
LAY_MT="application/vnd.crater.blueprint.v1.tar+gzip"

[ -n "${TOKEN:-}" ] && echo "::add-mask::$TOKEN"
echo "── $HOST ── $REF"

if [ -n "${TOKEN:-}" ]; then
  "$CRATER" registry login "$HOST" -u "$USER_NAME" -p "$TOKEN" >/dev/null
fi
"$CRATER" pkg push library/yq "$REF"

# 仓库路径与 tag。docker.io 的 API 主机是 registry-1.docker.io,别处即 HOST。
REPO_PATH="${REF%:*}"; REPO_PATH="${REPO_PATH#"$HOST"/}"
TAG="${REF##*:}"
case "$HOST" in
  docker.io) API="https://registry-1.docker.io" ;;
  127.0.0.1:*|localhost:*) API="http://$HOST" ;;
  *) API="https://$HOST" ;;
esac
URL="$API/v2/$REPO_PATH/manifests/$TAG"
ACCEPT='Accept: application/vnd.oci.image.manifest.v1+json'

# 标准流程:先裸请求,401 就照 WWW-Authenticate 的挑战换 bearer token。
# 比按 registry 硬编码 token 端点稳 —— 那种写法换一家就得改一次。
CHAL=$(curl -sS -o /dev/null -D - -H "$ACCEPT" "$URL" | tr -d '\r' \
       | grep -i '^www-authenticate:' | head -1 || true)
if [ -n "$CHAL" ]; then
  realm=$(sed -n 's/.*realm="\([^"]*\)".*/\1/p' <<<"$CHAL")
  service=$(sed -n 's/.*service="\([^"]*\)".*/\1/p' <<<"$CHAL")
  T=$(curl -sS -u "$USER_NAME:${TOKEN:-}" \
        --get "$realm" --data-urlencode "service=$service" \
        --data-urlencode "scope=repository:$REPO_PATH:pull" \
      | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d.get("token") or d.get("access_token",""))')
  M=$(curl -sS -H "Authorization: Bearer $T" -H "$ACCEPT" "$URL")
else
  M=$(curl -sS -H "$ACCEPT" "$URL")
fi

# manifest 走文件而不是管道:heredoc 已经占了 python 的 stdin,
# 管道里的字节根本读不到(第一版就栽在这)。
MF=$(mktemp); printf '%s' "$M" > "$MF"; trap 'rm -f "$MF"' EXIT
python3 - "$CFG_MT" "$LAY_MT" "$MF" <<'PY'
import sys, json
want_cfg, want_lay, path = sys.argv[1], sys.argv[2], sys.argv[3]
try:
    m = json.load(open(path))
except Exception as e:
    print("读回来的不是 JSON:", e, open(path).read()[:200]); sys.exit(1)
if "errors" in m:
    print("registry 拒绝了读取:", json.dumps(m["errors"], ensure_ascii=False)); sys.exit(1)
ok = True
def check(name, got, want):
    global ok
    good = got == want
    ok = ok and good
    print(f"  {'✓' if good else '✗'} {name:<20} {got}")
    if not good:
        print(f"    期望 {want}")
check("config.mediaType", m["config"]["mediaType"], want_cfg)
check("layer.mediaType",  m["layers"][0]["mediaType"], want_lay)
at = m.get("artifactType")
print(f"  {'✓' if at is None else '✗'} {'artifactType':<20} {at if at else '(未设 —— 正确)'}")
if at is not None:
    ok = False
print(f"\n  契约(config)  {m['config']['size']} B —— inspect 的全部下载量")
print(f"  蓝图层         {m['layers'][0]['size']} B")
sys.exit(0 if ok else 1)
PY

echo "── $HOST:自定义类型原样过线 ✓"
