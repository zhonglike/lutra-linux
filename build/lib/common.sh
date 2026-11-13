#!/bin/bash
# Lutra 构建公共库：路径约定、日志、错误处理
# 所有构建脚本 source 本文件后使用。

set -euo pipefail

LUTRA_ROOT="${LUTRA_ROOT:-/home/lutra/lutra}"
LUTRA_BUILD="${LUTRA_BUILD:-$LUTRA_ROOT/build}"
LUTRA_OUT="${LUTRA_OUT:-$LUTRA_ROOT/dist}"
LUTRA_CACHE="${LUTRA_CACHE:-$LUTRA_ROOT/_cache}"

KVER="${KVER:-6.12}"
KURL="https://cdn.kernel.org/pub/linux/kernel/v${KVER%%.*}.x/linux-${KVER}.tar.xz"
KSRC="$LUTRA_BUILD/linux-${KVER}"
KDEST="$LUTRA_OUT/kernel"

mkdir -p "$LUTRA_BUILD" "$LUTRA_OUT" "$KDEST" "$LUTRA_CACHE/ccache"

log()  { printf '\033[36m[%s]\033[0m %s\n' "$(date +%H:%M:%S)" "$*"; }
warn() { printf '\033[33m[WARN]\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[31m[FAIL]\033[0m %s\n' "$*" >&2; exit 1; }

require() {
    command -v "$1" >/dev/null 2>&1 || die "缺少依赖: $1"
}

export CCACHE_DIR="$LUTRA_CACHE/ccache"
export CCACHE_MAXSIZE="${CCACHE_MAXSIZE:-20G}"

JOBS="${JOBS:-8}"
