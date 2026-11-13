"""哈希链 JSONL 审计日志（与 Linux 端同构）。

每条记录：
{"ts": "...", "session": "...", "tool": "...", "args": {...},
 "result": {...}, "decision": "allow|deny", "prev_hash": "...", "hash": "..."}
hash 为去掉 hash 字段后规范化 JSON 的 SHA-256；prev_hash 指向上一条的 hash。
"""
from __future__ import annotations

import hashlib
import json
import os
import threading
from pathlib import Path

from . import config


class AuditLog:
    def __init__(self, directory: str | os.PathLike | None = None):
        self.directory = config.ensure_under_root(directory or config.AUDIT_DIR, "审计目录")
        self.directory.mkdir(parents=True, exist_ok=True)
        self._lock = threading.Lock()

    # -- 内部工具 ---------------------------------------------------------
    def path_for(self, day: str | None = None) -> Path:
        return self.directory / f"bridge-{day or config.local_day()}.jsonl"

    @staticmethod
    def _canonical(record: dict) -> str:
        return json.dumps(record, ensure_ascii=False, sort_keys=True, separators=(",", ":"))

    def _last_hash(self, path: Path) -> str:
        if not path.exists():
            return ""
        try:
            with path.open("rb") as fh:
                fh.seek(0, os.SEEK_END)
                size = fh.tell()
                block = min(size, 512 * 1024)
                fh.seek(size - block)
                data = fh.read()
        except OSError:
            return ""
        for line in reversed(data.decode("utf-8", "replace").splitlines()):
            line = line.strip()
            if not line:
                continue
            try:
                return str(json.loads(line).get("hash", ""))
            except ValueError:
                continue
        return ""

    # -- 写入 -------------------------------------------------------------
    def append(
        self,
        tool: str,
        args: dict | None,
        result: dict | None,
        session: str | None = None,
        decision: str = "allow",
    ) -> str:
        """追加一条记录，返回 evt_xxx。"""
        with self._lock:
            path = self.path_for()
            prev = self._last_hash(path)
            record = {
                "ts": config.utc_now_iso(),
                "session": session or "-",
                "tool": str(tool),
                "args": config.sanitize(args or {}),
                "result": config.sanitize(result or {}),
                "decision": decision,
                "prev_hash": prev,
            }
            digest = hashlib.sha256(self._canonical(record).encode("utf-8")).hexdigest()
            record["hash"] = digest
            line = self._canonical(record)
            path.parent.mkdir(parents=True, exist_ok=True)
            with path.open("a", encoding="utf-8", newline="\n") as fh:
                fh.write(line + "\n")
                fh.flush()
                os.fsync(fh.fileno())
            return "evt_" + digest[:12]

    # -- 读取 / 校验 ------------------------------------------------------
    def tail(self, lines: int = 50, day: str | None = None) -> dict:
        path = self.path_for(day)
        lines = max(1, min(int(lines), 500))
        if not path.exists():
            return {"file": str(path), "exists": False, "count": 0, "records": []}
        try:
            with path.open("rb") as fh:
                fh.seek(0, os.SEEK_END)
                size = fh.tell()
                block = min(size, 1024 * 1024)
                fh.seek(size - block)
                data = fh.read()
        except OSError as exc:
            raise config.BridgeError("EAUDIT", f"读取审计日志失败：{exc}") from exc
        raw_lines = data.decode("utf-8", "replace").splitlines()
        records = []
        for line in raw_lines[-lines:]:
            line = line.strip()
            if not line:
                continue
            try:
                records.append(json.loads(line))
            except ValueError:
                records.append({"_unparsed": config.sanitize(line)})
        return {
            "file": str(path),
            "exists": True,
            "count": len(records),
            "records": records,
            "chain": self.verify(day),
        }

    def verify(self, day: str | None = None) -> dict:
        path = self.path_for(day)
        if not path.exists():
            return {"ok": True, "records": 0, "file": str(path), "exists": False}
        prev = ""
        count = 0
        try:
            with path.open("r", encoding="utf-8", errors="replace") as fh:
                for lineno, line in enumerate(fh, 1):
                    line = line.strip()
                    if not line:
                        continue
                    try:
                        record = json.loads(line)
                    except ValueError:
                        return {"ok": False, "records": count, "file": str(path),
                                "broken_at": lineno, "reason": "JSON 解析失败"}
                    stored = str(record.pop("hash", ""))
                    if str(record.get("prev_hash", "")) != prev:
                        return {"ok": False, "records": count, "file": str(path),
                                "broken_at": lineno, "reason": "prev_hash 断链"}
                    expected = hashlib.sha256(self._canonical(record).encode("utf-8")).hexdigest()
                    if stored != expected:
                        return {"ok": False, "records": count, "file": str(path),
                                "broken_at": lineno, "reason": "hash 不匹配"}
                    prev = stored
                    count += 1
        except OSError as exc:
            return {"ok": False, "records": count, "file": str(path), "reason": str(exc)}
        return {"ok": True, "records": count, "file": str(path), "exists": True}
