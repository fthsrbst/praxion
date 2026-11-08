"""What the loop shares with the console: a control queue and a state slot.

The loop is the only writer. The server threads only read: ``publish`` swaps in
a whole new dict, so a reader never sees a half-updated state and no lock is
held while serving. Control flows the other way through :class:`ControlQueue`:
the server puts requests, the loop drains them once per step.
"""

from __future__ import annotations

import threading
from typing import Any, Optional

from ..evidence import Evidence
from ..ledger import Ledger
from ..memory import Memory


class ControlQueue:
    """Thread-safe hand-off of user requests from the server to the loop."""

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._items: list[dict[str, Any]] = []

    def put(self, request: dict[str, Any]) -> None:
        with self._lock:
            self._items.append(request)

    def drain(self) -> list[dict[str, Any]]:
        with self._lock:
            items, self._items = self._items, []
        return items


class Console:
    """The read-only accessor the server uses, plus the control queue."""

    def __init__(
        self,
        *,
        evidence: Evidence,
        ledger: Ledger,
        memory: Memory,
        world_id: str,
        core_dir: Optional[str] = None,
        active_dir: Optional[str] = None,
    ) -> None:
        self.evidence = evidence
        self.ledger = ledger
        self.memory = memory
        self.world_id = world_id
        self.core_dir = core_dir
        self.active_dir = active_dir
        self.control = ControlQueue()
        self._state: dict[str, Any] = {"world_id": world_id, "state": "starting"}

    def publish(self, state: dict[str, Any]) -> None:
        self._state = state

    @property
    def state(self) -> dict[str, Any]:
        return self._state


__all__ = ["Console", "ControlQueue"]
