"""系统托盘（可选）：启动/停止 VM、打开 MCP 配置目录、退出。

缺少 pystray / pillow 时打印安装提示并返回 1，不影响 mcp / vm 子命令。
"""
from __future__ import annotations

import os
import sys
import threading

from . import config, qemu


def _load_gui():
    deps = str(config.PY_DEPS_DIR)
    if deps not in sys.path:
        sys.path.insert(0, deps)
    try:
        import pystray
        from PIL import Image, ImageDraw
    except ImportError as exc:
        print(
            "托盘功能不可用：缺少 pystray / pillow。\n"
            f"  原因：{exc}\n"
            f"  安装：python -m pip install --target \"{config.PY_DEPS_DIR}\" pystray pillow\n"
            "  注意：不安装托盘不影响 `python -m lutra_bridge mcp` / `vm` / `exec` / `status`。",
            file=sys.stderr,
        )
        return None
    return pystray, Image, ImageDraw


def _icon_image(Image, ImageDraw):
    image = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    draw = ImageDraw.Draw(image)
    draw.ellipse((4, 4, 60, 60), fill=(32, 160, 120, 255))
    draw.rectangle((22, 17, 28, 48), fill=(255, 255, 255, 255))
    draw.rectangle((22, 42, 46, 48), fill=(255, 255, 255, 255))
    return image


def run() -> int:
    loaded = _load_gui()
    if not loaded:
        return 1
    pystray, Image, ImageDraw = loaded

    config.ensure_dirs()

    def notify(icon, title: str, message: str) -> None:
        try:
            icon.notify(message, title)
        except Exception:  # noqa: BLE001 - 部分 Windows 环境无通知支持
            print(f"[{title}] {message}", file=sys.stderr)

    def in_background(func) -> None:
        threading.Thread(target=func, daemon=True).start()

    def on_start(icon, _item) -> None:
        def work() -> None:
            try:
                result = qemu.start()
                notify(icon, "Lutra", f"VM 已启动（pid {result['pid']}，SSH 127.0.0.1:{result['ssh_port']}）")
            except config.BridgeError as exc:
                notify(icon, "Lutra 启动失败", exc.message)
        in_background(work)

    def on_stop(icon, _item) -> None:
        def work() -> None:
            try:
                result = qemu.stop()
                message = "VM 已停止" if result.get("stopped") else "VM 本来就没有运行"
                notify(icon, "Lutra", message)
            except config.BridgeError as exc:
                notify(icon, "Lutra 停止失败", exc.message)
        in_background(work)

    def on_open_config(icon, _item) -> None:
        try:
            os.startfile(str(config.PACKAGE_DIR))  # type: ignore[attr-defined]
        except Exception as exc:  # noqa: BLE001
            notify(icon, "Lutra", f"无法打开目录：{exc}")

    def status_text(_item) -> str:
        state = qemu.status()
        if state.get("running"):
            return f"状态：运行中（pid {state['pid']}）"
        return "状态：未运行"

    icon = pystray.Icon(
        "lutra-bridge",
        _icon_image(Image, ImageDraw),
        "Lutra Bridge",
        menu=pystray.Menu(
            pystray.MenuItem(status_text, None, enabled=False),
            pystray.Menu.SEPARATOR,
            pystray.MenuItem("启动 VM", on_start),
            pystray.MenuItem("停止 VM", on_stop),
            pystray.Menu.SEPARATOR,
            pystray.MenuItem("打开 MCP 配置目录", on_open_config),
            pystray.MenuItem("退出", lambda _icon, _item: _icon.stop()),
        ),
    )
    icon.run()
    return 0


if __name__ == "__main__":
    raise SystemExit(run())
