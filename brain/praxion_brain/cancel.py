"""One admission's cancel switch and the typed stop it raises.

A background admission outlives the moment that decides to stop it: the loop
closes, or the governor's ``admission`` deadline expires while the worker is
inside a practice run. The job owns one :class:`CancelToken`; the timeout and a
shutdown set it, ``admit`` checks it between practice runs and before it copies
a skill to ``active/``, and ``practice`` uses its callbacks to kill the live
server, forwarder and body. A cancelled admission never goes active.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` (background admission).
"""

from __future__ import annotations

import threading
from typing import Callable


class AdmissionCancelled(Exception):
    """An admission stopped before its skill went active.

    Deliberately not a ``RuntimeError``: :func:`praxion_brain.practice.practice`
    turns those into a typed body failure, and a cancelled run must not look
    like a body gap.
    """


class CancelToken:
    """The cancel switch of one admission; set once, never cleared.

    ``set`` runs the callbacks registered with :meth:`add` in the thread that
    calls it, so a canceller kills the practice stack and returns. A callback
    added after the token is set runs at once, which closes the race between
    "the admission is cancelled" and "the practice run has just started".
    """

    def __init__(self) -> None:
        self._lock = threading.Lock()
        self._cancelled = False
        self._callbacks: list[Callable[[], None]] = []

    def set(self) -> None:
        """Cancel the admission; every registered callback runs exactly once."""
        with self._lock:
            if self._cancelled:
                return
            self._cancelled = True
            callbacks, self._callbacks = self._callbacks, []
        for callback in callbacks:
            callback()

    def is_set(self) -> bool:
        with self._lock:
            return self._cancelled

    def add(self, callback: Callable[[], None]) -> None:
        """Run ``callback`` on cancel, or now if the token is already set."""
        with self._lock:
            if not self._cancelled:
                self._callbacks.append(callback)
                return
        callback()

    def discard(self, callback: Callable[[], None]) -> None:
        """Forget ``callback``; a callback that already ran is simply absent."""
        with self._lock:
            try:
                self._callbacks.remove(callback)
            except ValueError:
                pass


__all__ = ["AdmissionCancelled", "CancelToken"]
