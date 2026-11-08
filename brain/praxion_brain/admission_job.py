"""One admission run as a background job the loop can poll.

Admission takes minutes (write, check, practise on test servers, repair), so it
must never block the loop: the body keeps playing meanwhile. The job owns its
daemon worker thread, the cancel token the timeout and a shutdown set, the
progress the console shows and the one result; the loop starts at most one job
at a time and reads the result once the job is finished. The loop never joins
the thread: closing the loop cancels and abandons it, so a shutdown cannot hang
on a slow practice run and the run's own processes are killed.

Spec: ``docs/sessions/08-jev-llm-brain.md`` (no filler while the brain waits)
and the loop proposal section 3.5.
"""

from __future__ import annotations

import threading
from typing import Any, Callable, Mapping, Optional

from .admission import AdmissionReport
from .cancel import CancelToken
from .memory import GoalRecord
from .records import PendingGoal

#: Progress fields the job publishes; ``skill`` is fixed at start.
PROGRESS_FIELDS = ("skill", "attempt", "seed", "model")

#: What admission calls back with one progress step.
ProgressFn = Callable[[Mapping[str, Any]], None]

#: The job thread's body: the whole admission run.
RunFn = Callable[[], AdmissionReport]


class AdmissionJob:
    """One admission run in its own thread, with progress and one result."""

    def __init__(
        self,
        pending: PendingGoal,
        record: GoalRecord,
        *,
        skill: str,
        wall_clock: Callable[[], float],
    ) -> None:
        self.pending = pending
        #: The goal's record, open while the job runs; the play reuses it.
        self.record = record
        self.started_at = float(wall_clock())
        #: Set by the loop when it stops with the job still running.
        self.abandoned = False
        #: Set by the console's stop while the job runs: its goal is never
        #: played, even when the admission itself is admitted.
        self.stop_requested = False
        #: Set by the deadline and by a shutdown; the job's steps check it.
        self.cancel = CancelToken()

        self._lock = threading.Lock()
        self._progress: dict[str, Any] = {
            "skill": skill,
            "attempt": None,
            "seed": None,
            "model": None,
        }
        self._finished = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self.report: Optional[AdmissionReport] = None
        self.error: Optional[BaseException] = None

    # -- lifecycle ------------------------------------------------------ #
    def start(self, run: RunFn) -> "AdmissionJob":
        """Run ``run`` on a daemon thread; ``finished`` follows its end."""
        self._thread = threading.Thread(
            target=self._run,
            args=(run,),
            name=f"admission-{self._progress['skill']}",
            daemon=True,
        )
        self._thread.start()
        return self

    def _run(self, run: RunFn) -> None:
        try:
            self.report = run()
        except BaseException as error:  # noqa: BLE001 - the loop reads it as a result
            self.error = error
        finally:
            self._finished.set()

    def wait(self, timeout: float | None = None) -> bool:
        """Block until the job finished; tests use it, the loop never does."""
        return self._finished.wait(timeout)

    @property
    def finished(self) -> bool:
        return self._finished.is_set()

    # -- progress ------------------------------------------------------- #
    def update(self, fields: Mapping[str, Any]) -> None:
        """One progress step from admission; safe from the job thread."""
        with self._lock:
            for field in PROGRESS_FIELDS:
                if field in fields:
                    self._progress[field] = fields[field]

    def progress(self) -> dict[str, Any]:
        """The current progress, a fresh dict the caller may keep."""
        with self._lock:
            return {**self._progress, "started_at": self.started_at}


__all__ = ["PROGRESS_FIELDS", "AdmissionJob", "ProgressFn", "RunFn"]
