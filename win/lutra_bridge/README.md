# lutra-bridge（Windows 侧桥接）

把 Windows 上的 AI 客户端与 Lutra（QEMU 虚拟机）连接起来：托盘管理 VM、MCP 工具远程操作 Lutra、SSH 执行命令/传文件、哈希链审计。

- 仅使用 Python 3.12，无编译型依赖。
- 所有本地写入限制在 `M:\lutraos` 内；C 盘不写任何文件。
- 设计依据：`docs/04-bidirectional-bridge.md`、`docs/03-platforms-and-windows.md`。

## 1. 目录

```
win/lutra_bridge/
├─ __main__.py     命令行入口（tray / mcp / vm / exec / status）
├─ config.py       路径、错误码、危险命令策略、会话授权、审计脱敏
├─ mcp_server.py   MCP stdio JSON-RPC（14 个工具）
├─ qemu.py         QEMU 启停/查询，PID 文件，防重复启动
├─ ssh_client.py   paramiko 命令执行 + SFTP 上传下载
├─ audit.py        哈希链 JSONL 审计
├─ tray.py         托盘（pystray 可选）
├─ mcp_config.json 客户端配置示例
└─ requirements.txt
```

运行时目录（自动创建，均在 M 盘）：

```
M:\lutraos\_vm\
├─ build.qcow2         虚拟磁盘
├─ qemu.pid            QEMU PID/参数
├─ serial.log          串口日志
├─ audit\bridge-YYYYMMDD.jsonl
├─ consent\<session>.json
└─ screenshots\
```

## 2. 依赖

`paramiko` 已随工程放在 `M:\lutraos\_cache\python`，程序会自动加入 `sys.path`，无需安装。

托盘依赖是可选的：

```powershell
python -m pip install --target M:\lutraos\_cache\python pystray pillow
```

未安装时 `tray` 子命令会打印提示并退出，其余功能不受影响。

## 3. 运行

在 PowerShell 中：

```powershell
$env:PYTHONPATH = "M:\lutraos\win"
cd M:\lutraos\win

python -m lutra_bridge status
python -m lutra_bridge vm status
python -m lutra_bridge vm start --mem-mb 8192 --cpu 8
python -m lutra_bridge exec "uname -a" --yes
python -m lutra_bridge vm stop
python -m lutra_bridge tray
```

`vm start` 使用 `_toolchains\qemu\app\qemu-system-x86_64.exe`，参数与 `env/run-vm.ps1` 一致：`-accel whpx`、`-nic user,hostfwd=tcp::2222-:22`、`-display none`、串口写入 `_vm\serial.log`。WHPX 启动失败会自动降级 `tcg`。PID 写入 `_vm\qemu.pid`，重复启动返回 `EVM_RUNNING`。

`vm stop` 先经 SSH 尝试 `sudo -n systemctl poweroff`；失败或超时才终止进程。`--force` 必须配合 `--yes`。

## 4. 在 AI 客户端中配置 MCP

### Claude Code / Cursor / Trae / WorkBuddy（stdio）

把以下片段合并进客户端的 MCP 配置（Claude Code 为 `~/.claude.json` 或项目 `.mcp.json`；其他客户端为各自的 `mcpServers` 配置）：

```json
{
  "mcpServers": {
    "lutra": {
      "command": "python",
      "args": ["-m", "lutra_bridge", "mcp", "--stdio", "--profile", "default"],
      "env": {
        "PYTHONPATH": "M:\\lutraos\\win",
        "LUTRA_HOME": "M:\\lutraos",
        "LUTRA_BRIDGE_POLICY": "ask"
      }
    }
  }
}
```

仓库内 `mcp_config.json` 即上述示例，可直接复制。`command` 改成 `python.exe` 的绝对路径也可以（Python 安装在 C 盘不影响，程序本身不写 C 盘）。

### 多台 Lutra

```json
{
  "mcpServers": {
    "lutra-vm": {
      "command": "python",
      "args": ["-m", "lutra_bridge", "mcp", "--stdio", "--target", "vm"],
      "env": { "PYTHONPATH": "M:\\lutraos\\win", "LUTRA_HOME": "M:\\lutraos" }
    }
  }
}
```

当前仅实现本地 VM 目标；`--target 192.168.1.50:8443`（裸机 mTLS）会明确报“尚未实现”。

## 5. MCP 工具

| 工具 | 作用 | 危险级 | 需同意 | VM 未运行 |
|---|---|---|---|---|
| `lutra_status` | VM/SSH/磁盘/审计链状态 | 低 | 否 | ✅ |
| `lutra_vm_start` | 启动 VM（内存/CPU/端口可调） | 中 | 否 | ✅ |
| `lutra_vm_stop` | 优雅关机，失败后终止进程 | 中 | `force` 时需要 | — |
| `lutra_exec` | 在 Lutra 内执行命令 | 高 | **是** | — |
| `lutra_read_file` | 读文本文件 | 中 | 敏感路径需要 | — |
| `lutra_write_file` | 写文件（禁止系统路径） | 高 | **是** | — |
| `lutra_list_dir` | 列目录 | 低 | 否 | — |
| `lutra_pkg_install` | 安装 `.ltr` 包 | 高 | **是** | — |
| `lutra_pkg_list` | 列出已装包 | 低 | 否 | — |
| `lutra_screenshot` | 截图并保存到 `_vm\screenshots` | 中 | 否 | — |
| `lutra_clipboard_get` | 读 Lutra 剪贴板 | 低 | 否 | — |
| `lutra_clipboard_set` | 写 Lutra 剪贴板 | 中 | **是** | — |
| `lutra_open_url` | 在 Lutra 内打开 http/https | 中 | 否 | — |
| `lutra_audit_tail` | 读审计尾部并校验哈希链 | 低 | 否 | ✅ |

返回结构：

```json
{ "ok": true, "data": {}, "session": "sess_xxx", "audit": "evt_xxx" }
{ "ok": false, "error": { "code": "EPERM", "message": "用户拒绝授权" } }
```

MCP 层同时给出 `content`（JSON 文本；截图可内联 PNG）与 `isError`。

## 6. 安全策略

- **路径边界**：桥接自身所有本地写入（PID、审计、截图、授权文件、上传源）必须位于 `M:\lutraos`；越界返回 `EPATH`。
- **远端写入**：`lutra_write_file` 只允许 `/home`、`/tmp`、`/opt/ltr` 等用户空间；`/etc`、`/boot`、`/usr`、`/dev` 等默认拒绝。确需修改系统文件时设置 `LUTRA_BRIDGE_ALLOW_SYSTEM_WRITE=1`。
- **危险命令**：无论策略如何，`rm -rf /`、`mkfs`、`dd of=/dev/...`、fork 炸弹、`format X:`、`diskpart`、关机/重启等一律 `EPERM`。关机请用 `lutra_vm_stop`。
- **会话级同意**：`LUTRA_BRIDGE_POLICY` 可取 `ask`（默认）/`allow`/`deny`。
  - `ask`：MCP 的 stdin 不是终端，无法弹窗，因此默认拒绝高危工具；需要用户在终端运行（stdin 为 TTY）确认，或预先写授权文件：

    ```json
    M:\lutraos\_vm\consent\<session>.json
    { "session": "sess_xxx", "tools": ["lutra_exec"], "issued_at": 0, "expires_at": 1790000000 }
    ```

    TTL 上限 8 小时；`"*"` 表示该会话全部敏感工具。会话 ID 见 `lutra_status` 返回的 `session`。
  - `allow`：信任当前客户端，高危工具直接放行（危险命令黑名单仍然生效）。
  - `deny`：全部敏感工具拒绝。
- **不提供永久授权**；会话结束或 TTL 到期即失效。
- **审计**：每次工具调用（含被拒绝）写入 `_vm\audit\bridge-YYYYMMDD.jsonl`，字段 `ts / session / tool / args / result / decision / prev_hash / hash`。`hash = SHA-256(去掉 hash 的规范化 JSON)`，`prev_hash` 指向上一行。`lutra_audit_tail` 会校验整链并报告 `broken_at`。
- 参数中的 `password/token/secret/api_key/authorization/cookie` 自动脱敏，长文本截断到 400 字符。

SSH 凭据可用环境变量覆盖：`LUTRA_VM_HOST`、`LUTRA_VM_PORT`、`LUTRA_VM_USER`、`LUTRA_VM_PASS`（默认 `lutra/lutra`）。若 `_vm\keys\id_ed25519` 或 `id_rsa` 存在则优先用密钥。

## 7. 故障排查

| 现象 | 处理 |
|---|---|
| `EQEMU` | 确认 `M:\lutraos\_toolchains\qemu\app\qemu-system-x86_64.exe` 存在 |
| `EDISK` | 确认 `M:\lutraos\_vm\build.qcow2` 存在（先用构建脚本产出） |
| `ESSH` / `ETIMEOUT` | 看 `_vm\serial.log`；VM 刚启动需等 sshd 30~60 秒；确认 hostfwd 2222 |
| `EPERM` | 高危工具未获同意，见第 6 节；或命令命中危险模式 |
| `EAUDIT` | 检查 `_vm\audit` 可写（审计失败时操作会拒绝） |
| 托盘不显示 | 安装 pystray/pillow；或直接用 `mcp`/`vm` 子命令 |

## 8. 尚未实现

- HTTP+SSE 传输（`mcp --http`）、多客户端共享实例。
- 裸机 mTLS 8443 配对与证书签发。
- QMP 快照/回滚（`lutra_vm_snapshot` / `lutra_vm_rollback`）、`lutra_transfer` 投递分区、`lutra_session_list`。
- 首次启动自动生成 SSH 密钥并用 DPAPI 加密（目前使用密码或预先放置的 `_vm\keys`）。
- 截图/剪贴板/装包依赖 Lutra 内存在 `grim|import|scrot`、`wl-*|xclip`、`lutra-pkg`；命令名按工程约定假设，未在真实镜像上联调。
