#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""通过 GitHub Git Data API 推送仓库（适用于 git:// 与 https://github.com 被网络阻断的环境）。

用法：
  python env/gh-push.py [--repo zhonglike/lutra-linux] [--branch main]
"""
from __future__ import annotations

import argparse
import base64
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

ROOT = r"M:\lutraos"
API = "https://api.github.com"


def token() -> str:
    out = subprocess.run([r"E:\tools\gh\bin\gh.exe", "auth", "token"],
                         capture_output=True, text=True, check=True)
    return out.stdout.strip()


def api(method: str, path: str, payload: dict | None = None) -> dict:
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request(API + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    req.add_header("Accept", "application/vnd.github+json")
    req.add_header("X-GitHub-Api-Version", "2022-11-28")
    if data:
        req.add_header("Content-Type", "application/json")
    for attempt in range(4):
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.loads(resp.read().decode() or "{}")
        except urllib.error.HTTPError as exc:
            body = exc.read().decode(errors="replace")
            if exc.code in (502, 503, 504) and attempt < 3:
                time.sleep(2 * (attempt + 1))
                continue
            raise RuntimeError(f"{method} {path} -> {exc.code}: {body}") from exc
        except urllib.error.URLError:
            if attempt < 3:
                time.sleep(2 * (attempt + 1))
                continue
            raise
    return {}


def git(*args: str) -> str:
    return subprocess.run(["git", "-C", ROOT, *args], capture_output=True,
                          text=True, check=True).stdout


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default="zhonglike/lutra-linux")
    ap.add_argument("--branch", default="main")
    args = ap.parse_args()

    global TOKEN
    TOKEN = token()

    # 1. 文件清单（HEAD）
    entries = []
    for line in git("ls-tree", "-r", "HEAD").splitlines():
        meta, path = line.split("\t", 1)
        mode, typ, sha = meta.split()
        entries.append((path, mode, sha))

    # 2. 上传 blob
    tree = []
    for i, (path, mode, sha) in enumerate(entries, 1):
        raw = subprocess.run(["git", "-C", ROOT, "cat-file", "blob", sha],
                             capture_output=True, check=True).stdout
        try:
            text = raw.decode("utf-8")
            blob = api("POST", f"/repos/{args.repo}/git/blobs",
                       {"content": text, "encoding": "utf-8"})
        except UnicodeDecodeError:
            blob = api("POST", f"/repos/{args.repo}/git/blobs",
                       {"content": base64.b64encode(raw).decode(), "encoding": "base64"})
        tree.append({"path": path, "mode": mode, "type": "blob", "sha": blob["sha"]})
        if i % 20 == 0:
            print(f"  blob {i}/{len(entries)}")

    # 3. tree
    new_tree = api("POST", f"/repos/{args.repo}/git/trees", {"tree": tree})

    # 4. 父提交（如果远端已有内容）
    parents = []
    try:
        ref = api("GET", f"/repos/{args.repo}/git/ref/heads/{args.branch}")
        parents = [ref["object"]["sha"]]
    except RuntimeError:
        pass

    message = git("log", "-1", "--pretty=%B").strip()
    commit = api("POST", f"/repos/{args.repo}/git/commits", {
        "message": message, "tree": new_tree["sha"], "parents": parents,
    })

    # 5. 更新/创建分支
    if parents:
        api("PATCH", f"/repos/{args.repo}/git/refs/heads/{args.branch}",
            {"sha": commit["sha"], "force": True})
    else:
        api("POST", f"/repos/{args.repo}/git/refs",
            {"ref": f"refs/heads/{args.branch}", "sha": commit["sha"]})

    print(f"pushed {len(entries)} files -> {args.repo}@{args.branch} ({commit['sha'][:10]})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
