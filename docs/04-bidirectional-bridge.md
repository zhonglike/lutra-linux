# 04 · 双向桥接与 Windows MCP

> 本文定义 Windows 与 Lutra 之间的双向控制通道，以及供 Windows 侧 AI 客户端调用的 MCP Server。

---

## 1. 为什么需要双向

原设计只覆盖了一个方向：Lutra 系统内的 Agent 去操控 Windows。但实际使用中还有大量反向场景：

| 场景 | 方向 | 例子 |
|---|---|---|
| Lutra 内的 Agent 操作宿主 | Lutra → Windows | 打开 Windows 上的设计软件、读取 Windows 文件 |
| Windows 上的 AI 操作 Lutra | **Windows → Lutra** | 在 Claude Code / Cursor / Trae 里让 AI 检查 Lutra 构建状态、跑测试、改配置 |
| 两边 AI 协同 | 双向 | Windows 侧 Agent 派活给 Lutra 侧 Agent，回收结果 |

第二条是本设计新增的核心：**让 Windows 上已有的 AI 工具把 Lutra 当成一台可远程操作的目标机**。

---

## 2. 合并为一个进程

不拆分 `lutra-host`（托盘）与 `lutra-mcp`（MCP Server），统一为 **`lutra-bridge`**：

```
lutra-bridge.exe
├─ tray/       托盘界面：启动/停止 VM、投递文件、查看状态
├─ mcp/        MCP Server（stdio 或 HTTP+SSE），供 Windows AI 客户端连接
├─ vm/         QEMU 进程管理与 QMP 控制
├─ ssh/        SSH 通道（命令执行、文件传输）
├─ broker/     权限代理：高危操作弹窗确认
└─ audit/      双向审计日志（append-only）
```

合并理由：

1. 权限代理只有一份，规则不会两边不一致
2. 审计日志只有一个写入点，链条完整
3. 连接池复用，SSH 会话不必建两次
4. 用户只需运行一个程序

---

## 3. 架构

```
┌──────────────────── Windows ────────────────────┐
│                                                 │
│  AI 客户端（Claude Code / Cursor / Trae）        │
│        │ MCP (stdio / HTTP+SSE)                 │
│        ▼                                        │
│  lutra-bridge ──┬── QMP ──→ QEMU（启停/快照）    │
│        │        ├── SSH ──→ Lutra(VM) :2222     │
│        │        ├── mTLS ─→ Lutra(裸机) :8443    │
│        │        └── exFAT → 投递分区             │
│        │                                        │
│        └── 权限代理 → 弹窗确认 → 审计日志         │
└───────────────────┬─────────────────────────────┘
                    │
┌───────────────────▼───── Lutra ─────────────────┐
│  lutrad（会话与工具调度）                          │
│    ├─ lutra-guard（权限网关）                     │
│    ├─ lutra-mcpd（MCP Server 生命周期）           │
│    └─ fs / proc / net 能力出口                    │
│                                                 │
│  Lutra 内 Agent ──→ 反向经 bridge 操作 Windows     │
└─────────────────────────────────────────────────┘
```

---

## 4. 三种运行状态下的传输

| Lutra 状态 | 可用传输 | 可用工具 |
|---|---|---|
| **VM 运行中** | QMP + SSH(2222) + 投递分区 | 全部 |
| **裸机运行中**（另一台机器或双启动后） | LAN TCP 8443（mTLS）+ 投递分区 | 全部（无 QMP 类） |
| **未运行** | 无 | 仅 `lutra_vm_start`、`lutra_status`、`lutra_transfer` |

### 4.1 SSH 通道

- 密钥对由 `lutra-bridge` 首次启动时生成，存放 `M:\lutraos\_vm\keys\`，用 DPAPI 加密
- 安装器把公钥写入 Lutra 的 `authorized_keys`
- 命令执行走 `ssh -p 2222 lutra@127.0.0.1 -- <cmd>`，带 30 秒超时
- 文件传输走 SFTP，避免再引一套协议

### 4.2 裸机 mTLS 通道

- `lutrad` 监听 8443，强制双向 TLS
- 证书由 `lutra-bridge` 配对时一次性签发（6 位配对码 + 局域网发现）
- 未配对设备无法连接，也不响应发现广播

### 4.3 QMP 通道

- QEMU 以 `-qmp tcp:127.0.0.1:4444,server,nowait` 启动
- 用于：查询运行状态、优雅关机、创建/回滚快照、热插拔
- 不用于文件或命令，避免把 QMP 当通用后门

---

## 5. MCP 工具清单

| 工具名 | 作用 | 危险级 | 未运行时 |
|---|---|---|---|
| `lutra_status` | 运行模式、IP、内存/磁盘、会话数 | 低 | ✅ |
| `lutra_vm_start` | 启动 VM（可指定内存/CPU） | 中 | ✅ |
| `lutra_vm_stop` | 优雅关机 | 中 | — |
| `lutra_vm_snapshot` | 创建快照 | 中 | — |
| `lutra_vm_rollback` | 回滚快照 | 高 | — |
| `lutra_exec` | 在 Lutra 内执行命令 | **高** | — |
| `lutra_read_file` | 读文件 | 中 | — |
| `lutra_write_file` | 写文件 | **高** | — |
| `lutra_list_dir` | 列目录 | 低 | — |
| `lutra_transfer` | 双向传文件（走投递分区） | 中 | ✅ |
| `lutra_screenshot` | 截取 Lutra 画面 | 中 | — |
| `lutra_logs` | 读审计日志 | 低 | — |
| `lutra_pkg_install` | 安装 `.ltr` 包 | **高** | — |
| `lutra_session_list` | 列出/终止 AI 会话 | 中 | — |

工具返回统一结构：

```json
{ "ok": true, "data": {}, "session": "sess_xxx", "audit": "evt_xxx" }
{ "ok": false, "error": { "code": "EPERM", "message": "用户拒绝授权" } }
```

---

## 6. 权限与审计

### 6.1 授权一致性

Windows 侧与 Lutra 侧使用**同一套能力枚举**（见 `02-desktop-and-agent.md` 第 4.2 节），规则也一致：

- 会话级一次性授权，TTL 默认 2 小时
- 不提供永久授权
- 拒绝即返回 `EPERM`，不静默重试

### 6.2 高危操作的确认弹窗

当 Windows 侧 AI 调用 `lutra_exec` / `lutra_write_file` / `lutra_pkg_install` 时：

```
┌─ Lutra Bridge ──────────────────────────┐
│ AI 请求在 Lutra 内执行命令               │
│                                         │
│   rm -rf ~/build && make -j8            │
│                                         │
│ 影响：删除构建目录并重新编译              │
│ 来源：Claude Code (pid 1234)             │
│                                         │
│ [拒绝]  [本次允许]  [本会话允许 (2h)]     │
└─────────────────────────────────────────┘
```

### 6.3 审计

两个写入点合并成一条链：

```
M:\lutraos\_vm\audit\bridge-YYYYMMDD.jsonl
/var/lib/lutra/agent/audit-YYYYMMDD.jsonl
```

每条记录含 `prev` 字段指向上一行哈希，两侧日志在同步时做交叉校验，任何一侧被篡改都能发现。

---

## 7. 客户端配置

### 7.1 Claude Code / 通用 MCP 客户端

```json
{
  "mcpServers": {
    "lutra": {
      "command": "M:\\Lutra\\win\\lutra-bridge.exe",
      "args": ["mcp", "--stdio", "--profile", "default"],
      "env": { "LUTRA_HOME": "M:\\Lutra" }
    }
  }
}
```

### 7.2 需要多台 Lutra 时

```json
{
  "mcpServers": {
    "lutra-vm": {
      "command": "M:\\Lutra\\win\\lutra-bridge.exe",
      "args": ["mcp", "--stdio", "--target", "vm"]
    },
    "lutra-pi": {
      "command": "M:\\Lutra\\win\\lutra-bridge.exe",
      "args": ["mcp", "--stdio", "--target", "192.168.1.50:8443"]
    }
  }
}
```

### 7.3 HTTP+SSE 模式（多客户端共享）

```powershell
M:\lutraos\win\lutra-bridge.exe mcp --http 127.0.0.1:7788
```

用于同时给多个 AI 客户端提供同一实例，避免每个客户端各起一个进程抢 SSH 会话。

---

## 8. 与既有设计的关系

| 文档 | 需要同步的改动 |
|---|---|
| `00-architecture.md` | L5 层由 `lutra-host` 改为 `lutra-bridge`，明确双向 |
| `03-platforms-and-windows.md` | 第 3 章 Windows 桥接替换为对本文的引用 |
| `02-desktop-and-agent.md` | `lutra-mcpd` 与 bridge 的 mcp 模块职责区分：前者管内、后者管外 |

---

## 9. 实现顺序

1. **P0**：`lutra_status` / `lutra_vm_start` / `lutra_vm_stop`（先能控生命周期）
2. **P1**：SSH 通道 + `lutra_exec` / `lutra_read_file` / `lutra_write_file`
3. **P1**：权限弹窗 + 审计链
4. **P2**：`lutra_transfer`、`lutra_screenshot`
5. **P3**：QMP 快照、裸机 mTLS 配对
