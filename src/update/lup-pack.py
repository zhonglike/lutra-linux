#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""lup-pack —— 生成 Lutra 系统升级包 .lup。

结构：
  manifest.json
  payload/rootfs/            （要覆盖到 / 的文件树，可选）
  payload/kernel/*.deb       （可选）
  hooks/pre-update, post-update
  META-INF/SHA256SUMS
  META-INF/SIGNATURE         （用 Ed25519 私钥对 manifest.json 签名）

用法：
  ./lup-pack.py --from 1.0.0 --to 1.0.1 --severity security \
      --changelog "修复 ..." --cve CVE-2026-0001 \
      --rootfs ./new-root --kernel ./debs --sign-key ./private.pem -o 1.0.1.lup
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import tempfile


def sha256_file(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--from", dest="from_version", required=True)
    ap.add_argument("--to", dest="to_version", required=True)
    ap.add_argument("--severity", choices=["security", "feature", "patch"], required=True)
    ap.add_argument("--changelog", default="")
    ap.add_argument("--cve", action="append", default=[])
    ap.add_argument("--rootfs", help="新根文件树目录（会被覆盖到 /）")
    ap.add_argument("--kernel", help="包含内核 deb 的目录")
    ap.add_argument("--sign-key", help="Ed25519 私钥 PEM")
    ap.add_argument("-o", "--output", required=True)
    args = ap.parse_args()

    work = tempfile.mkdtemp(prefix="lup-")
    os.makedirs(os.path.join(work, "payload"), exist_ok=True)
    os.makedirs(os.path.join(work, "hooks"), exist_ok=True)
    os.makedirs(os.path.join(work, "META-INF"), exist_ok=True)

    if args.rootfs:
        subprocess.run(["cp", "-a", args.rootfs + "/.", os.path.join(work, "payload", "rootfs")],
                       check=True)
    if args.kernel:
        subprocess.run(["cp", "-a", args.kernel, os.path.join(work, "payload", "kernel")],
                       check=True)

    manifest = {
        "format": 1,
        "from_version": args.from_version,
        "to_version": args.to_version,
        "severity": args.severity,
        "cve": args.cve,
        "changelog": args.changelog,
        "created": subprocess.run(["date", "-Iseconds"], capture_output=True,
                                  text=True, check=True).stdout.strip(),
    }
    with open(os.path.join(work, "manifest.json"), "w", encoding="utf-8") as fh:
        json.dump(manifest, fh, ensure_ascii=False, indent=2)

    sums = []
    for root, _, files in os.walk(work):
        for name in files:
            p = os.path.join(root, name)
            rel = os.path.relpath(p, work)
            if rel.startswith("META-INF/"):
                continue
            sums.append(f"{sha256_file(p)}  {rel}")
    with open(os.path.join(work, "META-INF", "SHA256SUMS"), "w", encoding="utf-8") as fh:
        fh.write("\n".join(sorted(sums)) + "\n")

    if args.sign_key:
        try:
            from cryptography.hazmat.primitives import serialization
            from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
        except ImportError:
            raise SystemExit("需要 python3-cryptography 才能签名")
        with open(args.sign_key, "rb") as fh:
            key = serialization.load_pem_private_key(fh.read(), password=None)
        assert isinstance(key, Ed25519PrivateKey)
        with open(os.path.join(work, "manifest.json"), "rb") as fh:
            sig = key.sign(fh.read())
        with open(os.path.join(work, "META-INF", "SIGNATURE"), "wb") as fh:
            fh.write(sig)

    subprocess.run(["tar", "--zstd", "-cf", args.output, "-C", work, "."], check=True)
    print(f"生成 {args.output}")
    print(f"sha256: {sha256_file(args.output)}")
    print("请把 sha256 写入渠道索引 index.json")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
