"""Append-only JSONL evidence for every paid request and response.

One line per event in ``<dir>/brain.jsonl``. The writer and router pass the full
prompt text and the parsed output; API keys are never passed here.
"""

from __future__ import annotations

import json
import threading
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from pydantic import BaseModel


def _json_default(value: Any) -> Any:
    if isinstance(value, BaseModel):
        return value.model_dump()
    if isinstance(value, Path):
        return str(value)
    return str(value)


class Evidence:
    def __init__(self, directory: Path) -> None:
        self.directory = Path(directory)
        self.path = self.directory / "brain.jsonl"
        # The loop and a background admission job write to this file at once;
        # one lock keeps their lines whole.
        self._lock = threading.Lock()

    def write(self, kind: str, **fields: Any) -> dict[str, Any]:
        """Append one event and return what was written."""
        entry = {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "kind": kind,
            **fields,
        }
        self.directory.mkdir(parents=True, exist_ok=True)
        line = json.dumps(entry, ensure_ascii=False, default=_json_default)
        with self._lock:
            with self.path.open("a", encoding="utf-8") as handle:
                handle.write(line + "\n")
        return entry


__all__ = ["Evidence"]
