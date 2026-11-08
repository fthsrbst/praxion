"""An incremental line index over ``brain.jsonl``.

The evidence file grows without bound and holds whole prompts, so the console
never re-reads it: it remembers each complete line's byte offset and kind and
extends that index from the end of the file. Lines are numbered from 1.

Parsing a line also costs one open/seek/read, so the index keeps one read handle
open (guarded by :attr:`_read_lock`, never held while JSON is parsed) and the
list paths (:meth:`EvidenceLog.tail`) parse a whole page through it.
"""

from __future__ import annotations

import json
import re
import threading
from pathlib import Path
from typing import Any, BinaryIO, Iterable, Optional

_KIND = re.compile(rb'"kind":\s*"([^"]*)"')


def _parsed(number: int, raw: bytes) -> Optional[dict[str, Any]]:
    """Line ``number`` parsed with ``n`` added; ``None`` when it is not an object."""
    try:
        value = json.loads(raw)
    except ValueError:
        return None
    return {"n": number, **value} if isinstance(value, dict) else None


class EvidenceLog:
    def __init__(self, path: Path) -> None:
        self.path = Path(path)
        self._lock = threading.Lock()
        self._read_lock = threading.Lock()
        self._handle: Optional[BinaryIO] = None
        self._offsets: list[int] = []
        self._kinds: list[str] = []
        self._end = 0

    def close(self) -> None:
        with self._read_lock:
            self._close_handle()

    def _close_handle(self) -> None:
        """Drop the read handle; call with :attr:`_read_lock` held."""
        if self._handle is not None:
            try:
                self._handle.close()
            except OSError:
                pass
            self._handle = None

    def _read_file(self) -> BinaryIO:
        """The one read handle; call with :attr:`_read_lock` held."""
        if self._handle is None:
            self._handle = self.path.open("rb")
        return self._handle

    def refresh(self) -> int:
        """Index lines appended since the last call; return the line count."""
        with self._lock:
            try:
                size = self.path.stat().st_size
            except OSError:
                self._offsets, self._kinds, self._end = [], [], 0
                return 0
            if size < self._end:  # truncated or replaced
                self._offsets, self._kinds, self._end = [], [], 0
                with self._read_lock:
                    self._close_handle()  # the old file is not the indexed one
            if size > self._end:
                with self.path.open("rb") as handle:
                    handle.seek(self._end)
                    chunk = handle.read(size - self._end)
                position = self._end
                for raw in chunk.splitlines(keepends=True):
                    if not raw.endswith(b"\n"):
                        break  # a line still being written
                    match = _KIND.search(raw[:200])
                    self._offsets.append(position)
                    self._kinds.append(match.group(1).decode() if match else "")
                    position += len(raw)
                self._end = position
            return len(self._offsets)

    def kind(self, number: int) -> Optional[str]:
        with self._lock:
            return self._kinds[number - 1] if 1 <= number <= len(self._kinds) else None

    def entries(self, numbers: Iterable[int]) -> list[dict[str, Any]]:
        """Those of ``numbers`` that exist, parsed in order through one handle."""
        with self._lock:
            count = len(self._offsets)
            wanted = [(n, self._offsets[n - 1]) for n in numbers if 1 <= n <= count]
        if not wanted:
            return []
        found: list[dict[str, Any]] = []
        with self._read_lock:
            try:
                handle = self._read_file()
            except OSError:
                return found
            for number, offset in wanted:
                try:
                    handle.seek(offset)
                    raw = handle.readline()
                except (OSError, ValueError):
                    self._close_handle()  # reopen on the next call
                    break
                entry = _parsed(number, raw)
                if entry is not None:
                    found.append(entry)
        return found

    def entry(self, number: int) -> Optional[dict[str, Any]]:
        """Line ``number`` parsed, with ``n`` added; ``None`` when absent or unreadable."""
        found = self.entries([number])
        return found[0] if found else None

    def numbers(
        self,
        *,
        since: Optional[int] = None,
        kinds: Optional[Iterable[str]] = None,
        limit: int = 200,
    ) -> list[int]:
        """Line numbers after ``since`` (oldest first); the newest ``limit``
        lines when ``since`` is ``None``. ``kinds`` filters before the limit."""
        wanted = set(kinds) if kinds else None
        self.refresh()
        with self._lock:
            count = len(self._kinds)
            kind_list = self._kinds
            if since is None:
                found: list[int] = []
                for number in range(count, 0, -1):
                    if wanted is None or kind_list[number - 1] in wanted:
                        found.append(number)
                        if len(found) >= limit:
                            break
                return found[::-1]
            found = []
            for number in range(max(0, since) + 1, count + 1):
                if wanted is None or kind_list[number - 1] in wanted:
                    found.append(number)
                    if len(found) >= limit:
                        break
            return found

    def tail(
        self,
        *,
        since: Optional[int] = None,
        kinds: Optional[Iterable[str]] = None,
        limit: int = 200,
    ) -> list[dict[str, Any]]:
        """The newest matching lines (or those after ``since``), parsed in one pass."""
        return self.entries(self.numbers(since=since, kinds=kinds, limit=limit))


__all__ = ["EvidenceLog"]
