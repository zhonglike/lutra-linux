# Lutra Linux

> 一只会使用工具的水獭，钻得进任何一台机器。

Lutra 是一套从内核开始自建的轻量 Linux 发行版，面向「AI 原生」使用方式：系统本身就为 Agent 而生，能装、能跑、能被 AI 安全地操控。

- **代号**：LTR
- **吉祥物**：Lut（水獭）
- **当前版本**：`1.0.0 "Rill"`（源码发行）
- **工程根**：`M:\lutraos`（全部缓存与构建产物均在 M 盘）

---

## 设计目标

| 维度 | 目标 |
|---|---|
| 内存 | 2 GB 可用；4 GB 舒适；8 GB 运行完整 AI 套件 |
| 存储 | 20 GB 最小安装；40 GB 完整 AI 套件 |
| 架构 | x86_64（主线）→ ARM64（树莓派等）→ i686（Legacy，不含 AI） |
| 桌面 | 轻量 Wayland（labwc）+ 自研 GTK4 shell |
| 形态 | Live ISO（只读可试用）、可安装、可装入 U 盘、可与 Windows 共存 |
| 双栖 | U 盘裸机启动，或经由 Windows 侧便携 QEMU 虚拟化运行 |
| 智能 | 内置 Agent 运行时、MCP 工具集、会话级权限网关 |
| 更新 | `.lup` 升级包，三级分级，Btrfs 快照回滚 |

## 技术底座

- 基于 Debian trixie 的用户空间，内核自行编译（不直接使用发行版内核）
- 引导：GRUB2，UEFI + Legacy BIOS 双支持
- 文件系统：Btrfs（子卷 `@` / `@home` / `@snapshots` / `@var`），可选 LUKS2
- 显示：Wayland；音频：PipeWire；网络：NetworkManager

## 仓库结构

```
lutraos/
├─ src/
│  ├─ kernel/      内核配置碎片（base / desktop-<arch> / virt）
│  ├─ desktop/     labwc 配置 + GTK4 lutra-shell（Rust）
│  ├─ agent/       智能层 Rust workspace（lutrad / mcpd / audit / ltr）
│  ├─ update/      更新系统（lutra-update、lup-pack、systemd 单元）
│  └─ platforms/   树莓派镜像、Waydroid
├─ installer/      Lutra 安装器（整盘 / 共存 / U 盘 / 手动）
├─ win/            Windows 侧 lutra-bridge（托盘 + MCP + QEMU + SSH）
├─ build/          内核编译、Live ISO 构建脚本
├─ design/         桌面 UI 设计稿
├─ docs/           设计文档
├─ env/            构建环境（便携 QEMU、cloud-init、缓存重定向）
├─ dist/           产物（不入库）
├─ _vm/            构建虚拟机磁盘与种子盘（不入库）
├─ _cache/         所有缓存（不入库）
└─ _toolchains/    便携工具链（不入库）
```

## 快速开始

### 1. 构建虚拟机

```powershell
# 首次：生成 cloud-init 种子盘（只需一次）
python .\env\make-seed.py

# 启动构建机（SSH 127.0.0.1:2222，用户 lutra / 密码 lutra）
.\env\run-vm.ps1 -Mode Cloud -Disk 'M:\lutraos\_vm\build-cloud.qcow2'
```

### 2. 编译内核

```bash
# 在构建机内
cd ~/lutra
./build/10-kernel.sh amd64     # 或 arm64 / i686
ls dist/kernel/amd64/
```

### 3. 生成 Live ISO

```bash
sudo ./build/20-iso.sh amd64
```

### 4. 安装

```bash
# 先看磁盘分类，空盘/有数据/含系统一目了然
sudo ./lutra-installer.py --list

# 图形向导（Live 桌面顶栏「安装」按钮，或直接运行）
sudo ./lutra-installer-gui.py

# 整盘安装（非空盘会拒绝，除非显式 --force-nonempty）
sudo ./lutra-installer.py --disk /dev/nvme0n1 --mode erase \
     --rootfs /run/lutra/rootfs.tar.zst

# 与 Windows 共存：只使用未分配空间，绝不缩容现有分区
sudo ./lutra-installer.py --disk /dev/nvme0n1 --mode coexist \
     --rootfs /run/lutra/rootfs.tar.zst
```

## 里程碑

- [x] **M0** 工程骨架、缓存全量重定向到 M 盘
- [x] **M1** 自编内核可启动：`6.12.0-lutra` 在 QEMU 内启动到 multi-user，网络与 SSH 正常（`uname -r` 已验证）
- [x] **M2** Live ISO 已构建并实测：可启动到登录；安装器空盘/非空盘分类与拒绝逻辑验证通过
- [x] **M3** 桌面环境：labwc + GTK4 lutra-shell（已在构建机编译并在 Live 中验证）
- [x] **M4** 智能层：Agent 运行时、权限网关、`.ltr` 包管理、审计
- [x] **M5** Windows 桥接：`lutra-bridge`（托盘 + MCP Server + 便携 QEMU + 文件投递）
- [x] **M6** 多架构：ARM64 配置与树莓派镜像脚本、i686 配置、Waydroid
- [x] **M7** 系统更新：`.lup` 升级包、开机检查、安全更新强提示、快照回滚（已演练闭环）

## 构建环境约束

本项目**只允许在 M 盘构建**，所有缓存必须重定向，不得写入 C 盘：

```powershell
. .\env\set-cache-dirs.ps1
```

同时禁止在本机安装 WSL 与 Docker Desktop（二者强制写入系统盘）。

## 许可

本项目自有代码采用 MIT；内核部分遵循 GPL-2.0。`docs/` 与 `design/` 采用 CC BY 4.0。
