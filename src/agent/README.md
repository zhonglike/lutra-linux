# Lutra 智能层（src/agent）

Rust workspace，实现 `docs/00-architecture.md` L4 与 `docs/02-desktop-and-agent.md` 第 4/6 节定义的智能层。
本目录可独立构建，不依赖镜像里的其他组件。

## 1. 进程与数据流

```text
 MCP 客户端 (Claude Code / OpenCode / IDE)
        │  stdio, newline-delimited JSON-RPC 2.0
        ▼
 ┌─────────────────┐   每个工具一次 tool.call     ┌──────────────────────────┐
 │ lutra-mcpd      │ ───────────────────────────► │ lutrad                   │
 │ tools/list      │ ◄─────────────────────────── │  session 表 / 策略 / 执行 │
 │ tools/call      │        result / error         │  /run/lutra/lutrad.sock  │
 └─────────────────┘                               └───────┬─────────┬────────┘
                                                           │         │
                                    Ed25519 token 文件     │         │ JSONL 追加
                                    /run/lutra/sessions    │         ▼
                                                           │  /var/log/lutra/audit.jsonl
                                                           │  (prev_hash 哈希链)
                                                           ▼
                                              ┌──────────────────────┐
                                              │ lutra-audit verify   │
                                              │ lutra-audit tail     │
                                              └──────────────────────┘

 ltr (pack/install/remove/list/verify) ── .ltr = tar + zstd，安装到 /opt/ltr/<id>/
```

* `lutrad` 不持有 root；只做会话、能力判定与执行，授权确认由 `lutra-guard` 负责。
  本构建用 `EnvGuard` 占位（见 §5），接口仍是 `Approver`。
* 令牌 TTL 默认 2 小时（`DEFAULT_TOKEN_TTL_SECS`，上限 8 小时），存放在 tmpfs；
  重启后签名密钥与令牌一起丢失，符合“无永久授权”的设计原则。

## 2. crate 结构

| 路径 | 作用 |
|---|---|
| `crates/lutra-proto` | JSON-RPC 类型、能力枚举、令牌编解码、审计哈希链（含单元测试） |
| `crates/lutrad` | `main.rs` / `policy.rs` / `session.rs` / `server.rs` |
| `crates/lutra-mcpd` | stdio MCP：`initialize`、`tools/list`、`tools/call`，转发到 lutrad |
| `crates/lutra-audit` | `verify`（校验哈希链）、`tail`（查看尾部） |
| `crates/ltr` | `.ltr` 打包、安装、卸载、列表、校验 |

MCP 工具：`lutra_fs_read`、`lutra_fs_write`、`lutra_exec`、`lutra_net_fetch`、
`lutra_pkg_install`、`lutra_status`。

## 3. Socket 协议

`/run/lutra/lutrad.sock`，按行分隔的 JSON-RPC 2.0。请求/响应各一行，UTF-8。

创建会话：

```json
{"jsonrpc":"2.0","id":1,"method":"session.create","params":{"subject":"agent:claude-code","caps":["fs.read","net"],"ttl_secs":7200}}
{"jsonrpc":"2.0","id":1,"result":{"session":"sess-1a2b3c4d5e6f7a8b","subject":"agent:claude-code","caps":["fs.read","net"],"token":"ltr1.<hex claims>.<hex sig>","issued_at":1763000000,"expires_at":1763007200,"nonce":"..."}}
```

调用工具：

```json
{"jsonrpc":"2.0","id":2,"method":"tool.call","params":{"token":"ltr1....","tool":"lutra_fs_read","arguments":{"path":"/home/lutra/notes.txt"}}}
{"jsonrpc":"2.0","id":2,"result":{"path":"/home/lutra/notes.txt","size":42,"sha256":"...","encoding":"utf-8","content":"...","truncated":false}}
```

能力不足（需弹窗/升级）：

```json
{"jsonrpc":"2.0","id":2,"error":{"code":-32001,"message":"session lacks exec; call guard.authorize to request it","data":{"missing":["exec"]}}}
```

其它方法：`health`、`session.list`、`session.revoke`、`session.verify`、
`guard.authorize`、`policy.explain`。

策略拒绝与能力不足是两回事：`-32005 POLICY_DENIED` 表示目标本身永久禁止
（`/etc/shadow`、`rm`、`127.0.0.1` 等），即使升级能力也不会放行。

## 4. 能力与校验策略

| 能力 | 默认授权 | 备注 |
|---|---|---|
| `fs.read` | 无需确认 | 仅 `$HOME`、`/tmp`、`/var/tmp`；其他绝对路径额外要求 `fs.system` |
| `fs.write` | 需确认 | 同上；`/boot` `/etc` `/usr` `/opt/ltr` `/run/lutra` 等永久拒绝 |
| `exec` | 需确认 | 无 shell；拒绝 `sh/bash/rm/dd/mount/sudo/...`；解释器额外要求 `sys` |
| `net` | 需确认 | 拒绝 loopback/私网/链路本地/CGNAT/metadata；可配 host 白名单 |
| `pkg` | 需确认 | id 必须反向域名；本地 `.ltr` 只能来自 home、/tmp 或商店缓存 |
| `sys` / `display` | 需确认 | 授予 token 前必须经 guard |

额外硬规则：路径一律词法归一化并拒绝 `..`；读写前拒绝符号链接；包内
`safe_join` 拒绝绝对路径与 `..`，符号链接目标不得含 `..` 或绝对路径。

## 5. 令牌与审计实现要点

* 令牌格式 `ltr1.<hex(claims json)>.<hex(sig)>`，签名消息为 `"ltr1." + hex(payload)`，
  避免 payload 重新序列化带来的歧义。
* claims：`v/session/subject/caps/iat/exp/nonce`。`verify()` 同时检查签名、过期、
  会话是否被吊销，以及 nonce 是否已被升级替换（旧令牌立即失效）。
* 审计链：`hash = sha256(prev_hash || "\n" || json(body))`，首条 `prev_hash` 为 64 个 0；
  `AuditLog::open` 会校验尾部，链被篡改时拒绝继续追加。
* `lutra-audit verify` 逐行检查：genesis、seq 连续、prev_hash 链接、自身 hash。
* `ltr` 的发布摘要为 `sha256("LTR1\n" || manifest.json || "\n" || SHA256SUMS)`；
  Ed25519 私钥由 `--key` 指定，公钥通过 `--pubkey`、`LUTRA_PACKAGE_PUBKEY` 或
  `/etc/lutra/keys/ltr.pub` 建立信任锚。

## 6. 构建与运行

```bash
cd src/agent
cargo check                      # 或 cargo build --release
cargo test                       # 策略与审计链单元测试

# 开发用守护进程（临时 key/session 目录，guard 放行需显式设置）
LUTRA_GUARD_MODE=allow \
  cargo run -p lutrad -- --no-session-files --audit /tmp/lutra-audit.jsonl

# MCP server（stdio）
cargo run -p lutra-mcpd

# 审计
cargo run -p lutra-audit -- --path /tmp/lutra-audit.jsonl verify --json
cargo run -p lutra-audit -- --path /tmp/lutra-audit.jsonl tail -n 20

# 打包 / 安装 / 列表 / 校验
# 签名密钥文件为 32 字节十六进制，可用 openssl rand -hex 32 > ltr.key 生成
ltr pack --manifest ./app/manifest.json --payload ./app/payload \
         --hooks ./app/hooks --out ./photocraft.ltr --key ./ltr.key
ltr verify ./photocraft.ltr --pubkey ./ltr.pub
sudo ltr install ./photocraft.ltr --pubkey ./ltr.pub
ltr list
ltr remove com.artcraft.photocraft
```

## 7. 环境变量

| 变量 | 用途 |
|---|---|
| `LUTRA_GUARD_MODE` | `allow` / `deny`；缺省按 `LUTRA_GUARD_APPROVE` 逐能力放行 |
| `LUTRA_GUARD_APPROVE` | 逗号分隔的能力名，开发时模拟用户已确认 |
| `LUTRA_SESSION_TOKEN` | mcpd 复用已有令牌 |
| `LUTRA_SESSION_CAPS` | mcpd 申请的能力集，默认 `fs.read,fs.write,exec,net,pkg,sys,display` |
| `LUTRA_EXEC_ALLOW` | exec 白名单（非空时生效） |
| `LUTRA_NET_ALLOW_HOSTS` / `LUTRA_NET_DENY_HOSTS` | 域名后缀白/黑名单 |
| `LUTRA_LTR_BIN` | `lutra_pkg_install` 调用的 ltr 路径 |
| `LUTRA_PACKAGE_PUBKEY` | 默认包公钥路径 |

## 8. 本构建未实现 / 降级

| 项 | 现状 |
|---|---|
| `lutra-guard` 独立进程与 GTK 授权弹窗 | 未实现；`EnvGuard` 占位，默认拒绝中高风险能力 |
| `net` 的 TLS | 未实现；`https://` 明确返回 `NOT_IMPLEMENTED`，仅支持 http GET |
| 商店按 id 安装 | 未实现；需传本地 `.ltr` 路径 |
| 安装钩子 | 未执行（缺少 `systemd-run --scope` 沙箱），报告中标记 |
| `.desktop` 注册与 `/usr/local/bin` 软链 | 未实现 |
| `lutrad` 到 guard 的 D-Bus 通道 | 未实现，接口保留为 `Approver` |
| 审计按天轮转 | 未实现，单文件哈希链 |
| `ltr` 写审计 | 直接追加，未经过 `lutra-audit` 服务串行化；并发写入需外部锁 |

MCP 客户端配置示例（`claude_desktop_config` 风格）：

```json
{
  "mcpServers": {
    "lutra": { "command": "/usr/bin/lutra-mcpd", "args": [] }
  }
}
```
