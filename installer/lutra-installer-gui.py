#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Lutra 图形安装器（GTK4）。

设计原则与命令行版一致：
  - 只对用户明确选中的一块磁盘操作
  - 空盘 / 有数据 / 含系统 三种状态显式展示
  - 非空盘必须手动输入磁盘路径才能继续
  - 共存模式只使用未分配空间，不缩容任何现有分区
实际分区、格式化、解包仍调用 /opt/lutra/lutra-installer.py，保证只有一套安全逻辑。
"""

import importlib.util
import os
import subprocess
import sys
import threading

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

CORE = "/opt/lutra/lutra-installer.py"
ROOTFS_CANDIDATES = [
    "/run/live/medium/LUTRA/rootfs.tar.zst",
    "/run/lutra/rootfs.tar.zst",
    "/cdrom/LUTRA/rootfs.tar.zst",
]

CSS = b"""
window, .background { background-image: none; background-color: #1a1a1a; color: #e6e6e6; }
label { color: #e6e6e6; }
headerbar { background-image: none; background-color: #242424; color: #e6e6e6; border-bottom: 1px solid #333; box-shadow: none; }
button { background-image: none; color: #f2f2f2; background-color: #2f2f2f; border-radius: 8px; padding: 8px 18px; box-shadow: none; }
button:hover { background-color: #3a3a3a; }
label.title { font-size: 26px; font-weight: 700; color: #ffffff; }
label.subtitle { font-size: 14px; color: #9a9a9a; }
label.h2 { font-size: 18px; font-weight: 600; color: #ffffff; }
label.danger { color: #ff6b6b; }
label.ok { color: #57c07a; }
label.mono { font-family: monospace; color: #d0d0d0; }
button.suggested { background-image: none; background-color: #3584e4; color: #ffffff; }
button.destructive { background-image: none; background-color: #c01c28; color: #ffffff; }
list { background-color: #242424; border-radius: 8px; }
row { padding: 10px; }
row:selected { background-color: #3584e4; }
textview { background-color: #101010; color: #d0d0d0; font-family: monospace; }
textview text { background-color: #101010; color: #d0d0d0; }
entry { background-color: #242424; color: #ffffff; border-radius: 6px; }
"""


def load_core():
    spec = importlib.util.spec_from_file_location("lutra_installer_core", CORE)
    mod = importlib.util.module_from_spec(spec)
    # dataclass 需要通过 sys.modules 找到自己所属模块
    sys.modules[spec.name] = mod
    spec.loader.exec_module(mod)
    return mod


def find_rootfs() -> str:
    for p in ROOTFS_CANDIDATES:
        if os.path.exists(p):
            return p
    return ROOTFS_CANDIDATES[0]


class InstallerWindow(Gtk.ApplicationWindow):
    def __init__(self, app):
        super().__init__(application=app, title="安装 Lutra")
        self.set_default_size(920, 640)
        self.core = load_core()
        self.disks = []
        self.selected = None
        self.mode = "erase"
        self.rootfs = find_rootfs()

        provider = Gtk.CssProvider()
        provider.load_from_data(CSS)
        Gtk.StyleContext.add_provider_for_display(
            self.get_display(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
        )

        self.stack = Gtk.Stack()
        self.stack.set_transition_type(Gtk.StackTransitionType.SLIDE_LEFT_RIGHT)
        self.set_child(self.stack)
        self.stack.add_named(self.page_welcome(), "welcome")
        self.stack.add_named(self.page_disk(), "disk")
        self.stack.add_named(self.page_mode(), "mode")
        self.stack.add_named(self.page_confirm(), "confirm")
        self.stack.add_named(self.page_progress(), "progress")
        self.stack.add_named(self.page_done(), "done")
        self.refresh_disks()

    # ---------------------------------------------------------------- 页面
    def page_welcome(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=18,
                      margin_top=70, margin_bottom=40, margin_start=70, margin_end=70)
        title = Gtk.Label(label="安装 Lutra 1.0")
        title.add_css_class("title")
        sub = Gtk.Label(label="轻量、AI 原生的 Linux 发行版", xalign=0)
        sub.add_css_class("subtitle")
        tips = Gtk.Label(
            xalign=0, wrap=True,
            label=("安装前请确认：\n"
                   "• 已备份重要数据\n"
                   "• 整盘安装会清空所选磁盘\n"
                   "• 与 Windows 共存只使用未分配空间，不会缩容现有分区"))
        box.append(title)
        box.append(sub)
        box.append(tips)
        btn = Gtk.Button(label="开始安装")
        btn.add_css_class("suggested")
        btn.set_halign(Gtk.Align.START)
        btn.connect("clicked", lambda *_: self.stack.set_visible_child_name("disk"))
        box.append(btn)
        return box

    def page_disk(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                      margin_top=24, margin_bottom=24, margin_start=32, margin_end=32)
        head = Gtk.Label(label="选择目标磁盘", xalign=0)
        head.add_css_class("h2")
        hint = Gtk.Label(xalign=0, label="只会在你选中的磁盘上操作；其他磁盘不会被写入。")
        hint.add_css_class("subtitle")
        self.disk_list = Gtk.ListBox()
        self.disk_list.set_selection_mode(Gtk.SelectionMode.SINGLE)
        self.disk_list.connect("row-selected", self.on_disk_selected)
        scroll = Gtk.ScrolledWindow(vexpand=True)
        scroll.set_child(self.disk_list)

        btns = Gtk.Box(spacing=8, halign=Gtk.Align.END)
        refresh = Gtk.Button(label="刷新")
        refresh.connect("clicked", lambda *_: self.refresh_disks())
        back = Gtk.Button(label="返回")
        back.connect("clicked", lambda *_: self.stack.set_visible_child_name("welcome"))
        self.next_disk = Gtk.Button(label="下一步")
        self.next_disk.add_css_class("suggested")
        self.next_disk.set_sensitive(False)
        self.next_disk.connect("clicked", lambda *_: self.goto_mode())
        for w in (refresh, back, self.next_disk):
            btns.append(w)
        box.append(head)
        box.append(hint)
        box.append(scroll)
        box.append(btns)
        return box

    def page_mode(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                      margin_top=24, margin_bottom=24, margin_start=32, margin_end=32)
        head = Gtk.Label(label="选择安装方式", xalign=0)
        head.add_css_class("h2")
        group = Gtk.ToggleButton()
        self.mode_buttons = []
        for key, text, desc in [
            ("erase", "整盘安装", "清空所选磁盘并安装 Lutra"),
            ("coexist", "与 Windows 共存", "只使用磁盘上的未分配空间，不缩容现有分区"),
            ("usb", "安装到 U 盘", "写入可移动设备，引导方式为 removable"),
        ]:
            row = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=2)
            rb = Gtk.ToggleButton(label=text)
            rb.set_group(group)
            rb.set_active(key == "erase")
            rb.connect("toggled", self.on_mode_toggled, key)
            d = Gtk.Label(label=desc, xalign=0)
            d.add_css_class("subtitle")
            row.append(rb)
            row.append(d)
            box.append(row)
            self.mode_buttons.append(rb)
        self.mode_hint = Gtk.Label(xalign=0, wrap=True)
        self.mode_hint.add_css_class("subtitle")
        box.append(self.mode_hint)

        btns = Gtk.Box(spacing=8, halign=Gtk.Align.END)
        back = Gtk.Button(label="返回")
        back.connect("clicked", lambda *_: self.stack.set_visible_child_name("disk"))
        nxt = Gtk.Button(label="下一步")
        nxt.add_css_class("suggested")
        nxt.connect("clicked", lambda *_: self.goto_confirm())
        btns.append(back)
        btns.append(nxt)
        box.append(btns)
        return box

    def page_confirm(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12,
                      margin_top=24, margin_bottom=24, margin_start=32, margin_end=32)
        head = Gtk.Label(label="确认安装", xalign=0)
        head.add_css_class("h2")
        self.summary = Gtk.Label(xalign=0, wrap=True)
        self.summary.add_css_class("mono")
        self.warn = Gtk.Label(xalign=0, wrap=True)
        self.warn.add_css_class("danger")
        self.confirm_entry = Gtk.Entry()
        self.confirm_entry.set_placeholder_text("在此输入磁盘路径以确认")
        self.confirm_entry.connect("changed", lambda *_: self.update_confirm_sensitivity())

        btns = Gtk.Box(spacing=8, halign=Gtk.Align.END)
        back = Gtk.Button(label="返回")
        back.connect("clicked", lambda *_: self.stack.set_visible_child_name("mode"))
        self.install_btn = Gtk.Button(label="开始安装")
        self.install_btn.add_css_class("destructive")
        self.install_btn.connect("clicked", lambda *_: self.start_install())
        btns.append(back)
        btns.append(self.install_btn)

        box.append(head)
        box.append(self.summary)
        box.append(self.warn)
        box.append(self.confirm_entry)
        box.append(btns)
        return box

    def page_progress(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=10,
                      margin_top=20, margin_bottom=20, margin_start=24, margin_end=24)
        head = Gtk.Label(label="正在安装", xalign=0)
        head.add_css_class("h2")
        self.spinner = Gtk.Spinner()
        self.spinner.set_halign(Gtk.Align.START)
        self.log_view = Gtk.TextView(editable=False, monospace=True)
        scroll = Gtk.ScrolledWindow(vexpand=True)
        scroll.set_child(self.log_view)
        box.append(head)
        box.append(self.spinner)
        box.append(scroll)
        return box

    def page_done(self):
        box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=16,
                      margin_top=80, margin_bottom=40, margin_start=60, margin_end=60)
        self.done_title = Gtk.Label(label="安装完成", xalign=0)
        self.done_title.add_css_class("title")
        self.done_text = Gtk.Label(xalign=0, wrap=True,
                                   label="请取出安装介质，然后重启进入系统。")
        btn = Gtk.Button(label="重启")
        btn.add_css_class("suggested")
        btn.set_halign(Gtk.Align.START)
        btn.connect("clicked", lambda *_: subprocess.run(["sudo", "-n", "systemctl", "reboot"]))
        box.append(self.done_title)
        box.append(self.done_text)
        box.append(btn)
        return box

    # ---------------------------------------------------------------- 逻辑
    def refresh_disks(self):
        for row in self.disk_list:
            self.disk_list.remove(row)
        self.disks = self.core.scan_disks()
        for d in self.disks:
            kind = d.classify()
            label = {"empty": "空磁盘", "nonempty": "有数据", "system": "含系统/Windows"}[kind]
            row = Gtk.ListBoxRow()
            line = Gtk.Box(spacing=18)
            name = Gtk.Label(label=d.path, xalign=0, width_chars=16)
            size = Gtk.Label(label=f"{d.gib} GB", xalign=0, width_chars=8)
            state = Gtk.Label(label=label, xalign=0)
            state.add_css_class("ok" if kind == "empty" else "danger")
            line.append(name)
            line.append(size)
            line.append(state)
            row.set_child(line)
            row.disk = d
            self.disk_list.append(row)
        self.selected = None
        self.next_disk.set_sensitive(False)

    def on_disk_selected(self, _lb, row):
        self.selected = getattr(row, "disk", None) if row else None
        self.next_disk.set_sensitive(self.selected is not None)

    def goto_mode(self):
        if not self.selected:
            return
        kind = self.selected.classify()
        self.mode_hint.set_text({
            "empty": "目标磁盘为空，可以直接整盘安装。",
            "nonempty": "目标磁盘已有数据，整盘安装会全部删除，下一步需要手动确认。",
            "system": "目标磁盘上检测到系统或 Windows，若整盘安装会破坏它；建议选择「与 Windows 共存」。",
        }[kind])
        self.stack.set_visible_child_name("mode")

    def on_mode_toggled(self, btn, key):
        if btn.get_active():
            self.mode = key
            if key == "coexist" and self.selected:
                free = self.core.free_space_mib(self.selected)
                self.mode_hint.set_text(
                    f"未分配空间：{free} MiB。共存模式不会缩容现有分区；空间不足时安装会中止。")

    def goto_confirm(self):
        d = self.selected
        kind = d.classify()
        lines = [
            f"目标磁盘：{d.path}  ({d.gib} GB)",
            f"磁盘状态：{kind}",
            f"安装方式：{self.mode}",
            f"根文件系统：{self.rootfs}",
        ]
        if self.mode == "coexist":
            lines.append(f"可用未分配空间：{self.core.free_space_mib(d)} MiB")
        self.summary.set_text("\n".join(lines))
        if self.mode in ("erase", "usb") and kind != "empty":
            self.warn.set_text(f"警告：{d.path} 上的数据会被全部删除。请输入磁盘路径以确认。")
            self.confirm_entry.set_visible(True)
            self.install_btn.set_sensitive(False)
        else:
            self.warn.set_text("")
            self.confirm_entry.set_visible(False)
            self.install_btn.set_sensitive(True)
        self.stack.set_visible_child_name("confirm")

    def update_confirm_sensitivity(self):
        if not self.selected:
            return
        need = self.selected.classify() != "empty"
        ok = (not need) or self.confirm_entry.get_text().strip() == self.selected.path
        self.install_btn.set_sensitive(ok)

    def append_log(self, text):
        buf = self.log_view.get_buffer()
        buf.insert(buf.get_end_iter(), text)
        mark = buf.create_mark(None, buf.get_end_iter(), False)
        self.log_view.scroll_mark_onscreen(mark)

    def start_install(self):
        d = self.selected
        args = [sys.executable, CORE, "--disk", d.path, "--mode", self.mode,
                "--rootfs", self.rootfs, "--yes"]
        if d.classify() != "empty":
            args.append("--force-nonempty")
        self.stack.set_visible_child_name("progress")
        self.spinner.start()
        cmd = ["sudo", "-n"] + args

        def worker():
            try:
                proc = subprocess.Popen(cmd, stdout=subprocess.PIPE,
                                        stderr=subprocess.STDOUT, text=True, bufsize=1)
                for line in proc.stdout:
                    GLib.idle_add(self.append_log, line)
                code = proc.wait()
            except Exception as exc:  # noqa: BLE001
                code = -1
                GLib.idle_add(self.append_log, f"\n启动失败：{exc}\n")
            GLib.idle_add(self.finish, code)

        threading.Thread(target=worker, daemon=True).start()

    def finish(self, code):
        self.spinner.stop()
        if code == 0:
            self.stack.set_visible_child_name("done")
        else:
            self.done_title.set_text("安装失败")
            self.done_text.set_text(f"安装器退出码 {code}。上方日志最后几行是原因。")
            self.stack.set_visible_child_name("done")


class InstallerApp(Gtk.Application):
    def __init__(self):
        super().__init__(application_id="os.lutra.Installer")

    def do_activate(self):
        win = self.props.active_window or InstallerWindow(self)
        win.present()


if __name__ == "__main__":
    sys.exit(InstallerApp().run(sys.argv))
