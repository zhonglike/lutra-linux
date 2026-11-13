# 05 · 系统更新与版本发布

> 本文定义 Lutra 的版本号规则、升级包格式、更新检查策略、提示分级与回滚机制。

---

## 1. 版本号

采用语义化版本 `MAJOR.MINOR.PATCH`，首个正式版为 **1.0.0 "Rill"**。

| 位 | 递增条件 | 例子 |
|---|---|---|
| MAJOR | 不兼容变更（包格式、分区布局、内核大版本） | 1.0 → 2.0 |
| MINOR | 新增功能、桌面组件升级 | 1.0 → 1.1 |
| PATCH | 修 bug、安全补丁、驱动更新 | 1.0.0 → 1.0.1 |

**发布策略：每个正式版本都打 tag 并发布 GitHub Release，历史版本永久保留，不删除、不覆盖。**

```
v1.0.0   Rill       首个正式版
v1.0.1   Rill       安全补丁
v1.1.0   Cascade    功能更新
v1.2.0   Estuary    功能更新
```

每个 Release 的资产固定为：

```
lutra-1.0.0-amd64.iso
lutra-1.0.0-arm64.img.xz
lutra-1.0.0-i686.iso
lutra-1.0.0.update.lup          ← 升级包
SHA256SUMS
SHA256SUMS.sig
```

---

## 2. 更新分级

| 级别 | 标识 | 内容 | 提示方式 |
|---|---|---|---|
| **必须更新** | `security` | 提权漏洞、远程代码执行、内核高危 CVE、签名密钥泄露 | **开机后立即弹窗告知**，顶栏常驻提示，不可静默忽略 |
| 功能更新 | `feature` | 新功能、桌面改进、新驱动 | **不提醒**，仅在「设置 → 更新」显示 |
| 补丁 | `patch` | 普通 bug 修复、性能优化、文档 | **不提醒**，仅在「设置 → 更新」显示 |

用户原话对应的规则：

> 每次开机看一下更新，如果有必更更新（大漏洞）开机告诉用户，小的不提醒，得用户自己去设置里重启。

实现上就是：开机检查 → 只有 `security` 级会主动弹窗，其余静默进设置页。

---

## 3. 升级包格式 `.lup`

独立于应用包 `.ltr`，因为系统更新涉及根文件系统、内核和引导，必须支持快照回滚。

```
lutra-1.0.1.update.lup          （tar + zstd）
├── manifest.json
├── payload/
│   ├── rootfs/                 Btrfs send 增量流（相对 from_version）
│   ├── kernel/                 linux-image-*.deb
│   └── pkgs/                   新增或升级的 .ltr 包
├── hooks/
│   ├── pre-apply.sh
│   └── post-apply.sh
└── META-INF/
    ├── SIGNATURE               Ed25519
    └── SHA256SUMS
```

### 3.1 manifest.json

```json
{
  "schema": 1,
  "type": "patch",
  "channel": "stable",
  "from_version": "1.0.0",
  "to_version": "1.0.1",
  "arch": ["x86_64", "aarch64"],
  "size": { "download": 184000000, "applied": 520000000 },
  "reboot_required": true,
  "rolling_back_supported": true,
  "severity": "security",
  "cve": ["CVE-2026-XXXXX"],
  "changelog": {
    "zh_CN": ["修复内核提权漏洞", "更新 iwlwifi 固件"],
    "en": ["Fix kernel privilege escalation", "Update iwlwifi firmware"]
  }
}
```

`severity` 只允许三个值：`security` / `feature` / `patch`，与第 2 节的分级一一对应。

---

## 4. 检查机制

### 4.1 何时检查

| 时机 | 实现 |
|---|---|
| 开机 | `lutra-update-check.service`，在 `graphical.target` 之后 90 秒执行一次 |
| 周期 | `lutra-update-check.timer`，每 6 小时一次 |
| 手动 | 「设置 → 更新 → 检查更新」 |
| 网络恢复 | NetworkManager 的 `connectivity-changed` 触发一次 |

### 4.2 检查内容

只拉取索引，不自动下载升级包：

```
GET https://github.com/<org>/lutra-linux/releases/latest  →  重定向拿 tag
GET https://<mirror>/updates/stable/index.json            →  签名索引
```

索引包含每个版本的 `manifest`、下载地址、签名。客户端用内置公钥验签后比对本地版本。

### 4.3 网络与隐私

- 只发送：当前版本号、架构、渠道
- 不发送：设备 ID、用户名、任何硬件指纹
- 可完全关闭：设置里「不检查更新」后，不再发起任何请求

---

## 5. 提示策略

### 5.1 security 级（必须更新）

开机后如果检测到：

1. 桌面弹出不可忽略的系统通知，标题「安全更新」，正文列出 CVE 与一句话影响
2. 顶栏出现红色盾牌图标，点击直接跳到更新页
3. 更新页只有两个按钮：「立即更新」「稍后（24 小时内再次提醒）」
4. **不提供「永久忽略」**

### 5.2 feature / patch 级（可选更新）

- 开机不弹任何东西
- 顶栏无图标
- 仅在「设置 → 更新」里以列表呈现，用户自行决定何时安装
- 安装完成后提示「需要重启生效」，由用户点重启

### 5.3 用户可调

| 设置项 | 默认 |
|---|---|
| 自动检查更新 | 开 |
| 安全更新提醒 | 开（不可关） |
| 功能更新提醒 | 关 |
| 自动下载升级包 | 关 |
| 自动安装安全更新 | 关 |

---

## 6. 安装与回滚

### 6.1 安装流程

```
1  下载 .lup 到 /var/cache/lutra/updates/
2  校验 Ed25519 签名与 SHA256SUMS
3  检查 from_version 与当前系统一致（否则拒绝）
4  创建 Btrfs 快照  @snapshots/before-<to_version>
5  应用 rootfs 增量（btrfs receive）
6  安装内核 deb 与新增 .ltr 包
7  更新 initramfs 与 GRUB
8  写入 /var/lib/lutra/update/pending.json
9  提示重启（security 级强制提示，其余由用户决定）
```

### 6.2 重启验证与自动回滚

- 启动后 `lutra-update-verify.service` 检查关键服务是否全部拉起
- 若 60 秒内 `lutra-agent.target` 未就绪 → 自动 `btrfs subvolume set-default` 回上一个快照并重启
- GRUB 菜单额外提供「回滚到上一个版本」入口

### 6.3 快照保留

- 只保留最近 3 个更新前快照，超出自动删除最旧的
- 用户可在「设置 → 存储」里手动固定某个快照不被清理

---

## 7. 发布流程

```
1  构建三架构产物
2  生成升级包（相对上一个版本计算 Btrfs 增量）
3  用发布密钥签名所有产物
4  创建 GitHub Release（tag = v<版本>）
5  上传 ISO / IMG / .lup / SHA256SUMS / .sig
6  更新 updates/stable/index.json 并签名
7  旧版本 Release 保持原样，不删除
```

发布密钥与包签名密钥分离：发布密钥离线保存，包签名密钥在 CI 中受限使用。

---

## 8. 与 `.ltr` 的关系

| | `.ltr` | `.lup` |
|---|---|---|
| 层级 | 应用 | 系统 |
| 影响范围 | `/opt/ltr/<id>` | 根文件系统、内核、引导 |
| 是否需重启 | 否 | 通常需要 |
| 是否支持回滚 | 卸载即可 | Btrfs 快照回滚 |
| 签名密钥 | 应用密钥 | 系统发布密钥 |

---

## 9. 里程碑落点

更新系统放在 **M7**，依赖以下前置条件：

| 依赖 | 来自 |
|---|---|
| Btrfs 子卷与快照 | M2 |
| 内核 deb 打包 | M1 |
| `.ltr` 包管理器 | M4 |
| GitHub 仓库与签名体系 | M5 之后 |

在 M7 之前，系统以完整镜像重装的方式升级；M7 之后才提供增量升级包。
