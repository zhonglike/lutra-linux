# 03 · 多平台与 Windows 桥接

> 本文定义七种目标形态的落地方式、Windows 侧桥接程序的设计、资源预算与风险清单。

---

## 1. 平台矩阵

| # | 形态 | 本质 | 构建方式 | 产物 | 优先级 |
|---|---|---|---|---|---|
| 1 | x86_64 裸机 | 完整发行版 | 原生编译 | `*.iso` | **P0** |
| 2 | ARM64 通用 | 完整发行版 | 交叉编译 | `*.img.xz` | P1 |
| 3 | 树莓派 4/5 | 完整发行版 | 交叉编译 + 专用 firmware/dtb | `*.img.xz` | P1 |
| 4 | i686 | Legacy 最小底座 | 原生编译（32 位 VM） | `*.iso` | P2 |
| 5 | Waydroid | Linux 上跑安卓 app | 打包 + 内核 binder | `waydroid-image` 集成 | **P1（用户指定优先）** |
| 6 | Termux 容器 | 安卓上跑 Linux 用户空间 | proot-distro 根文件系统 | `lutra-rootfs-*.tar.zst` | P2 |
| 7 | postmarketOS | 刷机替换安卓 | 独立内核 + 设备树 | 按机型 | P3（风险最高） |

---

## 2. 各形态落地要点

### 2.1 ARM64 通用

- 用 `crossbuild-essential-arm64` + `qemu-user-static` 做 chroot 构建
- 内核 `make ARCH=arm64 CROSS_COMPILE=aarch64-linux-gnu-`
- 镜像用 `genimage` 生成 GPT + FAT32 boot + ext4/Btrfs root
- 引导：U-Boot（多数开发板）/ UEFI（服务器级 ARM）

### 2.2 树莓派

- 额外打包 `raspi-firmware`（`start4.elf`、`fixup4.dat`、`bootcode.bin`）与全量 `dtb`
- `config.txt` 预置 `dtoverlay=vc4-kms-v3d`（KMS 驱动，Wayland 必需）
- 树莓派 5 需 `dtoverlay=pcie-32bit-dma`
- 默认启用 zram（2/4 GB 机型内存紧张）

### 2.3 i686（Lutra Legacy）

| 项 | 定位 |
|---|---|
| 目标硬件 | 老 Atom 上网本、2008~2014 年 PC |
| 桌面 | labwc + 自研 shell（不含 Firefox 之外的现代应用） |
| 智能层 | **不提供**（Node 无官方 ia32 构建，Rust 工具链已放弃 32 位） |
| 商店 | 只提供 x86 架构包，AI 分类整体隐藏 |
| 内核 | `CONFIG_X86_32`，关闭 `CONFIG_X86_64`，禁用大内存特化 |
| 引导 | Legacy BIOS 为主；32 位 UEFI 作为可选实验项 |

> 这不是妥协，而是上游生态的客观事实。i686 版本的存在意义是「让老机器还能上网、能办公」，不承诺 AI 能力。

### 2.4 Waydroid（用户指定优先）

| 项 | 配置 |
|---|---|
| 内核依赖 | `CONFIG_ANDROID_BINDER_IPC`、`CONFIG_ANDROID_BINDERFS`、`CONFIG_ASHMEM` |
| 网络 | `nftables` + `iptables-nft`，Waydroid 自带 NAT 规则 |
| 镜像 | `lineage-20.0-*-vanilla.img`（无 GApps）作为默认；GApps 版作为商店可选项 |
| 会话 | Waydroid 以独立 Wayland 会话运行，通过 `waydroid show-full-ui` 调用 |
| 权限对齐 | 安卓 app 的权限申请映射到 Lutra 权限枚举，走同一套 `lutra-guard` |
| 限制 | 仅 x86_64 与 ARM64；i686 不支持 |

集成步骤：

1. 内核启用 binder，写入 `desktop-{amd64,arm64}.config`
2. 基础镜像安装 `waydroid` 包与 `lxc`
3. 首次启动自动执行 `waydroid init`（需 `net` 权限，走会话授权）
4. 在 `lutra-shell` 增加「安卓应用」入口

### 2.5 Termux 容器

- 产出 `lutra-rootfs-<arch>.tar.zst`，供 `proot-distro` 导入
- 复用同一套用户空间与 AI CLI，体积约 350 MB
- 限制：无 systemd、无内核模块、无 Wayland 原生；仅 CLI 与 X11 转发
- 定位：「在手机上用 Lutra 的 AI 工具链」，不是完整系统

### 2.6 postmarketOS

- 复用 pmOS 的设备树与内核补丁，替换用户空间为 Lutra
- 需要 `pmbootstrap` 参与，构建链与主线不同
- **风险**：机型支持严重受限，需先确认目标机型在 pmOS 支持列表内
- 建议：先只做 1~2 个社区支持良好的机型，不做通用承诺

---

## 3. Windows 桥接

> 双向控制通道与 MCP Server 的完整设计见 `04-bidirectional-bridge.md`。本章只保留与平台相关的部分。

### 3.1 目录结构

```
win/
├─ lutra-host/              主程序（Rust + Tauri 或 C# WPF）
│  ├─ ui/                   托盘、窗口、授权弹窗
│  ├─ vm/                   QEMU 进程管理与参数拼装
│  ├─ drop/                 投递分区监听与同步
│  └─ broker/               Windows 侧权限代理
├─ qemu/                    便携 QEMU（运行时下载到 M 盘）
└─ installer/               Inno Setup 打包脚本（可选）
```

### 3.2 两种运行模式

| 模式 | 方式 | 适用 |
|---|---|---|
| **虚拟磁盘模式** | QEMU 挂载 `M:\lutraos\_vm\lutra.vhdx` | 日常开发、无 U 盘时 |
| **物理分区直通** | QEMU 挂载 `\\.\PhysicalDriveN` | 直接使用 U 盘上已安装的系统 |

虚拟磁盘模式启动参数：

```powershell
qemu-system-x86_64.exe -accel whpx,kernel-irqchip=on `
  -smp 4 -m 8192 `
  -drive file="M:\lutraos\_vm\lutra.vhdx",if=virtio,format=vhdx `
  -virtfs local,path="L:\",mount_tag=drop,security_model=none `
  -nic user,model=virtio-net-pci `
  -spice port=5930,disable-ticketing=on
```

- **必须使用 SPICE 而非默认窗口**：SPICE 支持剪贴板双向、USB 重定向、动态分辨率，是「像本地系统一样用」的关键。
- WHPX 不可用时自动降级 TCG，并在 UI 上明确提示性能损失。

### 3.3 投递分区同步

| 环节 | 实现 |
|---|---|
| Windows 侧 | `FileSystemWatcher` 监听拖入事件；托盘图标接收拖拽 |
| 存储 | 写入 exFAT 分区（卷标 `LUTRA`），或虚拟机模式写入 virtiofs 共享目录 |
| Linux 侧 | `systemd.path` 单元监听 `/.lutra-drop`；新文件自动复制到 `~/Downloads/from-win` |
| 反馈 | 写入成功后在 Windows 托盘气泡提示，Linux 侧发 mako 通知 |

### 3.4 授权弹窗（对齐 UAC 体验）

必须说清楚一件事：**我们无法替代 Windows 系统的 UAC**，真正的提权弹窗只能由 Windows 自己产生。Lutra 能做的是**在 AI 发起高危操作前插入一次人工确认**，视觉与交互对齐 UAC。

| 场景 | 处理 |
|---|---|
| AI 要读 Windows 用户文件 | `lutra-host` 弹出确认窗（会话级，一次授权，TTL 2h） |
| AI 要执行 Windows 命令 | 同上，并显示完整命令行 |
| AI 需要管理员权限 | 无法绕过，必须由 Windows UAC 弹窗，用户手动确认 |
| 拒绝 | 返回错误码，记入审计日志，AI 收到明确拒绝原因 |

弹窗要素：操作描述、影响范围、命令行原文、倒计时、`[拒绝]` `[本次允许]` `[本会话允许]`。**不提供「永久允许」**。

### 3.5 引导与多系统

| 安装模式 | 引导处理 |
|---|---|
| **整盘安装** | 独占磁盘，GRUB 只含 Lutra。若磁盘原有 Windows，执行前明确警告「将删除整盘上的 Windows 与全部数据」，并要求输入磁盘型号确认 |
| **与 Windows 共存** | 写入 GRUB 并自动探测 Windows Boot Manager，加入菜单；顺序 Windows 在前，避免用户误以为系统装坏 |
| **U 盘便携** | 只写 U 盘自身的 ESP，不改内置磁盘引导项 |

- 共存模式提供「重启进入 Windows」桌面快捷方式（`systemctl reboot --boot-loader-entry=auto-windows`）
- Windows 快速启动（Fast Startup）会让 NTFS 分区处于脏状态，安装器检测到后提示先在 Windows 中关闭
- BitLocker 开启时分区缩容可能失败，安装器提示先在 Windows 中暂停 BitLocker 保护
- 整盘安装完成后，首次启动自动执行 `btrfs subvolume snapshot` 建立初始快照，便于回滚

---

## 4. 资源预算

| 场景 | 内存 | 存储 |
|---|---|---|
| Live 试用（无 AI） | 2 GB | — |
| 最小安装（桌面 + 终端） | 2 GB | 20 GB |
| 日常（+ 浏览器） | 4 GB | 25 GB |
| AI 基础（Claude Code / OpenCode） | 4 GB | 28 GB |
| AI 完整（+ Playwright + Browser Use） | 8 GB | 35 GB |
| AI + ArtCraft 全家桶 | 8 GB | 42 GB |
| 开发（+ 内核源码 + 工具链） | 16 GB | 90 GB |

Windows 侧虚拟化建议留 8 GB 给 VM、2 GB 给宿主；宿主机总内存 16 GB 起步。

---

## 5. 风险清单

| # | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R1 | ArtCraft 七件套仍是 early alpha | 崩溃、丢工程 | 标注「实验性」，默认不预装，商店单独警示 |
| R2 | postmarketOS 机型支持差 | 刷机失败变砖 | 只支持白名单机型，安装前做强校验 |
| R3 | i686 无法运行 AI | 用户预期落空 | 镜像名与文档显式标注 `Legacy · 无 AI` |
| R4 | WHPX 不可用 | 虚拟化性能骤降 | 自动检测并提示，提供 TCG 说明 |
| R5 | C 盘仅剩 1.1 GB | 任何误写都会导致系统异常 | 所有工具链走 `env\set-cache-dirs.ps1`；禁用 WSL/Docker |
| R6 | 非 ASCII 路径 | 内核/工具链构建失败 | 工程根固定 `M:\lutraos` |
| R7 | 供应链投毒 | 恶意包获取系统权限 | 全链路 Ed25519 签名；第三方包标「未验证」 |
| R8 | U 盘未加密丢失 | 数据泄露 | 文档明示风险，安装器提供一键启用 LUKS2 |
| R9 | Btrfs 上的 vhdx 碎片 | 虚拟机性能下降 | 镜像目录 `chattr +C` |
| R10 | 会话令牌被盗用 | 越权 | `tmpfs` 存放、TTL 上限 8 小时、进程间 SCM_RIGHTS 传递 |

---

## 6. 交付顺序建议

```
M0 骨架 ✅
 → M1 x86_64 内核可启动
 → M2 x86_64 Live ISO + 安装器
 → M3 桌面 + 默认应用
 → M4 智能层 + .ltr + 商店（含 ArtCraft 接入）
 → M5 Windows 桥接 + 双启动
 → M6 ARM64 → Waydroid → i686 → Termux → postmarketOS
```

Waydroid 虽然被指定为安卓侧优先项，但它依赖 M4 的权限映射与 M1 的内核 binder 配置，因此在时间线上自然落在 M6 首位。
