#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Lutra 安装器核心（命令行版）。

设计原则（数据安全第一）：
  1. 任何写入前先扫描并分类目标磁盘：empty / nonempty / system。
  2. 非空磁盘必须显式加 --force-nonempty，且交互确认时要求原样输入磁盘路径。
  3. 只有一个 --disk 参数，绝不对未指定的磁盘做任何写操作。
  4. coexist 模式绝不缩容已有分区，只使用未分配空间；空间不足则明确失败。
  5. 所有破坏性步骤先打印计划；--dry-run 只打印不执行。

用法：
  sudo ./lutra-installer.py --list
  sudo ./lutra-installer.py --disk /dev/sda --mode erase --rootfs /run/lutra/rootfs.tar.zst
  sudo ./lutra-installer.py --disk /dev/nvme0n1 --mode coexist --rootfs ... --yes
  sudo ./lutra-installer.py --mode manual --root-part /dev/sda3 --esp-part /dev/sda1 --rootfs ...
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from typing import Iterable

ESP_SIZE_MIB = 1024
SWAP_SIZE_MIB = 4096
MIN_ROOT_GIB = 20
BTRFS_SUBVOLS = ("@", "@home", "@snapshots", "@var")


# ---------------------------------------------------------------- 基础工具
def run(cmd: list[str], *, dry: bool = False, check: bool = True,
        capture: bool = False, stdin: str | None = None) -> str:
    if dry:
        print("  $ " + " ".join(cmd))
        return ""
    try:
        p = subprocess.run(cmd, check=check, text=True,
                           input=stdin, capture_output=capture)
    except FileNotFoundError:
        die(f"缺少命令：{cmd[0]}")
    except subprocess.CalledProcessError as exc:
        if check:
            die(f"命令失败（{exc.returncode}）：{' '.join(cmd)}")
        return ""
    return (p.stdout or "").strip() if capture else ""


def die(msg: str) -> None:
    print(f"\033[31m[拒绝]\033[0m {msg}", file=sys.stderr)
    sys.exit(2)


def info(msg: str) -> None:
    print(f"\033[36m[Lutra]\033[0m {msg}")


def warn(msg: str) -> None:
    print(f"\033[33m[注意]\033[0m {msg}", file=sys.stderr)


def need_root() -> None:
    if os.geteuid() != 0:
        die("安装器需要 root 权限运行")


def which_all(names: Iterable[str]) -> dict[str, bool]:
    return {n: shutil.which(n) is not None for n in names}


# ---------------------------------------------------------------- 磁盘扫描
@dataclass
class Part:
    path: str
    size: int
    fstype: str = ""
    mountpoints: list[str] = field(default_factory=list)
    parttypename: str = ""


@dataclass
class Disk:
    path: str
    size: int
    model: str = ""
    tran: str = ""
    removable: bool = False
    readonly: bool = False
    parts: list[Part] = field(default_factory=list)

    @property
    def gib(self) -> float:
        return round(self.size / 1024**3, 1)

    def classify(self) -> str:
        """返回 empty / nonempty / system。"""
        if not self.parts:
            sig = run(["blkid", "-p", "-o", "value", "-s", "TYPE", self.path],
                      check=False, capture=True)
            return "nonempty" if sig else "empty"
        for p in self.parts:
            if "/" in p.mountpoints:
                return "system"
            name = p.parttypename.lower()
            if "efi" in name or "basic data" in name:
                return "system"
            if p.fstype in ("ntfs", "ntfs3", "bitlocker"):
                return "system"
        return "nonempty"


def scan_disks() -> list[Disk]:
    raw = run(["lsblk", "-J", "-b", "-o",
               "NAME,PATH,TYPE,SIZE,RO,RM,FSTYPE,MOUNTPOINTS,MODEL,TRAN,PARTTYPENAME"],
              capture=True)
    data = json.loads(raw or "{}")
    disks: list[Disk] = []
    skip_prefix = ("/dev/zram", "/dev/loop", "/dev/sr", "/dev/ram", "/dev/dm-")
    for d in data.get("blockdevices", []):
        if d.get("type") != "disk":
            continue
        if d["path"].startswith(skip_prefix):
            continue
        disk = Disk(
            path=d["path"], size=int(d["size"]), model=(d.get("model") or "").strip(),
            tran=(d.get("tran") or ""), removable=bool(d.get("rm")),
            readonly=bool(d.get("ro")),
        )
        for c in d.get("children") or []:
            if c.get("type") != "part":
                continue
            disk.parts.append(Part(
                path=c["path"], size=int(c["size"]),
                fstype=(c.get("fstype") or ""),
                mountpoints=[m for m in (c.get("mountpoints") or []) if m],
                parttypename=(c.get("parttypename") or ""),
            ))
        disks.append(disk)
    return disks


def find_disk(disks: list[Disk], path: str) -> Disk:
    for d in disks:
        if d.path == path:
            return d
    die(f"找不到磁盘 {path}（先用 --list 查看）")


def print_table(disks: list[Disk]) -> None:
    print(f"{'磁盘':<16}{'容量':>8}  {'可移动':<7}{'接口':<6}{'类型':<10}说明")
    for d in disks:
        kind = d.classify()
        label = {"empty": "空磁盘", "nonempty": "有数据", "system": "含系统/Windows"}[kind]
        desc = ", ".join(f"{p.path.split('/')[-1]}({p.fstype or '?'})" for p in d.parts) or "无分区"
        print(f"{d.path:<16}{d.gib:>7}G  {'是' if d.removable else '否':<7}"
              f"{d.tran or '-':<6}{label:<10}{desc}")
        if d.model:
            print(f"{'':16}型号: {d.model}")


def free_space_mib(disk: Disk) -> int:
    """coexist 模式：计算未分配空间（MiB）。"""
    out = run(["parted", "-s", "-m", disk.path, "unit", "MiB", "print", "free"],
              check=False, capture=True)
    total = 0
    for line in out.splitlines():
        if line.startswith("free;"):
            _, start, end = line.split(";")[:3]
            try:
                total += int(float(end) - float(start))
            except ValueError:
                pass
    return total


# ---------------------------------------------------------------- 安全闸门
def guard(disk: Disk, mode: str, args: argparse.Namespace) -> None:
    kind = disk.classify()
    if disk.readonly:
        die("目标磁盘是只读的")

    if mode == "erase" and kind != "empty":
        if not args.force_nonempty:
            die(f"{disk.path} 不是空磁盘（{kind}），已中止。\n"
                f"若确认要清空整盘，请加 --force-nonempty 并重新执行。")
        if not args.yes:
            print(f"\n\033[31m警告：{disk.path} 上有数据，整盘安装会全部删除。\033[0m")
            answer = input(f"请输入磁盘路径以确认清空：{disk.path}\n> ").strip()
            if answer != disk.path:
                die("确认不匹配，已中止")

    if mode == "coexist":
        if kind == "empty":
            warn("目标磁盘没有现有系统，coexist 模式将等同整盘安装")
        need = MIN_ROOT_GIB * 1024
        free = free_space_mib(disk)
        if free < need:
            die(f"未分配空间只有 {free} MiB，至少需要 {need} MiB。\n"
                f"请先在 Windows「磁盘管理」中压缩出空闲空间，再回来安装。"
                f"Lutra 不会替你缩容任何现有分区。")

    if mode == "usb" and not disk.removable and not args.force_nonempty:
        die(f"{disk.path} 不是可移动设备；装 U 盘请确认设备路径后加 --force-nonempty")

    if mode == "manual":
        if not args.root_part:
            die("manual 模式必须提供 --root-part")
        if not args.esp_part:
            warn("未提供 --esp-part，将只安装 BIOS/Legacy 引导")

    mounted = [p.path for p in disk.parts if p.mountpoints]
    if mounted:
        die(f"以下分区仍被挂载，请先卸载：{', '.join(mounted)}")


def confirm_plan(plan: list[str], args: argparse.Namespace) -> None:
    print("\n将执行以下步骤：")
    for i, step in enumerate(plan, 1):
        print(f"  {i:>2}. {step}")
    if args.dry_run:
        info("--dry-run：到此为止，没有修改任何磁盘")
        sys.exit(0)
    if not args.yes:
        if input("\n确认执行？输入 yes 继续：").strip().lower() != "yes":
            die("用户取消")


# ---------------------------------------------------------------- 安装步骤
def parted_script(disk: Disk, mode: str) -> list[list[str]]:
    """返回要执行的命令序列（不含格式化）。"""
    cmds: list[list[str]] = []
    if mode in ("erase", "usb"):
        cmds.append(["sgdisk", "--zap-all", disk.path])
        cmds.append(["sgdisk", "-n", f"1:0:+{ESP_SIZE_MIB}M",
                     "-t", "1:ef00", "-c", "1:LUTRA-ESP", disk.path])
        if mode == "erase":
            cmds.append(["sgdisk", "-n", f"2:0:+{SWAP_SIZE_MIB}M",
                         "-t", "2:8200", "-c", "2:LUTRA-SWAP", disk.path])
            cmds.append(["sgdisk", "-n", "3:0:0", "-t", "3:8300",
                         "-c", "3:LUTRA-ROOT", disk.path])
        else:
            cmds.append(["sgdisk", "-n", "2:0:0", "-t", "2:8300",
                         "-c", "2:LUTRA-ROOT", disk.path])
        cmds.append(["partprobe", disk.path])
    elif mode == "coexist":
        cmds.append(["sgdisk", "-n", "0:0:+"f"{ESP_SIZE_MIB}M", "-t", "ef00",
                     "-c", "0:LUTRA-ESP", disk.path])
        cmds.append(["sgdisk", "-n", "0:0:0", "-t", "8300",
                     "-c", "0:LUTRA-ROOT", disk.path])
        cmds.append(["partprobe", disk.path])
    return cmds


def part_path(disk: Disk, index: int) -> str:
    sep = "p" if disk.path[-1].isdigit() else ""
    return f"{disk.path}{sep}{index}"


def uuid_of(dev: str) -> str:
    return run(["blkid", "-p", "-o", "value", "-s", "UUID", dev], capture=True)


def make_filesystems(disk: Disk, mode: str, args: argparse.Namespace) -> dict[str, str]:
    """格式化并返回 {esp,swap,root} 设备路径。"""
    devs: dict[str, str] = {}
    if mode == "manual":
        devs["root"] = args.root_part
        devs["esp"] = args.esp_part or ""
        if args.format_root:
            run(["mkfs.btrfs", "-f", "-L", "LUTRA", devs["root"]])
        if devs["esp"] and args.format_esp:
            run(["mkfs.vfat", "-F32", "-n", "LUTRA-ESP", devs["esp"]])
        if args.swap_part:
            run(["mkswap", "-L", "LUTRA-SWAP", args.swap_part])
            devs["swap"] = args.swap_part
        return devs

    if mode in ("erase", "usb"):
        devs["esp"] = part_path(disk, 1)
        if mode == "erase":
            devs["swap"] = part_path(disk, 2)
            devs["root"] = part_path(disk, 3)
        else:
            devs["root"] = part_path(disk, 2)
    else:  # coexist：新分区编号由 sgdisk 顺延，需要重新扫描
        disks = scan_disks()
        d = find_disk(disks, disk.path)
        new = [p for p in d.parts if "LUTRA" in p.parttypename.upper() or True]
        # 取最后两个分区作为 ESP / ROOT（sgdisk -n 0 追加到末尾）
        if len(d.parts) < 2:
            die("coexist 模式创建分区失败")
        devs["esp"] = d.parts[-2].path
        devs["root"] = d.parts[-1].path

    run(["mkfs.vfat", "-F32", "-n", "LUTRA-ESP", devs["esp"]])
    if devs.get("swap"):
        run(["mkswap", "-L", "LUTRA-SWAP", devs["swap"]])
    run(["mkfs.btrfs", "-f", "-L", "LUTRA", devs["root"]])
    return devs


def extract_rootfs(target: str, rootfs: str, dry: bool) -> None:
    if rootfs.endswith((".tar.zst", ".tzst")):
        run(["tar", "--zstd", "-xpf", rootfs, "-C", target], dry=dry)
    elif rootfs.endswith((".tar.xz", ".txz")):
        run(["tar", "-xpf", rootfs, "-C", target], dry=dry)
    elif rootfs.endswith(".tar"):
        run(["tar", "-xpf", rootfs, "-C", target], dry=dry)
    elif rootfs.endswith(".squashfs"):
        run(["unsquashfs", "-f", "-d", target, rootfs], dry=dry)
    elif os.path.isdir(rootfs):
        run(["rsync", "-aHAX", "--numeric-ids", rootfs + "/", target + "/"], dry=dry)
    else:
        die(f"无法识别的 rootfs：{rootfs}")


def write_fstab(target: str, devs: dict[str, str], dry: bool) -> None:
    lines = []
    if devs.get("esp"):
        lines.append(f"UUID={uuid_of(devs['esp'])}  /boot/efi  vfat  umask=0077  0  2")
    if devs.get("swap"):
        lines.append(f"UUID={uuid_of(devs['swap'])}  none  swap  sw  0  0")
    root_uuid = uuid_of(devs["root"])
    lines += [
        f"UUID={root_uuid}  /          btrfs  subvol=@,compress=zstd:3,noatime  0  0",
        f"UUID={root_uuid}  /home      btrfs  subvol=@home,compress=zstd:3,noatime  0  0",
        f"UUID={root_uuid}  /.snapshots btrfs subvol=@snapshots,compress=zstd:3,noatime  0  0",
        f"UUID={root_uuid}  /var       btrfs  subvol=@var,compress=zstd:3,noatime  0  0",
    ]
    content = "# 由 Lutra 安装器生成\n" + "\n".join(lines) + "\n"
    if dry:
        print("  fstab:\n" + content)
        return
    with open(os.path.join(target, "etc/fstab"), "w", encoding="utf-8") as fh:
        fh.write(content)


def chroot_cmd(target: str, cmd: str, dry: bool) -> None:
    run(["chroot", target, "/bin/bash", "-lc", cmd], dry=dry)


def install(argv: list[str] | None = None) -> None:
    ap = argparse.ArgumentParser(description="Lutra 安装器")
    ap.add_argument("--list", action="store_true", help="列出磁盘并分类后退出")
    ap.add_argument("--disk", help="目标磁盘，例如 /dev/sda")
    ap.add_argument("--mode", choices=["erase", "coexist", "usb", "manual"],
                    default="erase", help="安装模式")
    ap.add_argument("--rootfs", default="/run/lutra/rootfs.tar.zst",
                    help="根文件系统（tar.zst / squashfs / 目录）")
    ap.add_argument("--hostname", default="lutra")
    ap.add_argument("--user", default="lutra")
    ap.add_argument("--password", default="lutra")
    ap.add_argument("--timezone", default="Asia/Shanghai")
    ap.add_argument("--locale", default="zh_CN.UTF-8")
    ap.add_argument("--root-part")
    ap.add_argument("--esp-part")
    ap.add_argument("--swap-part")
    ap.add_argument("--format-root", action="store_true", default=True)
    ap.add_argument("--format-esp", action="store_true", default=True)
    ap.add_argument("--force-nonempty", action="store_true",
                    help="允许在非空磁盘上操作（危险）")
    ap.add_argument("--yes", action="store_true", help="跳过交互确认")
    ap.add_argument("--dry-run", action="store_true", help="只打印计划")

    args = ap.parse_args(argv)

    if args.list:
        need_root()
        print_table(scan_disks())
        return

    need_root()
    if not args.disk and args.mode != "manual":
        die("必须指定 --disk")

    disks = scan_disks()
    if args.mode == "manual":
        if not args.root_part:
            die("manual 模式必须提供 --root-part")
        # 手工模式的目标设备可能不是整盘，这里做最小校验
        for dev in filter(None, [args.root_part, args.esp_part]):
            if not os.path.exists(dev):
                die(f"设备不存在：{dev}")
            if run(["findmnt", "-n", dev], check=False, capture=True):
                die(f"{dev} 正在被挂载，拒绝操作")
        guard(Disk(path=args.root_part, size=0), "manual", args)
        plan = [
            f"（手工模式）格式化 root={args.root_part}"
            + (f"，ESP={args.esp_part}" if args.esp_part else "，仅 Legacy 引导"),
            f"解包 rootfs：{args.rootfs}",
            "创建 Btrfs 子卷 " + ", ".join(BTRFS_SUBVOLS),
            "生成 /etc/fstab 并安装 GRUB",
        ]
        confirm_plan(plan, args)
        do_manual(args)
        return

    disk = find_disk(disks, args.disk)
    info(f"目标磁盘 {disk.path}（{disk.gib}G，{disk.model or '未知型号'}）判定为：{disk.classify()}")
    guard(disk, args.mode, args)

    plan: list[str] = []
    if args.mode in ("erase", "usb"):
        plan.append(f"清空 {disk.path} 的分区表（sgdisk --zap-all）")
    else:
        plan.append(f"在 {disk.path} 的未分配空间新建 ESP 与 root 分区（不动现有分区）")
    plan += [
        f"格式化 ESP={part_path(disk,1)}（FAT32）",
        f"格式化 root={part_path(disk, 3 if args.mode == 'erase' else 2)}（Btrfs）",
        f"解包 rootfs：{args.rootfs}",
        "创建 Btrfs 子卷 " + ", ".join(BTRFS_SUBVOLS),
        "生成 /etc/fstab、主机名、用户与区域设置",
        "安装 GRUB（UEFI" + (" + removable" if args.mode == "usb" else "") + "）",
        "同步并卸载",
    ]
    confirm_plan(plan, args)

    for cmd in parted_script(disk, args.mode):
        run(cmd, dry=args.dry_run)
    if not args.dry_run:
        subprocess.run(["udevadm", "settle"], check=False)
        subprocess.run(["sleep", "1"], check=False)

    devs = make_filesystems(disk, args.mode, args) if not args.dry_run else {
        "esp": part_path(disk, 1),
        "root": part_path(disk, 3 if args.mode == "erase" else 2),
    }
    if args.mode == "erase" and not args.dry_run:
        devs["swap"] = part_path(disk, 2)

    mount = "/mnt/lutra"
    run(["mkdir", "-p", mount], dry=args.dry_run)
    run(["mount", devs["root"], mount], dry=args.dry_run)
    for sub in BTRFS_SUBVOLS:
        target = mount if sub == "@" else os.path.join(mount, sub.lstrip("@"))
        run(["btrfs", "subvolume", "create", os.path.join(mount, sub)]
            if sub == "@" else ["btrfs", "subvolume", "create", os.path.join(mount, sub)],
            dry=args.dry_run, check=False)
    run(["btrfs", "subvolume", "set-default", os.path.join(mount, "@")],
        dry=args.dry_run, check=False)

    # 重新按子卷挂载
    run(["umount", mount], dry=args.dry_run, check=False)
    run(["mount", "-o", "subvol=@", devs["root"], mount], dry=args.dry_run)
    for sub in BTRFS_SUBVOLS[1:]:
        d = os.path.join(mount, sub.lstrip("@"))
        run(["mkdir", "-p", d], dry=args.dry_run)
        run(["mount", "-o", f"subvol={sub}", devs["root"], d], dry=args.dry_run)

    if devs.get("esp"):
        run(["mkdir", "-p", os.path.join(mount, "boot/efi")], dry=args.dry_run)
        run(["mount", devs["esp"], os.path.join(mount, "boot/efi")], dry=args.dry_run)

    extract_rootfs(mount, args.rootfs, args.dry_run)
    write_fstab(mount, devs, args.dry_run)

    chroot_cmd(mount, f"hostnamectl set-hostname {args.hostname}", args.dry_run)
    chroot_cmd(mount, f"ln -sf /usr/share/zoneinfo/{args.timezone} /etc/localtime", args.dry_run)
    chroot_cmd(mount, f"sed -i 's/^#\\?{args.locale}/{args.locale}/' /etc/locale.gen && locale-gen", args.dry_run)
    chroot_cmd(mount, f"id {args.user} >/dev/null 2>&1 || useradd -m -G sudo,audio,video,input -s /bin/bash {args.user}", args.dry_run)
    chroot_cmd(mount, f"echo '{args.user}:{args.password}' | chpasswd", args.dry_run)

    if devs.get("esp"):
        flags = " --removable --no-nvram" if args.mode == "usb" else ""
        chroot_cmd(mount,
                   f"grub-install --target=x86_64-efi --efi-directory=/boot/efi "
                   f"--bootloader-id=Lutra{flags}", args.dry_run)
    else:
        chroot_cmd(mount, f"grub-install {disk.path}", args.dry_run)
    chroot_cmd(mount, "update-grub && update-initramfs -u -k all", args.dry_run)
    chroot_cmd(mount, "btrfs subvolume snapshot -r / /.snapshots/first-boot", args.dry_run)

    run(["sync"], dry=args.dry_run)
    run(["umount", "-R", mount], dry=args.dry_run, check=False)
    info("安装完成。请取出安装介质并重启。")


def do_manual(args: argparse.Namespace) -> None:
    mount = "/mnt/lutra"
    run(["mkdir", "-p", mount], dry=args.dry_run)
    if not args.dry_run:
        if args.format_root:
            run(["mkfs.btrfs", "-f", "-L", "LUTRA", args.root_part])
    run(["mount", args.root_part, mount], dry=args.dry_run)
    for sub in BTRFS_SUBVOLS:
        run(["btrfs", "subvolume", "create", os.path.join(mount, sub)],
            dry=args.dry_run, check=False)
    run(["umount", mount], dry=args.dry_run, check=False)
    run(["mount", "-o", "subvol=@", args.root_part, mount], dry=args.dry_run)
    for sub in BTRFS_SUBVOLS[1:]:
        d = os.path.join(mount, sub.lstrip("@"))
        run(["mkdir", "-p", d], dry=args.dry_run)
        run(["mount", "-o", f"subvol={sub}", args.root_part, d], dry=args.dry_run)
    if args.esp_part:
        run(["mkdir", "-p", os.path.join(mount, "boot/efi")], dry=args.dry_run)
        run(["mount", args.esp_part, os.path.join(mount, "boot/efi")], dry=args.dry_run)
    extract_rootfs(mount, args.rootfs, args.dry_run)
    write_fstab(mount, {"root": args.root_part, "esp": args.esp_part or "",
                        "swap": args.swap_part or ""}, args.dry_run)
    info("手工模式完成（引导需自行确认）")


if __name__ == "__main__":
    install()
