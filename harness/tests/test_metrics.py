"""Tests of the per-run metrics summarizer, on small synthetic evidence.

Every test builds its own ``brain.jsonl``, ``ledger.jsonl``, body
``events.jsonl`` and ``goals.jsonl`` under ``tmp_path``: no server, no network,
no real run directory. The synthetic lines keep the field names of the real
writers (``brain/praxion_brain/loop.py``, ``admission.py`` and the body's
``eventlog`` crate), so a field rename there should fail here.
"""

from __future__ import annotations

import json
import os
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Iterable, Optional

from praxion_harness.metrics import compare_table, main, render_markdown, summarize

#: The run under test and a neighbour run sharing the same brain evidence file.
WORLD = "test-body-0000000000000000000000000000aaaa"
OTHER = "test-body-1111111111111111111111111111bbbb"

#: Every synthetic timestamp is an offset from this moment.
BASE = datetime(2026, 11, 9, 6, 0, 0, tzinfo=timezone.utc)


def _at(seconds: float) -> str:
    """An ISO-8601 UTC timestamp ``seconds`` after :data:`BASE`."""
    return (BASE + timedelta(seconds=seconds)).isoformat()


def _epoch(seconds: float) -> float:
    """The world-memory clock of ``_at``: a POSIX epoch."""
    return (BASE + timedelta(seconds=seconds)).timestamp()


def _jsonl(path: Path, lines: Iterable[Any]) -> None:
    """Write lines as JSON, or verbatim when a line is already a string."""
    path.parent.mkdir(parents=True, exist_ok=True)
    text = "".join(
        (line if isinstance(line, str) else json.dumps(line)) + "\n" for line in lines
    )
    path.write_text(text, encoding="utf-8")


def _event(offset: float, kind: str, **fields: Any) -> dict:
    """One brain evidence line."""
    return {"timestamp": _at(offset), "kind": kind, **fields}


def _body(offset: float, tick: int, kind: str, **fields: Any) -> dict:
    """One body event line (the envelope is ``seq``, ``tick``, ``ts_ns``)."""
    return {
        "seq": tick,
        "tick": tick,
        "ts_ns": int(_epoch(offset) * 1_000_000_000),
        "kind": kind,
        **fields,
    }


def _goal(
    goal_id: str,
    *,
    condition: str,
    status: str,
    skill: str = "get_wood",
    failure_kind: Optional[str] = None,
    ticks: Optional[int] = None,
    start: float = 1.0,
    end: Optional[float] = None,
    command_id: Optional[str] = "c-0001",
    extra: Optional[dict] = None,
) -> dict:
    """One world-memory ``goals.jsonl`` line."""
    record: dict[str, Any] = {
        "goal_id": goal_id,
        "condition": condition,
        "skill": skill,
        "status": status,
        "failure_kind": failure_kind,
        "ticks": ticks,
        "command_id": command_id,
        "t_start": _epoch(start),
    }
    if end is not None:
        record["t_end"] = _epoch(end)
    if extra:
        record.update(extra)
    return record


def _build(
    tmp_path: Path,
    *,
    brain: Iterable[Any] = (),
    ledger: Optional[Iterable[Any]] = None,
    body: Optional[Iterable[Any]] = None,
    goals: Optional[Iterable[Any]] = None,
    world: str = WORLD,
) -> tuple[Path, Path, Path]:
    """Write the four evidence files; ``None`` leaves the file absent."""
    run_dir = tmp_path / "runs" / world
    brain_dir = tmp_path / "local" / "brain"
    worlds_dir = tmp_path / "local" / "worlds"
    run_dir.mkdir(parents=True)
    brain_dir.mkdir(parents=True)
    worlds_dir.mkdir(parents=True)
    _jsonl(brain_dir / "brain.jsonl", brain)
    if ledger is not None:
        _jsonl(brain_dir / "ledger.jsonl", ledger)
    if body is not None:
        _jsonl(run_dir / "body" / "events.jsonl", body)
    if goals is not None:
        _jsonl(worlds_dir / world / "goals.jsonl", goals)
    return run_dir, brain_dir, worlds_dir


def test_run_slice_ignores_the_other_runs_lines(tmp_path):
    brain = [
        _event(-100, "loop_started", world_id=OTHER),
        _event(-90, "goal_finished", goal_id="x-1", command_id="c-9", skill="get_wood",
               status="done", failure_kind=None, ticks=5, rungs_met=1),
        # The run under test: 20 seconds long.
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "command_sent", goal_id="g-0001", command_id="c-1", skill="get_wood",
               condition="has(#logs)", budget_ticks=6000),
        _event(20, "goal_finished", goal_id="g-0001", command_id="c-1", skill="get_wood",
               status="done", failure_kind=None, ticks=200, rungs_met=1),
        # The next run: its lines must not leak into this one.
        _event(30, "loop_started", world_id=OTHER),
        _event(40, "goal_finished", goal_id="x-2", command_id="c-10", skill="get_wood",
               status="failed", failure_kind="unconfirmed", ticks=9, rungs_met=3),
    ]
    goals = [
        _goal("g-0001", condition="has(#logs)", status="done", ticks=200, start=1, end=20),
        # A goal of another run that once used this world directory.
        _goal("x-1", condition="has(#logs)", status="done", ticks=5, start=-200, end=-190),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=goals)

    summary = summarize(run_dir, brain_dir, worlds_dir)

    assert summary["run"]["world_id"] == WORLD
    assert summary["run"]["brain_start"] == _at(0)
    assert summary["run"]["brain_end"] == _at(20)
    assert summary["run"]["wall_seconds"] == 20.0
    assert summary["run"]["brain_segments"] == 1
    assert summary["run"]["brain_events"] == 3
    # The other run's failing goal must not reach the rungs or the streak.
    assert summary["progress"]["rungs_met_max"] == 1
    assert summary["progress"]["goals_total"] == 1
    assert summary["progress"]["goals_by_status"] == {"done": 1}
    assert summary["progress"]["goals"][0]["goal_id"] == "g-0001"
    assert summary["stuck"]["longest_failed_goal_streak"] == 0


def test_restart_in_the_same_world_is_one_run_of_two_slices(tmp_path):
    """A brain restart writes a second loop_started: both slices are the run."""
    brain = [
        # First slice: 0 → 40 s.
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "command_sent", goal_id="g-1", command_id="c-1", skill="get_wood",
               condition="has(#logs)"),
        _event(20, "goal_finished", goal_id="g-1", command_id="c-1", skill="get_wood",
               status="done", failure_kind=None, ticks=200, rungs_met=1),
        _event(30, "command_sent", goal_id="g-2", command_id="c-2", skill="get_wood",
               condition="has(#logs)"),
        _event(40, "goal_finished", goal_id="g-2", command_id="c-2", skill="get_wood",
               status="done", failure_kind=None, ticks=90, rungs_met=2),
        # Another world while this one was down: not this run's evidence.
        _event(60, "loop_started", world_id=OTHER),
        _event(70, "goal_finished", goal_id="x-1", command_id="c-9", skill="get_wood",
               status="failed", failure_kind="unconfirmed", ticks=9, rungs_met=9),
        _event(90, "command_sent", goal_id="x-2", command_id="c-10", skill="get_wood",
               condition="has(#logs)"),
        # Second slice: 100 → 130 s, so the run is 70 s long, not 130.
        _event(100, "loop_started", world_id=WORLD),
        _event(105, "command_sent", goal_id="g-3", command_id="c-3", skill="get_planks",
               condition="has(#planks)"),
        _event(110, "goal_finished", goal_id="g-3", command_id="c-3", skill="get_planks",
               status="done", failure_kind=None, ticks=80, rungs_met=3),
        _event(130, "command_sent", goal_id="g-4", command_id="c-4", skill="get_planks",
               condition="has(#planks)"),
    ]
    goals = [
        _goal("g-1", condition="has(#logs)", status="done", ticks=200, start=1, end=20),
        _goal("g-2", condition="has(#logs)", status="done", ticks=90, start=30, end=40),
        # Opened while this world was down: the restart does not span it.
        _goal("x-1", condition="has(#logs)", status="failed", ticks=9, start=70, end=80),
        _goal("g-3", condition="has(#planks)", status="done", ticks=80, start=105, end=110),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=goals)

    summary = summarize(run_dir, brain_dir, worlds_dir)
    run, stuck, progress = summary["run"], summary["stuck"], summary["progress"]

    assert run["brain_segments"] == 2
    assert run["brain_start"] == _at(0) and run["brain_end"] == _at(130)
    assert run["wall_seconds"] == 70.0
    # The other world's three lines are in neither slice.
    assert run["brain_events"] == 9
    # Idle per slice: 10 s + 10 s, then 5 s + 20 s. The 60 s a restart kept the
    # brain down is not idle time (and was a 65 s gap before the slices split).
    assert stuck["idle_seconds"] == 45.0
    assert stuck["longest_idle_seconds"] == 20.0
    assert stuck["idle_fraction"] == 0.6429
    # Rungs continue across the restart, on the run's own clock: 40 s + 10 s.
    assert progress["rungs_met_max"] == 3
    assert [point["wall_seconds_from_start"] for point in progress["rung_timeline"]] == [
        20.0,
        40.0,
        50.0,
    ]
    # The goal opened while the brain was down belongs to no slice.
    assert progress["goals_total"] == 3
    assert [goal["goal_id"] for goal in progress["goals"]] == ["g-1", "g-2", "g-3"]
    assert stuck["longest_failed_goal_streak"] == 0


def test_idle_time_arithmetic(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "command_sent", goal_id="g-1", command_id="c-1", skill="get_wood",
               condition="has(#logs)"),
        _event(20, "goal_finished", goal_id="g-1", command_id="c-1", skill="get_wood",
               status="done", failure_kind=None, ticks=200, rungs_met=1),
        _event(30, "command_sent", goal_id="g-2", command_id="c-2", skill="get_planks",
               condition="has(#planks)"),
        _event(34, "goal_finished", goal_id="g-2", command_id="c-2", skill="get_planks",
               status="done", failure_kind=None, ticks=80, rungs_met=1),
        _event(40, "command_sent", goal_id="g-3", command_id="c-3", skill="get_wood",
               condition="has(#logs)"),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=[])

    stuck = summarize(run_dir, brain_dir, worlds_dir)["stuck"]

    # 10 s start-to-first-command, 10 s pick, 6 s pick; 26 s of 40 s.
    assert stuck["idle_seconds"] == 26.0
    assert stuck["longest_idle_seconds"] == 10.0
    assert stuck["idle_fraction"] == 0.65


def test_write_with_two_attempts_then_admission(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(5, "write_started", goal_id="g-1", skill="get_furnace",
               condition="has(furnace)", writer_model="gpt-6-luna"),
        _event(6, "admission_attempt", attempt=0, model="gpt-6-luna", candidate_path="/x/0"),
        _event(7, "admission_attempt", attempt=1, model="gpt-6.1-terra", candidate_path="/x/1"),
        _event(8, "admission_practice", attempt=0, seed=101, passed=True, outcome="done"),
        _event(9, "admission_body_failure", attempt=0, seed=404, failure_kind="needs_table"),
        _event(
            10,
            "admission_report",
            # A legacy report without ``report.goal``: nothing names its goal, so
            # the write can only bind it by position.
            report={
                "status": "admitted",
                "skill_name": "get_furnace",
                "attempts": [],
                "reason": None,
            },
        ),
        _event(12, "goal_finished", goal_id="g-2", command_id="c-5", skill="get_furnace",
               status="done", failure_kind=None, ticks=42, rungs_met=1),
    ]
    ledger = [
        {"timestamp": _at(6), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 10, "output_tokens": 10, "cost": 0.0002, "purpose": "write_skill"},
        {"timestamp": _at(8), "provider": "openai", "model": "gpt-6.1-terra",
         "input_tokens": 10, "output_tokens": 10, "cost": 0.0003, "purpose": "repair_skill"},
        {"timestamp": _at(8.5), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 10, "output_tokens": 10, "cost": 0.5, "purpose": "propose_goal"},
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=ledger, goals=[])

    writing = summarize(run_dir, brain_dir, worlds_dir)["skill_writing"]

    assert writing["writes_started"] == 1
    assert writing["writes_admitted"] == 1
    assert writing["median_seconds_to_admit"] == 5.0
    write = writing["writes"][0]
    assert write["skill"] == "get_furnace"
    assert write["condition"] == "has(furnace)"
    assert write["writer_model"] == "gpt-6-luna"
    assert write["outcome"] == "admitted"
    assert write["attempts"] == 2
    assert write["models"] == ["gpt-6-luna", "gpt-6.1-terra"]
    assert write["practice_runs"] == 1
    assert write["body_failures"] == 1
    assert write["seconds_to_outcome"] == 5.0
    # Only the two writing purposes are billed to the write.
    assert write["cost_usd"] == 0.0005
    assert writing["total_write_cost_usd"] == 0.0005
    # The write's own goal was never played; this play of the skill came later.
    assert write["first_use_after_admission"] == {
        "goal_id": "g-2",
        "status": "done",
        "ticks": 42,
        "same_goal": False,
    }


def test_admission_report_binds_by_its_goal(tmp_path):
    """The CLI's ``learn`` writes a report for another goal: never bind it."""
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(5, "write_started", goal_id="g-1", skill="get_furnace",
               condition="has(furnace)", writer_model="gpt-6-luna"),
        # A report for another goal, written into the same file meanwhile.
        _event(10, "admission_report",
               report={"goal": {"text": "Find coal.", "item": "coal", "count": 1},
                       "status": "admitted", "skill_name": "get_coal", "attempts": []}),
        _event(20, "admission_report",
               report={"goal": {"text": "Get a furnace.", "item": "furnace", "count": 1},
                       "status": "rejected", "skill_name": "get_furnace",
                       "attempts": [], "reason": "needs_table"}),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=[])

    writing = summarize(run_dir, brain_dir, worlds_dir)["skill_writing"]

    write = writing["writes"][0]
    assert write["outcome"] == "rejected"
    assert write["seconds_to_outcome"] == 15.0
    assert writing["writes_admitted"] == 0


def test_write_terminal_binds_by_goal_id_not_position(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(5, "write_started", goal_id="g-1", skill="get_furnace",
               condition="has(furnace)", writer_model="gpt-6-luna"),
        _event(6, "admission_attempt", attempt=0, model="gpt-6-luna"),
        # The CLI's own admission, run against the same brain.jsonl meanwhile.
        _event(7, "admission_report",
               report={"goal": {"text": "Find coal.", "item": "coal", "count": 1},
                       "status": "admitted", "skill_name": "get_coal", "attempts": []}),
        _event(8, "admission_practice", attempt=0, seed=1, passed=True, outcome="done"),
        _event(9, "admission_attempt", attempt=1, model="gpt-6.1-terra"),
        _event(12, "write_stopped", goal_id="g-1", skill="get_furnace",
               admission_status="rejected", error=None),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=[])

    write = summarize(run_dir, brain_dir, worlds_dir)["skill_writing"]["writes"][0]

    assert write["outcome"] == "rejected"
    assert write["seconds_to_outcome"] == 7.0
    assert write["attempts"] == 2
    assert write["models"] == ["gpt-6-luna", "gpt-6.1-terra"]
    assert write["practice_runs"] == 1


def test_write_ignores_a_report_for_another_goal(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(5, "write_started", goal_id="g-1", skill="get_furnace",
               condition="has(furnace)", writer_model="gpt-6-luna"),
        _event(10, "admission_report",
               report={"goal": {"text": "Find coal.", "item": "coal", "count": 1},
                       "status": "admitted", "skill_name": "get_coal", "attempts": []}),
        _event(20, "write_started", goal_id="g-2", skill="get_coal",
               condition="has(coal)", writer_model="gpt-6-luna"),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=[])

    writing = summarize(run_dir, brain_dir, worlds_dir)["skill_writing"]

    assert writing["writes_started"] == 2
    assert [write["outcome"] for write in writing["writes"]] == ["unfinished", "unfinished"]
    assert writing["writes_admitted"] == 0


def test_unknown_write_cost_makes_the_total_unknown(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        # No usable timestamp: this write's bill cannot be measured.
        {"timestamp": "not a time", "kind": "write_started", "goal_id": "g-1",
         "skill": "get_coal", "condition": "has(coal)", "writer_model": "gpt-6-luna"},
        _event(10, "admission_report",
               report={"goal": {"text": "Find coal.", "item": "coal", "count": 1},
                       "status": "rejected", "skill_name": "get_coal", "attempts": []}),
        _event(20, "write_started", goal_id="g-2", skill="get_coal",
               condition="has(coal)", writer_model="gpt-6-luna"),
        _event(25, "admission_report",
               report={"goal": {"text": "Find coal.", "item": "coal", "count": 1},
                       "status": "admitted", "skill_name": "get_coal", "attempts": []}),
    ]
    ledger = [
        {"timestamp": _at(5), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 10, "output_tokens": 10, "cost": 0.0002, "purpose": "write_skill"},
        {"timestamp": _at(22), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 10, "output_tokens": 10, "cost": 0.0003, "purpose": "write_skill"},
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=ledger, goals=[])

    writing = summarize(run_dir, brain_dir, worlds_dir)["skill_writing"]

    assert writing["writes"][0]["cost_usd"] is None
    assert writing["writes"][0]["seconds_to_outcome"] is None
    assert writing["writes"][1]["cost_usd"] == 0.0003
    # One unknown bill makes the total unknown; summing it as zero would lie.
    assert writing["total_write_cost_usd"] is None


def test_first_use_prefers_the_writes_own_goal(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(5, "write_started", goal_id="g-1", skill="get_furnace",
               condition="has(furnace)", writer_model="gpt-6-luna"),
        _event(10, "admission_report",
               report={"goal": {"text": "Get a furnace.", "item": "furnace", "count": 1},
                       "status": "admitted", "skill_name": "get_furnace", "attempts": []}),
        # The skill is borrowed for another goal first, the write's goal later.
        _event(20, "goal_finished", goal_id="g-2", command_id="c-2", skill="get_furnace",
               status="done", failure_kind=None, ticks=7, rungs_met=1),
        _event(30, "goal_finished", goal_id="g-1", command_id="c-3", skill="get_furnace",
               status="done", failure_kind=None, ticks=42, rungs_met=1),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=[])

    write = summarize(run_dir, brain_dir, worlds_dir)["skill_writing"]["writes"][0]

    assert write["first_use_after_admission"] == {
        "goal_id": "g-1",
        "status": "done",
        "ticks": 42,
        "same_goal": True,
    }


def test_unfinished_write_has_no_outcome(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(5, "write_started", goal_id="g-1", skill="get_coal",
               condition="has(coal)", writer_model="gpt-6-luna"),
        _event(6, "admission_attempt", attempt=0, model="gpt-6-luna"),
        _event(20, "loop_wake", cause="heartbeat", state="idle"),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=[])

    write = summarize(run_dir, brain_dir, worlds_dir)["skill_writing"]["writes"][0]

    assert write["outcome"] == "unfinished"
    assert write["seconds_to_outcome"] is None
    assert write["first_use_after_admission"] is None
    assert write["attempts"] == 1


def test_repeated_and_redone_conditions(tmp_path):
    goals = [
        _goal("g-1", condition="count(#logs) >= 4", status="failed",
              failure_kind="unconfirmed", ticks=10, start=1, end=5),
        _goal("g-2", condition="count(#logs) >= 4", status="failed",
              failure_kind="unconfirmed", ticks=11, start=6, end=9),
        _goal("g-3", condition="count(#planks) >= 4", status="done",
              ticks=12, start=10, end=12),
        _goal("g-4", condition="count(#planks) >= 4", status="done",
              ticks=13, start=13, end=14),
        _goal("g-5", condition="has(furnace)", status="failed",
              failure_kind="inconclusive_body", ticks=None, start=15,
              command_id=None, skill="get_furnace"),
    ]
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(20, "loop_wake", cause="heartbeat", state="idle"),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=goals)

    summary = summarize(run_dir, brain_dir, worlds_dir)

    assert summary["stuck"]["repeated_failed_conditions"] == {"count(#logs) >= 4": 2}
    assert summary["sense"]["redone_goals"] == {"count(#planks) >= 4": 1}
    assert summary["progress"]["goals_total"] == 5
    assert summary["progress"]["goals_by_status"] == {"done": 2, "failed": 3}
    # A goal with only a start has no wall time rather than a zero.
    assert summary["progress"]["goals"][4]["wall_seconds"] is None


def test_primitive_failure_counting(tmp_path):
    body = [
        _body(0, 0, "run_started", versions={"praxion": "dev", "minecraft": "26.3"}),
        _body(1, 10, "primitive_finished", primitive="mine", args=["#logs"],
              outcome="failed", failure_kind="no_block", ticks=10),
        _body(2, 20, "primitive_finished", primitive="mine", args=["#logs"],
              outcome="failed", failure_kind="no_block", ticks=20),
        _body(3, 30, "primitive_finished", primitive="mine", args=["#logs"],
              outcome="confirmed", failure_kind=None, ticks=30),
        _body(4, 40, "primitive_finished", primitive="explore", args=["#logs"],
              outcome="failed", failure_kind="not_found", ticks=5),
        _body(5, 50, "receipt_settled", receipt_id=0, intent_id=1, outcome="confirmed"),
        _body(6, 60, "receipt_settled", receipt_id=1, intent_id=2, outcome="unconfirmed"),
        _body(7, 70, "owner_changed", generation=2, owner="skill:1"),
        _body(8, 80, "owner_changed", generation=3, owner="reflex:melee"),
        _body(9, 90, "owner_changed", generation=4, owner="idle"),
        _body(10, 100, "late_evidence", receipt_id=1, evidence_kind="block_air"),
        _body(11, 110, "died", message=None),
    ]
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(20, "loop_wake", cause="heartbeat", state="idle"),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], body=body, goals=[])

    summary = summarize(brain_dir=brain_dir, worlds_dir=worlds_dir, run_dir=run_dir)
    stuck = summary["stuck"]
    sense = summary["sense"]

    assert summary["run"]["body_ticks"] == 110
    assert summary["run"]["body_events"] == 12
    assert summary["run"]["versions"] == {"praxion": "dev", "minecraft": "26.3"}
    assert stuck["primitive_failures"] == {"explore:not_found": 1, "mine:no_block": 2}
    assert stuck["failed_primitive_ticks"] == 35
    assert stuck["total_primitive_ticks"] == 65
    assert stuck["wasted_tick_fraction"] == 0.5385
    assert stuck["explore_not_found"] == {"count": 1, "ticks": 5}
    assert sense["receipts_by_outcome"] == {"confirmed": 1, "unconfirmed": 1}
    # Only a reflex taking the body is a preemption; skill and idle ownership are not.
    assert sense["reflex_preemptions"] == 1
    assert sense["late_evidence"] == 1
    assert sense["deaths"] == 1


def test_absent_body_log_gives_nulls_and_a_missing_entry(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "goal_finished", goal_id="g-1", command_id="c-1", skill="get_wood",
               status="done", failure_kind=None, ticks=200, rungs_met=1),
    ]
    goals = [_goal("g-1", condition="has(#logs)", status="done", ticks=200, start=1, end=10)]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=goals)

    summary = summarize(run_dir, brain_dir, worlds_dir)
    run, stuck, sense = summary["run"], summary["stuck"], summary["sense"]

    assert run["body_ticks"] is None
    assert run["body_events"] is None
    assert run["versions"] is None
    assert stuck["primitive_failures"] is None
    assert stuck["failed_primitive_ticks"] is None
    assert stuck["wasted_tick_fraction"] is None
    assert stuck["explore_not_found"] is None
    assert sense["receipts_by_outcome"] is None
    assert sense["reflex_preemptions"] is None
    assert sense["deaths"] is None
    missing = summary["health"]["missing_files"]
    assert len(missing) == 1 and missing[0].endswith("events.jsonl")


def test_absent_world_memory_gives_nulls(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "loop_wake", cause="heartbeat", state="idle"),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=None)

    summary = summarize(run_dir, brain_dir, worlds_dir)

    assert summary["progress"]["goals_total"] is None
    assert summary["progress"]["goals_by_status"] is None
    assert summary["progress"]["goals"] is None
    assert summary["stuck"]["repeated_failed_conditions"] is None
    assert summary["sense"]["redone_goals"] is None
    assert any(name.endswith("goals.jsonl") for name in summary["health"]["missing_files"])


def test_corrupt_lines_are_counted(tmp_path):
    brain = [
        "not json at all",
        _event(0, "loop_started", world_id=WORLD),
        "{broken inside the run",
        _event(10, "loop_wake", cause="heartbeat", state="idle"),
        _event(20, "loop_started", world_id=OTHER),
        # The next run's corrupt line is not this run's.
        "also broken, but elsewhere",
    ]
    body = [
        _body(0, 0, "run_started", versions={"praxion": "dev"}),
        "truncated body line",
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], body=body, goals=[])

    summary = summarize(run_dir, brain_dir, worlds_dir)

    # One corrupt brain line inside the slice plus one corrupt body line.
    assert summary["health"]["corrupt_lines"] == 2
    assert summary["run"]["brain_events"] == 2
    assert summary["run"]["body_events"] == 1


def test_corrupt_goals_lines_are_counted(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "loop_wake", cause="heartbeat", state="idle"),
    ]
    goals = [
        "{broken world memory",
        _goal("g-1", condition="has(#logs)", status="done", ticks=5, start=1, end=10),
        "also broken",
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=goals)

    summary = summarize(run_dir, brain_dir, worlds_dir)

    assert summary["health"]["corrupt_lines"] == 2
    assert summary["progress"]["goals_total"] == 1


def test_ledger_lines_without_a_time_or_cost_are_counted_apart(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(20, "loop_wake", cause="heartbeat", state="idle"),
    ]
    ledger = [
        {"timestamp": _at(5), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 1, "output_tokens": 1, "cost": 0.5, "purpose": "propose_goal"},
        # No cost at all.
        {"timestamp": _at(6), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 1, "output_tokens": 1, "purpose": "route"},
        # No usable timestamp.
        {"timestamp": "yesterday", "provider": "typesafe", "model": "jev-latest",
         "input_tokens": 1, "output_tokens": 0, "cost": 0.25, "purpose": "route"},
        "{truncated ledger line",
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=ledger, goals=[])

    summary = summarize(run_dir, brain_dir, worlds_dir)
    health, cost = summary["health"], summary["cost"]

    assert health["ledger_unattributed_lines"] == 2
    assert health["ledger_corrupt_lines"] == 1
    # A shared ledger's corruption is not this run's corrupt evidence.
    assert health["corrupt_lines"] == 0
    assert cost["total_usd"] == 0.5


def test_console_managed_world_reads_its_newest_bot_log(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(20, "loop_wake", cause="heartbeat", state="idle"),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=[], body=None)
    first = run_dir / "bots" / "attempt-1-aaaa" / "body" / "events.jsonl"
    second = run_dir / "bots" / "attempt-2-bbbb" / "body" / "events.jsonl"
    _jsonl(first, [_body(0, 5, "run_started", versions={"praxion": "first"})])
    _jsonl(
        second,
        [
            _body(0, 7, "run_started", versions={"praxion": "second"}),
            _body(1, 70, "died", message=None),
        ],
    )
    os.utime(first, (1_000, 1_000))
    os.utime(second, (2_000, 2_000))

    summary = summarize(run_dir, brain_dir, worlds_dir)
    run, sense = summary["run"], summary["sense"]

    assert run["body_log"] == "bots/attempt-2-bbbb/body/events.jsonl"
    assert run["body_ticks"] == 70
    assert run["body_events"] == 2
    assert run["versions"] == {"praxion": "second"}
    assert sense["deaths"] == 1
    assert summary["health"]["missing_files"] == []

    # A direct body log, when the run has one, is the one it used.
    _jsonl(run_dir / "body" / "events.jsonl", [_body(0, 1, "run_started")])
    again = summarize(run_dir, brain_dir, worlds_dir)
    assert again["run"]["body_log"] == "body/events.jsonl"
    assert again["run"]["body_ticks"] == 1


def test_negative_durations_are_null_and_counted(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "write_started", goal_id="g-1", skill="get_coal",
               condition="has(coal)", writer_model="gpt-6-luna"),
        # The terminal's timestamp is before the start: the clock went back.
        _event(5, "write_timeout", goal_id="g-1", skill="get_coal",
               call="writer", seconds=30),
        _event(20, "goal_finished", goal_id="g-2", command_id="c-2", skill="get_wood",
               status="done", failure_kind=None, ticks=10, rungs_met=1),
        _event(15, "command_sent", goal_id="g-3", command_id="c-3", skill="get_wood",
               condition="has(#logs)"),
        _event(30, "loop_wake", cause="heartbeat", state="idle"),
    ]
    goals = [
        # A world-memory record whose end is before its start.
        _goal("g-2", condition="has(#logs)", status="done", ticks=10, start=20, end=10),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=goals)

    summary = summarize(run_dir, brain_dir, worlds_dir)

    write = summary["skill_writing"]["writes"][0]
    assert write["seconds_to_outcome"] is None
    assert write["cost_usd"] is None
    assert summary["progress"]["goals"][0]["wall_seconds"] is None
    # A gap closed by an earlier timestamp is refused, not clamped to zero.
    assert summary["stuck"]["longest_idle_seconds"] is None
    assert summary["health"]["clock_regressions"] == 3


def test_ledger_window_excludes_lines_outside_the_run(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(20, "loop_wake", cause="heartbeat", state="idle"),
    ]
    ledger = [
        {"timestamp": _at(-50), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 1, "output_tokens": 1, "cost": 1.0, "purpose": "propose_goal"},
        {"timestamp": _at(5), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 1, "output_tokens": 1, "cost": 0.5, "purpose": "propose_goal"},
        {"timestamp": _at(15), "provider": "typesafe", "model": "jev-latest",
         "input_tokens": 1, "output_tokens": 0, "cost": 0.25, "purpose": "route"},
        {"timestamp": _at(500), "provider": "openai", "model": "gpt-6-luna",
         "input_tokens": 1, "output_tokens": 1, "cost": 9.0, "purpose": "propose_goal"},
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=ledger, goals=[])

    cost = summarize(run_dir, brain_dir, worlds_dir)["cost"]

    assert cost["total_usd"] == 0.75
    assert cost["by_provider"] == {"openai": 0.5, "typesafe": 0.25}
    assert cost["by_purpose"] == {"propose_goal": 0.5, "route": 0.25}
    # No rung was reached, so the per-rung price is unknown, not zero.
    assert cost["usd_per_rung"] is None


def test_absent_ledger_gives_null_cost(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "loop_wake", cause="heartbeat", state="idle"),
    ]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=None, goals=[])

    summary = summarize(run_dir, brain_dir, worlds_dir)

    assert summary["cost"]["total_usd"] is None
    assert summary["cost"]["by_provider"] is None
    assert summary["skill_writing"]["total_write_cost_usd"] is None
    assert any(name.endswith("ledger.jsonl") for name in summary["health"]["missing_files"])


def test_render_and_compare_tables(tmp_path):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "command_sent", goal_id="g-1", command_id="c-1", skill="get_wood",
               condition="has(#logs)"),
        _event(20, "goal_finished", goal_id="g-1", command_id="c-1", skill="get_wood",
               status="done", failure_kind=None, ticks=200, rungs_met=1),
    ]
    goals = [_goal("g-1", condition="has(#logs)", status="done", ticks=200, start=1, end=20)]
    run_dir, brain_dir, worlds_dir = _build(
        tmp_path, brain=brain, ledger=[], goals=goals, body=None
    )

    summary = summarize(run_dir, brain_dir, worlds_dir)
    report = render_markdown(summary)
    table = compare_table([summary, summary])

    sections = ("## Progress", "## Skill writing", "## Stuck", "## Sense", "## Cost", "## Health")
    for section in sections:
        assert section in report
    # A number the data cannot answer is an em dash, never a zero.
    assert "deaths —" in report
    assert "dropped notices: —" in report
    rows = [line for line in table.splitlines() if line.startswith("| `")]
    assert len(rows) == 2
    assert WORLD in rows[0]


def test_cli_prints_a_report_and_writes_it(tmp_path, capsys):
    brain = [
        _event(0, "loop_started", world_id=WORLD),
        _event(10, "command_sent", goal_id="g-1", command_id="c-1", skill="get_wood",
               condition="has(#logs)"),
        _event(20, "goal_finished", goal_id="g-1", command_id="c-1", skill="get_wood",
               status="done", failure_kind=None, ticks=200, rungs_met=1),
        _event(30, "loop_started", world_id=OTHER),
        _event(40, "loop_wake", cause="heartbeat", state="idle"),
    ]
    goals = [_goal("g-1", condition="has(#logs)", status="done", ticks=200, start=1, end=20)]
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=brain, ledger=[], goals=goals)
    other_dir = tmp_path / "runs" / OTHER
    other_dir.mkdir(parents=True)
    dirs = ["--brain-dir", str(brain_dir), "--worlds-dir", str(worlds_dir)]

    # One run: the markdown report, and --write leaves both files in the run dir.
    assert main([str(run_dir), *dirs, "--write"]) == 0
    printed = capsys.readouterr().out
    assert printed.startswith(f"# Run `{WORLD}`")
    written = json.loads((run_dir / "metrics.json").read_text(encoding="utf-8"))
    assert written["run"]["world_id"] == WORLD
    assert "## Skill writing" in (run_dir / "metrics.md").read_text(encoding="utf-8")

    # Several runs: the compare table, without writing anything.
    assert main([str(run_dir), str(other_dir), *dirs]) == 0
    table = capsys.readouterr().out
    rows = [line for line in table.splitlines() if line.startswith("| `")]
    assert len(rows) == 2
    assert WORLD in rows[0] and OTHER in rows[1]
    assert not (other_dir / "metrics.json").exists()


def test_missing_run_dir_is_refused(tmp_path, capsys):
    run_dir, brain_dir, worlds_dir = _build(tmp_path, brain=[], ledger=[], goals=[])

    code = main([str(tmp_path / "runs" / "nope"), "--brain-dir", str(brain_dir),
                 "--worlds-dir", str(worlds_dir)])

    assert code == 2
    assert "not a directory" in capsys.readouterr().err
    assert run_dir.is_dir()
