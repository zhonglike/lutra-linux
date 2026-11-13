#!/usr/bin/env python3
"""Lutra 构建 VM 远程执行助手（paramiko，纯 M 盘依赖）。

用法：
  python env/vm-ssh.py exec "<shell 命令>" [--timeout 秒]
  python env/vm-ssh.py put <本地> <远端>
  python env/vm-ssh.py get <远端> <本地>
  python env/vm-ssh.py shell            # 交互式（可选）

环境变量：LUTRA_VM_HOST(默认127.0.0.1) LUTRA_VM_PORT(2222)
          LUTRA_VM_USER(lutra) LUTRA_VM_PASS(lutra)
"""
import os
import sys
import time

sys.path.insert(0, r"M:\lutraos\_cache\python")

import paramiko  # noqa: E402

HOST = os.environ.get("LUTRA_VM_HOST", "127.0.0.1")
PORT = int(os.environ.get("LUTRA_VM_PORT", "2222"))
USER = os.environ.get("LUTRA_VM_USER", "lutra")
PASS = os.environ.get("LUTRA_VM_PASS", "lutra")


def connect(retries=30, delay=3):
    last = None
    for _ in range(retries):
        try:
            c = paramiko.SSHClient()
            c.set_missing_host_key_policy(paramiko.AutoAddPolicy())
            c.connect(HOST, port=PORT, username=USER, password=PASS,
                      timeout=10, banner_timeout=30, auth_timeout=30,
                      look_for_keys=False, allow_agent=False)
            return c
        except Exception as e:  # noqa: BLE001
            last = e
            time.sleep(delay)
    raise SystemExit(f"SSH 连接失败: {last}")


def run(cmd, timeout=1800, quiet=False):
    c = connect()
    try:
        chan = c.get_transport().open_session()
        chan.settimeout(timeout)
        chan.exec_command(cmd)
        out = []
        err = []
        while True:
            if chan.recv_ready():
                data = chan.recv(65536).decode("utf-8", "replace")
                out.append(data)
                if not quiet:
                    sys.stdout.write(data)
                    sys.stdout.flush()
            if chan.recv_stderr_ready():
                data = chan.recv_stderr(65536).decode("utf-8", "replace")
                err.append(data)
                if not quiet:
                    sys.stderr.write(data)
                    sys.stderr.flush()
            if chan.exit_status_ready() and not chan.recv_ready() and not chan.recv_stderr_ready():
                break
            time.sleep(0.05)
        code = chan.recv_exit_status()
        if quiet:
            return code, "".join(out), "".join(err)
        return code
    finally:
        c.close()


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        return 1
    op = sys.argv[1]
    if op == "exec":
        cmd = sys.argv[2]
        timeout = 1800
        if "--timeout" in sys.argv:
            timeout = int(sys.argv[sys.argv.index("--timeout") + 1])
        return run(cmd, timeout)
    if op in ("put", "get"):
        c = connect()
        try:
            sftp = c.open_sftp()
            if op == "put":
                sftp.put(sys.argv[2], sys.argv[3])
            else:
                sftp.get(sys.argv[2], sys.argv[3])
        finally:
            c.close()
        return 0
    if op == "shell":
        c = connect()
        chan = c.invoke_shell()
        try:
            while True:
                data = chan.recv(65536)
                if not data:
                    break
                sys.stdout.write(data.decode("utf-8", "replace"))
                sys.stdout.flush()
        except KeyboardInterrupt:
            pass
        finally:
            c.close()
        return 0
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main())
