"""命令行入口：tray / mcp / vm / exec / status。"""
from __future__ import annotations

import argparse
import json
import sys

from . import __version__, audit, config, qemu, ssh_client


def _configure_streams() -> None:
    for stream in (sys.stdout, sys.stderr):
        try:
            stream.reconfigure(encoding="utf-8", errors="replace")
        except (AttributeError, ValueError):
            pass


def _print_json(payload) -> None:
    print(json.dumps(payload, ensure_ascii=False, indent=2))


def _fail(exc: config.BridgeError) -> int:
    print(f"[{exc.code}] {exc.message}", file=sys.stderr)
    if exc.hint:
        print(f"提示：{exc.hint}", file=sys.stderr)
    return 4


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="python -m lutra_bridge",
        description="Lutra Bridge：Windows ↔ Lutra 双向桥接（托盘 / MCP / QEMU / SSH / 审计）",
    )
    parser.add_argument("--version", action="version", version=f"lutra-bridge {__version__}")
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("tray", help="启动系统托盘（缺少 pystray/pillow 时打印提示并退出）")

    mcp = sub.add_parser("mcp", help="启动 MCP stdio 服务器")
    mcp.add_argument("--stdio", action="store_true", help="使用 stdio 传输（默认）")
    mcp.add_argument("--http", metavar="HOST:PORT", help="HTTP+SSE 模式（当前未实现）")
    mcp.add_argument("--profile", default="default", help="配置名，默认 default")
    mcp.add_argument("--target", default="vm", help='目标，默认 vm；裸机地址（如 192.168.1.50:8443）尚未实现')

    vm = sub.add_parser("vm", help="虚拟机生命周期")
    vm_sub = vm.add_subparsers(dest="vm_action", required=True)

    vm_start = vm_sub.add_parser("start", help="启动 VM")
    vm_start.add_argument("--mem-mb", type=int, default=config.DEFAULT_MEM_MB, help="内存 MB，默认 4096")
    vm_start.add_argument("--cpu", type=int, default=config.DEFAULT_CPU, help="CPU 核数，默认 8")
    vm_start.add_argument("--ssh-port", type=int, default=config.SSH_PORT, help="SSH 转发端口，默认 2222")
    vm_start.add_argument("--disk", default=None, help="虚拟磁盘路径（必须位于 M:\\lutraos 内）")

    vm_stop = vm_sub.add_parser("stop", help="停止 VM")
    vm_stop.add_argument("--force", action="store_true", help="直接终止进程，跳过优雅关机")
    vm_stop.add_argument("--timeout", type=int, default=45, help="等待关机秒数，默认 45")
    vm_stop.add_argument("--yes", action="store_true", help="强停必须带此参数确认")

    vm_sub.add_parser("status", help="查看 VM 状态")

    exec_p = sub.add_parser("exec", help="在 Lutra 内执行命令（高危，需要 --yes）")
    exec_p.add_argument("cmd", help="要执行的命令")
    exec_p.add_argument("--timeout", type=int, default=30, help="超时秒数，默认 30")
    exec_p.add_argument("--cwd", default=None, help="工作目录")
    exec_p.add_argument("--yes", action="store_true", help="确认执行高危命令")

    sub.add_parser("status", help="查看桥接总体状态")
    return parser


def _cmd_mcp(args: argparse.Namespace) -> int:
    if args.http:
        print("HTTP+SSE 模式尚未实现；请使用 --stdio（默认）。", file=sys.stderr)
        return 2
    if args.target not in ("vm", "vm-running"):
        print(
            f"目标 {args.target} 尚未实现：当前仅支持本地 VM（--target vm）。"
            "裸机 mTLS 通道（8443）计划在后续版本提供。",
            file=sys.stderr,
        )
        return 2
    from .mcp_server import McpServer

    McpServer().serve()
    return 0


def _cmd_vm(args: argparse.Namespace) -> int:
    log = audit.AuditLog()
    session = config.new_session("cli")

    if args.vm_action == "start":
        result = qemu.start(mem_mb=args.mem_mb, cpu=args.cpu, ssh_port=args.ssh_port, disk=args.disk)
        log.append("cli.vm_start", vars(args), {"ok": True, "data": result}, session=session)
        _print_json(result)
        return 0

    if args.vm_action == "stop":
        if args.force and not args.yes:
            print(
                "强制停止会直接终止 QEMU 进程（可能丢失未落盘数据）。"
                "确认请追加 --yes。",
                file=sys.stderr,
            )
            return 3
        result = qemu.stop(force=args.force, timeout=args.timeout)
        log.append("cli.vm_stop", vars(args), {"ok": True, "data": result}, session=session)
        _print_json(result)
        return 0

    from .mcp_server import McpServer

    status = McpServer()._tool_status({"probe_ssh": True})
    _print_json(status)
    return 0


def _cmd_exec(args: argparse.Namespace) -> int:
    log = audit.AuditLog()
    session = config.new_session("cli")
    if not args.yes:
        print(
            "exec 是高危操作（会在 Lutra 内执行任意命令）。确认请追加 --yes。",
            file=sys.stderr,
        )
        return 3

    label = config.find_dangerous_command(args.cmd)
    if label:
        reason = f"命令被安全策略拒绝：匹配高危模式「{label}」"
        log.append("cli.exec", {"cmd": args.cmd}, {"ok": False, "error": reason}, session, "deny")
        print(reason, file=sys.stderr)
        return 4

    full_cmd = args.cmd
    if args.cwd:
        import shlex

        full_cmd = f"cd {shlex.quote(args.cwd)} && {args.cmd}"
    result = ssh_client.run(full_cmd, timeout=args.timeout)
    log.append("cli.exec", {"cmd": args.cmd, "cwd": args.cwd}, {"ok": True, "data": result}, session)
    if result["stdout"]:
        sys.stdout.write(str(result["stdout"]))
        if not str(result["stdout"]).endswith("\n"):
            sys.stdout.write("\n")
    if result["stderr"]:
        sys.stderr.write(result["stderr"])
        if not result["stderr"].endswith("\n"):
            sys.stderr.write("\n")
    code = int(result["exit_code"])
    return code if 0 <= code <= 255 else 1


def _cmd_status() -> int:
    from .mcp_server import McpServer

    _print_json(McpServer()._tool_status({"probe_ssh": True}))
    return 0


def main(argv: list[str] | None = None) -> int:
    _configure_streams()
    args = _build_parser().parse_args(argv)
    config.ensure_dirs()
    try:
        if args.command == "tray":
            from .tray import run as run_tray

            return run_tray()
        if args.command == "mcp":
            return _cmd_mcp(args)
        if args.command == "vm":
            return _cmd_vm(args)
        if args.command == "exec":
            return _cmd_exec(args)
        if args.command == "status":
            return _cmd_status()
        return 2
    except config.BridgeError as exc:
        return _fail(exc)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
