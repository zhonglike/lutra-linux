# 00 · 架构总览

> 本文定义 Lutra Linux 的整体分层、组件选型、启动流程与关键决策记录（ADR）。
> 后续所有构建、开发工作以此为唯一依据。

---

## 1. 设计原则

| # | 原则 | 含义 |
|---|---|---|
| P1 | **内核自主** | 不使用 Debian 预编译内核，每台机器跑的是我们自己的 config |
| P2 | **轻优先于全** | 默认不装的重型组件一律不进基础镜像，需要就进商店 |
| P3 | **AI 是一等公民** | 智能层不是外挂软件，而是与 systemd 平级的系统服务 |
| P4 | **危险操作默认拒绝** | Agent 的一切越权动作必须经会话授权，无静默放行通道 |
| P5 | **可解释、可回滚** | 每次系统变更留审计日志，Btrfs 快照保证可回退 |

---

## 2. 分层架构

```
┌──────────────────────────────────────────────────────────────┐
│ L5 桥接层    lutra-bridge (托盘 + MCP Server + QEMU 管理)      │
│              投递分区 / SSH / mTLS 双向通道                     │
├──────────────────────────────────────────────────────────────┤
│ L4 智能层    lutrad (Agent 守护) / lutra-guard (权限网关)      │
│              MCP 运行时 / 商店客户端 / .ltr 包管理器           │
├──────────────────────────────────────────────────────────────┤
│ L3 桌面层    labwc (Wayland) / lutra-shell / fcitx5 / 默认应用 │
├──────────────────────────────────────────────────────────────┤
│ L2 系统层    Debian trixie 用户空间 / systemd / Btrfs / LUKS2  │
├──────────────────────────────────────────────────────────────┤
│ L1 内核层    自编 Linux 6.12 LTS + 驱动补丁 + initramfs        │
├──────────────────────────────────────────────────────────────┤
│ L0 引导层    UEFI shim (MOK) / GRUB2 / Legacy BIOS             │
└──────────────────────────────────────────────────────────────┘
```

---

## 3. 组件选型清单

| 层 | 组件 | 选型 | 自研 | 说明 |
|---|---|---|---|---|
| L0 | 引导器 | GRUB2 2.12 | 否 | UEFI + Legacy 双支持 |
| L0 | 安全启动 | shim-signed + 自有 MOK | 部分 | 首次启动导入一次密钥 |
| L1 | 内核 | Linux 6.12 LTS | 配置自研 | 三套 config，统一补丁集 |
| L1 | initramfs | dracut | 否 | 需支持 LUKS + Btrfs + 网络引导 |
| L2 | 用户空间 | Debian trixie | 否 | debootstrap 最小化构建 |
| L2 | init | systemd 257 | 否 | 统一管理服务与 target |
| L2 | 文件系统 | Btrfs（子卷 + 快照） | 否 | `@`/`@home`/`@snapshots`/`@var` |
| L2 | 加密 | LUKS2（可选） | 否 | 装机时勾选 |
| L2 | 网络 | NetworkManager | 否 | 含 WiFi 与蓝牙前端 |
| L2 | 音频 | PipeWire + WirePlumber | 否 | 低延迟、蓝牙编解码完整 |
| L3 | 合成器 | labwc（wlroots） | 否 | 比 sway 更易做主题与自研面板 |
| L3 | 面板/外壳 | lutra-shell | **是** | GTK4 + Rust，含开始菜单与状态区 |
| L3 | 输入法 | fcitx5 + rime | 否 | 中文默认 |
| L3 | 文件管理器 | pcmanfm-qt | 否 | 轻量，MIT 友好 |
| L3 | 终端 | foot | 否 | Wayland 原生，极低延迟 |
| L4 | Agent 守护 | lutrad | **是** | Rust，会话与工具调度中枢 |
| L4 | 权限网关 | lutra-guard | **是** | 会话级 capability 授权 |
| L4 | MCP 运行时 | lutra-mcpd | **是** | 多 server 隔离与生命周期管理 |
| L4 | 包管理 | ltrpkg | **是** | `.ltr` 格式的安装/校验/回滚 |
| L4 | 商店 | lutra-store | **是** | GTK4 客户端 + 签名索引 |
| L5 | Windows 桥 | lutra-bridge | **是** | 托盘 + MCP Server + QEMU 管理 + 投递同步 |
| L5 | 虚拟化 | QEMU 便携版 | 否 | 解压即用，零 C 盘写入 |

---

## 4. 启动流程

### 4.1 裸机 UEFI 路径

```
上电
 └─ UEFI 固件
     └─ shimx64.efi（微软签名，Secure Boot 放行）
         └─ 校验 MOK 中的 Lutra 公钥
             └─ grubx64.efi（Lutra 签名内核的引导器）
                 ├─ 正常启动：vmlinuz-lutra + initramfs
                 └─ 恢复入口：快照回滚 / 救援 shell
                     └─ initramfs：解锁 LUKS → 挂载 Btrfs → switch_root
                         └─ systemd
                             ├─ sysinit.target
                             ├─ lutra-agent.target  ← AI 服务先于桌面拉起
                             ├─ graphical.target
                             └─ lutra-session.target ← 桌面就绪
```

### 4.2 Windows 内虚拟化路径

```
lutra-host.exe（托盘）
 └─ 检测 U 盘 / 本地 .vhdx
     └─ 拉起便携 QEMU（WHPX / TCG 回退）
         ├─ 若直通物理分区：-drive file=\\.\PhysicalDriveN
         └─ 若用虚拟磁盘：-drive file=M:\lutraos\_vm\lutra-test.vhdx
             └─ 显示：SPICE 窗口 / VNC
             └─ 文件：virtiofs 共享「投递分区」
```

---

## 5. 目录布局（运行时的 Linux 侧）

| 路径 | 用途 |
|---|---|
| `/opt/ltr/<id>/` | `.ltr` 包安装位置，每个应用一个独立目录 |
| `/var/lib/lutra/agent/` | Agent 会话状态、审计日志 |
| `/var/lib/lutra/store/` | 商店索引缓存与下载临时区 |
| `/etc/lutra/` | 全系统配置（权限策略、镜像源、虚拟内存） |
| `/usr/lib/lutra/hooks/` | 系统级钩子（快照、审计上报） |
| `/.lutra-drop/` | 投递分区在 Linux 侧的挂载点 |

---

## 6. 关键设计决策（ADR）

| 编号 | 决策 | 理由 | 代价 |
|---|---|---|---|
| ADR-001 | 底座选 Debian trixie，不做 LFS | 三方软件兼容性、驱动覆盖、可维护性；LFS 无法承载 Waydroid 与闭源固件 | 体积比 LFS 大，血统不纯粹 |
| ADR-002 | 桌面用 labwc 而非 KDE/GNOME | 空闲内存占用约 180MB，可完全自主控制外观 | 需要自研面板与设置中心 |
| ADR-003 | 文件系统 Btrfs | 子卷、快照回滚、压缩（zstd:1）对轻量目标帮助明显 | CoW 对虚拟机镜像有碎片问题，需关 `nodatacow` |
| ADR-004 | 自研包格式 `.ltr` | 需要与权限网关共用一套能力声明，deb 无法表达 | 生态从零建立 |
| ADR-005 | 授权按会话一次性 | 体验与安全的平衡点；高频任务不被弹窗打断 | 会话内一旦授权，风险敞口持续到会话结束 |
| ADR-006 | Windows 侧用便携 QEMU | 唯一能零 C 盘写入的方案；WSL2 与 Hyper-V 都会写系统盘 | 无 WHPX 时性能较差，需回退 TCG |
| ADR-007 | i686 定位 Legacy，不含 AI | Node 无官方 ia32 构建，Rust 工具链与浏览器已放弃 32 位 | 32 位用户无法使用智能层 |
| ADR-008 | LUKS2 默认关闭 | 用户选择；且默认开启会拖慢 U 盘随机读写 | U 盘丢失即数据泄露，需在文档中明示 |
| ADR-009 | Secure Boot 用自签 shim + MOK | 新机器无需关闭 Secure Boot 即可启动 | 用户需手动导入一次密钥 |
| ADR-010 | 工程路径强制纯 ASCII（`M:\lutraos`） | 内核与交叉工具链在非 ASCII 路径下会出现难以定位的构建失败 | 不能放在中文目录下 |

---

## 7. 内存预算（空闲态，实测目标）

| 组件 | 目标占用 |
|---|---|
| 内核 + systemd 基础 | 210 MB |
| labwc + lutra-shell + waybar | 180 MB |
| PipeWire + NetworkManager + 蓝牙栈 | 90 MB |
| lutrad + lutra-guard（空闲） | 60 MB |
| **桌面空闲合计** | **≈ 540 MB** |
| 加 foot + 文件管理器 | ≈ 700 MB |
| 加 Firefox（3 标签） | ≈ 1.5 GB |
| 加 Claude Code / OpenCode 会话 | ≈ 2.2 GB |
| 加 Playwright（Chromium 实例） | ≈ 3.4 GB |

> 结论：2GB 可完成「桌面 + 终端 + 单个 AI CLI」；4GB 为日常舒适线；8GB 才能同时跑浏览器自动化与多 Agent。

---

## 8. 与其他文档的关系

| 文档 | 内容 |
|---|---|
| `01-build-and-image.md` | 三架构构建管线、内核配置、ISO 生成 |
| `02-desktop-and-agent.md` | 桌面组件、默认应用、智能层、`.ltr` 规范、商店 |
| `03-platforms-and-windows.md` | 多平台矩阵、Windows 桥接、风险清单 |
| `04-bidirectional-bridge.md` | 双向桥接、Windows 侧 MCP Server、权限与审计 |
| `05-update-system.md` | 版本策略、`.lup` 升级包、更新分级、回滚 |
