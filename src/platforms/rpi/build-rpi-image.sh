#!/bin/bash
# 构建树莓派（ARM64）SD 卡镜像
# 用法：sudo ./build-rpi-image.sh /dev/sdX
# 说明：仅写入指定设备，未指定时只生成 .img 不落盘。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/../../.." && pwd)"
VERSION="$(cat "$ROOT/VERSION" 2>/dev/null || echo 1.0.0)"
OUT="$ROOT/dist/lutra-${VERSION}-rpi-arm64.img"
TARGET="${1:-}"
SUITE="${SUITE:-trixie}"
MIRROR="${MIRROR:-https://mirrors.tuna.tsinghua.edu.cn/debian}"
KDEB_DIR="${KDEB_DIR:-$ROOT/dist/kernel/arm64}"
IMG_SIZE_MB="${IMG_SIZE_MB:-6144}"

require() { command -v "$1" >/dev/null || { echo "缺少 $1" >&2; exit 1; }; }
require debootstrap
require parted
require losetup
require mkfs.vfat
require mkfs.ext4

if [ -n "$TARGET" ]; then
    echo "警告：即将写入 $TARGET，该设备上的数据会被清空。"
    read -r -p "输入设备路径确认： " ans
    [ "$ans" = "$TARGET" ] || { echo "已取消"; exit 1; }
fi

work="$(mktemp -d)"
trap 'sudo umount -R "$work/mnt" 2>/dev/null || true; rm -rf "$work"' EXIT
img="$work/lutra.img"

echo "创建 ${IMG_SIZE_MB}MB 镜像"
dd if=/dev/zero of="$img" bs=1M count="$IMG_SIZE_MB" status=none
parted -s "$img" mklabel msdos
parted -s "$img" mkpart primary fat32 1MiB 513MiB
parted -s "$img" mkpart primary ext4 513MiB 100%
parted -s "$img" set 1 lba on
parted -s "$img" set 1 boot on

loop="$(sudo losetup --show -fP "$img")"
sudo mkfs.vfat -F32 -n LUTRA-BOOT "${loop}p1"
sudo mkfs.ext4 -L LUTRA-ROOT "${loop}p2"

sudo mkdir -p "$work/mnt"
sudo mount "${loop}p2" "$work/mnt"
sudo mkdir -p "$work/mnt/boot"
sudo mount "${loop}p1" "$work/mnt/boot"

echo "debootstrap arm64 -> 镜像"
sudo debootstrap --arch=arm64 --variant=minbase "$SUITE" "$work/mnt" "$MIRROR"

# 拷贝内核 deb 并安装
if ls "$KDEB_DIR"/linux-image-*.deb >/dev/null 2>&1; then
    sudo cp "$KDEB_DIR"/*.deb "$work/mnt/tmp/"
    sudo chroot "$work/mnt" /bin/bash -c 'dpkg -i /tmp/linux-image-*.deb || true'
else
    echo "未找到 $KDEB_DIR 下的 arm64 内核 deb，跳过安装" >&2
fi

# 树莓派固件启动文件
sudo tee "$work/mnt/boot/config.txt" >/dev/null <<'EOF'
arm_64bit=1
enable_uart=1
dtoverlay=vc4-kms-v3d
max_framebuffers=2
EOF
sudo tee "$work/mnt/boot/cmdline.txt" >/dev/null <<'EOF'
console=serial0,115200 console=tty1 root=LABEL=LUTRA-ROOT rootfstype=ext4 rw rootwait
EOF

sudo sed -i 's|^LABEL=.*|LABEL=LUTRA-ROOT / ext4 defaults,noatime 0 1|' "$work/mnt/etc/fstab" 2>/dev/null || true
echo "LABEL=LUTRA-BOOT /boot vfat defaults 0 2" | sudo tee -a "$work/mnt/etc/fstab" >/dev/null

sudo umount -R "$work/mnt"
sudo losetup -d "$loop"

mkdir -p "$(dirname "$OUT")"
mv "$img" "$OUT"
echo "镜像已生成：$OUT"

if [ -n "$TARGET" ]; then
    echo "写入 $TARGET"
    sudo dd if="$OUT" of="$TARGET" bs=4M status=progress conv=fsync
    sync
    echo "写入完成"
fi
