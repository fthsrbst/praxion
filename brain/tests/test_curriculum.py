from __future__ import annotations

import pytest
from pydantic import ValidationError

from praxion_brain.curriculum import GoalProposal, validate
from praxion_brain.memory import GoalRecord, Memory


class FakeClock:
    def __init__(self, now: float = 1000.0) -> None:
        self.now = now

    def __call__(self) -> float:
        return self.now


def proposal(condition: str, text: str = "do the thing") -> GoalProposal:
    return GoalProposal(text=text, condition=condition, rationale="because")


def test_has_shape_validates_and_names_the_skill(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    result = validate(proposal("has(furnace)"), {}, memory, None)
    assert result.ok is True
    assert result.reason is None
    assert result.skill_name == "get_furnace"
    assert result.unverifiable is False
    assert result.met is False


def test_count_shape_names_the_item(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    result = validate(proposal("count(cobblestone) >= 8"), {}, memory, None)
    assert result.ok is True
    assert result.skill_name == "get_cobblestone"


def test_compound_or_tag_shape_has_no_default_skill_name(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    compound = validate(
        proposal("has(stone_pickaxe) and count(cobblestone) >= 16"), {}, memory, None
    )
    assert compound.ok is True
    assert compound.skill_name is None

    tag = validate(proposal("count(#logs) >= 1"), {}, memory, None)
    assert tag.ok is True
    assert tag.skill_name is None


def test_minecraft_namespace_is_normalized_in_conditions_and_inventory(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    named = validate(proposal("has(minecraft:furnace)"), {}, memory, None)
    assert named.ok is True
    assert named.skill_name == "get_furnace"

    met = validate(
        proposal("count(minecraft:cobblestone) >= 8"),
        {"minecraft:cobblestone": 8},
        memory,
        None,
    )
    assert met.ok is False
    assert met.reason == "already_met"
    assert met.met is True


def test_minecraft_tag_namespace_validates(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    result = validate(proposal("count(#minecraft:logs) >= 1"), {}, memory, None)
    assert result.ok is True
    assert result.unverifiable is False
    assert result.met is False
    assert result.skill_name is None


def test_string_argument_condition_is_unverifiable_not_met(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    result = validate(proposal('near("table", 5)'), {}, memory, None)
    assert result.ok is True
    assert result.unverifiable is True
    assert result.met is None


def test_already_met_is_rejected(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    result = validate(proposal("has(furnace)"), {"furnace": 1}, memory, None)
    assert result.ok is False
    assert result.reason == "already_met"
    assert result.met is True


def test_malformed_condition_is_rejected_with_the_parser_error(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    result = validate(proposal("has("), {}, memory, None)
    assert result.ok is False
    assert result.reason == "malformed_condition"
    assert result.detail and "expected" in result.detail


def test_unsupported_condition_runs_but_is_unverifiable(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    result = validate(proposal("near(#logs, 5)"), {}, memory, None)
    assert result.ok is True
    assert result.unverifiable is True
    assert result.skill_name is None
    assert result.met is None


def test_blocked_goal_is_rejected(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock(300.0))
    for index in range(2):
        memory.append_goal(
            GoalRecord(
                goal_id=f"g{index}",
                text="have coal",
                condition="has(coal)",
                status="failed",
                failure_kind="not_found",
                t_start=100.0 + index,
                t_end=100.0 + index,
            )
        )
    result = validate(proposal("has(coal)"), {}, memory, None)
    assert result.ok is False
    assert result.reason == "blocked"


def test_blocked_goal_is_rejected_across_condition_spellings(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock(300.0))
    for index, condition in enumerate(
        ("count( minecraft:cobblestone )>=8", "count(cobblestone) >= 8")
    ):
        memory.append_goal(
            GoalRecord(
                goal_id=f"g{index}",
                text="have cobblestone",
                condition=condition,
                status="failed",
                failure_kind="not_found",
                t_start=100.0 + index,
                t_end=100.0 + index,
            )
        )
    result = validate(proposal("count(cobblestone)>=8"), {}, memory, None)
    assert result.ok is False
    assert result.reason == "blocked"


def test_same_condition_as_the_last_goal_is_rejected(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    last = GoalProposal(text="have coal", condition="has(coal)")
    result = validate(proposal("has(coal) "), {}, memory, last)
    assert result.ok is False
    assert result.reason == "same_as_last"


def test_same_as_last_ignores_spelling_and_namespace(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    last = GoalProposal(text="have coal", condition="has(minecraft:coal)")
    result = validate(proposal("has( coal )"), {}, memory, last)
    assert result.ok is False
    assert result.reason == "same_as_last"


def test_a_different_last_goal_does_not_reject(tmp_path) -> None:
    memory = Memory("w", root=tmp_path, clock=FakeClock())
    last = GoalRecord(goal_id="g1", text="have coal", condition="has(coal)")
    result = validate(proposal("has(furnace)"), {}, memory, last)
    assert result.ok is True


def test_rationale_is_capped_at_two_hundred_characters() -> None:
    with pytest.raises(ValidationError):
        GoalProposal(text="x", condition="has(furnace)", rationale="y" * 201)
