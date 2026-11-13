# 01 · 构建系统与镜像

> 本文定义 Lutra 的构建主机、内核编译、根文件系统组装、分区方案与 ISO 产物规范。

---

## 1. 构建主机

### 1.1 物理主机

| 项 | 实测值 |
|---|---|
| 系统 | Windows 11 |
| CPU | Intel i5-1240P，12 核 16 线程 |
| 内存 | **15.7 GB 可用** → 分配给构建 VM 8 GB，宿主保留约 7 GB |
| 磁盘 | M 盘空闲 626 GB |
| 加速 | WHPX 实测可用 |
| 约束 | **禁止安装 WSL2 与 Docker Desktop**，二者强制写 C 盘 |

### 1.2 构建虚拟机

| 项 | 配置 |
|---|---|
| 虚拟化 | QEMU 11.1.0 便携版，已解压至 `M:\lutraos\_toolchains\qemu\app` |
| 磁盘 | `M:\lutraos\_vm\build.vhdx`，动态扩展，上限 120 GB |
| vCPU | 8（宿主 16 线程，留 8 给 Windows） |
| 内存 | 8 GB（宿主内存仅 15.7 GB，不可再高） |
| 网络 | user-mode NAT，宿主地址 `10.0.2.2` |
| 访客系统 | Debian trixie amd64（preseed 无人值守安装） |
| 加速 | WHPX；不可用时回退 TCG（性能降至约 1/4） |

> 8 GB 内存下编译内核需配合 VM 内的 zram，且 `-j` 不超过 8，否则链接阶段有 OOM 风险。

启动命令示例：

```powershell
& 'M:\lutraos\_toolchains\qemu\qemu-system-x86_64.exe' `
  -accel whpx,kernel-irqchip=on `
  -smp 8 -m 16384 `
  -drive file='M:\lutraos\_vm\build.vhdx',if=virtio,format=vhdx `
  -nic user,model=virtio-net-pci `
  -display default
```

### 1.3 缓存重定向

Windows 侧必须先执行 `env\set-cache-dirs.ps1`，把 `TEMP`、`CCACHE_DIR` 等指向 M 盘。
访客 VM 内同样需要把 `/var/cache/apt`、`~/.ccache` 显式放在项目盘挂载点上，避免 vhdx 无限膨胀。

---

## 2. 内核构建

### 2.1 版本与来源

| 项 | 值 |
|---|---|
| 版本 | Linux 6.12 LTS（`linux-stable` 分支） |
| 补丁集 | `patches/` 目录，按功能分类，用 `quilt` 管理 |
| 产出 | `linux-image-6.12.x-lutra_*.deb` + `linux-headers-*` |
| 版本后缀 | `LOCALVERSION=-lutra` |

### 2.2 配置策略

不手工维护单一 `.config`，改用 **基线 + 碎片** 三段式：

```
src/kernel/
├─ configs/
│  ├─ base.config        # defconfig 精简后的公共基线
│  ├─ desktop-amd64.config
│  ├─ desktop-arm64.config
│  ├─ legacy-i686.config
│  └─ virt.config        # 虚拟化必需项（与上面任意一份叠加）
└─ patches/
```

合并命令：

```bash
./scripts/kconfig/merge_config.sh -m base.config desktop-amd64.config virt.config
make olddefconfig
```

### 2.3 必须显式开启的关键项

| 分类 | 配置项 | 用途 |
|---|---|---|
| 压缩内存 | `CONFIG_ZRAM`、`CONFIG_ZSMALLOC`、`CONFIG_ZSTD_COMPRESS` | 虚拟内存方案基础 |
| 文件系统 | `CONFIG_BTRFS_FS`、`CONFIG_SQUASHFS`、`CONFIG_OVERLAY_FS`、`CONFIG_FUSE_FS` | 根分区与 Live 模式 |
| 加密 | `CONFIG_DM_CRYPT`、`CONFIG_DM_INTEGRITY` | LUKS2 可选开启 |
| 无线 | `CONFIG_CFG80211`、`CONFIG_MAC80211`、`CONFIG_IWLWIFI`、`CONFIG_RTW88`、`CONFIG_ATH11K`、`CONFIG_MT76` | WiFi 自适应 |
| 蓝牙 | `CONFIG_BT`、`CONFIG_BT_HCIBTUSB`、`CONFIG_BT_HCIUART` | 蓝牙键盘/耳机 |
| 显卡 | `CONFIG_DRM_I915`、`CONFIG_DRM_AMDGPU`、`CONFIG_DRM_NOUVEAU`、`CONFIG_DRM_VIRTIO_GPU` | 覆盖主流核显与虚拟显卡 |
| 虚拟化 | `CONFIG_VIRTIO_PCI`、`CONFIG_VIRTIO_BLK`、`CONFIG_VIRTIO_NET`、`CONFIG_VIRTIO_FS`、`CONFIG_9P_FS` | QEMU 内运行 + 文件共享 |
| 安卓容器 | `CONFIG_ANDROID_BINDER_IPC`、`CONFIG_ANDROID_BINDERFS`、`CONFIG_ASHMEM` | Waydroid 必需 |
| 安全 | `CONFIG_SECCOMP`、`CONFIG_SECCOMP_FILTER`、`CONFIG_CGROUP_BPF`、`CONFIG_BPF_SYSCALL` | Agent 沙箱与权限网关 |
| 内存管理 | `CONFIG_PSI`、`CONFIG_MEMCG`、`CONFIG_SWAP` | zram 调优与 OOM 策略 |

### 2.4 编译命令

```bash
export CCACHE_DIR=/mnt/project/_cache/ccache
make -j"$(nproc)" CC="ccache gcc" bindeb-pkg LOCALVERSION=-lutra
```

- 单次全量编译约需 18~25 GB 临时空间，必须放在 vhdx 内或挂载的项目盘上。
- 链接阶段峰值内存约 4~6 GB（`-j8` 时），16 GB 内存充裕。
- 首次启用 ccache 后，二次增量构建可缩短到原来的 10%~20%。

---

## 3. 根文件系统

### 3.1 基础构建

```bash
debootstrap --arch=amd64 --variant=minbase trixie rootfs https://deb.debian.org/debian
```

采用 **Debian 官方源**（用户指定），不使用国内镜像。

### 3.2 包清单分层

| 清单 | 内容 | 目标体积 |
|---|---|---|
| `base.list` | systemd、dbus、NetworkManager、PipeWire、Btrfs 工具、dracut | ≈ 420 MB |
| `desktop.list` | labwc、waybar、foot、pcmanfm-qt、fcitx5-rime、字体 | ≈ 380 MB |
| `lutra.list` | lutra-shell、lutrad、lutra-guard、ltrpkg、lutra-store | ≈ 90 MB |
| `ai.list` | Node 22、Python 3.12、Firefox、Playwright 依赖、Git | ≈ 1.4 GB |
| `firmware.list` | `firmware-linux-free`、`firmware-iwlwifi`、`firmware-realtek`、`firmware-atheros` | ≈ 210 MB |

> 最小安装（base + desktop + lutra + firmware）约 **1.1 GB 压缩后 / 3.2 GB 展开**，满足 20 GB 存储目标（含 Btrfs 快照余量）。

### 3.3 精简手段

1. `apt-mark minimize-manual` + `apt-get autoremove --purge`
2. 删除 `/usr/share/doc/*`、`/usr/share/man/*`（保留 man 索引占位）
3. 语言包仅保留 `zh_CN` 与 `en_US`
4. 固件按平台拆分，arm64 镜像不打包 Intel 无线固件

---

## 4. Live ISO

### 4.1 构建工具

使用 `live-build`（Debian 官方工具链），配置目录：

```
build/live/
├─ config/
│  ├─ package-lists/        # 引用第 3.2 节清单
│  ├─ includes.chroot/      # lutra 自研组件与主题
│  ├─ hooks/
│  │  ├─ 0100-lutra-branding.hook.chroot
│  │  ├─ 0200-zram-tune.hook.chroot
│  │  └─ 0300-live-lockdown.hook.chroot
│  ├─ bootloaders/          # grub-efi + syslinux（Legacy）
│  └─ archives/             # Debian 官方源配置
└─ auto/
```

### 4.2 Live 运行模型

| 项 | 实现 |
|---|---|
| 根文件系统 | squashfs 只读，`CONFIG_SQUASHFS_XZ` |
| 可写层 | tmpfs overlay（断电即失） |
| 内存占用 | 基础 2 GB；建议 4 GB |
| 持久化 | **不提供**（符合「试用」定位） |

### 4.3 「可试用、不可安装应用」的实现

这是用户的明确要求，按 `0300-live-lockdown.hook.chroot` 落地：

1. `ltrpkg` 在检测到 `/run/lutra/live` 标志时，`install` 子命令直接返回错误码 75，提示「Live 模式不支持安装，请先安装到磁盘」。
2. `lutra-store` 客户端读取同一标志，把「安装」按钮置灰并显示原因。
3. `apt` 通过 `/etc/apt/apt.conf.d/99lutra-live` 强制 `Acquire::AllowInsecureRepositories "false"` 并锁定写入路径。
4. 允许的例外：浏览器缓存、临时文件、终端内的一次性下载（写入 tmpfs，不持久）。

---

## 5. 磁盘布局与安装模式

> 安装器不是「只能装 U 盘」。Lutra 支持把系统装进台式机主硬盘作为唯一主系统，也支持与 Windows 共存，也支持 U 盘便携。**分区方案由用户在安装时决定，安装器只提供预设，不强制。**

### 5.1 四种安装模式（安装器首屏）

| 模式 | 行为 | 目标场景 |
|---|---|---|
| **整盘安装** | 抹除所选磁盘全部内容，Lutra 独占该盘 | 台式机主系统、旧机改造、专用机 |
| **与 Windows 共存** | 保留现有 Windows，在空闲空间或缩容空间中安装，自动配置 GRUB 双启动 | 日常主力机双系统 |
| **U 盘便携** | 只写入所选 U 盘，内置磁盘零改动 | 随身系统、救援盘 |
| **自定义分区** | 进入分区器，手动指定每个分区与挂载点 | 高级用户、多系统、LVM / RAID / 加密 |

默认高亮「整盘安装」，但**不预设任何结果**：必须先选中目标磁盘，并在执行前做一次带磁盘型号的二次确认。

### 5.2 三种预设布局（仅供参考，可逐项修改）

| 预设 | ESP | swap | 根 | 投递分区 |
|---|---|---|---|---|
| 整盘（≥20 GB） | 512 MB FAT32 | 内存 ≤8 GB 时等大，>8 GB 时 8 GB | 剩余空间 Btrfs | 不创建 |
| 双系统 | 复用现有 ESP | 不创建（zram + swapfile） | 用户指定的空闲区 | 不创建 |
| U 盘便携 | 512 MB FAT32 | 不创建（zram） | 剩余空间 Btrfs | 4 GB exFAT，卷标 `LUTRA` |

> **关键修正**：投递分区只在「需要从 Windows 往系统里拖文件」的场景创建（U 盘便携、移动硬盘共存）。整盘安装到台式机内置硬盘时**不创建投递分区**，也没有任何分区对 Windows 可见。

### 5.3 自定义分区器能力

| 能力 | 支持内容 |
|---|---|
| 文件系统 | Btrfs / ext4 / XFS |
| 加密 | LUKS2（整盘或单分区，默认关闭可选开启） |
| 卷管理 | LVM、mdadm RAID 0/1/5/10 |
| 引导 | UEFI / Legacy，可选独立 `/boot` |
| 挂载点 | `/`、`/home`、`/var`、`/boot`、`/boot/efi`、swap |
| 实时校验 | 容量不足、缺 ESP、未设根分区时禁止下一步 |
| 危险检测 | BitLocker 加密分区、Windows 快速启动未关闭、NTFS 脏标志 |
| 执行前预览 | 完整列出「将删除 / 创建 / 格式化」的分区清单，需输入磁盘型号确认 |

### 5.4 Windows 分区可见性（仅共存与 U 盘场景）

只有在这两种模式下才存在「Windows 能看到哪些分区」的问题。整盘安装时，Windows 根本不存在于该磁盘。

三重保险：

1. **类型 GUID**：Linux 分区使用 `0FC63DAF-…`，Windows 不识别 Btrfs/ext4，只显示为未知分区，不分配盘符。
2. **diskpart 属性**：
   ```cmd
   diskpart
   select volume N
   attributes volume set nodefaultdriveletter
   ```
3. **注册表**（可选，管理员）：
   ```
   HKLM\SYSTEM\CurrentControlSet\Services\partmgr\Parameters\SanPolicy = 4
   ```

> Windows 可能对未知分区弹出「需要格式化」提示。安装完成后会写入一条说明文档，提示用户点「取消」而非「格式化」。

### 5.5 Btrfs 子卷

| 子卷 | 挂载点 | 挂载选项 |
|---|---|---|
| `@` | `/` | `compress=zstd:1,noatime` |
| `@home` | `/home` | `compress=zstd:1,noatime` |
| `@var` | `/var` | `compress=zstd:1,noatime` |
| `@snapshots` | `/.snapshots` | — |
| `@drop` | `/.lutra-drop` | 仅当存在投递分区时绑定挂载 |

虚拟机镜像目录若放在 Btrfs 上，需对 `*.vhdx` 所在目录设 `chattr +C` 关闭 CoW。

---

## 6. 三架构产物

| 架构 | 引导 | 内核 config | 产物 | 备注 |
|---|---|---|---|---|
| x86_64 | UEFI x86_64 + Legacy BIOS | `desktop-amd64` | `lutra-1.0-rill-amd64.iso` | 主线 |
| ARM64 | UEFI arm64 + U-Boot | `desktop-arm64` | `lutra-1.0-rill-arm64.img.xz` | 树莓派另有专用镜像，附 `dtb` 与 `config.txt` |
| i686 | Legacy BIOS（32 位 UEFI 少见） | `legacy-i686` | `lutra-1.0-rill-i686.iso` | 不含 AI 套件，见 03 文档 |

### 6.1 产物校验

每个产物同时生成：

```
lutra-1.0-rill-amd64.iso
lutra-1.0-rill-amd64.iso.sha256
lutra-1.0-rill-amd64.iso.sig      # Ed25519，与 .ltr 包共用公钥体系
```

---

## 7. 构建脚本规划

```
build/
├─ build.sh              # 总入口：build.sh <arch> <stage>
├─ 00-fetch.sh           # 拉源码、内核、Debian 源
├─ 10-kernel.sh          # 编译内核并生成 deb
├─ 20-rootfs.sh          # debootstrap + 包安装 + 精简
├─ 30-lutra.sh           # 编译并安装 lutra 自研组件
├─ 40-live.sh            # live-build 生成 ISO
├─ 50-sign.sh            # 签名与校验文件
└─ lib/common.sh         # 日志、错误处理、路径约定
```

阶段可单独重跑，`build.sh amd64 kernel` 只重建内核。
