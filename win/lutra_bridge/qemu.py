"""QEMU 生命周期管理：启动 / 停止 / 查询，参数与 env/run-vm.ps1 对齐。"""
from __future__ import annotations

import ctypes
import json
import os
import subprocess
import time
from pathlib import Path

from . import config

_CREATE_NO_WINDOW = 0x08000000
_DETACHED_PROCESS = 0x00000008
_CREATE_NEW_PROCESS_GROUP = 0x00000200


# ---------------------------------------------------------------------------
# PID 文件与存活检测
# ---------------------------------------------------------------------------

def read_pid_file() -> dict | None:
    try:
        return json.loads(config.PID_FILE.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def remove_pid_file() -> None:
    try:
        config.PID_FILE.unlink(missing_ok=True)
    except OSError:
        pass


def pid_alive(pid: int) -> bool:
    if not isinstance(pid, int) or pid <= 0:
        return False
    if os.name != "nt":
        try:
            os.kill(pid, 0)
            return True
        except OSError:
            return False

    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.argtypes = (wintypes.DWORD, wintypes.BOOL, wintypes.DWORD)
    kernel32.OpenProcess.restype = wintypes.HANDLE
    kernel32.WaitForSingleObject.argtypes = (wintypes.HANDLE, wintypes.DWORD)
    kernel32.WaitForSingleObject.restype = wintypes.DWORD
    kernel32.CloseHandle.argtypes = (wintypes.HANDLE,)
    kernel32.CloseHandle.restype = wintypes.BOOL

    synchronize = 0x00100000
    handle = kernel32.OpenProcess(synchronize, False, int(pid))
    if not handle:
        return False
    try:
        return kernel32.WaitForSingleObject(handle, 0) == 0x102  # WAIT_TIMEOUT
    finally:
        kernel32.CloseHandle(handle)


def _tail_text(path: Path, lines: int = 30) -> str:
    try:
        data = path.read_bytes()[-64 * 1024:]
    except OSError:
        return ""
    return "\n".join(data.decode("utf-8", "replace").splitlines()[-lines:])


def _disk_info() -> dict:
    try:
        st = config.DISK_PATH.stat()
        return {"path": str(config.DISK_PATH), "exists": True, "bytes": st.st_size,
                "mtime": time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(st.st_mtime))}
    except OSError:
        return {"path": str(config.DISK_PATH), "exists": False, "bytes": 0, "mtime": None}


# ---------------------------------------------------------------------------
# 查询
# ---------------------------------------------------------------------------

def status() -> dict:
    info = read_pid_file()
    base = {
        "running": False,
        "pid": None,
        "accel": None,
        "mem_mb": None,
        "cpu": None,
        "ssh_port": config.SSH_PORT,
        "ssh": f"{config.SSH_HOST}:{config.SSH_PORT}",
        "disk": _disk_info(),
        "qemu": {"exe": str(config.QEMU_EXE), "exists": config.QEMU_EXE.exists()},
        "serial_log": str(config.SERIAL_LOG),
        "pid_file": str(config.PID_FILE),
    }
    if not info:
        return base

    pid = info.get("pid")
    alive = pid_alive(int(pid)) if str(pid).isdigit() else False
    base.update(
        running=alive,
        pid=int(pid) if str(pid).isdigit() else None,
        stale_pid_file=not alive,
        accel=info.get("accel"),
        mem_mb=info.get("mem_mb"),
        cpu=info.get("cpu"),
        ssh_port=info.get("ssh_port", config.SSH_PORT),
        started_at=info.get("started_at"),
    )
    if not alive:
        base["last"] = info
    return base


# ---------------------------------------------------------------------------
# 启动
# ---------------------------------------------------------------------------

def _build_argv(disk: Path, mem_mb: int, cpu: int, ssh_port: int, accel: str) -> list[str]:
    return [
        str(config.QEMU_EXE),
        "-accel", accel,
        "-smp", str(cpu),
        "-m", str(mem_mb),
        "-nic", f"user,model=virtio-net-pci,hostfwd=tcp::{ssh_port}-:22",
        "-display", "none",
        "-chardev", f"file,id=log,path={config.SERIAL_LOG}",
        "-serial", "chardev:log",
        "-drive", f"file={disk},if=virtio,format=qcow2",
    ]


def _validate(mem_mb: int, cpu: int, ssh_port: int, disk: Path) -> None:
    if not 512 <= int(mem_mb) <= 65536:
        raise config.BridgeError("EINVAL", f"内存大小超出范围：{mem_mb} MB（512~65536）")
    if not 1 <= int(cpu) <= 64:
        raise config.BridgeError("EINVAL", f"CPU 核数超出范围：{cpu}（1~64）")
    if not 1 <= int(ssh_port) <= 65535:
        raise config.BridgeError("EINVAL", f"SSH 端口非法：{ssh_port}")
    if not config.QEMU_EXE.exists():
        raise config.BridgeError(
            "EQEMU",
            f"未找到 QEMU：{config.QEMU_EXE}",
            "确认便携 QEMU 已解压到 M:\\lutraos\\_toolchains\\qemu\\app",
        )
    if not disk.exists():
        raise config.BridgeError(
            "EDISK",
            f"虚拟磁盘不存在：{disk}",
            "先构建镜像，或检查 M:\\lutraos\\_vm\\build.qcow2",
        )


def start(
    mem_mb: int = config.DEFAULT_MEM_MB,
    cpu: int = config.DEFAULT_CPU,
    ssh_port: int = config.SSH_PORT,
    disk: str | os.PathLike | None = None,
    accel: str | None = None,
) -> dict:
    config.ensure_dirs()
    disk_path = config.ensure_under_root(disk or config.DISK_PATH, "虚拟磁盘路径")
    _validate(mem_mb, cpu, ssh_port, disk_path)

    current = status()
    if current["running"]:
        raise config.BridgeError(
            "EVM_RUNNING",
            f"VM 已在运行（pid {current['pid']}）",
            "如需重启，先执行 vm stop",
        )

    env = os.environ.copy()
    env["TMP"] = str(config.TMP_DIR)
    env["TEMP"] = str(config.TMP_DIR)
    env["QEMU_TMPDIR"] = str(config.TMP_DIR)
    env["LUTRA_HOME"] = str(config.LUTRA_ROOT)

    attempts = [accel] if accel else ["whpx", "tcg"]
    err_path = config.TMP_DIR / "qemu.err"
    err_path.parent.mkdir(parents=True, exist_ok=True)

    last_error = ""
    for index, use_accel in enumerate(attempts):
        argv = _build_argv(disk_path, int(mem_mb), int(cpu), int(ssh_port), use_accel)
        mode = "wb" if index == 0 else "ab"
        with err_path.open(mode) as err_fh:
            proc = subprocess.Popen(
                argv,
                stdin=subprocess.DEVNULL,
                stdout=err_fh,
                stderr=err_fh,
                env=env,
                cwd=str(config.TMP_DIR),
                creationflags=_DETACHED_PROCESS | _CREATE_NEW_PROCESS_GROUP | _CREATE_NO_WINDOW,
            )
        time.sleep(1.2)
        if proc.poll() is None:
            pid_payload = {
                "pid": proc.pid,
                "started_at": config.utc_now_iso(),
                "accel": use_accel,
                "mem_mb": int(mem_mb),
                "cpu": int(cpu),
                "ssh_port": int(ssh_port),
                "disk": str(disk_path),
                "serial_log": str(config.SERIAL_LOG),
                "argv": argv,
            }
            config.atomic_write_text(config.PID_FILE, json.dumps(pid_payload, ensure_ascii=False, indent=2))
            return {
                "pid": proc.pid,
                "accel": use_accel,
                "mem_mb": int(mem_mb),
                "cpu": int(cpu),
                "ssh_port": int(ssh_port),
                "ssh": f"{config.SSH_HOST}:{int(ssh_port)}",
                "disk": str(disk_path),
                "serial_log": str(config.SERIAL_LOG),
                "qemu_err": str(err_path),
            }
        last_error = _tail_text(err_path)

    raise config.BridgeError(
        "EVM_START",
        "QEMU 启动失败，进程提前退出",
        last_error or f"查看 {err_path} 与 {config.SERIAL_LOG}",
    )


# ---------------------------------------------------------------------------
# 停止
# ---------------------------------------------------------------------------

def _terminate(pid: int) -> None:
    if not pid_alive(pid):
        return
    try:
        subprocess.run(
            ["taskkill", "/PID", str(pid), "/T", "/F"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            creationflags=_CREATE_NO_WINDOW,
            timeout=15,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        pass
    if pid_alive(pid) and os.name == "nt":
        kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
        process_terminate = 0x0001
        handle = kernel32.OpenProcess(process_terminate, False, pid)
        if handle:
            try:
                kernel32.TerminateProcess(handle, 1)
            finally:
                kernel32.CloseHandle(handle)


def stop(force: bool = False, timeout: int = 45) -> dict:
    timeout = max(1, min(int(timeout), 300))
    current = status()
    if not current["running"]:
        remove_pid_file()
        return {"stopped": False, "already_stopped": True}

    pid = int(current["pid"])
    graceful_started = False
    graceful_detail = None

    if not force:
        try:
            from . import ssh_client
            result = ssh_client.run("sudo -n systemctl poweroff || sudo -n poweroff", timeout=6)
            graceful_started = result.get("exit_code") == 0
            graceful_detail = result.get("stderr", "").strip() or None
        except config.BridgeError as exc:
            graceful_detail = exc.message
        if graceful_started:
            deadline = time.time() + timeout
            while time.time() < deadline and pid_alive(pid):
                time.sleep(0.5)
        else:
            time.sleep(1.0)

    if pid_alive(pid):
        _terminate(pid)
        for _ in range(20):
            if not pid_alive(pid):
                break
            time.sleep(0.25)

    if pid_alive(pid):
        raise config.BridgeError(
            "EVM_STOP",
            f"无法终止 QEMU 进程（pid {pid}）",
            f"手动执行：taskkill /PID {pid} /T /F",
        )

    remove_pid_file()
    return {
        "stopped": True,
        "pid": pid,
        "graceful": graceful_started,
        "force": bool(force),
        "graceful_detail": graceful_detail,
    }
