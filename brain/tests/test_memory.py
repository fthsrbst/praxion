from __future__ import annotations

import json
import logging

import pytest

from praxion_brain.memory import (
    BLOCK_SECONDS,
    DRIFT_GOALS,
    GoalRecord,
    Memory,
    SkillUse,
    normalize_condition,
    world_dir,
)


class FakeClock:
    def __init__(self, now: float = 0.0) -> None:
        self.now = now

    def __call__(self) -> float:
        return self.now


class FakeEvidence:
    def __init__(self) -> None:
        self.entries: list[dict] = []

    def write(self, kind: str, **fields) -> dict:
        entry = {"kind": kind, **fields}
        self.entries.append(entry)
        return entry


def goal(goal_id: str, **fields) -> GoalRecord:
    base = {
        "goal_id": goal_id,
        "text": f"goal {goal_id}",
        "condition": "has(furnace)",
    }
    base.update(fields)
    return GoalRecord(**base)


def failed(goal_id: str, condition: str, at: float, kind: str = "not_found") -> GoalRecord:
    return goal(
        goal_id,
        condition=condition,
        status="failed",
        failure_kind=kind,
        t_start=at - 10,
        t_end=at,
    )


def test_append_and_reload_both_stores(tmp_path) -> None:
    memory = Memory("world-1", root=tmp_path, clock=FakeClock(50))
    memory.append_goal(goal("g1", status="done", skill="get_furnace"))
    memory.append_skill_use(
        SkillUse(skill="get_furnace", goal_id="g1", outcome="done", ticks=120)
    )

    assert (tmp_path / "world-1" / "goals.jsonl").is_file()
    assert (tmp_path / "world-1" / "skill_uses.jsonl").is_file()

    reopened = Memory("world-1", root=tmp_path, clock=FakeClock(60))
    records = reopened.goals()
    assert len(records) == 1
    assert records[0].status == "done"
    assert records[0].t_start == 50
    assert records[0].t_end == 50
    uses = reopened.skill_uses()
    assert uses[0].skill == "get_furnace"
    assert uses[0].t == 50


def test_last_line_wins_per_goal_id(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    memory.append_goal(goal("g1", status="open"))
    memory.append_goal(goal("g2", status="open"))
    memory.append_goal(goal("g1", status="done"))

    records = memory.goals()
    assert [record.goal_id for record in records] == ["g2", "g1"]
    assert records[-1].status == "done"
    assert memory.recent(1)[0].goal_id == "g1"


def test_corrupt_last_line_is_skipped_and_logged(tmp_path, caplog) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    memory.append_goal(goal("g1", status="done"))
    with memory.goals_path.open("a", encoding="utf-8") as handle:
        handle.write("{not json}\n")

    with caplog.at_level(logging.WARNING):
        records = memory.goals()

    assert [record.goal_id for record in records] == ["g1"]
    assert "corrupt line" in caplog.text


def test_two_failures_block_the_goal_for_two_hours(tmp_path) -> None:
    clock = FakeClock(300)
    memory = Memory("w", root=tmp_path, clock=clock)
    memory.append_goal(failed("g1", "has(coal)", at=100))
    memory.append_goal(failed("g2", "has(coal)", at=200, kind="needs_tool"))

    entry = memory.failures()["has(coal)"]
    assert entry.attempts == 2
    assert entry.kinds == {"not_found": 1, "needs_tool": 1}
    assert entry.blocked_until == pytest.approx(200 + BLOCK_SECONDS)
    assert memory.is_blocked("has(coal)") is True
    assert set(memory.blocked()) == {"has(coal)"}

    assert memory.is_blocked("has(coal)", now=200 + BLOCK_SECONDS) is False
    assert memory.blocked(now=200 + BLOCK_SECONDS) == {}


def test_a_later_failure_after_expiry_starts_a_fresh_block(tmp_path) -> None:
    clock = FakeClock(200 + BLOCK_SECONDS + 1)
    memory = Memory("w", root=tmp_path, clock=clock)
    memory.append_goal(failed("g1", "has(coal)", at=100))
    memory.append_goal(failed("g2", "has(coal)", at=200))
    memory.append_goal(failed("g3", "has(coal)", at=200 + BLOCK_SECONDS + 1))

    entry = memory.failures()["has(coal)"]
    assert entry.attempts == 3
    assert entry.blocked_until == pytest.approx(200 + 2 * BLOCK_SECONDS + 1)
    assert memory.is_blocked("has(coal)") is True


def test_failure_keys_use_the_canonical_parsed_condition(tmp_path) -> None:
    clock = FakeClock(300)
    memory = Memory("w", root=tmp_path, clock=clock)
    memory.append_goal(failed("g1", "count(minecraft:cobblestone)>=8", at=100))
    memory.append_goal(failed("g2", "count(cobblestone) >= 8", at=200))

    key = "count(cobblestone) >= 8"
    assert normalize_condition("count(minecraft:cobblestone)>=8") == key
    entry = memory.failures()[key]
    assert entry.attempts == 2
    assert memory.is_blocked("count(cobblestone)>=8") is True
    assert memory.is_blocked("count( minecraft:cobblestone ) >= 8") is True
    # A tag is a different fact even when the tag name looks like the item.
    assert memory.is_blocked("count(#minecraft:cobblestone) >= 8") is False


def test_one_failure_does_not_block(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock(1000))
    memory.append_goal(failed("g1", "has(coal)", at=900))

    entry = memory.failures()["has(coal)"]
    assert entry.attempts == 1
    assert entry.blocked_until is None
    assert memory.is_blocked("has(coal)") is False


def test_failure_counts_come_from_skill_uses(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    for index, kind in enumerate(["not_found", "not_found", "budget"]):
        memory.append_skill_use(
            SkillUse(
                skill="get_coal",
                goal_id=f"g{index}",
                outcome="failed",
                failure_kind=kind,
            )
        )
    memory.append_skill_use(
        SkillUse(skill="get_coal", goal_id="g9", outcome="done")
    )
    memory.append_skill_use(
        SkillUse(skill="get_wood", goal_id="g10", outcome="failed", failure_kind="not_found")
    )

    assert memory.failure_counts("get_coal") == {"not_found": 2, "budget": 1}
    assert memory.failure_counts("get_wood") == {"not_found": 1}
    assert memory.failure_counts("unknown") == {}


def test_five_goals_without_rung_progress_write_one_evidence_line(tmp_path) -> None:
    evidence = FakeEvidence()
    memory = Memory("w", root=tmp_path, clock=FakeClock(), evidence=evidence)

    assert memory.note_rung_progress(2) is False  # baseline
    for index in range(DRIFT_GOALS - 1):
        assert memory.note_rung_progress(2, goal_id=f"g{index}") is False
    assert memory.note_rung_progress(2, goal_id="g5") is True

    assert len(evidence.entries) == 1
    entry = evidence.entries[0]
    assert entry["kind"] == "curriculum_drift"
    assert entry["goals_without_progress"] == DRIFT_GOALS
    assert entry["rungs_met"] == 2
    assert entry["goal_id"] == "g5"
    # The streak restarts, so the next flat goal is not a second drift line.
    assert memory.note_rung_progress(2) is False
    assert len(evidence.entries) == 1


def test_rung_progress_resets_the_drift_streak(tmp_path) -> None:
    evidence = FakeEvidence()
    memory = Memory("w", root=tmp_path, clock=FakeClock(), evidence=evidence)
    memory.note_rung_progress(2)
    memory.note_rung_progress(2)
    memory.note_rung_progress(2)
    assert memory.note_rung_progress(3) is False  # progress
    for _ in range(DRIFT_GOALS - 1):
        memory.note_rung_progress(3)
    assert evidence.entries == []
    assert memory.note_rung_progress(3) is True
    assert len(evidence.entries) == 1


def test_drift_without_an_evidence_sink_is_silent(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    memory.note_rung_progress(0)
    for _ in range(DRIFT_GOALS - 1):
        memory.note_rung_progress(0)
    assert memory.note_rung_progress(0) is True


def test_world_dir_is_one_segment(tmp_path) -> None:
    assert world_dir("world-1", tmp_path) == tmp_path / "world-1"
    with pytest.raises(ValueError):
        world_dir("../escape", tmp_path)
    with pytest.raises(ValueError):
        world_dir("", tmp_path)


def test_the_goal_line_shape_is_json(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock(5))
    memory.append_goal(goal("g1", status="done", failure_kind=None))
    payload = json.loads(memory.goals_path.read_text(encoding="utf-8").splitlines()[0])
    assert payload["goal_id"] == "g1"
    assert payload["condition"] == "has(furnace)"
    assert payload["status"] == "done"
    assert "failure_kind" not in payload  # None is omitted
