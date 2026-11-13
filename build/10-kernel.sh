#!/bin/bash
# 10-kernel.sh —— 编译 Lutra 内核并产出 deb 包
# 用法：./build/10-kernel.sh [amd64|arm64|i686]
# 环境变量：KVER（默认 6.12）、JOBS（默认 nproc）、KDEB_PKGVERSION（默认 1.0.0-lutra1）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

ARCH_ARG="${1:-amd64}"

case "$ARCH_ARG" in
    amd64)
        KARCH="x86_64"
        KCONFIG_ARCH="desktop-amd64"
        CROSS_COMPILE=""
        ;;
    arm64)
        KARCH="arm64"
        KCONFIG_ARCH="desktop-arm64"
        CROSS_COMPILE="${CROSS_COMPILE:-aarch64-linux-gnu-}"
        ;;
    i686)
        KARCH="i386"
        KCONFIG_ARCH="legacy-i686"
        CROSS_COMPILE="${CROSS_COMPILE:-i686-linux-gnu-}"
        ;;
    *)
        die "未知架构: $ARCH_ARG（可选 amd64 / arm64 / i686）"
        ;;
esac

require make
require gcc
require tar
require bc
require flex
require bison
command -v ccache >/dev/null 2>&1 || warn "未找到 ccache，编译会变慢"

CONFIG_DIR="$LUTRA_ROOT/src/kernel/configs"
TARBALL="$LUTRA_CACHE/linux-${KVER}.tar.xz"
BUILD_LOG="$LUTRA_BUILD/kernel-${ARCH_ARG}.log"
JOBS="${JOBS:-$(nproc)}"

# ---------- 1. 源码 ----------
if [ ! -f "$TARBALL" ]; then
    log "下载内核源码 linux-${KVER}"
    curl -fL --retry 3 --retry-delay 5 -o "$TARBALL" "$KURL" || die "内核源码下载失败"
fi

if [ ! -d "$KSRC" ]; then
    log "解压内核源码到 $LUTRA_BUILD"
    tar -xf "$TARBALL" -C "$LUTRA_BUILD"
fi

cd "$KSRC"

# ---------- 2. 配置 ----------
FRAGS=("$CONFIG_DIR/base.config" "$CONFIG_DIR/${KCONFIG_ARCH}.config" "$CONFIG_DIR/virt.config")
for f in "${FRAGS[@]}"; do
    [ -f "$f" ] || die "缺少配置碎片: $f"
done

log "生成默认配置：ARCH=$KARCH"
make ARCH="$KARCH" CROSS_COMPILE="$CROSS_COMPILE" defconfig >/dev/null

log "合并配置碎片：base + ${KCONFIG_ARCH} + virt"
./scripts/kconfig/merge_config.sh -m -O "$KSRC" "${FRAGS[@]}" >/dev/null
make ARCH="$KARCH" CROSS_COMPILE="$CROSS_COMPILE" olddefconfig >/dev/null

# ---------- 3. 关键项自检：缺失则立即失败，不浪费时间编译 ----------
check_cfg() {
    local sym="$1"
    if ! grep -q "^${sym}=y" .config && ! grep -q "^${sym}=m" .config; then
        die "配置自检失败：$sym 未启用"
    fi
}
check_cfg CONFIG_BTRFS_FS
check_cfg CONFIG_OVERLAY_FS
check_cfg CONFIG_SQUASHFS
check_cfg CONFIG_DEVTMPFS
check_cfg CONFIG_MODULES
check_cfg CONFIG_EXT4_FS
if [ "$ARCH_ARG" != "i686" ]; then
    check_cfg CONFIG_VIRTIO_PCI
fi
log "配置自检通过：$(grep -c '=y$' .config) 个内建项"

# ---------- 4. 编译 ----------
log "开始编译 $ARCH_ARG，并行度 $JOBS（ccache: $CCACHE_DIR）"
CC_WRAP="gcc"
command -v ccache >/dev/null 2>&1 && CC_WRAP="ccache gcc"

set +e
make ARCH="$KARCH" CROSS_COMPILE="$CROSS_COMPILE" -j"$JOBS" \
    CC="$CC_WRAP" \
    KDEB_PKGVERSION="${KDEB_PKGVERSION:-1.0.0-lutra1}" \
    bindeb-pkg 2>&1 | tee "$BUILD_LOG" | tail -n 40
RC=${PIPESTATUS[0]}
set -e

if [ "$RC" -ne 0 ]; then
    die "编译失败（退出码 $RC），完整日志：$BUILD_LOG"
fi

# ---------- 5. 收集 ----------
log "收集 deb 产物"
DEST="$KDEST/$ARCH_ARG"
mkdir -p "$DEST"
find "$LUTRA_BUILD" -maxdepth 1 -name "linux-*_*.deb" -newer "$TARBALL" -exec cp -v {} "$DEST/" \;
find "$LUTRA_BUILD" -maxdepth 1 -name "linux-*_*.changes" -newer "$TARBALL" -exec cp -v {} "$DEST/" \;

ls -lh "$DEST"
log "完成：$DEST"
