#!/usr/bin/env bash
# Build a fully static (musl) Linux binary of crater — runs on ANY x86_64 Linux
# with zero runtime deps (verified: `ldd` => "statically linked"). This is the
# portable binary to ship as the self-bootstrap agent (`--agent-bin`, D-027).
#
# Why musl: the default glibc build is dynamically linked, so it only runs where
# the target's glibc is compatible. The musl static build has no such constraint.
#
# Prereqs (one-time, Debian/Ubuntu):
#   rustup target add x86_64-unknown-linux-musl
#   apt-get install -y musl-tools          # provides musl-gcc (for `ring`'s C bits)
#
# Usage:
#   scripts/build-musl.sh                  # -> dist/crater-linux-x86_64 (static)
set -euo pipefail

ARCH="${1:-x86_64}"
TARGET="${ARCH}-unknown-linux-musl"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

echo "Building crater for ${TARGET} (static musl) ..."
# `ring` needs a C compiler for the musl target; point it at musl-gcc.
CC_x86_64_unknown_linux_musl=musl-gcc \
  cargo build --release --target "${TARGET}"

mkdir -p dist
OUT="dist/crater-linux-${ARCH}"
cp "target/${TARGET}/release/crater" "$OUT"
echo "OK -> ${OUT}"
file "$OUT" | sed 's/^/  /'
