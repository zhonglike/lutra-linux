# -*- coding: utf-8 -*-
"""更新系统核心逻辑：检查、下载、安装、验证、回滚。"""
from __future__ import annotations

import configparser
import datetime as _dt
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.request

from .manifest import (ChannelIndex, UpdateEntry, parse_version, sha256_file,
                       verify_ed25519)

STATE_DIR = "/var/lib/lutra/update"
STATE_FILE = os.path.join(STATE_DIR, "state.json")
CACHE_DIR = os.path.join(STATE_DIR, "cache")
NOTICE_DIR = "/run/lutra-notice"
RELEASE_FILE = "/etc/lutra-release"
CONF_FILE = "/etc/lutra/update.conf"
SNAPSHOT_DIR = "/.snapshots"


# ------------------------------------------------------------------ 基础
def now() -> str:
    return _dt.datetime.now().astimezone().isoformat(timespec="seconds")


def load_config() -> configparser.ConfigParser:
    cfg = configparser.ConfigParser()
    cfg.read_dict({
        "update": {
            "channel": "stable",
            "index_url": "https://updates.lutra.os/stable/index.json",
            "require_signature": "true",
            "pubkey": "/etc/lutra/update-pubkey.pem",
        }
    })
    cfg.read(CONF_FILE, encoding="utf-8")
    return cfg


def current_version() -> str:
    try:
        with open(RELEASE_FILE, encoding="utf-8") as fh:
            for token in re.findall(r"\d+\.\d+\.\d+", fh.read()):
                return token
    except OSError:
        pass
    return "1.0.0"


def load_state() -> dict:
    try:
        with open(STATE_FILE, encoding="utf-8") as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return {
            "current": current_version(),
            "phase": "idle",
            "available": None,
            "snapshot": None,
            "pending_since": None,
            "boot_attempts": 0,
            "last_check": None,
        }


def save_state(state: dict) -> None:
    os.makedirs(STATE_DIR, exist_ok=True)
    tmp = STATE_FILE + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        json.dump(state, fh, ensure_ascii=False, indent=2)
    os.replace(tmp, STATE_FILE)


def require_root() -> None:
    if os.geteuid() != 0:
        sys.exit("需要 root 权限")


def run(cmd: list[str], check: bool = True, capture: bool = False) -> str:
    p = subprocess.run(cmd, check=check, text=True, capture_output=capture)
    return (p.stdout or "").strip() if capture else ""


# ------------------------------------------------------------------ 通知
def notify(severity: str, title: str, body: str) -> None:
    """security 级通知会写 /run/lutra-notice 供桌面 shell 开机弹出。"""
    if severity == "patch":
        return
    os.makedirs(NOTICE_DIR, exist_ok=True)
    path = os.path.join(NOTICE_DIR, f"{severity}.json")
    with open(path, "w", encoding="utf-8") as fh:
        json.dump({"severity": severity, "title": title, "body": body, "ts": now()},
                  fh, ensure_ascii=False)
    # 有图形会话时顺带弹一条
    if shutil.which("notify-send"):
        subprocess.run(["notify-send", "-u", "critical" if severity == "security" else "normal",
                        title, body], check=False)


def clear_notice(severity: str) -> None:
    path = os.path.join(NOTICE_DIR, f"{severity}.json")
    if os.path.exists(path):
        os.remove(path)


# ------------------------------------------------------------------ 检查
def check(verbose: bool = True) -> UpdateEntry | None:
    cfg = load_config()
    index_url = cfg.get("update", "index_url")
    state = load_state()
    state["last_check"] = now()

    try:
        with urllib.request.urlopen(index_url, timeout=20) as resp:
            index = ChannelIndex.from_json(resp.read())
    except Exception as exc:  # noqa: BLE001
        state["phase"] = "idle"
        state["last_error"] = f"检查更新失败：{exc}"
        save_state(state)
        if verbose:
            print(state["last_error"])
        return None

    cur = current_version()
    entry = index.best_update(cur)
    state["current"] = cur
    if entry is None:
        state["available"] = None
        state["phase"] = "idle"
        clear_notice("security")
        clear_notice("feature")
        save_state(state)
        if verbose:
            print(f"已是最新版本 {cur}")
        return None

    state["available"] = {
        "version": entry.to_version,
        "severity": entry.severity,
        "cve": entry.cve,
        "changelog": entry.changelog,
        "url": entry.url,
        "sha256": entry.sha256,
        "size": entry.size,
    }
    state["phase"] = "available"
    save_state(state)

    if entry.severity == "security":
        notify("security", f"Lutra 安全更新 {entry.to_version}",
               "包含重要安全修复，建议立即安装并重启。")
    elif entry.severity == "feature":
        notify("feature", f"Lutra 功能更新 {entry.to_version}",
               "可在「设置 → 系统更新」中查看。")
    if verbose:
        print(f"发现 {entry.severity} 更新：{cur} → {entry.to_version}")
    return entry


# ------------------------------------------------------------------ 下载
def download(entry: UpdateEntry, progress: bool = True) -> str:
    os.makedirs(CACHE_DIR, exist_ok=True)
    name = entry.url.split("/")[-1] or f"lutra-{entry.to_version}.lup"
    dest = os.path.join(CACHE_DIR, name)
    if os.path.exists(dest) and sha256_file(dest).lower() == entry.sha256.lower():
        return dest
    tmp = dest + ".part"

    def hook(blocks: int, block_size: int, total: int) -> None:
        if progress and total > 0:
            done = min(blocks * block_size, total)
            pct = done * 100 // total
            print(f"\r下载 {pct:3d}%  {done // 1048576}MB/{total // 1048576}MB", end="", flush=True)

    urllib.request.urlretrieve(entry.url, tmp, reporthook=hook)
    if progress:
        print()
    if not verify_sha256_simple(tmp, entry.sha256):
        os.remove(tmp)
        raise RuntimeError("下载文件校验失败（sha256 不匹配）")
    os.replace(tmp, dest)
    return dest


def verify_sha256_simple(path: str, expected: str) -> bool:
    return sha256_file(path).lower() == expected.lower()


# ------------------------------------------------------------------ 快照
def btrfs_root_is_subvol() -> bool:
    try:
        out = run(["findmnt", "-n", "-o", "FSTYPE", "/"], capture=True)
        return out.strip() == "btrfs"
    except subprocess.CalledProcessError:
        return False


def create_snapshot(tag: str) -> dict | None:
    if not btrfs_root_is_subvol():
        return None
    os.makedirs(SNAPSHOT_DIR, exist_ok=True)
    name = f"@update-{tag}"
    path = os.path.join(SNAPSHOT_DIR, name)
    if os.path.exists(path):
        shutil.rmtree(path, ignore_errors=True)
    run(["btrfs", "subvolume", "snapshot", "-r", "/", path])
    rootid = run(["btrfs", "inspect-internal", "rootid", path], capture=True)
    return {"name": name, "path": path, "subvolid": int(rootid), "created": now()}


def snapshot_list() -> list[str]:
    try:
        return sorted(n for n in os.listdir(SNAPSHOT_DIR) if n.startswith("@update-"))
    except OSError:
        return []


# ------------------------------------------------------------------ 安装
def extract_lup(lup: str, workdir: str) -> dict:
    run(["tar", "--zstd", "-xf", lup, "-C", workdir])
    manifest_path = os.path.join(workdir, "manifest.json")
    if not os.path.exists(manifest_path):
        raise RuntimeError("无效的 .lup：缺少 manifest.json")
    with open(manifest_path, encoding="utf-8") as fh:
        manifest = json.load(fh)

    cfg = load_config()
    if cfg.getboolean("update", "require_signature", fallback=True):
        sig_path = os.path.join(workdir, "META-INF", "SIGNATURE")
        if not os.path.exists(sig_path):
            raise RuntimeError("更新包缺少签名，已拒绝安装")
        with open(sig_path, "rb") as fh:
            sig = fh.read()
        if not verify_ed25519(open(manifest_path, "rb").read(), sig,
                              cfg.get("update", "pubkey")):
            raise RuntimeError("更新包签名校验失败，已拒绝安装")
    return manifest


def install(lup: str, assume_yes: bool = False) -> None:
    require_root()
    state = load_state()
    if state.get("phase") in ("pending-reboot",):
        sys.exit("已有待重启的更新，请先重启")

    work = tempfile.mkdtemp(prefix="lutra-upd-", dir="/var/tmp")
    try:
        manifest = extract_lup(lup, work)
        to_version = manifest["to_version"]
        from_version = manifest["from_version"]
        cur = current_version()
        if parse_version(from_version) != parse_version(cur):
            sys.exit(f"更新包要求从 {from_version} 升级，当前是 {cur}")

        if not assume_yes:
            print(f"将升级 {from_version} → {to_version}（{manifest.get('severity')}）")
            if input("确认？输入 yes 继续：").strip().lower() != "yes":
                sys.exit("已取消")

        snap = create_snapshot(to_version)
        state["snapshot"] = snap
        state["phase"] = "installing"
        state["available"] = {"version": to_version, "severity": manifest.get("severity", "patch")}
        save_state(state)

        pre = os.path.join(work, "hooks", "pre-update")
        if os.path.exists(pre):
            run(["bash", pre])

        root_payload = os.path.join(work, "payload", "rootfs")
        if os.path.isdir(root_payload):
            for cmd in (["rsync", "-aHAX", "--numeric-ids", root_payload + "/", "/"],
                        ):
                run(cmd)

        kernel_dir = os.path.join(work, "payload", "kernel")
        if os.path.isdir(kernel_dir):
            debs = [os.path.join(kernel_dir, f) for f in os.listdir(kernel_dir)
                    if f.endswith(".deb")]
            if debs:
                run(["dpkg", "-i", *debs], check=False)
                run(["apt-get", "-f", "install", "-y"], check=False)

        post = os.path.join(work, "hooks", "post-update")
        if os.path.exists(post):
            run(["bash", post])

        state["phase"] = "pending-reboot"
        state["pending_since"] = now()
        state["boot_attempts"] = 0
        state["target"] = to_version
        save_state(state)
        notify("security" if manifest.get("severity") == "security" else "feature",
               f"Lutra {to_version} 已安装", "重启后生效。")
        print("更新已安装，重启后生效。")
    finally:
        shutil.rmtree(work, ignore_errors=True)


# ------------------------------------------------------------------ 启动验证
def boot_tick() -> None:
    """由开机服务调用：累计尝试次数，超过阈值则自动回滚。"""
    require_root()
    state = load_state()
    if state.get("phase") != "pending-reboot":
        return
    state["boot_attempts"] = int(state.get("boot_attempts", 0)) + 1
    save_state(state)
    if state["boot_attempts"] > 1:
        print("更新后连续启动失败，执行自动回滚")
        auto_rollback(state)


def mark_success() -> None:
    require_root()
    state = load_state()
    if state.get("phase") == "pending-reboot":
        state["phase"] = "idle"
        state["current"] = state.get("target", current_version())
        state["pending_since"] = None
        state["boot_attempts"] = 0
        save_state(state)
        clear_notice("security")
        clear_notice("feature")


# ------------------------------------------------------------------ 回滚
def rewrite_root_fstab(subvolid: int) -> None:
    with open("/etc/fstab", encoding="utf-8") as fh:
        lines = fh.readlines()
    out = []
    for line in lines:
        if re.match(r"^UUID=\S+\s+/\s+btrfs\s", line):
            opts = []
            for opt in line.split()[3].split(","):
                if opt.startswith("subvol="):
                    continue
                opts.append(opt)
            opts.insert(0, f"subvolid={subvolid}")
            parts = line.split()
            parts[3] = ",".join(opts)
            out.append("  ".join(parts) + "\n")
        else:
            out.append(line)
    with open("/etc/fstab", "w", encoding="utf-8") as fh:
        fh.writelines(out)


def auto_rollback(state: dict, reboot: bool = True) -> None:
    snap = state.get("snapshot") or {}
    if not snap.get("subvolid"):
        sys.exit("没有可回滚的快照")
    run(["btrfs", "subvolume", "set-default", str(snap["subvolid"]), "/"])
    rewrite_root_fstab(int(snap["subvolid"]))
    state["phase"] = "rolled-back"
    state["last_error"] = "boot failure auto rollback"
    save_state(state)
    if reboot:
        run(["systemctl", "reboot"], check=False)


def rollback(snapshot: str | None = None) -> None:
    require_root()
    names = snapshot_list()
    if not names:
        sys.exit("没有可用快照")
    name = snapshot or names[-1]
    path = os.path.join(SNAPSHOT_DIR, name)
    if not os.path.isdir(path):
        sys.exit(f"快照不存在：{name}")
    subvolid = int(run(["btrfs", "inspect-internal", "rootid", path], capture=True))
    run(["btrfs", "subvolume", "set-default", str(subvolid), "/"])
    rewrite_root_fstab(subvolid)
    print(f"已设默认子卷为 {name}（id={subvolid}），重启生效")


# ------------------------------------------------------------------ 状态
def status() -> dict:
    state = load_state()
    state["current"] = current_version()
    state["snapshots"] = snapshot_list()
    return state
