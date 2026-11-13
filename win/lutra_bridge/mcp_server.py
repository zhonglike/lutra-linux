"""MCP stdio JSON-RPC 服务器，对外暴露 14 个 lutra_* 工具。"""
from __future__ import annotations

import base64
import json
import os
import re
import shlex
import sys
import time
import traceback
import urllib.parse
from pathlib import Path

from . import audit, config, qemu, ssh_client

PROTOCOL_VERSION = "2024-11-05"
SUPPORTED_VERSIONS = {"2024-11-05", "2025-03-26", "2025-06-18"}
MAX_TEXT = 65536


def _tool(name: str, description: str, properties: dict, required: list[str] | None = None) -> dict:
    return {
        "name": name,
        "description": description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required or [],
            "additionalProperties": False,
        },
    }


TOOLS = [
    _tool("lutra_status", "查询 Lutra 运行模式、VM 进程、SSH 可达性、磁盘与审计链状态。",
          {"probe_ssh": {"type": "boolean", "description": "是否探测 SSH（默认 true）"}}),
    _tool("lutra_vm_start", "启动 Lutra 虚拟机（QEMU，hostfwd SSH 到 127.0.0.1:2222）。",
          {
              "mem_mb": {"type": "integer", "minimum": 512, "maximum": 65536, "description": "内存 MB，默认 4096"},
              "cpu": {"type": "integer", "minimum": 1, "maximum": 64, "description": "CPU 核数，默认 8"},
              "ssh_port": {"type": "integer", "minimum": 1, "maximum": 65535, "description": "宿主 SSH 转发端口，默认 2222"},
              "disk": {"type": "string", "description": "可选，覆盖虚拟磁盘路径（必须在 M:\\lutraos 内）"},
          }),
    _tool("lutra_vm_stop", "停止 Lutra 虚拟机：先尝试 SSH 优雅关机，失败后终止进程。",
          {
              "force": {"type": "boolean", "description": "跳过优雅关机，直接终止进程（需用户同意）"},
              "timeout_s": {"type": "integer", "minimum": 1, "maximum": 300, "description": "等待关机秒数，默认 45"},
          }),
    _tool("lutra_exec", "在 Lutra 内执行 shell 命令（高危，需用户同意）。",
          {
              "cmd": {"type": "string", "description": "要执行的命令"},
              "timeout_s": {"type": "integer", "minimum": 1, "maximum": 3600, "description": "超时秒数，默认 30"},
              "cwd": {"type": "string", "description": "可选工作目录"},
          },
          ["cmd"]),
    _tool("lutra_read_file", "读取 Lutra 内的文本文件。",
          {
              "path": {"type": "string", "description": "Lutra 内绝对路径"},
              "max_bytes": {"type": "integer", "minimum": 1024, "maximum": 4194304, "description": "最多读取字节，默认 256 KiB"},
              "encoding": {"type": "string", "description": "文本编码，默认 utf-8"},
          },
          ["path"]),
    _tool("lutra_write_file", "写入 Lutra 内的文件（高危，需用户同意；默认禁止写系统路径）。",
          {
              "path": {"type": "string", "description": "Lutra 内绝对路径（/home、/tmp、/opt/ltr 可写）"},
              "content": {"type": "string", "description": "UTF-8 文本内容"},
              "mode": {"type": "string", "description": "八进制权限，默认 0644"},
              "create_dirs": {"type": "boolean", "description": "自动创建父目录，默认 true"},
          },
          ["path", "content"]),
    _tool("lutra_list_dir", "列出 Lutra 内目录内容。",
          {"path": {"type": "string", "description": "目录绝对路径，默认 /home/lutra"}}),
    _tool("lutra_pkg_install", "安装 .ltr 包（高危，需用户同意）；本地包会先上传到 /tmp/lutra-pkg。",
          {
              "path": {"type": "string", "description": "M:\\lutraos 内的 .ltr 路径，或 Lutra 内已有路径"},
              "yes": {"type": "boolean", "description": "向 lutra-pkg 传 --yes，默认 true"},
          },
          ["path"]),
    _tool("lutra_pkg_list", "列出 Lutra 已安装的 .ltr 包（lutra-pkg list）。", {}),
    _tool("lutra_screenshot", "截取 Lutra 画面（grim / ImageMagick import / scrot），保存到 M:\\lutraos\\_vm\\screenshots。",
          {"inline": {"type": "boolean", "description": "是否把 PNG base64 一并返回，默认 false"}}),
    _tool("lutra_clipboard_get", "读取 Lutra 剪贴板文本（wl-paste 或 xclip）。", {}),
    _tool("lutra_clipboard_set", "写入 Lutra 剪贴板文本（需用户同意）。",
          {"text": {"type": "string", "description": "要写入的文本"}},
          ["text"]),
    _tool("lutra_open_url", "在 Lutra 内用默认浏览器打开 http/https 链接。",
          {"url": {"type": "string", "description": "必须是 http:// 或 https:// 链接"}},
          ["url"]),
    _tool("lutra_audit_tail", "读取桥接审计日志尾部，并校验哈希链。",
          {
              "lines": {"type": "integer", "minimum": 1, "maximum": 500, "description": "返回行数，默认 50"},
              "date": {"type": "string", "description": "可选日期 YYYYMMDD，默认今天"},
          }),
]


def _str_arg(args: dict, name: str, required: bool = False, default: str | None = None) -> str | None:
    value = args.get(name, default)
    if value is None:
        if required:
            raise config.BridgeError("EINVAL", f"缺少参数：{name}")
        return None
    if not isinstance(value, str):
        raise config.BridgeError("EINVAL", f"参数 {name} 必须是字符串")
    return value


def _int_arg(args: dict, name: str, default: int, minimum: int, maximum: int) -> int:
    value = args.get(name, default)
    if isinstance(value, bool):
        raise config.BridgeError("EINVAL", f"参数 {name} 必须是整数")
    try:
        number = int(value)
    except (TypeError, ValueError) as exc:
        raise config.BridgeError("EINVAL", f"参数 {name} 必须是整数") from exc
    if not minimum <= number <= maximum:
        raise config.BridgeError("EINVAL", f"参数 {name} 超出范围（{minimum}~{maximum}）：{number}")
    return number


def _clip(text: str, limit: int = MAX_TEXT) -> tuple[str, bool]:
    if len(text) <= limit:
        return text, False
    return text[:limit] + f"\n…(截断，共 {len(text)} 字符)", True


class McpServer:
    def __init__(self, session: str | None = None, policy: str | None = None, allow_prompt: bool = False):
        config.ensure_dirs()
        env_session = os.environ.get("LUTRA_BRIDGE_SESSION")
        chosen = session or env_session or config.new_session()
        if not config.valid_session(chosen):
            chosen = config.new_session()
        self.session = chosen
        self.audit = audit.AuditLog()
        self.authorizer = config.Authorizer(chosen, policy=policy, allow_prompt=allow_prompt)
        self.handlers = {
            "lutra_status": self._tool_status,
            "lutra_vm_start": self._tool_vm_start,
            "lutra_vm_stop": self._tool_vm_stop,
            "lutra_exec": self._tool_exec,
            "lutra_read_file": self._tool_read_file,
            "lutra_write_file": self._tool_write_file,
            "lutra_list_dir": self._tool_list_dir,
            "lutra_pkg_install": self._tool_pkg_install,
            "lutra_pkg_list": self._tool_pkg_list,
            "lutra_screenshot": self._tool_screenshot,
            "lutra_clipboard_get": self._tool_clipboard_get,
            "lutra_clipboard_set": self._tool_clipboard_set,
            "lutra_open_url": self._tool_open_url,
            "lutra_audit_tail": self._tool_audit_tail,
        }

    # ------------------------------------------------------------------
    # 协议层
    # ------------------------------------------------------------------
    def serve(self, stdin=None, stdout=None) -> None:
        stdin = stdin or sys.stdin.buffer
        stdout = stdout or sys.stdout.buffer
        while True:
            try:
                message = self._read_message(stdin)
            except ValueError as exc:
                self._send(stdout, {"jsonrpc": "2.0", "id": None,
                                    "error": {"code": -32700, "message": f"JSON 解析失败：{exc}"}})
                continue
            if message is None:
                break
            if isinstance(message, list):
                responses = [self._handle(item) for item in message if isinstance(item, dict) and "id" in item]
                if responses:
                    for response in responses:
                        self._send(stdout, response)
                continue
            if not isinstance(message, dict):
                continue
            if "id" not in message:
                continue  # notification（initialized / cancelled）
            response = self._handle(message)
            if response is not None:
                self._send(stdout, response)

    @staticmethod
    def _read_message(stdin):
        line = stdin.readline()
        if not line:
            return None
        if not line.strip():
            return {}
        if line.lower().startswith(b"content-length:"):
            try:
                length = int(line.split(b":", 1)[1].strip())
            except (IndexError, ValueError) as exc:
                raise ValueError(f"非法 Content-Length：{exc}") from exc
            while True:
                header = stdin.readline()
                if not header or header in (b"\r\n", b"\n"):
                    break
            body = stdin.read(length)
            return json.loads(body.decode("utf-8"))
        return json.loads(line.decode("utf-8"))

    @staticmethod
    def _send(stdout, payload: dict) -> None:
        data = (json.dumps(payload, ensure_ascii=False) + "\n").encode("utf-8")
        try:
            stdout.write(data)
            stdout.flush()
        except TypeError:
            stdout.write(data.decode("utf-8"))
            stdout.flush()

    def _handle(self, message: dict) -> dict | None:
        msg_id = message.get("id")
        method = message.get("method")
        params = message.get("params") or {}
        try:
            if method == "initialize":
                requested = params.get("protocolVersion")
                version = requested if requested in SUPPORTED_VERSIONS else PROTOCOL_VERSION
                result = {
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": False}},
                    "serverInfo": {"name": "lutra-bridge", "version": config.__version__},
                    "instructions": (
                        "Lutra Bridge：可启动/停止本地 QEMU 虚拟机，并通过 SSH 操作 Lutra。"
                        "exec / 写文件 / 装包属于高危操作，默认需要用户显式同意。"
                    ),
                }
                return {"jsonrpc": "2.0", "id": msg_id, "result": result}
            if method == "ping":
                return {"jsonrpc": "2.0", "id": msg_id, "result": {}}
            if method == "tools/list":
                return {"jsonrpc": "2.0", "id": msg_id, "result": {"tools": TOOLS}}
            if method == "tools/call":
                return {"jsonrpc": "2.0", "id": msg_id, "result": self._call_tool(params)}
            return {"jsonrpc": "2.0", "id": msg_id,
                    "error": {"code": -32601, "message": f"未知方法：{method}"}}
        except config.BridgeError as exc:
            return {"jsonrpc": "2.0", "id": msg_id, "error": {"code": -32602, "message": exc.message}}
        except Exception as exc:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            return {"jsonrpc": "2.0", "id": msg_id,
                    "error": {"code": -32603, "message": f"内部错误：{type(exc).__name__}: {exc}"}}

    # ------------------------------------------------------------------
    # 工具调用
    # ------------------------------------------------------------------
    def _call_tool(self, params: dict) -> dict:
        name = params.get("name")
        arguments = params.get("arguments") or {}
        if not isinstance(name, str) or name not in self.handlers:
            raise config.BridgeError("ENOTOOL", f"未知工具：{name}")
        if not isinstance(arguments, dict):
            raise config.BridgeError("EINVAL", "arguments 必须是对象")

        reasons = config.consent_reasons(name, arguments)
        decision = "allow"
        if reasons:
            allowed, reason = self.authorizer.authorize(name, arguments)
            if not allowed:
                error = {"code": "EPERM", "message": reason}
                payload = {"ok": False, "error": error, "session": self.session}
                payload["audit"] = self._audit(name, arguments, {"ok": False, "error": error}, "deny")
                return self._format(payload)

        try:
            data = self.handlers[name](arguments)
            image = data.pop("_image_base64", None)
            payload = {"ok": True, "data": data, "session": self.session}
            payload["audit"] = self._audit(name, arguments, {"ok": True, "data": data}, decision)
            return self._format(payload, image=image)
        except config.BridgeError as exc:
            error = exc.to_dict()
            payload = {"ok": False, "error": error, "session": self.session}
            payload["audit"] = self._audit(name, arguments, {"ok": False, "error": error}, "deny")
            return self._format(payload)
        except Exception as exc:  # noqa: BLE001
            traceback.print_exc(file=sys.stderr)
            error = {"code": "EINTERNAL", "message": f"{type(exc).__name__}: {exc}"}
            payload = {"ok": False, "error": error, "session": self.session}
            payload["audit"] = self._audit(name, arguments, {"ok": False, "error": error}, "deny")
            return self._format(payload)

    def _audit(self, tool: str, args: dict, result: dict, decision: str) -> str:
        try:
            return self.audit.append(tool, args, result, session=self.session, decision=decision)
        except Exception as exc:  # noqa: BLE001
            raise config.BridgeError(
                "EAUDIT",
                f"审计日志写入失败，操作已拒绝：{exc}",
                f"检查 {config.AUDIT_DIR} 是否可写",
            ) from exc

    @staticmethod
    def _format(payload: dict, image: str | None = None) -> dict:
        content = [{"type": "text", "text": json.dumps(payload, ensure_ascii=False, indent=2)}]
        if image:
            content.append({"type": "image", "data": image, "mimeType": "image/png"})
        return {"content": content, "isError": not payload.get("ok", False)}

    # ------------------------------------------------------------------
    # 各工具实现
    # ------------------------------------------------------------------
    def _tool_status(self, args: dict) -> dict:
        vm = qemu.status()
        probe = bool(args.get("probe_ssh", True))
        ssh_info = {"reachable": False, "detail": "VM 未运行"}
        if vm.get("running") and probe:
            started = time.time()
            try:
                result = ssh_client.run("uname -sr && (uptime -p 2>/dev/null || uptime)", timeout=3)
                ssh_info = {
                    "reachable": result["exit_code"] == 0,
                    "output": result["stdout"].strip(),
                    "exit_code": result["exit_code"],
                    "latency_ms": int((time.time() - started) * 1000),
                }
            except config.BridgeError as exc:
                ssh_info = {"reachable": False, "detail": exc.message,
                            "latency_ms": int((time.time() - started) * 1000)}
        elif not vm.get("running"):
            ssh_info = {"reachable": False, "detail": "VM 未运行；先调用 lutra_vm_start"}

        chain = self.audit.verify()
        return {
            "mode": "vm-running" if vm.get("running") else "vm-stopped",
            "vm": vm,
            "ssh": ssh_info,
            "audit": {"dir": str(config.AUDIT_DIR), "chain": chain},
            "policy": {"mode": self.authorizer.policy, "session": self.session},
            "paths": {
                "lutra_root": str(config.LUTRA_ROOT),
                "qemu": str(config.QEMU_EXE),
                "disk": str(config.DISK_PATH),
                "serial_log": str(config.SERIAL_LOG),
            },
        }

    def _tool_vm_start(self, args: dict) -> dict:
        mem_mb = _int_arg(args, "mem_mb", config.DEFAULT_MEM_MB, 512, 65536)
        cpu = _int_arg(args, "cpu", config.DEFAULT_CPU, 1, 64)
        ssh_port = _int_arg(args, "ssh_port", config.SSH_PORT, 1, 65535)
        disk = _str_arg(args, "disk")
        return qemu.start(mem_mb=mem_mb, cpu=cpu, ssh_port=ssh_port, disk=disk)

    def _tool_vm_stop(self, args: dict) -> dict:
        force = bool(args.get("force", False))
        timeout = _int_arg(args, "timeout_s", 45, 1, 300)
        return qemu.stop(force=force, timeout=timeout)

    def _tool_exec(self, args: dict) -> dict:
        cmd = _str_arg(args, "cmd", required=True)
        assert cmd is not None
        timeout = _int_arg(args, "timeout_s", 30, 1, 3600)
        cwd = _str_arg(args, "cwd")
        if cwd and ("\x00" in cwd):
            raise config.BridgeError("EINVAL", "cwd 包含非法字符")
        label = config.find_dangerous_command(cmd)
        if label:
            raise config.BridgeError(
                "EPERM",
                f"命令被安全策略拒绝：匹配高危模式「{label}」",
                "请手动在 Lutra 内执行，或改用非破坏性命令",
            )
        full_cmd = f"cd {shlex.quote(cwd)} && {cmd}" if cwd else cmd
        result = ssh_client.run(full_cmd, timeout=timeout)
        stdout, out_cut = _clip(str(result["stdout"]))
        stderr, err_cut = _clip(result["stderr"])
        return {
            "command": cmd,
            "cwd": cwd,
            "exit_code": result["exit_code"],
            "stdout": stdout,
            "stderr": stderr,
            "duration_ms": result["duration_ms"],
            "truncated": result["truncated"] or out_cut or err_cut,
        }

    def _tool_read_file(self, args: dict) -> dict:
        path = config.normalize_remote_path(_str_arg(args, "path", required=True) or "")
        max_bytes = _int_arg(args, "max_bytes", 256 * 1024, 1024, 4 * 1024 * 1024)
        encoding = _str_arg(args, "encoding", default="utf-8") or "utf-8"
        return ssh_client.read_file(path, max_bytes=max_bytes, encoding=encoding)

    def _tool_write_file(self, args: dict) -> dict:
        path = config.check_remote_write_path(_str_arg(args, "path", required=True) or "")
        content = _str_arg(args, "content", required=True)
        assert content is not None
        mode_text = _str_arg(args, "mode", default="0644") or "0644"
        try:
            mode = int(mode_text, 8)
        except ValueError as exc:
            raise config.BridgeError("EINVAL", f"mode 必须是八进制字符串：{mode_text}") from exc
        create_dirs = bool(args.get("create_dirs", True))
        return ssh_client.write_file(path, content, mode=mode, create_dirs=create_dirs)

    def _tool_list_dir(self, args: dict) -> dict:
        path = config.normalize_remote_path(_str_arg(args, "path", default="/home/lutra") or "/home/lutra")
        return ssh_client.list_dir(path)

    def _tool_pkg_install(self, args: dict) -> dict:
        raw = _str_arg(args, "path", required=True) or ""
        assume_yes = bool(args.get("yes", True))

        is_windows = bool(re.match(r"^[A-Za-z]:[\\/]", raw)) or raw.startswith("\\\\")
        local_exists = False
        if not is_windows:
            try:
                local_exists = Path(raw).exists()
            except OSError:
                local_exists = False

        upload_info = None
        if is_windows or local_exists:
            local = config.ensure_under_root(raw, "安装包路径")
            if not local.exists():
                raise config.BridgeError("ENOENT", f"安装包不存在：{local}")
            if local.suffix.lower() != ".ltr":
                raise config.BridgeError("EINVAL", f"只接受 .ltr 包：{local.name}")
            remote = f"/tmp/lutra-pkg/{local.name}"
            mkdir = ssh_client.run("mkdir -p /tmp/lutra-pkg", timeout=15)
            if mkdir["exit_code"] != 0:
                raise config.BridgeError("ESFTP", "创建 /tmp/lutra-pkg 失败", mkdir["stderr"].strip() or None)
            upload_info = ssh_client.upload(local, remote)
            target = remote
        else:
            target = config.normalize_remote_path(raw)

        flag = "--yes " if assume_yes else ""
        result = ssh_client.run(f"lutra-pkg install {flag}{shlex.quote(target)}", timeout=600)
        if result["exit_code"] != 0:
            raise config.BridgeError(
                "EPKG",
                f"安装失败（exit {result['exit_code']}）：{target}",
                (result["stderr"] or result["stdout"]).strip()[:1000] or None,
            )
        return {
            "package": local.name if upload_info else target,
            "remote": target,
            "uploaded": upload_info,
            "exit_code": result["exit_code"],
            "output": (result["stdout"] or result["stderr"]).strip()[:4000],
        }

    def _tool_pkg_list(self, args: dict) -> dict:
        result = ssh_client.run("lutra-pkg list --json", timeout=60)
        if result["exit_code"] == 0:
            text = str(result["stdout"]).strip()
            try:
                return {"packages": json.loads(text), "raw": None}
            except ValueError:
                return {"packages": None, "raw": text[:MAX_TEXT]}
        fallback = ssh_client.run("lutra-pkg list", timeout=60)
        if fallback["exit_code"] != 0:
            raise config.BridgeError(
                "EPKG",
                "lutra-pkg 不可用",
                (fallback["stderr"] or fallback["stdout"]).strip()[:1000] or "确认 VM 内已安装 lutra-pkg",
            )
        return {"packages": None, "raw": str(fallback["stdout"]).strip()[:MAX_TEXT]}

    def _tool_screenshot(self, args: dict) -> dict:
        inline = bool(args.get("inline", False))
        script = (
            "if command -v grim >/dev/null 2>&1; then grim -t png -; "
            "elif command -v import >/dev/null 2>&1; then import -window root png:-; "
            "elif command -v scrot >/dev/null 2>&1; then "
            "f=$(mktemp /tmp/lutra-shot-XXXXXX.png); scrot -o \"$f\" >/dev/null 2>&1 && cat \"$f\"; "
            "rm -f \"$f\"; "
            "else echo '未找到截图工具（grim / import / scrot）' >&2; exit 127; fi"
        )
        result = ssh_client.run(script, timeout=30, binary=True)
        data = result["stdout"]
        if result["exit_code"] != 0 or not isinstance(data, bytes) or not data:
            raise config.BridgeError(
                "ESCREENSHOT",
                f"截图失败（exit {result['exit_code']}）",
                result["stderr"].strip() or "在 Lutra 内安装 grim / imagemagick / scrot",
            )
        if not (data.startswith(b"\x89PNG\r\n\x1a\n") or data.startswith(b"\xff\xd8\xff")):
            raise config.BridgeError("ESCREENSHOT", "返回数据不是 PNG/JPEG 图像")

        suffix = ".png" if data.startswith(b"\x89PNG") else ".jpg"
        name = time.strftime("shot-%Y%m%d-%H%M%S") + suffix
        path = config.atomic_write_bytes(config.SCREENSHOT_DIR / name, data)
        payload = {"path": str(path), "bytes": len(data), "mime": "image/png" if suffix == ".png" else "image/jpeg"}
        if inline:
            payload["_image_base64"] = base64.b64encode(data).decode("ascii")
        return payload

    def _tool_clipboard_get(self, args: dict) -> dict:
        cmd = (
            "if command -v wl-paste >/dev/null 2>&1; then wl-paste --no-newline; "
            "elif command -v xclip >/dev/null 2>&1; then xclip -o -selection clipboard; "
            "else echo '未找到 wl-paste / xclip' >&2; exit 127; fi"
        )
        result = ssh_client.run(cmd, timeout=15)
        if result["exit_code"] != 0:
            raise config.BridgeError("ECLIP", "读取剪贴板失败", result["stderr"].strip() or None)
        text = str(result["stdout"])
        return {"text": text, "chars": len(text)}

    def _tool_clipboard_set(self, args: dict) -> dict:
        text = _str_arg(args, "text", required=True)
        assert text is not None
        cmd = (
            "if command -v wl-copy >/dev/null 2>&1; then wl-copy; "
            "elif command -v xclip >/dev/null 2>&1; then xclip -selection clipboard; "
            "else echo '未找到 wl-copy / xclip' >&2; exit 127; fi"
        )
        result = ssh_client.run(cmd, timeout=15, stdin_data=text)
        if result["exit_code"] != 0:
            raise config.BridgeError("ECLIP", "写入剪贴板失败", result["stderr"].strip() or None)
        return {"written": True, "chars": len(text)}

    def _tool_open_url(self, args: dict) -> dict:
        url = (_str_arg(args, "url", required=True) or "").strip()
        if len(url) > 2048 or re.search(r"[\s\x00-\x1f]", url):
            raise config.BridgeError("EINVAL", "URL 非法或过长")
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc:
            raise config.BridgeError("EINVAL", "只允许打开 http:// 或 https:// 链接")
        cmd = (
            "if ! command -v xdg-open >/dev/null 2>&1; then "
            "echo '未找到 xdg-open' >&2; exit 127; fi; "
            f"nohup xdg-open {shlex.quote(url)} >/dev/null 2>&1 &"
        )
        result = ssh_client.run(cmd, timeout=15)
        if result["exit_code"] != 0:
            raise config.BridgeError("EOPEN", "打开链接失败", result["stderr"].strip() or None)
        return {"url": url, "launched": True}

    def _tool_audit_tail(self, args: dict) -> dict:
        lines = _int_arg(args, "lines", 50, 1, 500)
        day = _str_arg(args, "date")
        if day is not None and not re.fullmatch(r"\d{8}", day):
            raise config.BridgeError("EINVAL", "date 必须是 YYYYMMDD")
        return self.audit.tail(lines=lines, day=day)


def main(argv: list[str] | None = None) -> int:
    """供 __main__ 调用：启动 stdio MCP 服务。"""
    server = McpServer()
    server.serve()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
