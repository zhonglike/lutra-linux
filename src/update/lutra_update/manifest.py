# -*- coding: utf-8 -*-
"""更新包与索引的清单结构、版本比较、签名校验。"""
from __future__ import annotations

import hashlib
import json
import os
import re
from dataclasses import dataclass, field

SEVERITIES = ("security", "feature", "patch")
_VERSION_RE = re.compile(r"^(\d+)\.(\d+)\.(\d+)$")


def parse_version(text: str) -> tuple[int, int, int]:
    m = _VERSION_RE.match(text.strip())
    if not m:
        raise ValueError(f"非法版本号：{text}")
    return tuple(int(x) for x in m.groups())  # type: ignore[return-value]


def version_gt(a: str, b: str) -> bool:
    return parse_version(a) > parse_version(b)


@dataclass
class UpdateEntry:
    to_version: str
    severity: str
    url: str
    sha256: str
    size: int = 0
    cve: list[str] = field(default_factory=list)
    changelog: str = ""
    mandatory_reboot: bool = True

    @classmethod
    def from_dict(cls, d: dict) -> "UpdateEntry":
        sev = d.get("severity", "patch")
        if sev not in SEVERITIES:
            raise ValueError(f"未知更新级别：{sev}")
        return cls(
            to_version=d["to_version"],
            severity=sev,
            url=d["url"],
            sha256=d["sha256"],
            size=int(d.get("size", 0)),
            cve=list(d.get("cve", [])),
            changelog=d.get("changelog", ""),
            mandatory_reboot=bool(d.get("mandatory_reboot", True)),
        )


@dataclass
class ChannelIndex:
    channel: str
    generated: str
    updates: list[UpdateEntry]

    @classmethod
    def from_json(cls, raw: bytes) -> "ChannelIndex":
        d = json.loads(raw.decode("utf-8"))
        return cls(
            channel=d.get("channel", "stable"),
            generated=d.get("generated", ""),
            updates=[UpdateEntry.from_dict(u) for u in d.get("updates", [])],
        )

    def best_update(self, current: str) -> UpdateEntry | None:
        candidates = [u for u in self.updates if version_gt(u.to_version, current)]
        if not candidates:
            return None
        # 安全更新优先，其次版本高者
        candidates.sort(key=lambda u: (u.severity != "security", parse_version(u.to_version)),
                        reverse=False)
        for u in candidates:
            if u.severity == "security":
                return u
        return max(candidates, key=lambda u: parse_version(u.to_version))


def sha256_file(path: str, chunk: int = 1 << 20) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        while True:
            data = fh.read(chunk)
            if not data:
                break
            h.update(data)
    return h.hexdigest()


def verify_sha256(path: str, expected: str) -> bool:
    return sha256_file(path).lower() == expected.lower()


def verify_ed25519(payload: bytes, signature: bytes, pubkey_path: str) -> bool:
    """验证 .lup 的 META-INF/SIGNATURE。缺少 cryptography 时保守地返回 False。"""
    try:
        from cryptography.hazmat.primitives.serialization import load_pem_public_key
        from cryptography.exceptions import InvalidSignature
    except ImportError:
        return False
    if not os.path.exists(pubkey_path):
        return False
    with open(pubkey_path, "rb") as fh:
        key = load_pem_public_key(fh.read())
    try:
        key.verify(signature, payload)  # type: ignore[call-arg]
        return True
    except InvalidSignature:
        return False
