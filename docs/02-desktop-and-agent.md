# 02 · 桌面、智能层与生态

> 本文定义桌面组件、默认应用、虚拟内存方案、Agent 智能层、`.ltr` 包格式规范与商店机制。

---

## 1. 桌面层

### 1.1 组件

| 角色 | 选型 | 说明 |
|---|---|---|
| 合成器 | labwc | wlroots 系，配置为 XML，易做主题；比 sway 更适合自研面板 |
| 面板 | lutra-shell（自研） | GTK4 + Rust；任务栏、开始菜单、状态区、通知中心 |
| 启动器 | fuzzel | Wayland 原生，轻量，可换皮肤 |
| 通知 | mako | 支持 Markdown 与操作按钮（权限确认用） |
| 壁纸 | swaybg | 支持动态切换 |
| 截图 | grim + slurp | 与自研截图工具共用底层 |
| 输入法 | fcitx5 + rime | 中文默认，拼音 + 五笔可选 |
| 字体 | Noto Sans CJK SC、Noto Mono | 仅保留 `zh_CN`、`en_US` |

### 1.2 视觉规范

| 项 | 值 |
|---|---|
| 主色 | `#2E8B8B`（水青） |
| 辅色 | `#7A5C43`（水獭棕） |
| 背景 | `#15191C`（深水） |
| 文字 | `#E6EDF0` |
| 圆角 | 10px |
| 字体 | Noto Sans CJK SC Medium / Regular |
| 图标 | 自研扁平线性图标集，与吉祥物 Lut 同一视觉语言 |

---

## 2. 默认应用

用户点名的六件必须有；其余按「轻量 + 无订阅 + 可离线」原则补齐。

| 类别 | 应用 | 选型 | 自研 | 体积 |
|---|---|---|---|---|
| 计算器 | Lutra Calc | 自研（GTK4 + Rust） | ✅ | 4 MB |
| 磁盘管理 | Lutra Disk | 自研前端 + udisks2 后端 | ✅ | 8 MB |
| 文件管理 | pcmanfm-qt | 复用 | — | 22 MB |
| 终端 | foot | 复用 | — | 2 MB |
| 设置 | Lutra Settings | 自研（GTK4 + Rust） | ✅ | 12 MB |
| 浏览器 | Firefox ESR | 复用 | — | 260 MB |
| 文本编辑器 | GNOME Text Editor | 复用 | — | 18 MB |
| 截图 | Lutra Shot | 自研（grim/slurp 封装） | ✅ | 3 MB |
| 看图 | Loupe | 复用 | — | 12 MB |
| 压缩 | File Roller | 复用 | — | 9 MB |
| 影音 | Celluloid | 复用 | — | 14 MB |
| 商店 | Lutra Store | 自研 | ✅ | 16 MB |
| Agent 控制台 | Lutra Agent Console | 自研 | ✅ | 20 MB |

> 浏览器选 Firefox ESR 而非 Chromium 的原因：Debian 原生维护、无 Snap 依赖、Playwright 另带独立 Chromium 实例互不干扰。

---

## 3. 虚拟内存

三种机制叠加，由 `Lutra Settings → 内存` 统一管理，界面按「内存 X GB + 虚拟 Y GB」呈现。

| 机制 | 默认策略 | 参数 |
|---|---|---|
| **zram** | 始终开启 | 大小 = 物理内存 × 50%，上限 8 GB；算法 `zstd`；`vm.page-cluster=0` |
| **swapfile** | ≥ 4 GB 内存默认关闭；2 GB 机型默认 4 GB | Btrfs `@swap` 子卷，`chattr +C` 关 CoW，`mkswap` 后 `pri=10` |
| **zswap** | 关闭（避免与 zram 重复） | 仅在用户手动关闭 zram 时作为替代 |

### 3.1 实现要点

```ini
# /etc/systemd/zram-generator.conf
[zram0]
zram-size = min(ram / 2, 8192)
compression-algorithm = zstd
swap-priority = 100
fs-type = swap
```

```conf
# /etc/sysctl.d/90-lutra-memory.conf
vm.swappiness = 100
vm.page-cluster = 0
vm.watermark_boost_factor = 0
vm.watermark_scale_factor = 125
```

- U 盘 / SD 卡安装时，设置中心自动把 swapfile 标为「不推荐」，并提示闪存寿命风险。
- 内存 ≤ 2 GB 时，自动启用 `systemd-oomd` 并降低 `lutrad` 的优先级（`Nice=10`、`MemoryHigh=256M`）。

---

## 4. 智能层

### 4.1 进程模型

```
lutra-agent.target
├─ lutrad.service        常驻，会话与工具调度中枢（Rust）
├─ lutra-guard.service   权限网关，独立进程（最小权限，CAP 受限）
├─ lutra-mcpd.service    MCP Server 生命周期管理
└─ lutra-audit.service   审计日志写入（append-only）
```

| 组件 | 职责 | 安全约束 |
|---|---|---|
| `lutrad` | 会话创建/销毁、工具注册、请求路由 | 不直接持有 root；通过 D-Bus 调 guard |
| `lutra-guard` | 计算所需能力集、弹窗确认、签发短期令牌 | 唯一拥有弹出授权 UI 权限的进程 |
| `lutra-mcpd` | 每个 MCP Server 一个独立 namespace + seccomp 白名单 | 默认禁网，需 `net` 权限才放行 |
| `lutra-audit` | 链式哈希日志，防篡改 | 只写不改，按天轮转 |

### 4.2 权限枚举

| 能力 | 含义 | 危险级 |
|---|---|---|
| `fs.read` | 读取用户目录 | 低 |
| `fs.write` | 写入用户目录 | 中 |
| `fs.system` | 读写系统路径 | 高 |
| `net` | 出站网络 | 中 |
| `display` | 读取屏幕 / 截屏 | 中 |
| `input` | 模拟键鼠 | 高 |
| `exec` | 执行子进程 | 高 |
| `usb` | 访问 USB 设备 | 高 |
| `audio` / `camera` / `clipboard` | 对应设备 | 中 |
| `sudo` | 提权执行 | 极高 |

### 4.3 会话级授权流程（用户已确认的模型）

```
Agent 发起请求
  → lutrad 解析所需能力集 C
  → 查询当前会话令牌是否覆盖 C
      ├─ 覆盖 → 直接放行，记审计
      └─ 未覆盖 → lutra-guard 弹出确认
                    ├─ 用户同意 → 签发令牌（绑定会话 ID + 能力集 + 过期时间）
                    └─ 用户拒绝 → 返回 EPERM，记审计
  → 会话结束（手动或 TTL 到期）→ 令牌全部吊销
```

- 默认 TTL：**2 小时**，可在设置中改为 30 分钟 / 2 小时 / 8 小时 / 手动。
- 令牌使用 Ed25519 签名，`/run/lutra/tokens/` 下存放，`tmpfs`，重启即失。
- **不提供**「永久授权」选项，这是有意为之：永久授权等于把 P4 原则架空。

### 4.4 审计日志

```
/var/lib/lutra/agent/audit-YYYYMMDD.jsonl
{"ts":"...","session":"...","tool":"browser.navigate","caps":["net"],"decision":"allow","hash":"...","prev":"..."}
```

每行包含前一行哈希，形成链；`lutra-agent verify` 可校验完整性。

---

## 5. 内置 AI 能力

### 5.1 预装 CLI

| 工具 | 分发方式 | 说明 |
|---|---|---|
| Claude Code | npm 全局包 | 需 `net` + `fs.read/write` |
| OpenCode | 官方安装脚本 → `.ltr` 封装 | 同上 |
| WorkBuddy | `.ltr` 封装 | 用户指定的自有工具 |
| DSH（CLI + App） | 待用户提供仓库地址 | 预留 `src/agent/dsh/` 目录 |
| REA（MCP 集） | 待用户提供 GitHub 地址 | 预留 `src/agent/rea/` 目录，插件位 |

### 5.2 内置 MCP Server

| 名称 | 接入方式 | 默认状态 |
|---|---|---|
| Browser Use | 独立 namespace，自带 Chromium | 安装后默认关闭，商店里一键启用 |
| Playwright MCP | 官方 npm 包 | 同上 |
| awesome-agent-skills | 只取索引（`skills.json`），按需下载技能 | 默认启用索引，不整仓克隆 |
| REA | 预留插件位 | 待定 |

> `awesome-agent-skills` 不整仓 clone：该仓库会持续增长，整仓内置会让最小镜像失控。改为同步索引文件，用户点选后从商店拉取。

---

## 6. `.ltr` 包格式规范 v1

### 6.1 容器

`package.ltr` = `tar` + `zstd`，扩展名固定 `.ltr`。

```
package.ltr
├── manifest.json
├── payload/                  # 完整文件树，安装到 /opt/ltr/<id>/
├── hooks/
│   ├── pre-install.sh
│   ├── post-install.sh
│   └── pre-remove.sh
└── META-INF/
    ├── SIGNATURE             # Ed25519，对 manifest.json + payload 摘要签名
    └── sha256sums
```

### 6.2 manifest.json

```json
{
  "schema": 1,
  "id": "com.artcraft.photocraft",
  "name": "PhotoCraft",
  "version": "0.21.0",
  "arch": ["x86_64", "aarch64"],
  "summary": "开源 Photoshop 替代，支持真实 PSD",
  "homepage": "https://github.com/storytold/photocraft",
  "license": "MIT OR Apache-2.0",
  "maintainer": "ArtCraft <dev@artcraft.dev>",
  "depends": ["libgtk-4-1", "libvulkan1", "libxkbcommon0"],
  "conflicts": [],
  "permissions": ["fs.read", "fs.write", "display", "net"],
  "entrypoints": {
    "gui": "/opt/ltr/com.artcraft.photocraft/bin/photocraft",
    "cli": null
  },
  "size": { "download": 78000000, "installed": 240000000 },
  "hooks": { "post_install": "hooks/post-install.sh" },
  "source": { "type": "deb", "upstream": "photocraft-0.21.0-linux-x86_64.deb" }
}
```

### 6.3 安装行为

| 步骤 | 动作 |
|---|---|
| 1 | 校验签名与 `sha256sums`，失败即中止 |
| 2 | 校验 `arch` 与当前系统匹配 |
| 3 | 解析 `depends`，缺失则经商店拉取或提示 |
| 4 | 展示 `permissions` 清单（首次安装必经） |
| 5 | 解包到 `/opt/ltr/<id>/` |
| 6 | 执行 `post-install.sh`（在 `systemd-run --scope` 沙箱内，禁用网络） |
| 7 | 写 `.desktop` 到 `/usr/share/applications/`，CLI 软链到 `/usr/local/bin/` |
| 8 | 记审计日志 |

### 6.4 deb 适配层

ArtCraft 官方只发 `.deb`，因此 `ltrpkg` 内置一个转换器：

```bash
ltrpkg convert ./photocraft-0.21.0-linux-x86_64.deb -o photocraft.ltr
```

转换逻辑：解包 deb → 提取 `DEBIAN/control` → 映射为 manifest 字段 → 合并 payload → 用 Lutra 密钥重新签名。这样既不破坏 `.ltr` 的统一性，又能直接消费上游产物。

---

## 7. 商店

### 7.1 架构

```
lutra-store (GTK4 客户端)
  → 静态索引 index.json（Ed25519 签名）
     → 包元数据 + 下载地址 + 权限声明
        → 直链下载 .ltr（校验 sha256 + 签名）
```

索引托管在 GitHub Releases / 对象存储；客户端只信任内置公钥。

### 7.2 分类与首发内容

| 分类 | 首发内容 |
|---|---|
| **AI 平台** | Claude Code、OpenCode、WorkBuddy、DSH、Ollama、Aider、Continue |
| **创意设计** | **ArtCraft 七件套**（PhotoCraft / VectorCraft / FilmCraft / LightCraft / EffectCraft / DesignCraft / PdfCraft）、Blender、Krita、Inkscape |
| **开发工具** | VS Code（OSS 构建）、Git 工具、Docker CLI（不含守护进程） |
| **系统工具** | GParted、Timeshift、htop、btop |
| **影音** | VLC、Audacity、OBS |

ArtCraft 七件套作为**独立分类「ArtCraft 全家桶」**整组展示，支持「一键安装全部」（约 1.6 GB）与单个安装。

### 7.3 安装确认

用户点「安装」后，客户端展示：

1. 下载体积与安装后体积
2. 依赖清单
3. **权限清单**（中文解释，不是裸枚举）
4. 明确按钮：「下载并安装」

默认**不自动更新**；更新需用户确认，可在设置中改为「仅安全更新自动」。

### 7.4 第三方提交

- 任何开发者可向索引仓库提 PR，附 `.ltr` 签名与构建脚本
- CI 校验：签名有效性、manifest schema、权限声明与行为一致性抽检
- 未通过审核的包只能以「未验证」标签安装，UI 上用黄色警示条标出

---

## 8. 待补齐项

| 项 | 状态 |
|---|---|
| REA(MCP) 仓库地址 | 等待用户提供 |
| DSH app + cli 仓库地址 | 等待用户提供（`M:\奇思妙想\dsh-android`、`M:\DSH-bot` 待确认） |
| ArtCraft 各仓库实际 release 版本 | 官网与镜像站版本号不一致，集成时以 GitHub Releases 实际产物为准 |
| WorkBuddy 分发形式 | 待确认是否有 Linux 构建 |
