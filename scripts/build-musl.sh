#!/usr/bin/env bash
# Build a fully static (musl) Linux binary of crater — runs on ANY Linux of the
# given arch with zero runtime deps (verified: `ldd` => "statically linked").
# This is the portable binary shipped as the self-bootstrap agent (D-027) and
# as the per-arch GitHub release asset (v0.1.0+).
#
# Why musl: the default glibc build is dynamically linked, so it only runs where
# the target's glibc is compatible. The musl static build has no such constraint.
#
# Prereqs (one-time, Debian/Ubuntu):
#   x86_64:  rustup target add x86_64-unknown-linux-musl
#            apt-get install -y musl-tools            # musl-gcc (ring's C bits)
#   aarch64: rustup target add aarch64-unknown-linux-musl
#            # REAL musl cross toolchain (musl headers+libc; the Ubuntu
#            # gcc-aarch64-linux-gnu compiles against GLIBC headers and the
#            # link then fails on __*_chk / __isoc23_* symbols musl lacks):
#            curl -fLO https://musl.cc/aarch64-linux-musl-cross.tgz
#            tar xzf aarch64-linux-musl-cross.tgz -C /opt
#
# Usage:
#   scripts/build-musl.sh            # -> dist/crater-linux-x86_64
#   scripts/build-musl.sh aarch64    # -> dist/crater-linux-aarch64
#   scripts/build-musl.sh all        # both
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO"

build_one() {
  local ARCH="$1"
  local TARGET="${ARCH}-unknown-linux-musl"
  echo "Building crater for ${TARGET} (static musl) ..."
  case "$ARCH" in
    x86_64)
      # `ring` needs a C compiler for the musl target; point it at musl-gcc.
      CC_x86_64_unknown_linux_musl=musl-gcc \
        cargo build --release --target "$TARGET"
      ;;
    aarch64)
      # musl.cc cross toolchain: musl headers AND musl libc — no glibc-header
      # symbol skew (fortify __*_chk, __isoc23_*) in aws-lc/ring's C bits.
      local MUSL_CC="${AARCH64_MUSL_CC:-/opt/aarch64-linux-musl-cross/bin/aarch64-linux-musl-gcc}"
      if [ ! -x "$MUSL_CC" ]; then
        echo "missing $MUSL_CC — see prereqs in this script (musl.cc toolchain)" >&2
        exit 1
      fi
      CC_aarch64_unknown_linux_musl="$MUSL_CC" \
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="$MUSL_CC" \
        cargo build --release --target "$TARGET"
      ;;
    *)
      echo "unsupported arch '$ARCH' (x86_64 | aarch64 | all)" >&2
      exit 1
      ;;
  esac
  mkdir -p dist
  local OUT="dist/crater-linux-${ARCH}"
  cp "target/${TARGET}/release/crater" "$OUT"
  echo "OK -> ${OUT}"
  file "$OUT" | sed 's/^/  /'
}

ARCH="${1:-x86_64}"
if [ "$ARCH" = "all" ]; then
  build_one x86_64
  build_one aarch64
else
  build_one "$ARCH"
fi
