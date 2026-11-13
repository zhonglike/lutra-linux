"""Lutra Bridge 的配置、路径与安全策略。

所有本地写入都必须落在 LUTRA_ROOT（默认 M:\\lutraos）之内。
"""
from __future__ import annotations

import json
import os
import re
import secrets
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

__version__ = "0.1.0"

PACKAGE_DIR = Path(__file__).resolve().parent
WIN_DIR = PACKAGE_DIR.parent


def _default_root() -> Path:
    return WIN_DIR.parent


# 默认根目录为 M:\lutraos。LUTRA_HOME 只允许指向其子目录，
# 防止把审计/PID/截图等写入写到 M 盘之外。
_DEFAULT_ROOT = _default_root().resolve()
_env_root = os.environ.get("LUTRA_HOME")
LUTRA_ROOT = _DEFAULT_ROOT
if _env_root:
    try:
        _candidate = Path(_env_root).resolve()
        _common = os.path.commonpath([str(_DEFAULT_ROOT), str(_candidate)])
        if os.path.normcase(_common) == os.path.normcase(str(_DEFAULT_ROOT)):
            LUTRA_ROOT = _candidate
    except (OSError, ValueError):
        LUTRA_ROOT = _DEFAULT_ROOT

PY_DEPS_DIR = LUTRA_ROOT / "_cache" / "python"
TMP_DIR = LUTRA_ROOT / "_cache" / "tmp"

QEMU_DIR = LUTRA_ROOT / "_toolchains" / "qemu" / "app"
QEMU_EXE = QEMU_DIR / "qemu-system-x86_64.exe"

VM_DIR = LUTRA_ROOT / "_vm"
DISK_PATH = VM_DIR / "build.qcow2"
SERIAL_LOG = VM_DIR / "serial.log"
PID_FILE = VM_DIR / "qemu.pid"
AUDIT_DIR = VM_DIR / "audit"
CONSENT_DIR = VM_DIR / "consent"
SCREENSHOT_DIR = VM_DIR / "screenshots"
KEYS_DIR = VM_DIR / "keys"

SSH_HOST = os.environ.get("LUTRA_VM_HOST", "127.0.0.1")
SSH_PORT = int(os.environ.get("LUTRA_VM_PORT", "2222"))
SSH_USER = os.environ.get("LUTRA_VM_USER", "lutra")
SSH_PASSWORD = os.environ.get("LUTRA_VM_PASS", "lutra")

DEFAULT_MEM_MB = 4096
DEFAULT_CPU = 8
MAX_CONSENT_TTL = 8 * 3600


class BridgeError(Exception):
    """带错误码与可操作提示的桥接错误。"""

    def __init__(self, code: str, message: str, hint: str | None = None):
        super().__init__(message)
        self.code = code
        self.message = message
        self.hint = hint

    def to_dict(self) -> dict:
        data = {"code": self.code, "message": self.message}
        if self.hint:
            data["hint"] = self.hint
        return data


# ---------------------------------------------------------------------------
# 时间 / 路径工具
# ---------------------------------------------------------------------------

def utc_now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")


def local_day() -> str:
    return datetime.now().strftime("%Y%m%d")


def ensure_dirs() -> None:
    for d in (VM_DIR, AUDIT_DIR, CONSENT_DIR, SCREENSHOT_DIR, TMP_DIR):
        Path(d).mkdir(parents=True, exist_ok=True)


def ensure_under_root(path: str | os.PathLike, what: str = "路径") -> Path:
    """校验本地路径位于 LUTRA_ROOT 之内。"""
    try:
        resolved = Path(path).expanduser().resolve()
    except (OSError, ValueError) as exc:
        raise BridgeError("EPATH", f"无法解析{what}：{path}（{exc}）") from exc
    try:
        common = os.path.commonpath([str(LUTRA_ROOT), str(resolved)])
    except ValueError as exc:
        raise BridgeError("EPATH", f"{what}越界：{resolved} 不在 {LUTRA_ROOT} 内") from exc
    if os.path.normcase(common) != os.path.normcase(str(LUTRA_ROOT)):
        raise BridgeError(
            "EPATH",
            f"{what}越界：{resolved} 不在 {LUTRA_ROOT} 内",
            "本程序只允许访问 M:\\lutraos 下的文件",
        )
    return resolved


def atomic_write_bytes(path: str | os.PathLike, data: bytes) -> Path:
    target = ensure_under_root(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=target.name + ".", suffix=".tmp", dir=str(target.parent))
    try:
        with os.fdopen(fd, "wb") as fh:
            fh.write(data)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp_name, target)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass
    return target


def atomic_write_text(path: str | os.PathLike, text: str) -> Path:
    return atomic_write_bytes(path, text.encode("utf-8"))


def normalize_remote_path(path: str) -> str:
    """规范化 Lutra 内的绝对路径，拒绝 NUL 与 .. 穿越。"""
    if not isinstance(path, str) or not path.strip():
        raise BridgeError("EINVAL", "路径不能为空")
    if "\x00" in path:
        raise BridgeError("EINVAL", "路径包含非法字符 NUL")
    raw = path.strip().replace("\\", "/")
    if not raw.startswith("/"):
        raise BridgeError("EINVAL", f"必须是 Lutra 内的绝对路径：{path}", "例如 /home/lutra/notes.txt")
    parts: list[str] = []
    for seg in raw.split("/"):
        if seg in ("", "."):
            continue
        if seg == "..":
            raise BridgeError("EPERM", f"路径不允许包含 ..：{path}")
        parts.append(seg)
    return "/" + "/".join(parts)


_PROTECTED_WRITE_PREFIXES = (
    "/etc", "/boot", "/usr", "/bin", "/sbin", "/lib", "/lib64",
    "/dev", "/proc", "/sys", "/run",
)


def check_remote_write_path(path: str) -> str:
    normalized = normalize_remote_path(path)
    if normalized == "/":
        raise BridgeError("EPERM", "拒绝直接写入根目录 /")
    if os.environ.get("LUTRA_BRIDGE_ALLOW_SYSTEM_WRITE") == "1":
        return normalized
    for prefix in _PROTECTED_WRITE_PREFIXES:
        if normalized == prefix or normalized.startswith(prefix + "/"):
            raise BridgeError(
                "EPERM",
                f"拒绝写入系统路径：{normalized}",
                "写操作仅限 /home、/tmp、/opt/ltr 等用户空间路径；"
                "确需修改系统文件时设置 LUTRA_BRIDGE_ALLOW_SYSTEM_WRITE=1",
            )
    return normalized


_SECRET_PATTERNS = tuple(
    re.compile(p, re.IGNORECASE)
    for p in (
        r"^/etc/(shadow|gshadow|sudoers)",
        r"^/root/",
        r"/\.ssh/",
        r"(^|/)id_(rsa|ed25519|ecdsa)$",
        r"\.pem$",
        r"^/etc/ssl/private/",
    )
)


def is_secret_path(path: str) -> bool:
    try:
        normalized = normalize_remote_path(path)
    except BridgeError:
        return False
    return any(pattern.search(normalized) for pattern in _SECRET_PATTERNS)


# ---------------------------------------------------------------------------
# 危险命令策略
# ---------------------------------------------------------------------------

_DANGEROUS_PATTERNS: tuple[tuple[re.Pattern[str], str], ...] = (
    (re.compile(r"\brm\s+(?:-[^\s]+\s+)*(?:/|/\*|~|~/)(?:\s|$|;|&|\|)"), "递归删除根/家目录"),
    (re.compile(r"--no-preserve-root"), "绕过删除保护"),
    (re.compile(r"\bmkfs(?:\.[a-z0-9]+)?\b", re.IGNORECASE), "格式化文件系统"),
    (re.compile(r"\bdd\b[^\n;&|]*\bof\s*=\s*/dev/", re.IGNORECASE), "直接写块设备"),
    (re.compile(r">\s*/dev/(?:sd|nvme|vd|mmcblk|hd)", re.IGNORECASE), "重定向写块设备"),
    (re.compile(r":\(\)\s*\{[^}]*:\s*\|\s*:"), "fork 炸弹"),
    (re.compile(r"\bchmod\s+-R\s+777\s+/(?:\s|$)", re.IGNORECASE), "全盘 777"),
    (re.compile(r"\b(?:shutdown|reboot|poweroff|halt|init\s+0)\b", re.IGNORECASE), "关机/重启"),
    (re.compile(r"\bsystemctl\s+(?:poweroff|reboot|halt)\b", re.IGNORECASE), "关机/重启"),
    (re.compile(r"\bformat\s+[a-z]:", re.IGNORECASE), "格式化 Windows 分区"),
    (re.compile(r"\b(?:del|erase)\s+/[fsq]", re.IGNORECASE), "Windows 静默删除"),
    (re.compile(r"\b(?:rd|rmdir)\s+/s", re.IGNORECASE), "Windows 递归删除"),
    (re.compile(r"\bRemove-Item\b[^\n]*-Recurse[^\n]*[A-Za-z]:\\", re.IGNORECASE), "PowerShell 递归删除"),
    (re.compile(r"\bdiskpart\b|\bbcdedit\b|\bcipher\s+/w", re.IGNORECASE), "磁盘/引导级操作"),
)


def find_dangerous_command(cmd: str) -> str | None:
    if not isinstance(cmd, str) or not cmd:
        return None
    text = cmd.replace("\x00", "")
    for pattern, label in _DANGEROUS_PATTERNS:
        if pattern.search(text):
            return label
    return None


# ---------------------------------------------------------------------------
# 权限代理（会话级同意）
# ---------------------------------------------------------------------------

SENSITIVE_TOOLS = {
    "lutra_exec": "在 Lutra 内执行命令",
    "lutra_write_file": "写入 Lutra 内文件",
    "lutra_pkg_install": "安装 .ltr 系统包",
    "lutra_clipboard_set": "写入 Lutra 剪贴板",
}


def consent_reasons(tool: str, args: dict | None = None) -> list[str]:
    args = args or {}
    reasons: list[str] = []
    if tool in SENSITIVE_TOOLS:
        reasons.append(SENSITIVE_TOOLS[tool])
    if tool == "lutra_read_file" and is_secret_path(str(args.get("path", ""))):
        reasons.append("读取敏感文件（密钥/口令/系统配置）")
    if tool == "lutra_vm_stop" and bool(args.get("force")):
        reasons.append("强制终止虚拟机进程")
    return reasons


def new_session(prefix: str = "sess") -> str:
    return f"{prefix}_{secrets.token_hex(6)}"


def valid_session(session: str) -> bool:
    return bool(re.fullmatch(r"[A-Za-z0-9_-]{1,64}", session or ""))


def consent_file(session: str) -> Path:
    if not valid_session(session):
        raise BridgeError("EINVAL", f"非法会话 ID：{session!r}")
    return CONSENT_DIR / f"{session}.json"


def read_session_consent(session: str) -> dict | None:
    try:
        path = consent_file(session)
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError, BridgeError):
        return None
    try:
        if float(data.get("expires_at", 0)) < time.time():
            return None
    except (TypeError, ValueError):
        return None
    return data


def grant_session(session: str, tools: list[str], ttl: int = 7200) -> Path:
    """写会话授权文件（供外部 UI/脚本预授权）。"""
    if not valid_session(session):
        raise BridgeError("EINVAL", f"非法会话 ID：{session!r}")
    ttl = max(60, min(int(ttl), MAX_CONSENT_TTL))
    ensure_dirs()
    payload = {
        "session": session,
        "tools": sorted({str(t) for t in tools}),
        "issued_at": int(time.time()),
        "expires_at": int(time.time()) + ttl,
    }
    return atomic_write_text(consent_file(session), json.dumps(payload, ensure_ascii=False, indent=2))


class Authorizer:
    """高危操作的策略钩子：策略模式 -> 会话授权文件 -> 交互确认 -> 拒绝。"""

    def __init__(self, session: str, policy: str | None = None, allow_prompt: bool = False):
        mode = (policy or os.environ.get("LUTRA_BRIDGE_POLICY", "ask")).strip().lower()
        self.session = session
        self.policy = mode if mode in ("allow", "deny", "ask") else "ask"
        self.allow_prompt = allow_prompt

    def authorize(self, tool: str, args: dict | None = None) -> tuple[bool, str]:
        reasons = consent_reasons(tool, args)
        if not reasons:
            return True, "auto"

        if self.policy == "allow":
            return True, "policy-allow"
        if self.policy == "deny":
            return False, "策略已拒绝敏感操作（LUTRA_BRIDGE_POLICY=deny）"

        consent = read_session_consent(self.session)
        if consent:
            granted = {str(t) for t in (consent.get("tools") or [])}
            if "*" in granted or tool in granted:
                return True, "session-consent"

        if self.allow_prompt and sys.stdin.isatty():
            print(f"[lutra-bridge] AI 请求：{'、'.join(reasons)}", file=sys.stderr)
            print(f"[lutra-bridge] 工具：{tool}  参数：{json.dumps(sanitize(args), ensure_ascii=False)}", file=sys.stderr)
            try:
                answer = input("允许本次操作？[y/N] ").strip().lower()
            except (EOFError, KeyboardInterrupt):
                answer = ""
            if answer in ("y", "yes"):
                return True, "user-confirmed"
            return False, "用户拒绝授权"

        return False, "需用户同意：" + "、".join(reasons) + (
            "。设置 LUTRA_BRIDGE_POLICY=allow，"
            f"或创建 {consent_file(self.session)} 授予该工具（TTL 上限 8 小时）"
        )


# ---------------------------------------------------------------------------
# 审计脱敏
# ---------------------------------------------------------------------------

_SECRET_KEY_RE = re.compile(r"(pass(word)?|token|secret|api[_-]?key|authorization|cookie)", re.IGNORECASE)
_MAX_AUDIT_STRING = 400


def sanitize(value, depth: int = 0):
    if depth > 6:
        return "<max-depth>"
    if isinstance(value, dict):
        out = {}
        for key, item in value.items():
            if _SECRET_KEY_RE.search(str(key)):
                out[str(key)] = "<redacted>"
            else:
                out[str(key)] = sanitize(item, depth + 1)
        return out
    if isinstance(value, (list, tuple)):
        return [sanitize(item, depth + 1) for item in list(value)[:50]]
    if isinstance(value, bytes):
        return f"<bytes:{len(value)}>"
    if isinstance(value, str):
        if len(value) <= _MAX_AUDIT_STRING:
            return value
        return value[:_MAX_AUDIT_STRING] + f"…(截断，共 {len(value)} 字符)"
    if isinstance(value, Path):
        return str(value)
    if value is None or isinstance(value, (int, float, bool)):
        return value
    return str(value)
