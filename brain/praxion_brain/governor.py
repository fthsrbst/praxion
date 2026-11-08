"""The cost governor: hourly warning, session hard stop, breaker and deadlines.

Pure logic over an injected clock and an existing
:class:`~praxion_brain.ledger.Ledger`. It never calls a provider itself. The
loop asks :meth:`Governor.allow` before every paid call, runs the call through
:meth:`Governor.call` so it carries its class deadline, and reports the result
back with :meth:`Governor.record_success` / :meth:`Governor.record_error`.

Rules (proposal section 3.7):

* warn once per hour when the last hour's spend reaches
  ``PRAXION_WARN_USD_PER_HOUR`` (default 1.00 USD);
* a hard stop at ``PRAXION_HARD_STOP_USD`` (default: the ledger's remaining
  budget, summed over providers). The comparison is the spend of *this
  session*, from the ledger's own timestamps; the ledger's per-provider budget
  stays the absolute cap (section 3.7, decision A2);
* a breaker after three consecutive errors/timeouts: open for five minutes,
  then exactly one probe call before closing again;
* per-call-class rate limits and deadlines (:data:`CALL_TIMEOUTS`); a timeout
  is an error for the breaker;
* code-only mode: no propose/route/write. The goal is the first unmet ladder
  rung that an active or core skill already covers
  (:func:`praxion_brain.ladder.code_only_goal`); otherwise the loop idles.

Money is wall-clock time because the ledger's timestamps are; the breaker and
the rate limits use the injected ``clock`` (monotonic in production).
"""

from __future__ import annotations

import os
import threading
import time
from collections import defaultdict
from typing import Any, Callable, Mapping, Optional

from pydantic import BaseModel

from .evidence import Evidence
from .ledger import DEFAULT_BUDGETS, BudgetExceeded, Ledger

#: Deadline per call class, in seconds (numbers by measurement; proposal 3.7).
#: ``admission`` is one whole write-check-practise-repair run, so it gets the
#: only number that is not from 3.7: 30 min, pending measurement 2.
CALL_TIMEOUTS: dict[str, float] = {
    "propose": 60.0,
    "route": 5.0,
    "write": 180.0,
    "admission": 1800.0,
}
#: Provider a call class is charged to.
CALL_PROVIDERS: dict[str, str] = {
    "propose": "openai",
    "route": "typesafe",
    "write": "openai",
    "admission": "openai",
}
#: (calls allowed, window seconds) per call class.
DEFAULT_RATE_LIMITS: dict[str, tuple[int, float]] = {
    "propose": (30, 3600.0),
    "route": (120, 3600.0),
    "write": (6, 3600.0),
    "admission": (6, 3600.0),
}
#: Consecutive errors that open the breaker.
BREAKER_THRESHOLD = 3
#: How long the breaker stays open before one probe.
BREAKER_OPEN_SECONDS = 5 * 60.0
#: The warning window is one hour.
WARN_WINDOW_SECONDS = 3600.0

WARN_ENV = "PRAXION_WARN_USD_PER_HOUR"
HARD_STOP_ENV = "PRAXION_HARD_STOP_USD"


class GovernorDecision(BaseModel):
    """Whether one paid call may run, and why not when it may not."""

    allowed: bool
    reason: Optional[str] = None


class CallTimeout(RuntimeError):
    """A paid call did not answer within its class deadline (A4)."""

    def __init__(self, call_class: str, seconds: float) -> None:
        super().__init__(f"{call_class} call did not finish within {seconds:g}s")
        self.call_class = call_class
        self.seconds = seconds


def _env_float(name: str) -> Optional[float]:
    raw = os.environ.get(name)
    if raw is None or not raw.strip():
        return None
    try:
        return float(raw)
    except ValueError as error:
        raise ValueError(f"{name} must be a number, got {raw!r}") from error


# --------------------------------------------------------------------------- #
# Governor
# --------------------------------------------------------------------------- #
class Governor:
    """Budget, breaker, rate limits, deadlines and the code-only decision."""

    def __init__(
        self,
        ledger: Ledger,
        *,
        evidence: Evidence | None = None,
        clock: Callable[[], float] | None = None,
        wall_clock: Callable[[], float] | None = None,
        session_start: float | None = None,
        warn_usd_per_hour: float | None = None,
        hard_stop_usd: float | None = None,
        breaker_threshold: int = BREAKER_THRESHOLD,
        breaker_open_seconds: float = BREAKER_OPEN_SECONDS,
        rate_limits: Mapping[str, tuple[int, float]] | None = None,
        call_timeouts: Mapping[str, float] | None = None,
        log: Callable[[str], None] | None = None,
    ) -> None:
        if breaker_threshold < 1:
            raise ValueError(f"breaker_threshold must be positive, got {breaker_threshold}")
        self.ledger = ledger
        self.evidence = evidence
        self._clock = clock if clock is not None else time.monotonic
        self._wall = wall_clock if wall_clock is not None else time.time
        self._log = log if log is not None else print
        #: Wall-clock moment this loop started; the hard stop is measured on it.
        self.session_start = (
            float(session_start) if session_start is not None else float(self._wall())
        )

        env_warn = _env_float(WARN_ENV)
        self.warn_usd_per_hour = (
            float(warn_usd_per_hour)
            if warn_usd_per_hour is not None
            else (env_warn if env_warn is not None else 1.00)
        )
        env_stop = _env_float(HARD_STOP_ENV)
        self._hard_stop = (
            float(hard_stop_usd)
            if hard_stop_usd is not None
            else env_stop
        )
        self.breaker_threshold = int(breaker_threshold)
        self.breaker_open_seconds = float(breaker_open_seconds)
        self.rate_limits = dict(DEFAULT_RATE_LIMITS)
        if rate_limits:
            self.rate_limits.update(rate_limits)
        self.timeouts = dict(CALL_TIMEOUTS)
        if call_timeouts:
            self.timeouts.update({name: float(value) for name, value in call_timeouts.items()})

        self.warnings: list[str] = []
        self._calls: dict[str, list[float]] = defaultdict(list)
        self._breaker = "closed"
        self._failures = 0
        self._open_until = 0.0
        self._probe_outstanding = False
        self._warned_hour: Optional[int] = None

    # -- budget --------------------------------------------------------- #
    def spent_total(self) -> float:
        return sum(self.ledger.totals().values())

    def spent_session(self) -> float:
        """Spend recorded since this loop started (the ledger's timestamps)."""
        return self.ledger.spent_since(self.session_start)

    def remaining_budget(self) -> float:
        """Remaining provider budgets, summed (the user's stop is total)."""
        total = 0.0
        providers = set(self.ledger.totals()) | set(DEFAULT_BUDGETS)
        for provider in providers:
            try:
                limit = self.ledger.budget(provider)
            except ValueError:
                continue
            total += max(0.0, limit - self.ledger.spent(provider))
        return total

    @property
    def hard_stop_usd(self) -> float:
        return self._hard_stop if self._hard_stop is not None else self.remaining_budget()

    @property
    def hard_stop_reached(self) -> bool:
        """This session's spend against the session hard stop (A2).

        The ledger is brain-wide and survives restarts, so comparing its whole
        total would trip the stop on an old session's spend before the first
        pick. The per-provider budget in :meth:`Ledger.ensure` is still the
        absolute cap.
        """
        return self.spent_session() >= self.hard_stop_usd

    # -- breaker -------------------------------------------------------- #
    @property
    def breaker_state(self) -> str:
        return self._breaker

    def _breaker_allows(self, now: float) -> bool:
        if self._breaker == "closed":
            return True
        if self._breaker == "open":
            if now < self._open_until:
                return False
            self._breaker = "half_open"
            self._probe_outstanding = False
        if self._probe_outstanding:
            return False
        self._probe_outstanding = True
        return True

    def record_success(self) -> None:
        self._failures = 0
        self._breaker = "closed"
        self._probe_outstanding = False
        self._open_until = 0.0

    def record_error(self) -> None:
        now = self._clock()
        if self._breaker == "half_open":
            self._failures = max(self._failures + 1, self.breaker_threshold)
        else:
            self._failures += 1
        if self._failures >= self.breaker_threshold:
            self._breaker = "open"
            self._open_until = now + self.breaker_open_seconds
            self._probe_outstanding = False
            if self.evidence is not None:
                self.evidence.write(
                    "governor_breaker_open",
                    failures=self._failures,
                    open_until=self._open_until,
                )

    # -- rate limits and warning ---------------------------------------- #
    def _rate_allows(self, call_class: str, now: float) -> bool:
        limits = self.rate_limits.get(call_class)
        if limits is None:
            return True
        count, window = limits
        calls = [moment for moment in self._calls[call_class] if now - moment < window]
        self._calls[call_class] = calls
        return len(calls) < count

    def _maybe_warn(self) -> None:
        if self.warn_usd_per_hour <= 0:
            return
        wall = self._wall()
        hour = int(wall // WARN_WINDOW_SECONDS)
        if self._warned_hour == hour:
            return
        recent = self.ledger.spent_since(wall - WARN_WINDOW_SECONDS)
        if recent < self.warn_usd_per_hour:
            return
        self._warned_hour = hour
        message = (
            f"cost warning: ${recent:.2f} in the last hour "
            f"(threshold ${self.warn_usd_per_hour:.2f})"
        )
        self.warnings.append(message)
        self._log(message)
        if self.evidence is not None:
            self.evidence.write(
                "governor_warning",
                spent_last_hour=recent,
                warn_usd_per_hour=self.warn_usd_per_hour,
                hour=hour,
            )

    # -- decisions ------------------------------------------------------ #
    def allow(
        self,
        call_class: str,
        estimate: float = 0.0,
        *,
        provider: str | None = None,
    ) -> GovernorDecision:
        """May one paid call of ``call_class`` run now?"""
        if call_class not in CALL_TIMEOUTS:
            raise ValueError(f"unknown call class {call_class!r}")
        now = self._clock()
        self._maybe_warn()

        if self.hard_stop_reached:
            return GovernorDecision(allowed=False, reason="hard_stop")
        if not self._rate_allows(call_class, now):
            return GovernorDecision(allowed=False, reason="rate_limit")
        charged_to = provider or CALL_PROVIDERS[call_class]
        try:
            self.ledger.ensure(charged_to, float(estimate))
        except BudgetExceeded:
            return GovernorDecision(allowed=False, reason="budget")
        if not self._breaker_allows(now):
            return GovernorDecision(allowed=False, reason="breaker_open")

        self._calls[call_class].append(now)
        return GovernorDecision(allowed=True)

    def timeout(self, call_class: str) -> float:
        try:
            return float(self.timeouts[call_class])
        except KeyError as error:
            raise ValueError(f"unknown call class {call_class!r}") from error

    def call(self, call_class: str, fn: Callable[[], Any]) -> Any:
        """Run one blocking provider call with its class deadline (A4).

        The call runs on a daemon worker thread and the caller waits at most
        :meth:`timeout` seconds. On expiry it raises :class:`CallTimeout` and
        the worker is abandoned, so no provider can hang the loop; the step
        records a typed ``timeout`` failure and the breaker counts it.
        """
        seconds = self.timeout(call_class)
        box: dict[str, Any] = {}
        finished = threading.Event()

        def run() -> None:
            try:
                box["value"] = fn()
            except BaseException as error:  # noqa: BLE001 - re-raised on the caller
                box["error"] = error
            finally:
                finished.set()

        worker = threading.Thread(target=run, name=f"call-{call_class}", daemon=True)
        worker.start()
        if not finished.wait(seconds):
            raise CallTimeout(call_class, seconds)
        if "error" in box:
            raise box["error"]
        return box["value"]

    @property
    def code_only(self) -> bool:
        """No LLM work while the hard stop is reached or the breaker is open."""
        return self.hard_stop_reached or self._breaker == "open"

    def budget_snapshot(self) -> dict[str, float]:
        """The numbers the briefing prints for its Budget section."""
        spent = self.spent_total()
        return {
            "last_hour": self.ledger.spent_since(
                self._wall() - WARN_WINDOW_SECONDS
            ),
            "spent": spent,
            "budget": spent + self.remaining_budget(),
        }


__all__ = [
    "BREAKER_OPEN_SECONDS",
    "BREAKER_THRESHOLD",
    "CALL_PROVIDERS",
    "CALL_TIMEOUTS",
    "CallTimeout",
    "DEFAULT_RATE_LIMITS",
    "Governor",
    "GovernorDecision",
    "HARD_STOP_ENV",
    "WARN_ENV",
    "WARN_WINDOW_SECONDS",
]
