"""One practice run on the headless body, judged by the referee.

The brain starts the same server/forwarder/body stack as the smoke test, gives
the body nothing, passes the attempt's candidate directory and reads the
inventory from the referee. The run directory is kept as evidence.

A live run is registered in this module: a cancelled admission kills it through
the token's callback, and :func:`stop_live_runs` kills every live run when the
interpreter exits, so a shutdown never leaves a server, forwarder or body behind.
"""

from __future__ import annotations

import atexit
import threading
from functools import partial
from pathlib import Path
from typing import Any, Collection, Literal, Sequence

from pydantic import BaseModel, Field

from . import config  # noqa: F401  (adds harness/ to sys.path in one place)
from .cancel import AdmissionCancelled, CancelToken
from .goal import Goal
from .skills import parse_skill_text
from praxion_harness.body import start_body

#: Event kinds kept in the repair digest.
DIGEST_KINDS = {"primitive_finished", "skill_frame", "rule_selected"}
DIGEST_EVENTS = 30

#: Consecutive candidate rule selections with no confirmed primitive between
#: them that make a max_ticks/timeout run a skill loop instead of a body gap.
LOOP_RULES = 10

FailureClass = Literal["skill", "body"]

#: The practice runs alive right now, by ``id(run)``. A cancelled admission or
#: the interpreter exiting stops them, so no server, forwarder or body outlives
#: the brain (the harness only cleans up after itself when it gets the chance).
_LIVE_LOCK = threading.Lock()
_LIVE: dict[int, Any] = {}


def _stop_run(run: Any) -> None:
    """Stop one run's body, forwarder and server; never raise (cancel/atexit)."""
    try:
        run.stop()
    except Exception:  # noqa: BLE001 - a cleanup must not break the canceller
        pass


def _register(run: Any) -> None:
    with _LIVE_LOCK:
        _LIVE[id(run)] = run


def _unregister(run: Any) -> None:
    with _LIVE_LOCK:
        _LIVE.pop(id(run), None)


def stop_live_runs() -> None:
    """Stop every practice run still alive; the ``atexit`` handler too."""
    with _LIVE_LOCK:
        runs = list(_LIVE.values())
        _LIVE.clear()
    for run in runs:
        _stop_run(run)


atexit.register(stop_live_runs)


class PracticeResult(BaseModel):
    passed: bool
    outcome: str
    ticks: int = 0
    receipts: dict[str, int] = Field(default_factory=dict)
    deaths: int = 0
    run_dir: str
    digest: str
    seed: int
    failure_kind: str | None = None
    failure_class: FailureClass | None = None
    inventory: dict[str, int] = Field(default_factory=dict)


def _peaceful(fixture: Any, referee: Any, run: Any) -> None:
    """Daytime, no mobs, nothing in the inventory."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)


def _count(inventory: dict[str, int], item: str) -> int:
    return inventory.get(f"minecraft:{item}", inventory.get(item, 0))


def _event_line(event: dict[str, Any]) -> str:
    kind = event.get("kind")
    if kind == "primitive_finished":
        args = ",".join(str(arg) for arg in event.get("args") or [])
        line = (
            f"primitive_finished {event.get('primitive', '?')}({args}) -> "
            f"{event.get('outcome', '?')} in {event.get('ticks', 0)} ticks"
        )
        if event.get("failure_kind"):
            line += f" [{event['failure_kind']}]"
        return line
    if kind == "skill_frame":
        line = (
            f"skill_frame {event.get('skill', '?')} depth={event.get('depth', '?')} -> "
            f"{event.get('outcome', '?')}"
        )
        if event.get("failure_kind"):
            line += f" [{event['failure_kind']}]"
        return line
    if kind == "rule_selected":
        return f"rule_selected {event.get('skill', '?')} rule={event.get('rule', '?')}"
    return f"{kind} {event}"


def _failure_kind(outcome: str, events: list[dict[str, Any]]) -> str | None:
    # The body reports a failed goal as `failed(<kind>)`.
    if outcome.startswith("failed(") and outcome.endswith(")"):
        return outcome[len("failed(") : -1] or "failed"
    for event in reversed(events):
        if event.get("kind") in ("primitive_finished", "skill_frame"):
            if event.get("outcome") == "failed" and event.get("failure_kind"):
                return str(event["failure_kind"])
    if outcome == "done":
        return None
    return outcome or "unknown"


def _candidate_skill_names(
    candidate_dir: Path | str, fallback: str
) -> tuple[str, ...]:
    """Every skill defined in the candidate directory, ``fallback`` if none."""
    names: list[str] = []
    directory = Path(candidate_dir)
    if directory.is_dir():
        for path in sorted(directory.glob("*.skill")):
            try:
                text = path.read_text(encoding="utf-8")
            except OSError:
                continue
            names.extend(
                skill.name for skill in parse_skill_text(text, path, "active")
            )
    return tuple(names) or (fallback,)


def _failing_frame(events: list[dict[str, Any]]) -> dict[str, Any] | None:
    """The innermost skill frame in the run's final failure chain.

    The interpreter logs a child's ``skill_frame`` before its caller's, so the
    chain sits at the tail of the run, outermost last. The deepest frame in it
    is the one whose failure originated.
    """
    chain: list[dict[str, Any]] = []
    for event in reversed(events):
        if event.get("kind") != "skill_frame" or event.get("outcome") != "failed":
            break
        chain.append(event)
    if not chain:
        return None
    return max(chain, key=lambda event: int(event.get("depth") or 0))


def _candidate_loop(
    events: list[dict[str, Any]], candidate_skills: Collection[str]
) -> bool:
    """True when the run's last 10 rule selections are candidate loops.

    Kept deliberately simple: all ten selections belong to candidate skills and
    no primitive was confirmed from the first of them to the end of the run.
    """
    indices = [
        index
        for index, event in enumerate(events)
        if event.get("kind") == "rule_selected"
    ]
    if len(indices) < LOOP_RULES:
        return False
    start = indices[-LOOP_RULES]
    tail = [events[index] for index in indices[-LOOP_RULES:]]
    if any(str(event.get("skill", "")) not in candidate_skills for event in tail):
        return False
    return not any(
        event.get("kind") == "primitive_finished"
        and event.get("outcome") == "confirmed"
        for event in events[start:]
    )


def classify_failure(
    events: list[dict[str, Any]],
    candidate_skills: Collection[str],
    outcome: str,
    failure_kind: str | None,
) -> FailureClass:
    """Attribute one failed practice run to the skill or to the body.

    Skill failures are fixable by the writer: the referee disagrees with a
    ``done``, a candidate frame failed with ``no_rule``, or a max_ticks/timeout
    run is a candidate loop. Everything else is a body gap (a primitive or core
    skill failed, a death, a body error, or max_ticks inside core skills).
    """
    if failure_kind == "goal_not_met":
        return "skill"

    frame = _failing_frame(events)
    if (
        frame is not None
        and str(frame.get("skill", "")) in candidate_skills
        and frame.get("failure_kind") == "no_rule"
    ):
        return "skill"

    if outcome in ("max_ticks", "timeout") and _candidate_loop(
        events, candidate_skills
    ):
        return "skill"

    return "body"


def _digest(events: list[dict[str, Any]], failure_kind: str | None) -> str:
    kept = [event for event in events if event.get("kind") in DIGEST_KINDS][-DIGEST_EVENTS:]
    lines = [f"failure kind: {failure_kind or 'none'}"]
    lines.extend(_event_line(event) for event in kept)
    return "\n".join(lines)


def practice(
    skill: str,
    goal: Goal,
    candidate_dir: Path | str,
    seed: int,
    max_ticks: int = 12_000,
    candidate_skills: Sequence[str] | None = None,
    cancel: CancelToken | None = None,
) -> PracticeResult:
    """Start one headless run for ``seed`` and judge it by the inventory.

    ``cancel`` is the admission's token: when it is set while this run is live,
    its server, forwarder and body are killed and the run stops being a verdict;
    a token already set starts no server at all.
    """
    if cancel is not None and cancel.is_set():
        raise AdmissionCancelled(f"practice run for seed {seed} was cancelled first")
    timeout = max_ticks / 20 * 1.6 + 120
    skills = (
        tuple(candidate_skills)
        if candidate_skills
        else _candidate_skill_names(candidate_dir, skill)
    )
    try:
        return _run(skill, goal, candidate_dir, seed, max_ticks, timeout, skills, cancel)
    except (RuntimeError, TimeoutError) as error:
        # The stack did not start or the body died without a summary: a typed
        # practice failure the writer can see, not a crash of the admission.
        return PracticeResult(
            passed=False,
            outcome="body_error",
            run_dir="",
            digest=f"failure kind: body_error\n{error}"[:2000],
            seed=seed,
            failure_kind="body_error",
            failure_class="body",
        )


def _run(
    skill: str,
    goal: Goal,
    candidate_dir: Path | str,
    seed: int,
    max_ticks: int,
    timeout: float,
    candidate_skills: Collection[str],
    cancel: CancelToken | None,
) -> PracticeResult:
    with start_body(
        seed,
        skill,
        setup=_peaceful,
        max_ticks=max_ticks,
        keep=True,
        extra_args=["--candidates", str(candidate_dir)],
    ) as run:
        # A shutdown that reaches the interpreter must not leave this stack
        # behind, and a cancel must kill it while it still runs.
        _register(run)
        stop = partial(_stop_run, run)
        if cancel is not None:
            # `add` runs at once when the token was set while the stack was
            # starting, so a cancel cannot slip between the check and this line.
            cancel.add(stop)
        try:
            timed_out = False
            try:
                summary = run.wait_for_outcome(timeout=timeout)
            except TimeoutError:
                timed_out = True
                summary = run.summary() or {}
            if cancel is not None and cancel.is_set():
                # The stack was just killed: its referee is gone and this run is
                # not a verdict. `admit` records the stage and ends the run.
                raise AdmissionCancelled(
                    f"the practice run for seed {seed} was cancelled"
                )
            inventory = run.referee.player_inventory()
            events = run.events()
            run_dir = run.run_dir
        finally:
            if cancel is not None:
                cancel.discard(stop)
            _unregister(run)

    outcome = "timeout" if timed_out else str(summary.get("goal", {}).get("outcome", "unknown"))
    have = _count(inventory, goal.item)
    passed = have >= goal.count
    failure_kind = None if outcome == "done" else _failure_kind(outcome, events)
    if failure_kind is None and not passed:
        # The skill said done but the referee disagrees: the repair must know.
        failure_kind = "goal_not_met"
    failure_class = (
        None
        if passed
        else classify_failure(events, candidate_skills, outcome, failure_kind)
    )
    referee = f"referee: {goal.item} {have}/{goal.count} (body outcome {outcome})"
    return PracticeResult(
        passed=passed,
        outcome=outcome,
        ticks=int(summary.get("ticks", 0)),
        receipts={str(key): int(value) for key, value in (summary.get("receipts") or {}).items()},
        deaths=int(summary.get("deaths", 0)),
        run_dir=str(run_dir),
        digest=_digest(events, failure_kind) + "\n" + referee,
        seed=seed,
        failure_kind=failure_kind,
        failure_class=failure_class,
        inventory={str(key): int(value) for key, value in inventory.items()},
    )


__all__ = [
    "DIGEST_EVENTS",
    "DIGEST_KINDS",
    "LOOP_RULES",
    "FailureClass",
    "PracticeResult",
    "classify_failure",
    "practice",
    "stop_live_runs",
]
