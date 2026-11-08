"""One practice run on the headless body, judged by the referee.

The brain starts the same server/forwarder/body stack as the smoke test, gives
the body nothing, passes the attempt's candidate directory and reads the
inventory from the referee. The run directory is kept as evidence.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

from pydantic import BaseModel, Field

from . import config  # noqa: F401  (adds harness/ to sys.path in one place)
from .goal import Goal
from praxion_harness.body import start_body

#: Event kinds kept in the repair digest.
DIGEST_KINDS = {"primitive_finished", "skill_frame", "rule_selected"}
DIGEST_EVENTS = 30


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
) -> PracticeResult:
    """Start one headless run for ``seed`` and judge it by the inventory."""
    timeout = max_ticks / 20 * 1.6 + 120
    try:
        return _run(skill, goal, candidate_dir, seed, max_ticks, timeout)
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
        )


def _run(
    skill: str,
    goal: Goal,
    candidate_dir: Path | str,
    seed: int,
    max_ticks: int,
    timeout: float,
) -> PracticeResult:
    with start_body(
        seed,
        skill,
        setup=_peaceful,
        max_ticks=max_ticks,
        keep=True,
        extra_args=["--candidates", str(candidate_dir)],
    ) as run:
        timed_out = False
        try:
            summary = run.wait_for_outcome(timeout=timeout)
        except TimeoutError:
            timed_out = True
            summary = run.summary() or {}
        inventory = run.referee.player_inventory()
        events = run.events()
        run_dir = run.run_dir

    outcome = "timeout" if timed_out else str(summary.get("goal", {}).get("outcome", "unknown"))
    have = _count(inventory, goal.item)
    passed = have >= goal.count
    failure_kind = None if outcome == "done" else _failure_kind(outcome, events)
    if failure_kind is None and not passed:
        # The skill said done but the referee disagrees: the repair must know.
        failure_kind = "goal_not_met"
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
        inventory={str(key): int(value) for key, value in inventory.items()},
    )


__all__ = ["DIGEST_EVENTS", "DIGEST_KINDS", "PracticeResult", "practice"]
