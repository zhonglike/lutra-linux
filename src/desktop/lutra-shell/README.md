# lutra-shell

Lutra 桌面外壳（Wayland + labwc）。一个 GTK4 进程里持有两个 layer-shell 表面：

- **顶栏**：开始按钮、工作区指示、任务栏、系统区（网络 / 音量 / 电量 / AI 状态 / 时钟）
- **开始菜单**：搜索框实时过滤 `*.desktop`，空查询时按分类网格展示

```
lutra-shell/
├── Cargo.toml
├── data/
│   ├── style.css              # 深色主题（编译期 include_str! 进二进制）
│   ├── icons/*.svg            # 14 个应用图标 + 5 个系统区线图标
│   └── lutra-shell.desktop
└── src/
    ├── main.rs                # Application 入口，注册两个 layer-shell 表面
    ├── panel.rs               # 顶栏
    ├── startmenu.rs           # 应用索引 / 搜索 / 分类 / 启动
    ├── taskbar.rs             # 运行中应用按钮 + 运行指示点
    ├── clock.rs               # 系统区时钟
    └── theme.rs               # CSS 加载与图标解析
```

## 依赖

Debian trixie（Lutra 基础镜像）：

```sh
sudo apt install build-essential pkg-config \
    libgtk-4-dev libgtk4-layer-shell-dev
```

- `gtk4` 绑定需要 GTK ≥ 4.12（Cargo 中启用了 `v4_12`，用于
  `CssProvider::load_from_string`）。
- 发行版没有 `libgtk4-layer-shell-dev` 时，按 gtk4-layer-shell 上游说明用
  meson 构建安装后再 `cargo build`（本 crate 通过 pkg-config 查找它）。
- 运行时还需要 `librsvg2-common`（让 gdk-pixbuf 能加载自带 SVG 图标）。

## 构建与运行

```sh
cd src/desktop/lutra-shell
cargo build            # 构建
cargo run              # 在已有的 labwc / Wayland 会话里直接启动
```

`cargo run` 要求当前会话满足：

- `WAYLAND_DISPLAY` 有效，合成器支持 `wlr-layer-shell`（labwc 满足）；
- 已有另一个终端或快捷键可切出，否则顶栏会占住屏幕顶部 30px。

调试小技巧：

```sh
RUST_BACKTRACE=1 G_MESSAGES_DEBUG=all cargo run
LUTRA_SHELL_ICONS=$PWD/data/icons cargo run   # 从源码目录运行时显式指定图标
```

## 安装

```sh
cargo build --release
sudo install -Dm755 target/release/lutra-shell /usr/bin/lutra-shell
sudo install -d /usr/share/lutra-shell
sudo cp -r data/icons /usr/share/lutra-shell/icons
sudo install -Dm644 data/lutra-shell.desktop /usr/share/applications/lutra-shell.desktop
sudo install -Dm644 ../labwc/rc.xml         /etc/xdg/labwc/rc.xml
sudo install -Dm644 ../labwc/environment    /etc/xdg/labwc/environment
sudo install -Dm755 ../labwc/autostart      /etc/xdg/labwc/autostart
```

`autostart` 必须有可执行位；`environment` 是纯粹的 `KEY=VALUE` 列表，不是 shell 脚本。

## 主题

`data/style.css` 以设计稿变量为准：

| 设计稿变量 | 值 | GTK4 写法 |
|---|---|---|
| `--desk` | `#1a1a1a` | `@define-color desk` |
| `--win` | `#242424` | `@define-color win` |
| `--hdr` | `#2b2b2b` | `@define-color hdr` |
| `--side` | `#2f2f2f` | `@define-color side` |
| `--sel` | `#3584e4` | `@define-color sel` |
| `--term` | `#161616` | `@define-color term` |

GTK4 的 CSS 不支持 `--var()` 自定义属性，因此用 `@define-color` 一一对应；
样式表在编译期嵌入，运行时不需要定位 `style.css`。

## 已知限制（协议缺口导致的降级）

1. **任务栏窗口列表**：labwc/wlroots 没有 `swaymsg` 式 IPC，GTK4 也没有
   `wlr-foreign-toplevel-management` 绑定，libwnck 只适用于 X11。任务栏因此只
   显示「由本 shell 启动过」的应用；点击会重新执行该应用的 `Exec`，单实例应用
   经 xdg-activation 把已有窗口提到前台。真正的最小化/窗口枚举要等合成器暴露
   协议后接入。
2. **工作区切换**：顶栏只做指示与本地高亮，实际切换用 `rc.xml` 的 `W-1`…`W-4`
   （`GoToDesktop`）。
3. **音量 / 网络**：不引入 libpulse、libnm；音量按钮打开 `pavucontrol`，网络
   状态直接读 `/sys/class/net/*/operstate`。电量读
   `/sys/class/power_supply/BAT*/capacity`，无电池时整块隐藏；AI 状态扫描
   `/proc/*/comm` 判断 `lutrad` 是否在运行。
