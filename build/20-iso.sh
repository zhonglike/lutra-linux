#!/bin/bash
# 20-iso.sh —— 构建 Lutra Live ISO（在构建机内运行）
# 用法：sudo ./build/20-iso.sh [amd64]
# 依赖：debootstrap squashfs-tools xorriso mtools grub-pc-bin grub-efi-amd64-bin
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib/common.sh"

ARCH_ARG="${1:-amd64}"
case "$ARCH_ARG" in
    amd64) DEB_ARCH="amd64"; GRUB_TARGETS="--modules=part_gpt part_msdos fat ext2 normal linux search search_label" ;;
    arm64) DEB_ARCH="arm64" ;;
    *) die "暂只支持 amd64 / arm64" ;;
esac

VERSION="$(cat "$LUTRA_ROOT/VERSION" 2>/dev/null || echo 1.0.0)"
CODENAME="Rill"
SUITE="${SUITE:-trixie}"
MIRROR="${MIRROR:-https://mirrors.tuna.tsinghua.edu.cn/debian}"
ROOTFS="$LUTRA_BUILD/iso-rootfs-$ARCH_ARG"
ISODIR="$LUTRA_BUILD/iso-staging-$ARCH_ARG"
IMG="$LUTRA_OUT/lutra-${VERSION}-${ARCH_ARG}.iso"
KERNEL_DEST="$KDEST/$ARCH_ARG"

require debootstrap
require mksquashfs
require xorriso
require grub-mkrescue

# ---------- 0. 前置检查 ----------
if ! ls "$KERNEL_DEST"/linux-image-*.deb >/dev/null 2>&1; then
    die "未找到内核 deb：$KERNEL_DEST（先运行 10-kernel.sh $ARCH_ARG）"
fi
if [ ! -f "$LUTRA_ROOT/installer/lutra-installer.py" ]; then
    die "缺少安装器：installer/lutra-installer.py"
fi

# ---------- 1. 根文件系统 ----------
if [ ! -x "$ROOTFS/bin/bash" ]; then
    log "debootstrap $SUITE -> $ROOTFS"
    rm -rf "$ROOTFS"
    debootstrap --arch="$DEB_ARCH" --variant=minbase \
        --include=ca-certificates,apt-utils \
        "$SUITE" "$ROOTFS" "$MIRROR"
fi

mount_chroot() {
    mount --bind /dev "$ROOTFS/dev"
    mount -t devpts devpts "$ROOTFS/dev/pts" 2>/dev/null || true
    mount -t proc proc "$ROOTFS/proc"
    mount -t sysfs sysfs "$ROOTFS/sys"
    mount --bind /run "$ROOTFS/run"
}
umount_chroot() {
    for m in run sys proc dev/pts dev; do
        umount -lf "$ROOTFS/$m" 2>/dev/null || true
    done
}
trap umount_chroot EXIT

cp /etc/resolv.conf "$ROOTFS/etc/resolv.conf"
mount_chroot

# ---------- 2. 软件包 ----------
PKGS=(
    systemd-sysv dbus sudo locales tzdata console-setup keyboard-configuration
    network-manager netbase iproute2 iputils-ping curl wget ca-certificates gnupg
    labwc wayland-utils xwayland seatd waybar foot pcmanfm libgtk4-layer-shell0
    libgtk-4-1 libgtk-4-bin libglib2.0-bin adwaita-icon-theme fonts-noto-cjk fonts-dejavu-core
    fuzzel mako-notifier wl-clipboard grim slurp swaybg
    pipewire pipewire-pulse wireplumber alsa-utils
    fcitx5 fcitx5-chinese-addons fcitx5-rime
    btrfs-progs e2fsprogs dosfstools parted gdisk util-linux
    squashfs-tools live-boot live-config live-config-systemd
    grub-common grub2-common grub-efi-amd64 os-prober efibootmgr
    openssh-server python3 python3-cryptography python3-yaml python3-gi gir1.2-gtk-4.0
    less nano vim-tiny rsync zstd xz-utils psmisc
    zram-tools earlyoom
)
log "在根文件系统中安装软件包（约 2-4 分钟）"
chroot "$ROOTFS" /bin/bash -c "
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends ${PKGS[*]}
" 2>&1 | tail -n 5

log "安装 Lutra 内核"
# 清掉历史内核，避免 ISO 误用没有光驱驱动的旧镜像
rm -rf "$ROOTFS/lib/modules/6.12.0"
rm -f "$ROOTFS/boot/vmlinuz-6.12.0" "$ROOTFS/boot/initrd.img-6.12.0"
cp "$KERNEL_DEST"/linux-image-*.deb "$ROOTFS/tmp/"
chroot "$ROOTFS" /bin/bash -c "dpkg -i /tmp/linux-image-*.deb || apt-get -f install -y"
rm -f "$ROOTFS"/tmp/linux-image-*.deb

# ---------- 3. 系统配置 ----------
log "写入 Live 系统配置"
cat > "$ROOTFS/etc/hostname" <<'EOF'
lutra-live
EOF
cat > "$ROOTFS/etc/hosts" <<'EOF'
127.0.0.1 localhost lutra-live
::1       localhost
EOF

# 默认用户
chroot "$ROOTFS" /bin/bash -c "
    getent group seat >/dev/null || groupadd -r seat
    id lutra >/dev/null 2>&1 || useradd -m -s /bin/bash -G sudo,audio,video,input,render,seat lutra
    echo 'lutra:lutra' | chpasswd
    echo 'lutra ALL=(ALL) NOPASSWD:ALL' > /etc/sudoers.d/90-lutra
    chmod 440 /etc/sudoers.d/90-lutra
"
mkdir -p "$ROOTFS/home/lutra"
chown 1000:1000 "$ROOTFS/home/lutra" 2>/dev/null || true

# 自动登录 tty1 并启动 labwc
mkdir -p "$ROOTFS/etc/systemd/system/getty@tty1.service.d"
cat > "$ROOTFS/etc/systemd/system/getty@tty1.service.d/autologin.conf" <<'EOF'
[Service]
ExecStart=
ExecStart=-/sbin/agetty --autologin lutra --noclear %I $TERM
EOF

cat > "$ROOTFS/home/lutra/.bash_profile" <<'EOF'
# Lutra Live：登录 tty1 直接进入桌面
if [ -z "$WAYLAND_DISPLAY" ] && [ "$(tty)" = "/dev/tty1" ]; then
    export XDG_SESSION_TYPE=wayland
    export XDG_CURRENT_DESKTOP=Lutra
    export MOZ_ENABLE_WAYLAND=1
    exec labwc
fi
EOF
chroot "$ROOTFS" chown lutra:lutra /home/lutra/.bash_profile

# 安装器与桌面入口
install -d "$ROOTFS/opt/lutra"
cp "$LUTRA_ROOT/installer/lutra-installer.py" "$ROOTFS/opt/lutra/"
cp "$LUTRA_ROOT/installer/lutra-installer-gui.py" "$ROOTFS/opt/lutra/" 2>/dev/null || true
chmod 755 "$ROOTFS/opt/lutra/lutra-installer.py"
[ -f "$ROOTFS/opt/lutra/lutra-installer-gui.py" ] && chmod 755 "$ROOTFS/opt/lutra/lutra-installer-gui.py"
cat > "$ROOTFS/usr/share/applications/lutra-installer.desktop" <<'EOF'
[Desktop Entry]
Type=Application
Name=安装 Lutra
Comment=将 Lutra 安装到磁盘（图形向导）
Exec=sudo -E /opt/lutra/lutra-installer-gui.py
Icon=drive-harddisk
Terminal=false
Categories=System;
EOF

# 桌面配置：labwc + waybar（lutra-shell 未编译时的可用兜底）
install -d "$ROOTFS/etc/xdg/labwc" "$ROOTFS/etc/xdg/waybar"
if [ -d "$LUTRA_ROOT/src/desktop/labwc" ]; then
    install -m 644 "$LUTRA_ROOT/src/desktop/labwc/rc.xml" "$ROOTFS/etc/xdg/labwc/rc.xml"
    install -m 755 "$LUTRA_ROOT/src/desktop/labwc/autostart" "$ROOTFS/etc/xdg/labwc/autostart"
    install -m 644 "$LUTRA_ROOT/src/desktop/labwc/environment" "$ROOTFS/etc/xdg/labwc/environment"
fi
if [ -d "$LUTRA_ROOT/src/desktop/waybar" ]; then
    install -m 644 "$LUTRA_ROOT/src/desktop/waybar/config.jsonc" "$ROOTFS/etc/xdg/waybar/config.jsonc"
    install -m 644 "$LUTRA_ROOT/src/desktop/waybar/style.css" "$ROOTFS/etc/xdg/waybar/style.css"
fi

# 自研桌面外壳（已编译时优先使用）
if [ -x "$LUTRA_ROOT/dist/lutra-shell/lutra-shell" ]; then
    install -Dm755 "$LUTRA_ROOT/dist/lutra-shell/lutra-shell" "$ROOTFS/usr/bin/lutra-shell"
    install -d "$ROOTFS/usr/share/lutra-shell"
    cp -a "$LUTRA_ROOT/dist/lutra-shell/icons" "$ROOTFS/usr/share/lutra-shell/" 2>/dev/null || true
    install -m644 "$LUTRA_ROOT/src/desktop/lutra-shell/data/lutra-shell.desktop" \
        "$ROOTFS/usr/share/applications/lutra-shell.desktop" 2>/dev/null || true
fi

# Live 环境允许密码登录（仅体验用途，用户 lutra/lutra）
sed -i 's/^#*PasswordAuthentication.*/PasswordAuthentication yes/' "$ROOTFS/etc/ssh/sshd_config" 2>/dev/null || true
grep -q '^PasswordAuthentication yes' "$ROOTFS/etc/ssh/sshd_config" 2>/dev/null || \
    echo 'PasswordAuthentication yes' >> "$ROOTFS/etc/ssh/sshd_config"
if [ -f "$LUTRA_ROOT/installer/lutra-firstboot.sh" ]; then
    install -m 755 "$LUTRA_ROOT/installer/lutra-firstboot.sh" "$ROOTFS/opt/lutra/"
fi

# live-boot 配置
mkdir -p "$ROOTFS/etc/live"
cat > "$ROOTFS/etc/live/config.conf" <<'EOF'
LIVE_HOSTNAME="lutra-live"
LIVE_USERNAME="lutra"
LIVE_USER_FULLNAME="Lutra User"
LIVE_LOCALES="zh_CN.UTF-8 en_US.UTF-8"
LIVE_TIMEZONE="Asia/Shanghai"
LIVE_KEYBOARD_LAYOUTS="us"
EOF

# 强制把 VMware / VirtualBox 显卡驱动打进 initramfs，避免虚拟机里黑屏
mkdir -p "$ROOTFS/etc/initramfs-tools"
for m in vmwgfx bochs-drm; do
    grep -qx "$m" "$ROOTFS/etc/initramfs-tools/modules" 2>/dev/null || echo "$m" >> "$ROOTFS/etc/initramfs-tools/modules"
done

chroot "$ROOTFS" /bin/bash -c "update-initramfs -u -k all" 2>&1 | tail -n 3

# 中文 locale（避免 GTK 回退到 C locale）
chroot "$ROOTFS" /bin/bash -c "sed -i 's/^# *zh_CN.UTF-8/zh_CN.UTF-8/' /etc/locale.gen; locale-gen >/dev/null 2>&1 || true"

# ---------- 4. squashfs ----------
# 先卸载 chroot 的伪文件系统，否则 mksquashfs 会卡在 /proc、/sys
log "卸载 chroot 挂载点"
umount_chroot
mkdir -p "$ROOTFS/dev" "$ROOTFS/proc" "$ROOTFS/sys" "$ROOTFS/run" "$ROOTFS/tmp"

log "生成 squashfs"
rm -rf "$ISODIR"
mkdir -p "$ISODIR/live" "$ISODIR/boot/grub" "$ISODIR/LUTRA"
mksquashfs "$ROOTFS" "$ISODIR/live/filesystem.squashfs" \
    -comp zstd -Xcompression-level 9 -b 1M -noappend \
    -e boot 2>&1 | tail -n 3

KVER_IMG="$(ls -1 "$ROOTFS"/boot/vmlinuz-*lutra* 2>/dev/null | sort -V | tail -1)"
INITRD_IMG="$(ls -1 "$ROOTFS"/boot/initrd.img-*lutra* 2>/dev/null | sort -V | tail -1)"
[ -n "$KVER_IMG" ] && [ -n "$INITRD_IMG" ] || die "根文件系统里没有 Lutra 内核镜像"
cp "$KVER_IMG" "$ISODIR/live/vmlinuz"
cp "$INITRD_IMG" "$ISODIR/live/initrd.img"
cp "$LUTRA_ROOT/installer/lutra-installer.py" "$ISODIR/LUTRA/"

# 安装器所需的压缩根文件系统（安装时直接解包，无需联网）
log "生成安装用 rootfs.tzst"
tar -C "$ROOTFS" --exclude='./proc/*' --exclude='./sys/*' --exclude='./dev/*' \
    --exclude='./run/*' --exclude='./tmp/*' -cf - . | zstd -12 -T0 -o "$ISODIR/LUTRA/rootfs.tar.zst"

cat > "$ISODIR/boot/grub/grub.cfg" <<EOF
set default=0
set timeout=8

menuentry "Lutra ${VERSION} \"${CODENAME}\" (Live)" {
    linux /live/vmlinuz boot=live components quiet splash console=tty0 console=ttyS0,115200
    initrd /live/initrd.img
}
menuentry "Lutra ${VERSION} (Live, safe graphics)" {
    linux /live/vmlinuz boot=live components nomodeset console=tty0 console=ttyS0,115200
    initrd /live/initrd.img
}
menuentry "Lutra ${VERSION} (Install)" {
    linux /live/vmlinuz boot=live components quiet console=tty0 console=ttyS0,115200
    initrd /live/initrd.img
}
menuentry "从本地磁盘启动" {
    exit
}
EOF

# 卷标用于 live-boot 定位与 U 盘持久化
LABEL="LUTRA_${VERSION//./_}"
log "打包 ISO：$IMG"
mkdir -p "$(dirname "$IMG")"
grub-mkrescue -o "$IMG" "$ISODIR" \
    -- -volid "$LABEL" 2>&1 | tail -n 5

sha256sum "$IMG" > "$IMG.sha256"
ls -lh "$IMG" "$IMG.sha256"
log "完成：$IMG"
