# -*- coding: utf-8 -*-
"""命令行入口：lutra-update。"""
from __future__ import annotations

import argparse
import json
import sys

from . import core
from .manifest import UpdateEntry


def cmd_check(args: argparse.Namespace) -> int:
    entry = core.check(verbose=not args.json)
    if args.json:
        print(json.dumps({"available": entry.to_version if entry else None,
                          "severity": entry.severity if entry else None},
                         ensure_ascii=False))
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    print(json.dumps(core.status(), ensure_ascii=False, indent=2))
    return 0


def cmd_install(args: argparse.Namespace) -> int:
    path = args.file
    if not path:
        state = core.load_state()
        avail = state.get("available")
        if not avail:
            print("没有可用更新")
            return 1
        entry = UpdateEntry(
            to_version=avail["version"], severity=avail.get("severity", "patch"),
            url=avail["url"], sha256=avail["sha256"], size=avail.get("size", 0),
        )
        path = core.download(entry)
    core.install(path, assume_yes=args.yes)
    return 0


def cmd_rollback(args: argparse.Namespace) -> int:
    core.rollback(args.snapshot)
    return 0


def cmd_boot_tick(_: argparse.Namespace) -> int:
    core.boot_tick()
    return 0


def cmd_mark_success(_: argparse.Namespace) -> int:
    core.mark_success()
    return 0


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="lutra-update", description="Lutra 系统更新")
    sub = ap.add_subparsers(dest="cmd", required=True)

    p = sub.add_parser("check", help="检查更新")
    p.add_argument("--json", action="store_true")
    p.set_defaults(func=cmd_check)

    p = sub.add_parser("status", help="查看更新状态")
    p.set_defaults(func=cmd_status)

    p = sub.add_parser("install", help="安装更新")
    p.add_argument("--file", help="直接指定 .lup 文件")
    p.add_argument("--yes", action="store_true")
    p.set_defaults(func=cmd_install)

    p = sub.add_parser("rollback", help="回滚到快照")
    p.add_argument("--snapshot")
    p.set_defaults(func=cmd_rollback)

    p = sub.add_parser("boot-tick", help="（内部）启动计数")
    p.set_defaults(func=cmd_boot_tick)

    p = sub.add_parser("mark-success", help="（内部）标记启动成功")
    p.set_defaults(func=cmd_mark_success)

    args = ap.parse_args(argv)
    return int(args.func(args) or 0)


if __name__ == "__main__":
    sys.exit(main())
