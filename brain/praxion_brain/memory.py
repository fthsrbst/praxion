"""World memory: goals, skill uses and failure memory for the loop.

Small append-only JSON Lines stores under ``local/worlds/<world_id>/`` (proposal
section 5). Both files are loaded into an in-RAM index at start; a corrupted
line is skipped and logged. A goal line is keyed by ``goal_id`` and the last
line wins. Skill uses are append-only.

Failure memory (section 4.4) is derived from the goal history, not stored
twice: a goal key is its normalized condition, its attempts are the failed
records with that key, and the second failure blocks the key for two hours.
Drift is in-RAM state: five goals in a row without more rungs met write one
``curriculum_drift`` line to the injected evidence sink.

Time is injected as a clock callable so tests can move it without sleeping.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` sections 4.4 and 5.
"""

from __future__ import annotations

import json
import logging
import threading
import time
from collections import Counter
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, Literal, Optional, Protocol

from pydantic import BaseModel, Field

from . import config
from .conditions import ConditionError, canonical_text

_LOGGER = logging.getLogger(__name__)

#: Two failures block a goal for two hours (proposal section 4.4).
BLOCK_SECONDS = 2 * 60 * 60.0
#: Consecutive goals without rung progress that count as curriculum drift.
DRIFT_GOALS = 5
#: Failure kinds that end a goal without saying anything about the goal or its
#: skill: the body died (a mob, a fall) or its world mirror desynchronised
#: (decisions 2026-11-10). The wire receipt stays ``failed(<kind>)``; the
#: brain records the goal ``interrupted``, so it neither blocks the goal nor
#: counts against the skill. A history written before that rule may still hold
#: ``failed`` records of these kinds; failure memory skips them too.
NOT_FAILURE_KINDS = frozenset({"died", "world_desync"})


class EvidenceSink(Protocol):
    """Anything with the :class:`praxion_brain.evidence.Evidence` write shape."""

    def write(self, kind: str, **fields: Any) -> Any:  # pragma: no cover - protocol
        ...


class GoalRecord(BaseModel):
    """One line of ``goals.jsonl`` (proposal section 5).

    ``status`` is the goal's life: ``open`` while a command is in flight,
    ``done`` only when the body verified the condition, ``unverified`` for a
    ``done`` whose condition it could not decide (excluded from the success
    statistics, A7), ``interrupted`` for a goal the brain lost track of without
    a failure (A1; it does not count toward blocking), ``failed`` for a real
    failure and ``blocked`` for a blocked key.
    """

    goal_id: str
    text: str
    condition: str
    unverifiable: bool = False
    rung: Optional[str] = None
    #: ``user`` for a goal given through the console, ``survival`` for one the
    #: loop started from the body facts (a survival goal keeps its exemption
    #: from the interrupt check across a reconnect).
    origin: Optional[Literal["user", "survival"]] = None
    status: Literal[
        "open", "done", "unverified", "interrupted", "failed", "blocked"
    ] = "open"
    command_id: Optional[str] = None
    intent_id: Optional[int] = None
    skill: Optional[str] = None
    skill_origin: Optional[Literal["core", "active", "written"]] = None
    ticks: Optional[int] = None
    failure_kind: Optional[str] = None
    condition_result: Optional[str] = None
    referee: Optional[str] = None
    cost_usd: Optional[float] = None
    t_start: Optional[float] = None
    t_end: Optional[float] = None


class SkillUse(BaseModel):
    """One line of ``skill_uses.jsonl``."""

    skill: str
    goal_id: str
    outcome: str
    failure_kind: Optional[str] = None
    ticks: Optional[int] = None
    t: Optional[float] = None


class FailureEntry(BaseModel):
    """Derived failure memory for one goal key."""

    key: str
    attempts: int = 0
    kinds: Dict[str, int] = Field(default_factory=dict)
    last_failure: Optional[float] = None
    blocked_until: Optional[float] = None

    def blocked(self, now: float) -> bool:
        return self.blocked_until is not None and self.blocked_until > now


def normalize_condition(text: str) -> str:
    """The failure-memory key: the condition in canonical parsed form.

    Parsing makes spelling differences (whitespace, ``minecraft:`` prefixes,
    tags) share one key. Text that does not parse falls back to whitespace
    collapse, so a legacy or malformed record still gets a stable key.
    """
    try:
        return canonical_text(text)
    except ConditionError:
        return " ".join(text.split())


def world_dir(world_id: str, root: Path | None = None) -> Path:
    """``<root>/<world_id>``; ``root`` defaults to ``local/worlds``."""
    if not world_id or Path(world_id).name != world_id:
        raise ValueError(f"world id must be one path segment, got {world_id!r}")
    base = Path(root) if root is not None else config.local_dir() / "worlds"
    return base / world_id


def _last_time(record: GoalRecord) -> float:
    for value in (record.t_end, record.t_start):
        if value is not None:
            return float(value)
    return 0.0


class Memory:
    """The two world stores plus the derived failure and drift state."""

    def __init__(
        self,
        world_id: str,
        *,
        root: Path | None = None,
        clock: Callable[[], float] | None = None,
        evidence: EvidenceSink | None = None,
    ) -> None:
        self.world_id = world_id
        self.directory = world_dir(world_id, root)
        self.goals_path = self.directory / "goals.jsonl"
        self.skill_uses_path = self.directory / "skill_uses.jsonl"
        self._clock = clock if clock is not None else time.time
        self._evidence = evidence
        self._best_rungs: Optional[int] = None
        self._without_progress = 0
        # Goals are read incrementally: the console polls the state once a
        # second and the history only grows, so only appended lines are parsed.
        self._goals_lock = threading.RLock()
        self._goals: Dict[str, GoalRecord] = {}
        self._goals_offset = 0  # bytes of goals.jsonl already indexed
        self._goals_stamp: Optional[tuple[int, int]] = None  # (size, mtime_ns)
        self._goal_lines = 0  # lines consumed, for the warnings
        self._failures_cache: Optional[Dict[str, FailureEntry]] = None
        #: Goal lines parsed so far (a cache diagnostic, not state).
        self.goals_parsed = 0

    def now(self) -> float:
        return float(self._clock())

    # ------------------------------------------------------------------ #
    # Append
    # ------------------------------------------------------------------ #
    def _append(self, path: Path, payload: Dict[str, Any]) -> None:
        self.directory.mkdir(parents=True, exist_ok=True)
        line = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        with path.open("a", encoding="utf-8") as handle:
            handle.write(line + "\n")
            handle.flush()

    def append_goal(self, record: GoalRecord) -> GoalRecord:
        """Append one goal line, filling the timestamps the caller left out."""
        now = self.now()
        updates: Dict[str, Any] = {}
        if record.t_start is None:
            updates["t_start"] = now
        if record.t_end is None and record.status != "open":
            updates["t_end"] = now
        stored = record.model_copy(update=updates) if updates else record
        self._append(
            self.goals_path,
            stored.model_dump(mode="json", exclude_none=True),
        )
        return stored

    def append_skill_use(self, use: SkillUse) -> SkillUse:
        """Append one skill-use line, defaulting ``t`` to the clock."""
        stored = use if use.t is not None else use.model_copy(update={"t": self.now()})
        self._append(
            self.skill_uses_path,
            stored.model_dump(mode="json", exclude_none=True),
        )
        return stored

    # ------------------------------------------------------------------ #
    # Load
    # ------------------------------------------------------------------ #
    def _read_lines(self, path: Path) -> Iterable[tuple[int, Dict[str, Any]]]:
        try:
            text = path.read_text(encoding="utf-8")
        except FileNotFoundError:
            return
        for number, line in enumerate(text.splitlines(), start=1):
            line = line.strip()
            if not line:
                continue
            try:
                payload = json.loads(line)
            except json.JSONDecodeError as error:
                _LOGGER.warning(
                    "skipping corrupt line %d in %s: %s", number, path, error
                )
                continue
            if not isinstance(payload, dict):
                _LOGGER.warning(
                    "skipping corrupt line %d in %s: not an object", number, path
                )
                continue
            yield number, payload

    def goals(self) -> list[GoalRecord]:
        """Every goal, latest line per ``goal_id``, in last-write order.

        The file is read incrementally (:meth:`_refresh_goals`): a poll of the
        console state parses only the lines appended since its last look, never
        the whole unbounded history.
        """
        with self._goals_lock:
            self._refresh_goals()
            return list(self._goals.values())

    def _refresh_goals(self) -> None:
        """Extend the goal index from the end of ``goals.jsonl``; lock held."""
        try:
            stat = self.goals_path.stat()
        except OSError:
            if self._goals or self._goals_offset:
                self._goals.clear()
                self._goals_offset = 0
                self._goal_lines = 0
                self._goals_stamp = None
                self._failures_cache = None
            return
        stamp = (stat.st_size, stat.st_mtime_ns)
        if stamp == self._goals_stamp:
            return
        if stat.st_size < self._goals_offset:  # truncated or replaced
            self._goals.clear()
            self._goals_offset = 0
            self._goal_lines = 0
            self._failures_cache = None
        if stat.st_size > self._goals_offset:
            try:
                with self.goals_path.open("rb") as handle:
                    handle.seek(self._goals_offset)
                    chunk = handle.read(stat.st_size - self._goals_offset)
            except OSError:
                return
            position = self._goals_offset
            for raw in chunk.splitlines(keepends=True):
                if not raw.endswith(b"\n"):
                    break  # a line still being written; keep the stamp stale
                position += len(raw)
                self._goal_lines += 1
                self.goals_parsed += 1
                self._consume_goal_line(self._goal_lines, raw)
            self._goals_offset = position
        self._goals_stamp = (
            stamp if self._goals_offset == stat.st_size else None
        )

    def _consume_goal_line(self, number: int, raw: bytes) -> None:
        """Parse one appended goal line into the index; lock held."""
        try:
            payload = json.loads(raw)
        except json.JSONDecodeError as error:
            _LOGGER.warning(
                "skipping corrupt line %d in %s: %s", number, self.goals_path, error
            )
            return
        if not isinstance(payload, dict):
            _LOGGER.warning(
                "skipping corrupt line %d in %s: not an object", number, self.goals_path
            )
            return
        try:
            record = GoalRecord.model_validate(payload)
        except Exception as error:  # pydantic's message names the field
            _LOGGER.warning(
                "skipping invalid goal line %d in %s: %s",
                number,
                self.goals_path,
                error,
            )
            return
        # Re-insert so dict order follows the newest line.
        self._goals.pop(record.goal_id, None)
        self._goals[record.goal_id] = record
        self._failures_cache = None

    def recent(self, n: int) -> list[GoalRecord]:
        """The newest ``n`` goals, newest first."""
        if n <= 0:
            return []
        return list(reversed(self.goals()[-n:]))

    def skill_uses(self) -> list[SkillUse]:
        uses: list[SkillUse] = []
        for number, payload in self._read_lines(self.skill_uses_path):
            try:
                uses.append(SkillUse.model_validate(payload))
            except Exception as error:
                _LOGGER.warning(
                    "skipping invalid skill use line %d in %s: %s",
                    number,
                    self.skill_uses_path,
                    error,
                )
        return uses

    # ------------------------------------------------------------------ #
    # Failure memory
    # ------------------------------------------------------------------ #
    def failures(self) -> Dict[str, FailureEntry]:
        """Failure memory per normalized condition, from the goal history.

        Only ``failed`` records count, and not those of a
        :data:`NOT_FAILURE_KINDS` kind. ``interrupted`` (the brain lost the
        intent, A1) and ``unverified`` (a done the body could not verify, A7)
        are not failures and never block a goal. Survival goals (origin
        ``survival``) are never failure-blocked (decision 2026-11-10): the loop
        backs a failed survival skill off for a few hundred ticks instead. Derived once per goal-history
        change: the console asks for it for every rung on every state poll.
        """
        with self._goals_lock:
            self._refresh_goals()
            if self._failures_cache is None:
                self._failures_cache = self._derive_failures()
            return dict(self._failures_cache)

    def _derive_failures(self) -> Dict[str, FailureEntry]:
        groups: Dict[str, list[GoalRecord]] = {}
        for record in self._goals.values():
            if (
                record.status != "failed"
                or record.failure_kind in NOT_FAILURE_KINDS
                or record.origin == "survival"
            ):
                continue
            key = normalize_condition(record.condition)
            groups.setdefault(key, []).append(record)

        entries: Dict[str, FailureEntry] = {}
        for key, records in groups.items():
            records.sort(key=_last_time)
            kinds = Counter(
                record.failure_kind or "unknown" for record in records
            )
            blocked_until: Optional[float] = None
            if len(records) >= 2:
                # The second failure starts a two-hour block; any later failure
                # (only possible after the block expired) starts a fresh one.
                blocked_until = _last_time(records[-1]) + BLOCK_SECONDS
            entries[key] = FailureEntry(
                key=key,
                attempts=len(records),
                kinds=dict(kinds),
                last_failure=_last_time(records[-1]),
                blocked_until=blocked_until,
            )
        return entries

    def blocked(self, now: float | None = None) -> Dict[str, FailureEntry]:
        """Failure entries whose block has not expired."""
        moment = self.now() if now is None else float(now)
        return {
            key: entry
            for key, entry in self.failures().items()
            if entry.blocked(moment)
        }

    def is_blocked(self, condition: str, now: float | None = None) -> bool:
        key = normalize_condition(condition)
        entry = self.failures().get(key)
        moment = self.now() if now is None else float(now)
        return entry is not None and entry.blocked(moment)

    def failure_counts(self, skill: str) -> Dict[str, int]:
        """Failure kinds recorded for one skill (``skill_uses.jsonl``)."""
        counts: Counter[str] = Counter()
        for use in self.skill_uses():
            if (
                use.skill != skill
                or use.outcome != "failed"
                or use.failure_kind in NOT_FAILURE_KINDS
            ):
                continue
            counts[use.failure_kind or "unknown"] += 1
        return dict(counts)

    # ------------------------------------------------------------------ #
    # Drift
    # ------------------------------------------------------------------ #
    def note_rung_progress(self, rungs_met: int, goal_id: str | None = None) -> bool:
        """Track rung progress after a goal; report drift at five flat goals.

        Returns ``True`` when this call wrote ``curriculum_drift`` to evidence.
        The first call only sets the baseline. A new high resets the streak.
        """
        if rungs_met < 0:
            raise ValueError(f"rungs_met must not be negative, got {rungs_met}")
        if self._best_rungs is None:
            self._best_rungs = rungs_met
            return False
        if rungs_met > self._best_rungs:
            self._best_rungs = rungs_met
            self._without_progress = 0
            return False
        self._without_progress += 1
        if self._without_progress < DRIFT_GOALS:
            return False
        if self._evidence is not None:
            self._evidence.write(
                "curriculum_drift",
                world_id=self.world_id,
                goal_id=goal_id,
                goals_without_progress=self._without_progress,
                rungs_met=rungs_met,
            )
        self._without_progress = 0
        return True


__all__ = [
    "BLOCK_SECONDS",
    "DRIFT_GOALS",
    "EvidenceSink",
    "FailureEntry",
    "GoalRecord",
    "Memory",
    "NOT_FAILURE_KINDS",
    "SkillUse",
    "normalize_condition",
    "world_dir",
]
