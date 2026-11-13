"""SSH/SFTP 通道（paramiko）。

paramiko 预置在 M:\\lutraos\\_cache\\python，本模块会自动把它加入 sys.path。
"""
from __future__ import annotations

import io
import posixpath
import shlex
import socket
import stat as stat_mod
import sys
import time

from . import config

_paramiko = None
_MAX_CAPTURE = 1024 * 1024  # 单次命令最多保留 1 MB 输出


def _load_paramiko():
    global _paramiko
    if _paramiko is not None:
        return _paramiko
    deps = str(config.PY_DEPS_DIR)
    if deps not in sys.path:
        sys.path.insert(0, deps)
    try:
        import paramiko  # noqa: WPS433
    except ImportError as exc:
        raise config.BridgeError(
            "EDEP",
            f"缺少 paramiko：{exc}",
            f"paramiko 应位于 {config.PY_DEPS_DIR}；"
            f"或执行 python -m pip install --target \"{config.PY_DEPS_DIR}\" paramiko",
        ) from exc
    _paramiko = paramiko
    return paramiko


def _connect_hint() -> str:
    return (
        "VM 可能未启动或 SSH 尚未就绪。检查步骤："
        "1) python -m lutra_bridge vm status；"
        f"2) 串口日志 {config.SERIAL_LOG}；"
        "3) 首次启动需等 sshd 就绪（约 30~60 秒）；"
        "4) 凭据可用 LUTRA_VM_USER / LUTRA_VM_PASS 覆盖（默认 lutra/lutra）"
    )


def connect(timeout: int = 8):
    paramiko = _load_paramiko()
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())

    kwargs = {
        "hostname": config.SSH_HOST,
        "port": config.SSH_PORT,
        "username": config.SSH_USER,
        "timeout": timeout,
        "banner_timeout": 30,
        "auth_timeout": 30,
        "look_for_keys": False,
        "allow_agent": False,
    }
    key_file = None
    for name in ("id_ed25519", "id_rsa"):
        candidate = config.KEYS_DIR / name
        if candidate.exists():
            key_file = str(candidate)
            break
    if key_file:
        kwargs["key_filename"] = key_file
    else:
        kwargs["password"] = config.SSH_PASSWORD

    try:
        client.connect(**kwargs)
        return client
    except paramiko.AuthenticationException as exc:
        client.close()
        raise config.BridgeError(
            "EAUTH",
            f"SSH 认证失败：{config.SSH_USER}@{config.SSH_HOST}:{config.SSH_PORT}",
            "确认 VM 内已写入 bridge 公钥，或设置 LUTRA_VM_USER / LUTRA_VM_PASS",
        ) from exc
    except Exception as exc:  # noqa: BLE001 - paramiko 异常种类较多
        client.close()
        raise config.BridgeError(
            "ESSH",
            f"SSH 连接失败（{config.SSH_HOST}:{config.SSH_PORT}）：{type(exc).__name__}: {exc}",
            _connect_hint(),
        ) from exc


def run(
    cmd: str,
    timeout: int = 30,
    stdin_data: str | bytes | None = None,
    binary: bool = False,
) -> dict:
    """在 Lutra 内执行命令，返回 exit_code/stdout/stderr。"""
    if not isinstance(cmd, str) or not cmd.strip():
        raise config.BridgeError("EINVAL", "命令不能为空")
    timeout = max(1, min(int(timeout), 3600))
    client = connect()
    started = time.time()
    channel = None
    try:
        channel = client.get_transport().open_session()
        channel.exec_command(cmd)
        if stdin_data is not None:
            payload = stdin_data.encode("utf-8") if isinstance(stdin_data, str) else stdin_data
            if payload:
                channel.sendall(payload)
            try:
                channel.shutdown_write()
            except OSError:
                pass

        out = bytearray()
        err = bytearray()
        truncated = False
        deadline = started + timeout

        while True:
            if time.time() > deadline:
                raise config.BridgeError(
                    "ETIMEOUT",
                    f"命令执行超时（{timeout}s）：{cmd[:120]}",
                    "可调大 timeout_s，或检查远端进程是否卡住",
                )
            got = False
            if channel.recv_ready():
                chunk = channel.recv(65536)
                got = True
                if len(out) < _MAX_CAPTURE:
                    out.extend(chunk)
                else:
                    truncated = True
            if channel.recv_stderr_ready():
                chunk = channel.recv_stderr(65536)
                got = True
                if len(err) < _MAX_CAPTURE:
                    err.extend(chunk)
                else:
                    truncated = True
            if channel.exit_status_ready() and not channel.recv_ready() and not channel.recv_stderr_ready():
                break
            if not got:
                time.sleep(0.02)

        exit_code = channel.recv_exit_status()
        duration_ms = int((time.time() - started) * 1000)
        stdout = bytes(out)
        stderr = bytes(err).decode("utf-8", "replace")
        if binary:
            stdout_value: str | bytes = stdout
        else:
            stdout_value = stdout.decode("utf-8", "replace")
        return {
            "exit_code": exit_code,
            "stdout": stdout_value,
            "stderr": stderr,
            "duration_ms": duration_ms,
            "truncated": truncated or len(stdout) >= _MAX_CAPTURE or len(stderr) >= _MAX_CAPTURE,
        }
    except config.BridgeError:
        raise
    except socket.timeout as exc:
        raise config.BridgeError(
            "ETIMEOUT",
            f"命令执行超时（{timeout}s）：{cmd[:120]}",
            "可调大 timeout_s，或检查远端进程是否卡住",
        ) from exc
    except Exception as exc:  # noqa: BLE001
        raise config.BridgeError(
            "ESSH",
            f"SSH 命令执行失败：{type(exc).__name__}: {exc}",
            _connect_hint(),
        ) from exc
    finally:
        try:
            if channel is not None:
                channel.close()
        finally:
            client.close()


def _open_sftp(client):
    try:
        return client.open_sftp()
    except Exception as exc:  # noqa: BLE001
        raise config.BridgeError(
            "ESFTP",
            f"无法打开 SFTP 通道：{type(exc).__name__}: {exc}",
            _connect_hint(),
        ) from exc


def read_file(path: str, max_bytes: int = 256 * 1024, encoding: str = "utf-8") -> dict:
    max_bytes = max(1024, min(int(max_bytes), 4 * 1024 * 1024))
    try:
        "".encode(encoding)
    except LookupError as exc:
        raise config.BridgeError("EINVAL", f"不支持的编码：{encoding}") from exc

    client = connect()
    try:
        sftp = _open_sftp(client)
        try:
            st = sftp.stat(path)
            if stat_mod.S_ISDIR(st.st_mode):
                raise config.BridgeError("EINVAL", f"这是一个目录：{path}", "请使用 lutra_list_dir")
            with sftp.open(path, "rb") as remote:
                data = remote.read(max_bytes + 1)
        finally:
            sftp.close()
    except config.BridgeError:
        raise
    except FileNotFoundError as exc:
        raise config.BridgeError("ENOENT", f"文件不存在：{path}") from exc
    except Exception as exc:  # noqa: BLE001
        raise config.BridgeError(
            "ESFTP",
            f"读取失败：{path}（{type(exc).__name__}: {exc}）",
            _connect_hint(),
        ) from exc
    finally:
        client.close()

    truncated = len(data) > max_bytes
    payload = data[:max_bytes]
    return {
        "path": path,
        "size": int(st.st_size),
        "read_bytes": len(payload),
        "truncated": truncated,
        "encoding": encoding,
        "content": payload.decode(encoding, "replace"),
    }


def write_file(path: str, content: str | bytes, mode: int = 0o644, create_dirs: bool = True) -> dict:
    data = content.encode("utf-8") if isinstance(content, str) else bytes(content)
    if create_dirs:
        parent = posixpath.dirname(path) or "/"
        result = run(f"mkdir -p -- {shlex.quote(parent)}", timeout=15)
        if result["exit_code"] != 0:
            raise config.BridgeError(
                "ESFTP",
                f"创建目录失败：{parent}",
                result["stderr"].strip() or None,
            )

    client = connect()
    try:
        sftp = _open_sftp(client)
        try:
            sftp.putfo(io.BytesIO(data), path, confirm=True)
            try:
                sftp.chmod(path, mode)
            except OSError:
                pass
            size = sftp.stat(path).st_size
        finally:
            sftp.close()
    except config.BridgeError:
        raise
    except Exception as exc:  # noqa: BLE001
        raise config.BridgeError(
            "ESFTP",
            f"写入失败：{path}（{type(exc).__name__}: {exc}）",
            _connect_hint(),
        ) from exc
    finally:
        client.close()

    return {"path": path, "bytes": len(data), "remote_size": int(size), "mode": oct(mode)}


def upload(local_path: str, remote_path: str) -> dict:
    client = connect()
    try:
        sftp = _open_sftp(client)
        try:
            sftp.put(str(local_path), remote_path, confirm=True)
            size = sftp.stat(remote_path).st_size
        finally:
            sftp.close()
    except config.BridgeError:
        raise
    except Exception as exc:  # noqa: BLE001
        raise config.BridgeError(
            "ESFTP",
            f"上传失败：{local_path} -> {remote_path}（{type(exc).__name__}: {exc}）",
            _connect_hint(),
        ) from exc
    finally:
        client.close()
    return {"local": str(local_path), "remote": remote_path, "bytes": int(size)}


def download(remote_path: str, local_path: str) -> dict:
    config.ensure_under_root(local_path, "本地下载路径")
    client = connect()
    try:
        sftp = _open_sftp(client)
        try:
            sftp.get(remote_path, str(local_path))
        finally:
            sftp.close()
    except config.BridgeError:
        raise
    except Exception as exc:  # noqa: BLE001
        raise config.BridgeError(
            "ESFTP",
            f"下载失败：{remote_path} -> {local_path}（{type(exc).__name__}: {exc}）",
            _connect_hint(),
        ) from exc
    finally:
        client.close()
    return {"remote": remote_path, "local": str(local_path)}


def list_dir(path: str) -> dict:
    client = connect()
    try:
        sftp = _open_sftp(client)
        try:
            attrs = sftp.listdir_attr(path)
        finally:
            sftp.close()
    except config.BridgeError:
        raise
    except FileNotFoundError as exc:
        raise config.BridgeError("ENOENT", f"目录不存在：{path}") from exc
    except Exception as exc:  # noqa: BLE001
        raise config.BridgeError(
            "ESFTP",
            f"列目录失败：{path}（{type(exc).__name__}: {exc}）",
            _connect_hint(),
        ) from exc
    finally:
        client.close()

    entries = []
    for attr in attrs:
        entries.append({
            "name": attr.filename,
            "type": "dir" if stat_mod.S_ISDIR(attr.st_mode or 0) else "file",
            "size": int(attr.st_size or 0),
            "mode": oct(attr.st_mode or 0),
            "mtime": time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(attr.st_mtime or 0)),
        })
    entries.sort(key=lambda item: (item["type"] != "dir", item["name"].lower()))
    return {"path": path, "count": len(entries), "entries": entries}
