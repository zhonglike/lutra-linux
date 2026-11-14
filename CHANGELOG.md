# 更新日志

本项目的每个发行版本都会保留，不删除历史版本。

## 1.0.0 "Rill"

首个源码发行版。

### 内核与构建

- 自编 Linux 6.12 LTS 内核，配置采用 `base + 桌面架构 + 虚拟化` 三段碎片合并
- 已在 QEMU 内验证 `6.12.0-lutra` 启动到 multi-user，网络与 SSH 正常
- `build/10-kernel.sh` 支持 amd64 / arm64 / i686，产出 `bindeb-pkg` deb
- 构建环境完全落在 M 盘：便携 QEMU + Debian trixie cloud image + cloud-init 种子盘
- `build/20-iso.sh` 生成 Live ISO：debootstrap 根文件系统、squashfs、live-boot、GRUB 双引导
- Live ISO 已在 QEMU 内实测：启动到登录、`uname -r` 为 `6.12.0-lutra`、安装器可运行

### 安装器

- `installer/lutra-installer.py` 支持四种模式：整盘、与 Windows 共存、U 盘、手动分区
- 新增 GTK4 图形向导 `installer/lutra-installer-gui.py`，复用同一套分区安全逻辑
- Live 桌面提供 waybar 顶栏与「安装」入口，图形向导可直接选择磁盘、模式并安装
- 写入前对目标磁盘分类（空盘 / 有数据 / 含系统），非空盘必须 `--force-nonempty` 并输入磁盘路径确认
- 共存模式**只使用未分配空间**，绝不缩容现有分区
- Btrfs 子卷 `@`、`@home`、`@snapshots`、`@var`，首启自动快照

### 桌面

- labwc 合成器 + 自研 GTK4 `lutra-shell`：顶栏、任务栏、开始菜单
- 深色 Adwaita 风格，5 层结构彩色图标，6 层圆形水獭标志
- 默认中文输入法 fcitx5 + rime

### 智能层

- `lutrad` / `lutra-guard` / `lutra-mcpd` / `lutra-audit` 四进程模型
- 能力枚举与路径白名单、Ed25519 会话令牌（TTL 2 小时）
- 哈希链审计日志，`lutra-audit verify` 校验完整性
- `.ltr` 应用包与 `ltr` 打包/安装/卸载工具；MCP 工具集可供外部 AI 调用

### Windows 桥

- `lutra-bridge`：托盘、便携 QEMU 生命周期、SSH 执行、文件投递、MCP stdio 服务器
- 14 个 `lutra_*` 工具，危险命令黑名单与会话授权，全部调用写哈希链审计

### 系统更新

- `.lup` 升级包：manifest + rootfs + 内核 deb + hooks + Ed25519 签名
- 三级更新：`security`（开机强提示）/ `feature`（设置页）/ `patch`（静默）
- 开机 90 秒检查、之后每 6 小时；Btrfs 快照 + 启动失败自动回滚

### 多架构

- 提供 ARM64（树莓派）与 i686 Legacy 配置碎片
- 安卓方向规划 Waydroid（见 `docs/03-platforms-and-windows.md`）
