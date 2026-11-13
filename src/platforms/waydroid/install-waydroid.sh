#!/bin/bash
# 在 Lutra 上安装 Waydroid（安卓容器）
# 前置：内核已启用 CONFIG_ANDROID_BINDERFS / CONFIG_ANDROID_BINDER_IPC
set -euo pipefail

if [ "${XDG_SESSION_TYPE:-}" != "wayland" ]; then
    echo "Waydroid 需要 Wayland 会话，请在 Lutra 桌面内运行" >&2
    exit 1
fi
if ! grep -q binder /proc/filesystems; then
    echo "内核未启用 binderfs，请使用 Lutra 自带内核（含 CONFIG_ANDROID_BINDERFS）" >&2
    exit 1
fi

sudo apt-get update
sudo apt-get install -y curl ca-certificates python3

curl -sS https://repo.waydro.id | sudo bash
sudo apt-get install -y waydroid

# 初始化（默认不带 GApps；需要谷歌服务改为 -s GAPPS）
if [ ! -f /var/lib/waydroid/waydroid.cfg ]; then
    sudo waydroid init
fi

sudo systemctl enable --now waydroid-container

echo "安装完成。请在应用菜单中启动「Waydroid」，或用命令：waydroid show-full-ui"
