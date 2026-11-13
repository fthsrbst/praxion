"""Per-run metrics for an autonomy run: read the evidence of one run and sum it up.

The evidence of a run is spread over four files (see ``docs/decisions.md`` and
``brain/praxion_brain/loop.py``), and this module only reads them:

* ``<brain_dir>/brain.jsonl`` — the brain's append-only evidence. One file serves
  every run, so a run is sliced positionally: it starts at a ``loop_started``
  line whose ``world_id`` is the run directory's name and ends at the next
  ``loop_started`` or at the end of the file. A brain restart in the same world
  (the console's bot restart does exactly that) writes a second
  ``loop_started`` for the same ``world_id``, so a run is *all* of its slices:
  they are read in file order and the lines of other worlds between them are
  left out. Every slice is measured on its own and the durations are summed, so
  the time between two slices never counts as run time.
* ``<brain_dir>/ledger.jsonl`` — the cost ledger, one file for every run. A line
  belongs to a run when its ``timestamp`` falls inside one of that run's slices.
* ``<run_dir>/body/events.jsonl`` — the body's event log of this very run. A
  console-managed world starts one bot per attempt under
  ``<run_dir>/bots/attempt-*/body/events.jsonl``; the newest of those is read
  when the direct path is absent, and ``run.body_log`` names the file used.
* ``<worlds_dir>/<world_id>/goals.jsonl`` — world memory; the last line per
  ``goal_id`` wins, and only the goals whose ``t_start`` lies inside the run's
  slices count (a world directory may outlive one run).

Writing and admission are paid for by the ledger lines whose ``purpose`` is one
of :data:`WRITE_PURPOSES`: ``write_skill`` for a first draft and
``repair_skill`` for a repair attempt (``brain/praxion_brain/writer.py`` sets the
purpose from the failure context). No other purpose — ``propose_goal``,
``route``, ``map_command`` — pays for a write.

A write ends at the first of :data:`WRITE_TERMINALS` that names its goal:
``admission_report`` is the admission's own verdict (``admitted`` /
``rejected`` / ``inconclusive_body``) and names its goal only through
``report.goal``; ``write_stopped``, ``write_timeout``, ``admission_error`` and
``write_abandoned`` are the loop's endings and carry the write's ``goal_id``
(only ``write_stopped`` also carries a verdict, ``admission_status``). A
terminal that names no goal at all (a legacy line) is bound by position, the
first one after the write. A write with no ending line is ``unfinished``.

Every number the data cannot answer is ``None``, never ``0`` and never a guess;
a count of a kind that is absent from a file that exists is a real ``0``.
Corrupt JSON lines are skipped and counted in ``health.corrupt_lines``; the
ledger, which is shared by every run, counts its corrupt lines and its lines
without a usable ``timestamp`` or ``cost`` in ``health.ledger_corrupt_lines``
and ``health.ledger_unattributed_lines``. A duration whose end lies before its
start is a clock regression: it is ``None`` and counted in
``health.clock_regressions``.

The summary has exactly seven sections: ``run``, ``progress``, ``skill_writing``,
``stuck``, ``sense``, ``cost`` and ``health``.

    python -m praxion_harness.metrics RUN_DIR [RUN_DIR ...]
        [--brain-dir local/brain] [--worlds-dir local/worlds] [--write]
"""

from __future__ import annotations

import argparse
import json
import statistics
import sys
from collections import Counter
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Optional, Sequence

from .paths import repo_root

#: One JSON object read from a JSONL file. ``None`` marks a corrupt line.
Json = dict[str, Any]

#: Ledger purposes that pay for writing a skill or repairing it during admission.
WRITE_PURPOSES = frozenset({"write_skill", "repair_skill"})

#: The evidence kinds that end a write; the first one after ``write_started``
#: belongs to that write (the loop runs one admission job at a time).
WRITE_TERMINALS = frozenset(
    {
        "admission_report",
        "write_stopped",
        "write_timeout",
        "admission_error",
        "write_abandoned",
    }
)

#: ``admission_report.status`` -> the outcome name of the summary.
ADMISSION_STATUS = {
    "admitted": "admitted",
    "rejected": "rejected",
    "inconclusive_body": "inconclusive",
}

#: ``primitive_finished.outcome`` values that are a failure. ``preempted`` is not
#: one: the primitive was replaced, which is control flow, not a failure.
PRIMITIVE_FAILURE_OUTCOMES = frozenset({"failed"})

#: Seconds of slack when placing a world-memory goal in the run's window; the
#: memory and the evidence share one wall clock, so this only absorbs rounding.
WINDOW_SLACK_SECONDS = 2.0


# --------------------------------------------------------------------------- #
# Small readers and formatters
# --------------------------------------------------------------------------- #
def _parse_time(raw: Any) -> Optional[datetime]:
    """An ISO-8601 timestamp with a zone; a naive one is read as UTC."""
    if not isinstance(raw, str) or not raw:
        return None
    try:
        moment = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError:
        return None
    if moment.tzinfo is None:
        moment = moment.replace(tzinfo=timezone.utc)
    return moment


def _moment(epoch: Any) -> Optional[datetime]:
    """A POSIX epoch number as a moment; ``None`` when absent or unusable."""
    value = _number(epoch)
    if value is None:
        return None
    try:
        return datetime.fromtimestamp(value, timezone.utc)
    except (OverflowError, OSError, ValueError):
        return None


def _read_jsonl(path: Path) -> Optional[list[Optional[Json]]]:
    """Every non-blank line of a JSONL file as a dict, ``None`` when corrupt.

    Returns ``None`` when the file does not exist or cannot be read, so the
    caller can tell "no file" from "an empty file".
    """
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return None
    lines: list[Optional[Json]] = []
    for raw in text.splitlines():
        if not raw.strip():
            continue
        try:
            value = json.loads(raw)
        except ValueError:
            value = None
        lines.append(value if isinstance(value, dict) else None)
    return lines


def _events(lines: Iterable[Optional[Json]]) -> list[Json]:
    """The readable events of a file, in file order."""
    return [line for line in lines if line is not None]


def _corrupt(lines: Iterable[Optional[Json]]) -> int:
    """How many lines of a file were corrupt JSON."""
    return sum(1 for line in lines if line is None)


def _display(path: Path) -> str:
    """A path as reported in ``health.missing_files``: repo-relative when inside."""
    try:
        return str(path.resolve().relative_to(repo_root()))
    except (OSError, ValueError):
        return str(path)


def _seconds(value: Optional[float]) -> Optional[float]:
    return None if value is None else round(float(value), 3)


def _ratio(value: Optional[float]) -> Optional[float]:
    return None if value is None else round(float(value), 4)


def _usd(value: Optional[float]) -> Optional[float]:
    return None if value is None else round(float(value), 6)


def _number(value: Any) -> Optional[float]:
    """A JSON number, not a bool and not a string."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value)


def _count_of(counter: Optional[Counter], kind: str) -> Optional[int]:
    """The count of one evidence kind, or ``None`` when the file was not read."""
    return None if counter is None else int(counter.get(kind, 0))


# --------------------------------------------------------------------------- #
# Slicing
# --------------------------------------------------------------------------- #
def _run_segments(
    lines: Sequence[Optional[Json]], world_id: str
) -> Optional[list[list[Optional[Json]]]]:
    """Every slice of one run, in file order.

    A slice starts at a ``loop_started`` of this world and ends at the next
    ``loop_started`` of any world. A brain restart in the same world starts
    another slice, so a run is all of its slices; the lines of another world
    between two of them belong to that world and are left out. ``None`` when the
    file holds no ``loop_started`` for this world: nothing can then be
    attributed to the run.
    """
    segments: list[list[Optional[Json]]] = []
    current: Optional[list[Optional[Json]]] = None
    for line in lines:
        if line is not None and line.get("kind") == "loop_started":
            if current is not None:
                segments.append(current)
            current = [line] if line.get("world_id") == world_id else None
            continue
        if current is not None:
            current.append(line)
    if current is not None:
        segments.append(current)
    return segments or None


#: One slice of a run: its first and last timestamp, and how long it lasted.
SegmentWindow = tuple[Optional[datetime], Optional[datetime], Optional[float]]


class _Clock:
    """Durations of one run, and the pairs whose end lies before their start."""

    def __init__(self) -> None:
        #: How many durations had to be refused: end before start.
        self.regressions = 0

    def seconds(self, delta: float) -> Optional[float]:
        """A duration in seconds; a negative one is a clock regression, not a zero."""
        if delta < 0:
            self.regressions += 1
            return None
        return _seconds(delta)

    def duration(self, start: Optional[datetime], end: Optional[datetime]) -> Optional[float]:
        """Seconds from ``start`` to ``end``; ``None`` when either is unknown."""
        if start is None or end is None:
            return None
        return self.seconds((end - start).total_seconds())

    def span(self, start: Optional[float], end: Optional[float]) -> Optional[float]:
        """Seconds between two epoch numbers (world memory keeps its own clock)."""
        if start is None or end is None:
            return None
        return self.seconds(end - start)


def _timestamps(events: Iterable[Json]) -> list[datetime]:
    """Every parseable timestamp of one slice, in file order."""
    stamps = (_parse_time(event.get("timestamp")) for event in events)
    return [moment for moment in stamps if moment is not None]


def _segment_windows(
    segments: Sequence[Sequence[Optional[Json]]], clock: _Clock
) -> list[SegmentWindow]:
    """One window per slice: its bounds and its duration, never the gap between two."""
    windows: list[SegmentWindow] = []
    for segment in segments:
        stamps = _timestamps(_events(segment))
        start = stamps[0] if stamps else None
        end = stamps[-1] if stamps else None
        windows.append((start, end, clock.duration(start, end)))
    return windows


def _window_span(windows: Sequence[SegmentWindow]) -> Optional[float]:
    """The run's wall clock: the durations of its slices, summed."""
    durations = [window[2] for window in windows if window[2] is not None]
    return _seconds(sum(durations)) if durations else None


def _in_windows(
    moment: Optional[datetime], windows: Sequence[SegmentWindow], slack: float = 0.0
) -> bool:
    """Whether ``moment`` lies in one of the run's slices; unknown bounds never exclude."""
    if moment is None or not windows:
        return True
    for start, end, _duration in windows:
        if start is None or end is None:
            return True
        if start.timestamp() - slack <= moment.timestamp() <= end.timestamp() + slack:
            return True
    return False


# --------------------------------------------------------------------------- #
# The sections
# --------------------------------------------------------------------------- #
def _run_section(
    world_id: str,
    brain: list[Json],
    body: Optional[list[Json]],
    windows: Sequence[SegmentWindow],
    body_log: Optional[str],
    brain_ok: bool,
    body_ok: bool,
) -> dict[str, Any]:
    """The ``run`` section; the windows of its slices bound every other metric."""
    known = [window for window in windows if window[0] is not None and window[1] is not None]
    brain_start = known[0][0] if known else None
    brain_end = known[-1][1] if known else None

    body_ticks = None
    versions = None
    if body_ok and body is not None:
        ticks = [
            int(event["tick"])
            for event in body
            if isinstance(event.get("tick"), int) and not isinstance(event.get("tick"), bool)
        ]
        body_ticks = max(ticks) if ticks else None
        for event in body:
            if event.get("kind") == "run_started" and isinstance(event.get("versions"), dict):
                versions = event["versions"]
                break

    section = {
        "world_id": world_id,
        "brain_start": brain_start.isoformat() if brain_start is not None else None,
        "brain_end": brain_end.isoformat() if brain_end is not None else None,
        "wall_seconds": _window_span(windows),
        "brain_segments": len(windows) if brain_ok else None,
        "body_ticks": body_ticks,
        "body_log": body_log,
        "versions": versions,
        "brain_events": len(brain) if brain_ok else None,
        "body_events": len(body) if body_ok and body is not None else None,
    }
    return section


def _goal_wall_seconds(record: Json, clock: _Clock) -> Optional[float]:
    """How long a goal was open, from its world-memory ``t_start``/``t_end``."""
    return clock.span(_number(record.get("t_start")), _number(record.get("t_end")))


def _last_per_goal(lines: Optional[list[Optional[Json]]]) -> Optional[list[Json]]:
    """World memory as one record per ``goal_id``: the last line wins."""
    if lines is None:
        return None
    records: dict[str, Json] = {}
    for line in lines:
        if line is None:
            continue
        goal_id = line.get("goal_id")
        if isinstance(goal_id, str):
            records[goal_id] = line  # the last line per goal_id wins
    return list(records.values())


def _progress_section(
    goals: Optional[list[Json]],
    segments: Sequence[Sequence[Optional[Json]]],
    windows: Sequence[SegmentWindow],
    brain_ok: bool,
    clock: _Clock,
) -> dict[str, Any]:
    """Goals, rungs and the rung timeline.

    ``goals`` is world memory in first-appearance order: every goal record the
    brain wrote, including a write's own goal (a condition, no command), so
    ``goals_total`` counts played and written goals alike.

    The timeline is on the run's own clock: the durations of the slices that
    precede a rung are added, so a restart does not stretch the axis.
    """
    goal_rows: Optional[list[Json]] = None
    by_status: Optional[dict[str, int]] = None
    if goals is not None:
        by_status = Counter(str(record.get("status") or "unknown") for record in goals)
        goal_rows = [
            {
                "goal_id": record.get("goal_id"),
                "skill": record.get("skill"),
                "condition": record.get("condition"),
                "status": record.get("status"),
                "failure_kind": record.get("failure_kind"),
                "ticks": record.get("ticks"),
                "wall_seconds": _goal_wall_seconds(record, clock),
            }
            for record in goals
        ]

    rungs = [
        int(event["rungs_met"])
        for segment in segments
        for event in _events(segment)
        if event.get("kind") == "goal_finished"
        and isinstance(event.get("rungs_met"), int)
        and not isinstance(event.get("rungs_met"), bool)
    ]
    timeline: Optional[list[Json]] = None
    rungs_max: Optional[int] = None
    rungs_per_hour: Optional[float] = None
    if brain_ok:
        timeline = []
        best = 0
        elapsed = 0.0
        for segment, (start, _end, duration) in zip(segments, windows):
            for event in _events(segment):
                if event.get("kind") != "goal_finished":
                    continue
                met = event.get("rungs_met")
                if not isinstance(met, int) or isinstance(met, bool) or met <= best:
                    continue
                best = met
                moment = _parse_time(event.get("timestamp"))
                offset = clock.duration(start, moment)
                if offset is not None:
                    offset = _seconds(offset + elapsed)
                timeline.append(
                    {
                        "rungs_met": met,
                        "wall_seconds_from_start": offset,
                        "skill": event.get("skill"),
                    }
                )
            elapsed += duration or 0.0

        rungs_max = max(rungs) if rungs else None
        wall_seconds = _window_span(windows)
        if rungs_max is not None and wall_seconds:
            rungs_per_hour = _ratio(rungs_max / (wall_seconds / 3600.0))

    return {
        "goals_total": len(goal_rows) if goal_rows is not None else None,
        "goals_by_status": dict(sorted(by_status.items())) if by_status is not None else None,
        "rungs_met_max": rungs_max,
        "rungs_per_hour": rungs_per_hour,
        "rung_timeline": timeline,
        "goals": goal_rows,
    }


def _write_outcome(terminal: Optional[Json]) -> str:
    """What ended a write, from its terminal evidence line."""
    if terminal is None:
        return "unfinished"
    kind = terminal.get("kind")
    if kind == "admission_report":
        report = terminal.get("report")
        status = report.get("status") if isinstance(report, dict) else None
        return ADMISSION_STATUS.get(str(status), "inconclusive")
    if kind == "write_stopped":
        return ADMISSION_STATUS.get(str(terminal.get("admission_status")), "unfinished")
    if kind == "write_abandoned":
        return "unfinished"
    # write_timeout and admission_error: the admission ended without a verdict.
    return "inconclusive"


def _sum_cost(
    ledger: Optional[list[Json]],
    start: Optional[datetime],
    end: Optional[datetime],
    purposes: Optional[frozenset[str]] = None,
    windows: Optional[Sequence[SegmentWindow]] = None,
) -> Optional[float]:
    """The USD spent in ``[start, end]`` on ``purposes``; ``None`` without a ledger.

    ``windows`` limits the bill to the run's own slices, so the time a restart
    kept the brain down is never billed to the run.
    """
    if ledger is None or start is None or end is None:
        return None
    total = 0.0
    for line in ledger:
        moment = _parse_time(line.get("timestamp"))
        cost = _number(line.get("cost"))
        if moment is None or cost is None:
            continue
        if purposes is not None and line.get("purpose") not in purposes:
            continue
        if start <= moment <= end and _in_windows(moment, windows or ()):
            total += cost
    return _usd(total)


def _reported_skill(terminal: Json) -> Optional[str]:
    """The skill named by an ``admission_report``'s ``report.goal``.

    ``AdmissionReport.goal`` is the goal the admission ran for (``text``,
    ``item``, ``count``), not its record, so the item is the only identity the
    line carries: ``item=furnace`` is the ``get_furnace`` write.
    """
    report = terminal.get("report")
    goal = report.get("goal") if isinstance(report, dict) else None
    item = goal.get("item") if isinstance(goal, dict) else None
    if not isinstance(item, str) or not item:
        return None
    return f"get_{item}"


def _names_a_goal(event: Json) -> bool:
    """Whether an evidence line says which goal it belongs to."""
    return (
        isinstance(event.get("goal_id"), str)
        or isinstance(event.get("skill"), str)
        or _reported_skill(event) is not None
    )


def _terminal_matches(terminal: Json, write: Json) -> bool:
    """Whether ``terminal`` ends this exact write: same ``goal_id``, else same goal."""
    write_goal = write.get("goal_id")
    terminal_goal = terminal.get("goal_id")
    if isinstance(write_goal, str) and isinstance(terminal_goal, str):
        return write_goal == terminal_goal
    write_skill = write.get("skill")
    terminal_skill = terminal.get("skill") or _reported_skill(terminal)
    return isinstance(write_skill, str) and write_skill == terminal_skill


def _write_terminal(
    brain: Sequence[Json], index: int, limit: int, write: Json
) -> tuple[Optional[Json], int]:
    """The terminal of one write and the index its window ends at.

    A terminal that names the write's goal binds it, whichever admission it
    follows; the CLI's ``learn`` writes its own report into the same file, so
    position alone is only the fallback for a line that names no goal. A second
    ``write_started`` before any terminal means this write never ended.
    """
    positional: Optional[tuple[Json, int]] = None
    for cursor in range(index + 1, limit):
        event = brain[cursor]
        if event.get("kind") not in WRITE_TERMINALS:
            continue
        if not _names_a_goal(event):
            if positional is None:
                positional = (event, cursor)
            continue
        if _terminal_matches(event, write):
            return event, cursor
    if positional is not None:
        return positional
    return None, limit


def _skill_writing_section(
    brain: list[Json],
    ledger: Optional[list[Json]],
    brain_ok: bool,
    brain_end: Optional[datetime],
    windows: Sequence[SegmentWindow],
    clock: _Clock,
) -> dict[str, Any]:
    """One entry per ``write_started``, with its admission and its bill."""
    writes: Optional[list[Json]] = None
    if brain_ok:
        writes = []
        starts = [
            index for index, event in enumerate(brain) if event.get("kind") == "write_started"
        ]
        for position, index in enumerate(starts):
            event = brain[index]
            limit = starts[position + 1] if position + 1 < len(starts) else len(brain)
            started = _parse_time(event.get("timestamp"))
            terminal, end_index = _write_terminal(brain, index, limit, event)
            window = brain[index + 1 : end_index]
            finished = _parse_time(terminal.get("timestamp")) if terminal is not None else None
            outcome = _write_outcome(terminal)

            attempts = 0
            models: list[str] = []
            practice_runs = 0
            body_failures = 0
            for item in window:
                kind = item.get("kind")
                if kind == "admission_attempt":
                    attempts += 1
                    model = item.get("model")
                    if isinstance(model, str) and model not in models:
                        models.append(model)
                elif kind == "admission_practice":
                    practice_runs += 1
                elif kind == "admission_body_failure":
                    body_failures += 1

            seconds = clock.duration(started, finished)
            # A window whose end lies before its start bills nothing trustworthy.
            ended = finished if terminal is not None else brain_end
            in_order = started is None or ended is None or ended >= started

            first_use = _first_use(brain, end_index, event) if outcome == "admitted" else None

            writes.append(
                {
                    "skill": event.get("skill"),
                    "condition": event.get("condition"),
                    "writer_model": event.get("writer_model"),
                    "started_at": started.isoformat() if started is not None else None,
                    "attempts": attempts,
                    "models": models,
                    "practice_runs": practice_runs,
                    "body_failures": body_failures,
                    "outcome": outcome,
                    "seconds_to_outcome": seconds,
                    "cost_usd": (
                        _sum_cost(ledger, started, ended, WRITE_PURPOSES, windows)
                        if in_order
                        else None
                    ),
                    "first_use_after_admission": first_use,
                }
            )

    admitted_seconds = [
        write["seconds_to_outcome"]
        for write in writes or ()
        if write["outcome"] == "admitted" and write["seconds_to_outcome"] is not None
    ]
    total_cost = None
    if ledger is not None and writes is not None:
        costs = [write["cost_usd"] for write in writes]
        # One unknown bill makes the total unknown: a partial sum would lie.
        if all(cost is not None for cost in costs):
            total_cost = _usd(sum(costs))

    return {
        "writes": writes,
        "writes_started": len(writes) if writes is not None else None,
        "writes_admitted": (
            sum(1 for write in writes if write["outcome"] == "admitted")
            if writes is not None
            else None
        ),
        "median_seconds_to_admit": (
            _seconds(statistics.median(admitted_seconds)) if admitted_seconds else None
        ),
        "total_write_cost_usd": total_cost,
    }


def _first_use(brain: Sequence[Json], end_index: int, write: Json) -> Optional[Json]:
    """The write's own goal when it was played; else a later play of its skill.

    The goal the admission ran for is the write's ``goal_id`` (``loop.py`` puts
    it on ``goal_finished``). The same skill name may be played later for
    another goal, which is only a stand-in and is marked ``same_goal: false``.
    """
    goal_id = write.get("goal_id")
    skill = write.get("skill")
    later: Optional[Json] = None
    for item in brain[end_index + 1 :]:
        if item.get("kind") != "goal_finished" or item.get("command_id") is None:
            continue
        if isinstance(goal_id, str) and item.get("goal_id") == goal_id:
            return _first_use_entry(item, same_goal=True)
        if later is None and isinstance(skill, str) and item.get("skill") == skill:
            later = item
    return _first_use_entry(later, same_goal=False) if later is not None else None


def _first_use_entry(event: Json, *, same_goal: bool) -> Json:
    """One ``first_use_after_admission`` value."""
    return {
        "goal_id": event.get("goal_id"),
        "status": event.get("status"),
        "ticks": event.get("ticks"),
        "same_goal": same_goal,
    }


def _idle_gaps(
    segments: Sequence[Sequence[Optional[Json]]],
    windows: Sequence[SegmentWindow],
    clock: _Clock,
) -> Optional[list[float]]:
    """Every pick-to-command gap of the run, in seconds, one slice at a time.

    A gap opens when a played goal settles (``goal_finished`` with a
    ``command_id``; a write's own record has none) and closes at the next
    ``command_sent``. The first gap of a slice starts at that slice's first
    evidence line, so start-to-first-command is counted too. The time between
    two slices (a brain restart) is never a gap. ``None`` when no command was
    ever sent: nothing about picking can then be measured.
    """
    gaps: list[float] = []
    sent = False
    for segment, (start, _end, _duration) in zip(segments, windows):
        anchor = start
        for event in _events(segment):
            kind = event.get("kind")
            moment = _parse_time(event.get("timestamp"))
            played = kind == "goal_finished" and event.get("command_id") is not None
            if played and moment is not None:
                anchor = moment
            elif kind == "command_sent" and moment is not None:
                sent = True
                if anchor is not None:
                    gap = clock.duration(anchor, moment)
                    if gap is not None:
                        gaps.append(gap)
                anchor = None
    return gaps if sent else None


def _failed_streak(brain: list[Json], brain_ok: bool) -> Optional[int]:
    """The longest run of consecutive played goals that ended ``failed``."""
    streak = 0
    best = 0
    seen = False
    for event in brain:
        if event.get("kind") != "goal_finished" or event.get("command_id") is None:
            continue
        seen = True
        if event.get("status") == "failed":
            streak += 1
            best = max(best, streak)
        else:
            streak = 0
    return best if (brain_ok and seen) else None


def _stuck_section(
    brain: list[Json],
    body: Optional[list[Json]],
    goals: Optional[list[Json]],
    counts: Optional[Counter],
    segments: Sequence[Sequence[Optional[Json]]],
    windows: Sequence[SegmentWindow],
    brain_ok: bool,
    body_ok: bool,
    clock: _Clock,
) -> dict[str, Any]:
    """Idle time, back-offs and the body's wasted ticks."""
    gaps = _idle_gaps(segments, windows, clock) if brain_ok else None
    wall_seconds = _window_span(windows)
    idle_seconds = _seconds(sum(gaps)) if gaps is not None else None
    idle_fraction = None
    if gaps is not None and wall_seconds:
        idle_fraction = _ratio(sum(gaps) / wall_seconds)
    longest_idle = _seconds(max(gaps)) if gaps else None

    repeated: Optional[dict[str, int]] = None
    if goals is not None:
        failed_conditions = Counter(
            record["condition"]
            for record in goals
            if record.get("status") == "failed" and isinstance(record.get("condition"), str)
        )
        repeated = dict(
            sorted(
                (condition, count)
                for condition, count in failed_conditions.items()
                if count >= 2
            )
        )

    primitive_failures: Optional[dict[str, int]] = None
    failed_ticks: Optional[int] = None
    total_ticks: Optional[int] = None
    wasted_fraction: Optional[float] = None
    explore_not_found: Optional[Json] = None
    if body_ok and body is not None:
        primitives = [event for event in body if event.get("kind") == "primitive_finished"]
        total_ticks = sum(
            int(event["ticks"]) for event in primitives if isinstance(event.get("ticks"), int)
        )
        failed = [
            event for event in primitives if event.get("outcome") in PRIMITIVE_FAILURE_OUTCOMES
        ]
        failed_ticks = sum(
            int(event["ticks"]) for event in failed if isinstance(event.get("ticks"), int)
        )
        primitive_failures = dict(
            sorted(
                Counter(
                    f"{event.get('primitive')}:{event.get('failure_kind') or 'unknown'}"
                    for event in failed
                ).items()
            )
        )
        if total_ticks:
            wasted_fraction = _ratio(failed_ticks / total_ticks)
        exploring = [
            event
            for event in primitives
            if event.get("primitive") == "explore" and event.get("failure_kind") == "not_found"
        ]
        explore_not_found = {
            "count": len(exploring),
            "ticks": sum(
                int(event["ticks"]) for event in exploring if isinstance(event.get("ticks"), int)
            ),
        }

    return {
        "idle_seconds": idle_seconds,
        "idle_fraction": idle_fraction,
        "longest_idle_seconds": longest_idle,
        "nothing_to_do": _count_of(counts, "nothing_to_do"),
        "pick_backoff": _count_of(counts, "pick_backoff"),
        "code_only_idle": _count_of(counts, "code_only_idle"),
        "night_wait": _count_of(counts, "night_wait"),
        "longest_failed_goal_streak": _failed_streak(brain, brain_ok),
        "repeated_failed_conditions": repeated,
        "primitive_failures": primitive_failures,
        "failed_primitive_ticks": failed_ticks,
        "total_primitive_ticks": total_ticks,
        "wasted_tick_fraction": wasted_fraction,
        "explore_not_found": explore_not_found,
    }


def _redone_goals(goals: Optional[list[Json]]) -> Optional[dict[str, int]]:
    """Conditions commanded again after one of them already ended ``done``.

    A repeated condition is the brain asking for a world fact it had already
    reached: work that the previous command should have kept satisfied.
    """
    if goals is None:
        return None
    counts: Counter = Counter()
    done: set[str] = set()
    for record in goals:
        condition = record.get("condition")
        if not isinstance(condition, str):
            continue
        if condition in done:
            counts[condition] += 1
        if record.get("status") == "done":
            done.add(condition)
    return dict(sorted(counts.items()))


def _sense_section(
    brain: list[Json],
    body: Optional[list[Json]],
    goals: Optional[list[Json]],
    counts: Optional[Counter],
    body_ok: bool,
) -> dict[str, Any]:
    """What the run wasted on itself: refusals, drift, and body-side reflex take-overs."""
    proposals_total = _count_of(counts, "proposal_request")
    invalid = _count_of(counts, "proposal_invalid")
    invalid_rate = None
    if proposals_total and invalid is not None:
        invalid_rate = _ratio(invalid / proposals_total)

    reflex_preemptions: Optional[int] = None
    receipts_by_outcome: Optional[dict[str, int]] = None
    late_evidence: Optional[int] = None
    deaths: Optional[int] = None
    if body_ok and body is not None:
        reflex_preemptions = sum(
            1
            for event in body
            if event.get("kind") == "owner_changed"
            and str(event.get("owner") or "").startswith("reflex:")
        )
        receipts_by_outcome = dict(
            sorted(
                Counter(
                    str(event.get("outcome") or "unknown")
                    for event in body
                    if event.get("kind") == "receipt_settled"
                ).items()
            )
        )
        late_evidence = sum(1 for event in body if event.get("kind") == "late_evidence")
        deaths = sum(1 for event in body if event.get("kind") == "died")

    return {
        "redone_goals": _redone_goals(goals),
        "proposal_invalid": invalid,
        "proposals_total": proposals_total,
        "proposal_invalid_rate": invalid_rate,
        "governor_denied": _count_of(counts, "governor_denied"),
        "curriculum_drift": _count_of(counts, "curriculum_drift"),
        "drift_brake": _count_of(counts, "drift_brake"),
        "receipt_unmatched": _count_of(counts, "receipt_unmatched"),
        "loop_errors": _count_of(counts, "loop_error"),
        "reflex_preemptions": reflex_preemptions,
        "receipts_by_outcome": receipts_by_outcome,
        "late_evidence": late_evidence,
        "deaths": deaths,
    }


def _cost_section(
    ledger: Optional[list[Json]],
    ledger_ok: bool,
    windows: Sequence[SegmentWindow],
    rungs_met_max: Optional[int],
) -> dict[str, Any]:
    """The ledger lines inside the run's slices, by provider, model and purpose."""
    if not ledger_ok or ledger is None:
        return {
            "by_provider": None,
            "by_model": None,
            "by_purpose": None,
            "total_usd": None,
            "usd_per_rung": None,
        }
    by_provider: Counter = Counter()
    by_model: Counter = Counter()
    by_purpose: Counter = Counter()
    total = 0.0
    for line in ledger:
        moment = _parse_time(line.get("timestamp"))
        cost = _number(line.get("cost"))
        if moment is None or cost is None or not _in_windows(moment, windows):
            continue
        total += cost
        by_provider[str(line.get("provider") or "unknown")] += cost
        by_model[str(line.get("model") or "unknown")] += cost
        by_purpose[str(line.get("purpose") or "unknown")] += cost
    return {
        "by_provider": {key: _usd(value) for key, value in sorted(by_provider.items())},
        "by_model": {key: _usd(value) for key, value in sorted(by_model.items())},
        "by_purpose": {key: _usd(value) for key, value in sorted(by_purpose.items())},
        "total_usd": _usd(total),
        "usd_per_rung": _usd(total / rungs_met_max) if rungs_met_max else None,
    }


def _health_section(
    brain: list[Json],
    body: Optional[list[Json]],
    *,
    corrupt_lines: int,
    ledger_unattributed: Optional[int],
    ledger_corrupt: Optional[int],
    clock_regressions: int,
    missing: list[str],
) -> dict[str, Any]:
    """Whether the evidence can be trusted at all."""
    desync = None
    for event in brain:
        if event.get("kind") == "world_desync":
            desync = str(event.get("reason") or "unknown")
            break
    if desync is None:
        for event in body or ():
            if event.get("kind") == "world_desync":
                desync = str(event.get("reason") or "unknown")
                break
    return {
        "world_desync": desync,
        # The body's log has no ``dropped`` kind: dropped frames are a port
        # handshake counter, never an evidence line, so this is not derivable.
        "dropped_notices": None,
        "corrupt_lines": corrupt_lines,
        # The ledger is one file for every run: a line without a usable time or
        # cost belongs to no run at all, and a corrupt line cannot be placed.
        "ledger_unattributed_lines": ledger_unattributed,
        "ledger_corrupt_lines": ledger_corrupt,
        "clock_regressions": clock_regressions,
        "missing_files": missing,
    }


# --------------------------------------------------------------------------- #
# Public API
# --------------------------------------------------------------------------- #
def _body_log(run_dir: Path) -> Optional[Path]:
    """The body's event log of a run, or ``None`` when it has none.

    A plain run writes ``<run_dir>/body/events.jsonl``. A console-managed world
    keeps one bot per attempt under ``<run_dir>/bots/attempt-*/body/``; the
    newest attempt (by file modification time) is the body that ran last.
    """
    direct = run_dir / "body" / "events.jsonl"
    if direct.is_file():
        return direct
    attempts = list(run_dir.glob("bots/*/body/events.jsonl"))
    if not attempts:
        return None
    return max(attempts, key=lambda path: (path.stat().st_mtime, str(path)))


def summarize(run_dir: Path, brain_dir: Path, worlds_dir: Path) -> dict:
    """Every metric of one run, read from the four evidence sources; reads only.

    ``run_dir`` is the run's directory (its name is the ``world_id``),
    ``brain_dir`` holds ``brain.jsonl`` and ``ledger.jsonl``, and ``worlds_dir``
    holds ``<world_id>/goals.jsonl``. Writing costs are the ledger lines whose
    purpose is in :data:`WRITE_PURPOSES` (``write_skill``, ``repair_skill``).
    """
    run_dir = Path(run_dir)
    brain_dir = Path(brain_dir)
    worlds_dir = Path(worlds_dir)
    world_id = run_dir.name
    missing: list[str] = []
    corrupt_lines = 0
    clock = _Clock()

    brain_file = brain_dir / "brain.jsonl"
    brain_lines = _read_jsonl(brain_file)
    if brain_lines is None:
        missing.append(_display(brain_file))
        brain_lines = []
    segments = _run_segments(brain_lines, world_id)
    brain_ok = segments is not None
    slice_segments = segments or []
    corrupt_lines += sum(_corrupt(segment) for segment in slice_segments)
    brain = [event for segment in slice_segments for event in _events(segment)]
    counts = Counter(event.get("kind") for event in brain) if brain_ok else None
    windows = _segment_windows(slice_segments, clock)
    brain_end = next(
        (window[1] for window in reversed(windows) if window[1] is not None), None
    )

    body_path = _body_log(run_dir)
    body: Optional[list[Json]] = None
    body_ok = False
    if body_path is None:
        missing.append(_display(run_dir / "body" / "events.jsonl"))
        body_log = None
    else:
        body_lines = _read_jsonl(body_path)
        body_log = body_path.relative_to(run_dir).as_posix()
        if body_lines is None:
            missing.append(_display(body_path))
        else:
            corrupt_lines += _corrupt(body_lines)
            body = _events(body_lines)
            body_ok = True

    ledger_file = brain_dir / "ledger.jsonl"
    ledger_lines = _read_jsonl(ledger_file)
    ledger_unattributed: Optional[int] = None
    ledger_corrupt: Optional[int] = None
    if ledger_lines is None:
        missing.append(_display(ledger_file))
        ledger: Optional[list[Json]] = None
        ledger_ok = False
    else:
        # The ledger is one file for every run: a corrupt line cannot be placed
        # in a run at all, and neither can a line without a usable time or cost.
        ledger_corrupt = _corrupt(ledger_lines)
        ledger = _events(ledger_lines)
        ledger_unattributed = sum(
            1
            for line in ledger
            if _parse_time(line.get("timestamp")) is None or _number(line.get("cost")) is None
        )
        ledger_ok = True

    goals_file = worlds_dir / world_id / "goals.jsonl"
    goals_lines = _read_jsonl(goals_file)
    goals_ok = goals_lines is not None
    if not goals_ok:
        missing.append(_display(goals_file))
    else:
        corrupt_lines += _corrupt(goals_lines)

    run = _run_section(world_id, brain, body, windows, body_log, brain_ok, body_ok)

    goals: Optional[list[Json]] = None
    if goals_ok and goals_lines is not None:
        records = _last_per_goal(goals_lines) or []
        goals = [
            record
            for record in records
            if _in_windows(_moment(record.get("t_start")), windows, WINDOW_SLACK_SECONDS)
        ]

    progress = _progress_section(goals, slice_segments, windows, brain_ok, clock)
    skill_writing = _skill_writing_section(brain, ledger, brain_ok, brain_end, windows, clock)
    stuck = _stuck_section(
        brain, body, goals, counts, slice_segments, windows, brain_ok, body_ok, clock
    )
    sense = _sense_section(brain, body, goals, counts, body_ok)
    cost = _cost_section(ledger, ledger_ok, windows, progress["rungs_met_max"])
    health = _health_section(
        brain,
        body,
        corrupt_lines=corrupt_lines,
        ledger_unattributed=ledger_unattributed,
        ledger_corrupt=ledger_corrupt,
        clock_regressions=clock.regressions,
        missing=missing,
    )

    return {
        "run": run,
        "progress": progress,
        "skill_writing": skill_writing,
        "stuck": stuck,
        "sense": sense,
        "cost": cost,
        "health": health,
    }


# --------------------------------------------------------------------------- #
# Rendering
# --------------------------------------------------------------------------- #
DASH = "—"


def _fmt(value: Any, digits: Optional[int] = None) -> str:
    """A summary value for markdown; ``None`` is an em dash, never a zero."""
    if value is None:
        return DASH
    if isinstance(value, bool):
        return "yes" if value else "no"
    if isinstance(value, float):
        if digits is None:
            return f"{value:.2f}"
        return f"{value:.{digits}f}"
    return str(value)


def _fmt_counts(values: Optional[dict[str, int]]) -> str:
    """A count dict as ``a ×2, b ×1``; ``None`` is an em dash."""
    if values is None:
        return DASH
    if not values:
        return "none"
    return ", ".join(f"`{key}` ×{count}" for key, count in values.items())


def _fmt_minutes(seconds: Optional[float]) -> str:
    if seconds is None:
        return DASH
    return f"{seconds / 60.0:.1f} min"


def _fmt_first_use(value: Optional[dict]) -> str:
    """A write's first play after admission; a play of another goal is marked."""
    if value is None:
        return DASH
    text = (
        f"{_fmt(value['goal_id'])} {_fmt(value['status'])} {_fmt(value['ticks'])} ticks"
    )
    return text if value.get("same_goal") else text + " (later play)"


def _table(header: Sequence[str], rows: Iterable[Sequence[str]]) -> list[str]:
    """One markdown table."""
    lines = ["| " + " | ".join(header) + " |", "|" + "|".join("---" for _ in header) + "|"]
    for row in rows:
        lines.append("| " + " | ".join(row) + " |")
    return lines


def render_markdown(summary: dict) -> str:
    """The whole summary as a readable markdown report."""
    run = summary["run"]
    progress = summary["progress"]
    writing = summary["skill_writing"]
    stuck = summary["stuck"]
    sense = summary["sense"]
    cost = summary["cost"]
    health = summary["health"]

    lines: list[str] = [f"# Run `{run['world_id']}`", ""]
    lines += _table(
        ("field", "value"),
        [
            ("brain window", f"`{_fmt(run['brain_start'])}` → `{_fmt(run['brain_end'])}`"),
            ("brain segments", _fmt(run["brain_segments"])),
            ("wall time", _fmt_minutes(run["wall_seconds"])),
            ("body log", f"`{run['body_log']}`" if run["body_log"] else DASH),
            ("body ticks", _fmt(run["body_ticks"])),
            ("brain events", _fmt(run["brain_events"])),
            ("body events", _fmt(run["body_events"])),
        ],
    )
    versions = run.get("versions")
    if isinstance(versions, dict) and versions:
        lines += ["", "versions: " + ", ".join(f"{key} {value}" for key, value in versions.items())]

    lines += ["", "## Progress", ""]
    status = progress["goals_by_status"]
    if status:
        counted = ", ".join(f"{key} {value}" for key, value in status.items())
        lines.append(f"- goals: {_fmt(progress['goals_total'])} ({counted})")
    else:
        lines.append(f"- goals: {_fmt(progress['goals_total'])}")
    lines.append(
        f"- rungs met: {_fmt(progress['rungs_met_max'])} "
        f"({_fmt(progress['rungs_per_hour'])} per hour)"
    )
    timeline = progress["rung_timeline"]
    if timeline:
        lines.append("- rung timeline:")
        for point in timeline:
            lines.append(
                f"  - {point['rungs_met']} at {_fmt(point['wall_seconds_from_start'])} s "
                f"({_fmt(point['skill'])})"
            )
    elif timeline is not None:
        lines.append("- rung timeline: none")
    goals = progress["goals"]
    if goals:
        lines += [""]
        lines += _table(
            ("goal", "skill", "condition", "status", "failure", "ticks", "wall s"),
            [
                (
                    _fmt(goal["goal_id"]),
                    _fmt(goal["skill"]),
                    f"`{goal['condition']}`" if goal["condition"] else DASH,
                    _fmt(goal["status"]),
                    _fmt(goal["failure_kind"]),
                    _fmt(goal["ticks"]),
                    _fmt(goal["wall_seconds"], 1),
                )
                for goal in goals
            ],
        )

    lines += ["", "## Skill writing", ""]
    lines.append(
        f"- writes started: {_fmt(writing['writes_started'])}, "
        f"admitted: {_fmt(writing['writes_admitted'])}"
    )
    lines.append(f"- median seconds to admit: {_fmt(writing['median_seconds_to_admit'], 1)}")
    lines.append(f"- total write cost: {_fmt(writing['total_write_cost_usd'], 6)} USD")
    writes = writing["writes"]
    if writes:
        lines += [""]
        lines += _table(
            (
                "skill",
                "condition",
                "outcome",
                "attempts",
                "practice",
                "body failures",
                "seconds",
                "cost USD",
                "first use",
            ),
            [
                (
                    _fmt(write["skill"]),
                    f"`{write['condition']}`" if write["condition"] else DASH,
                    _fmt(write["outcome"]),
                    _fmt(write["attempts"]),
                    _fmt(write["practice_runs"]),
                    _fmt(write["body_failures"]),
                    _fmt(write["seconds_to_outcome"], 1),
                    _fmt(write["cost_usd"], 6),
                    _fmt_first_use(write["first_use_after_admission"]),
                )
                for write in writes
            ],
        )
    elif writes is not None:
        lines.append("- no write was started")

    lines += ["", "## Stuck", ""]
    lines.append(
        f"- idle: {_fmt(stuck['idle_seconds'], 1)} s "
        f"({_fmt(stuck['idle_fraction'])}) of the run, "
        f"longest {_fmt(stuck['longest_idle_seconds'], 1)} s"
    )
    lines.append(
        f"- nothing_to_do {_fmt(stuck['nothing_to_do'])}, "
        f"pick_backoff {_fmt(stuck['pick_backoff'])}, "
        f"code_only_idle {_fmt(stuck['code_only_idle'])}, "
        f"night_wait {_fmt(stuck['night_wait'])}"
    )
    lines.append(f"- longest failed goal streak: {_fmt(stuck['longest_failed_goal_streak'])}")
    repeated = stuck["repeated_failed_conditions"]
    lines.append(f"- repeated failed conditions: {_fmt_counts(repeated)}")
    lines.append(
        f"- primitive ticks: {_fmt(stuck['total_primitive_ticks'])}, "
        f"failed {_fmt(stuck['failed_primitive_ticks'])} "
        f"({_fmt(stuck['wasted_tick_fraction'])} wasted)"
    )
    explore = stuck["explore_not_found"]
    if explore is not None:
        lines.append(f"- explore not found: {explore['count']} ({explore['ticks']} ticks)")
    failures = stuck["primitive_failures"]
    if failures:
        lines += [""]
        lines += _table(
            ("primitive:failure", "count"),
            [(f"`{key}`", str(value)) for key, value in failures.items()],
        )

    lines += ["", "## Sense", ""]
    lines.append(
        f"- proposals: {_fmt(sense['proposals_total'])}, "
        f"invalid {_fmt(sense['proposal_invalid'])} ({_fmt(sense['proposal_invalid_rate'])})"
    )
    lines.append(f"- redone goals: {_fmt_counts(sense['redone_goals'])}")
    lines.append(
        f"- governor_denied {_fmt(sense['governor_denied'])}, "
        f"curriculum_drift {_fmt(sense['curriculum_drift'])}, "
        f"drift_brake {_fmt(sense['drift_brake'])}, "
        f"receipt_unmatched {_fmt(sense['receipt_unmatched'])}, "
        f"loop_errors {_fmt(sense['loop_errors'])}"
    )
    lines.append(f"- reflex preemptions: {_fmt(sense['reflex_preemptions'])}")
    lines.append(f"- receipts: {_fmt_counts(sense['receipts_by_outcome'])}")
    lines.append(
        f"- late evidence {_fmt(sense['late_evidence'])}, deaths {_fmt(sense['deaths'])}"
    )

    lines += ["", "## Cost", ""]
    lines.append(f"- total: {_fmt(cost['total_usd'], 6)} USD")
    lines.append(f"- per rung: {_fmt(cost['usd_per_rung'], 6)} USD")
    breakdowns = (("provider", "by_provider"), ("model", "by_model"), ("purpose", "by_purpose"))
    for title, key in breakdowns:
        values = cost[key]
        if values:
            lines += [""]
            lines += _table(
                (title, "USD"), [(f"`{name}`", _fmt(value, 6)) for name, value in values.items()]
            )

    lines += ["", "## Health", ""]
    lines.append(f"- world desync: {_fmt(health['world_desync'])}")
    lines.append(f"- dropped notices: {_fmt(health['dropped_notices'])}")
    lines.append(f"- corrupt lines: {_fmt(health['corrupt_lines'])}")
    lines.append(
        f"- ledger lines without a usable time or cost: "
        f"{_fmt(health['ledger_unattributed_lines'])}"
    )
    lines.append(f"- corrupt ledger lines: {_fmt(health['ledger_corrupt_lines'])}")
    lines.append(f"- clock regressions: {_fmt(health['clock_regressions'])}")
    missing = health["missing_files"]
    listed = ", ".join(f"`{name}`" for name in missing) if missing else "none"
    lines.append(f"- missing files: {listed}")
    return "\n".join(lines) + "\n"


COMPARE_COLUMNS = (
    "run",
    "wall (min)",
    "goals",
    "done",
    "failed",
    "rungs",
    "rungs/h",
    "writes",
    "admitted",
    "idle",
    "wasted",
    "USD",
    "USD/rung",
    "deaths",
)


def compare_table(summaries: list[dict]) -> str:
    """One markdown row per run, with the headline numbers.

    Ratios are fractions of the run (``idle`` of wall time, ``wasted`` of
    primitive ticks); ``—`` is a number the data could not answer.
    """
    rows: list[tuple[str, ...]] = []
    for summary in summaries:
        run = summary["run"]
        progress = summary["progress"]
        writing = summary["skill_writing"]
        stuck = summary["stuck"]
        sense = summary["sense"]
        cost = summary["cost"]
        status = progress["goals_by_status"] or {}
        wall = run["wall_seconds"]
        rows.append(
            (
                f"`{run['world_id']}`",
                _fmt(None if wall is None else wall / 60.0, 1),
                _fmt(progress["goals_total"]),
                _fmt(status.get("done")),
                _fmt(status.get("failed")),
                _fmt(progress["rungs_met_max"]),
                _fmt(progress["rungs_per_hour"], 2),
                _fmt(writing["writes_started"]),
                _fmt(writing["writes_admitted"]),
                _fmt(stuck["idle_fraction"], 3),
                _fmt(stuck["wasted_tick_fraction"], 3),
                _fmt(cost["total_usd"], 6),
                _fmt(cost["usd_per_rung"], 6),
                _fmt(sense["deaths"]),
            )
        )
    return "\n".join(_table(COMPARE_COLUMNS, rows)) + "\n"


# --------------------------------------------------------------------------- #
# CLI
# --------------------------------------------------------------------------- #
def _directory(raw: Optional[str], default: Path) -> Path:
    """A CLI directory argument; a relative one is resolved against the repo root."""
    if raw is None:
        return default
    path = Path(raw).expanduser()
    if not path.is_absolute():
        path = repo_root() / path
    return path


def main(argv: Optional[list[str]] = None) -> int:
    """Print the report of one run or the compare table of several."""
    parser = argparse.ArgumentParser(
        prog="praxion_harness.metrics",
        description="Summarise the evidence of autonomy runs, one report or a compare table.",
    )
    parser.add_argument(
        "run_dirs",
        nargs="+",
        metavar="RUN_DIR",
        help="a run directory (its name is the world_id)",
    )
    parser.add_argument(
        "--brain-dir",
        default=None,
        help="where brain.jsonl and ledger.jsonl live (default: <repo>/local/brain)",
    )
    parser.add_argument(
        "--worlds-dir",
        default=None,
        help="where <world_id>/goals.jsonl live (default: <repo>/local/worlds)",
    )
    parser.add_argument(
        "--write",
        action="store_true",
        help="also write metrics.json and metrics.md into each RUN_DIR",
    )
    args = parser.parse_args(argv)

    root = repo_root()
    brain_dir = _directory(args.brain_dir, root / "local" / "brain")
    worlds_dir = _directory(args.worlds_dir, root / "local" / "worlds")

    run_dirs = [Path(raw) for raw in args.run_dirs]
    for run_dir in run_dirs:
        if not run_dir.is_dir():
            print(f"not a directory: {run_dir}", file=sys.stderr)
            return 2

    summaries = [summarize(run_dir, brain_dir, worlds_dir) for run_dir in run_dirs]
    if len(summaries) == 1:
        print(render_markdown(summaries[0]), end="")
    else:
        print(compare_table(summaries), end="")

    if args.write:
        for run_dir, summary in zip(run_dirs, summaries):
            (run_dir / "metrics.json").write_text(
                json.dumps(summary, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
            )
            (run_dir / "metrics.md").write_text(render_markdown(summary), encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())


__all__ = ["summarize", "render_markdown", "compare_table", "main"]
