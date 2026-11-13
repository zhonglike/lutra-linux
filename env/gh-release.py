#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""创建 GitHub tag 与 Release（走 API，适配 github.com 直连受限的网络）。"""
from __future__ import annotations

import json
import subprocess
import sys
import urllib.error
import urllib.request

REPO = "zhonglike/lutra-linux"
ROOT = r"M:\lutraos"


def token() -> str:
    return subprocess.run([r"E:\tools\gh\bin\gh.exe", "auth", "token"],
                          capture_output=True, text=True, check=True).stdout.strip()


TOKEN = token()


def api(method: str, path: str, payload: dict | None = None) -> dict:
    data = json.dumps(payload).encode() if payload is not None else None
    req = urllib.request.Request("https://api.github.com" + path, data=data, method=method)
    req.add_header("Authorization", f"Bearer {TOKEN}")
    req.add_header("Accept", "application/vnd.github+json")
    if data:
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=60) as resp:
        return json.loads(resp.read().decode() or "{}")


def main() -> int:
    version = open(rf"{ROOT}\VERSION", encoding="utf-8").read().strip()
    tag = f"v{version}"
    changelog = open(rf"{ROOT}\CHANGELOG.md", encoding="utf-8").read()

    head = api("GET", f"/repos/{REPO}/git/ref/heads/main")["object"]["sha"]

    try:
        api("GET", f"/repos/{REPO}/git/ref/tags/{tag}")
        print(f"tag {tag} 已存在")
    except urllib.error.HTTPError:
        api("POST", f"/repos/{REPO}/git/refs", {"ref": f"refs/tags/{tag}", "sha": head})
        print(f"created tag {tag} -> {head[:10]}")

    body = "首个源码发行版。\n\n" + changelog.split("## 1.0.0", 1)[-1].strip()
    try:
        rel = api("POST", f"/repos/{REPO}/releases", {
            "tag_name": tag,
            "name": f"Lutra {version} \"Rill\"",
            "body": body[:60000],
            "draft": False,
            "prerelease": False,
        })
        print("release:", rel.get("html_url"))
    except urllib.error.HTTPError as exc:
        print("release failed:", exc.code, exc.read().decode(errors="replace")[:300])
    return 0


if __name__ == "__main__":
    sys.exit(main())
