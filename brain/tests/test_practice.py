"""Unit tests for one practice run, with the body stack faked out."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Optional

from praxion_brain.goal import Goal
from praxion_brain.practice import _failure_kind, practice


class FakeReferee:
    def __init__(self, inventory: dict[str, int]) -> None:
        self._inventory = inventory

    def player_inventory(self) -> dict[str, int]:
        return dict(self._inventory)


class FakeRun:
    def __init__(
        self,
        *,
        summary: Optional[dict[str, Any]],
        inventory: dict[str, int],
        events: list[dict[str, Any]],
        run_dir: Path,
    ) -> None:
        self._summary = summary
        self.referee = FakeReferee(inventory)
        self._events = events
        self.run_dir = run_dir

    def wait_for_outcome(self, timeout: float) -> dict[str, Any]:
        if self._summary is None:
            raise TimeoutError("the body never wrote a summary")
        return self._summary

    def summary(self) -> Optional[dict[str, Any]]:
        return self._summary

    def events(self) -> list[dict[str, Any]]:
        return list(self._events)


class FakeSession:
    def __init__(self, run: FakeRun) -> None:
        self._run = run

    def __enter__(self) -> FakeRun:
        return self._run

    def __exit__(self, *_exc: Any) -> None:
        return None


def fake_start_body(
    *, run: Optional[FakeRun] = None, error: Optional[BaseException] = None
):
    def start(*_args: Any, **_kwargs: Any) -> FakeSession:
        if error is not None:
            raise error
        assert run is not None
        return FakeSession(run)

    return start


GOAL = Goal(text="get one furnace", item="furnace", count=1)


def test_runtime_error_becomes_a_body_error(monkeypatch: Any, tmp_path: Path) -> None:
    monkeypatch.setattr(
        "praxion_brain.practice.start_body",
        fake_start_body(error=RuntimeError("no body binary")),
    )

    result = practice("get_furnace", GOAL, tmp_path, seed=7)

    assert result.passed is False
    assert result.outcome == "body_error"
    assert result.failure_kind == "body_error"
    assert result.run_dir == ""
    assert result.seed == 7
    assert result.inventory == {}
    assert "no body binary" in result.digest


def test_timeout_error_becomes_a_body_error(monkeypatch: Any, tmp_path: Path) -> None:
    monkeypatch.setattr(
        "praxion_brain.practice.start_body",
        fake_start_body(error=TimeoutError("the body never joined")),
    )

    result = practice("get_furnace", GOAL, tmp_path, seed=8)

    assert result.passed is False
    assert result.outcome == "body_error"
    assert result.failure_kind == "body_error"
    assert "the body never joined" in result.digest


def test_done_without_the_item_is_goal_not_met(
    monkeypatch: Any, tmp_path: Path
) -> None:
    events = [
        {
            "kind": "skill_frame",
            "skill": "get_furnace",
            "depth": 1,
            "outcome": "done",
        }
    ]
    run = FakeRun(
        summary={
            "goal": {"outcome": "done"},
            "ticks": 42,
            "receipts": {"done": 1},
            "deaths": 0,
        },
        inventory={},
        events=events,
        run_dir=tmp_path / "run",
    )
    monkeypatch.setattr(
        "praxion_brain.practice.start_body", fake_start_body(run=run)
    )

    result = practice("get_furnace", GOAL, tmp_path, seed=9)

    assert result.passed is False
    assert result.outcome == "done"
    assert result.failure_kind == "goal_not_met"
    assert result.ticks == 42
    assert result.receipts == {"done": 1}
    assert result.inventory == {}
    assert result.run_dir == str(tmp_path / "run")
    assert "failure kind: goal_not_met" in result.digest
    assert "skill_frame get_furnace depth=1 -> done" in result.digest
    assert "referee: furnace 0/1 (body outcome done)" in result.digest


def test_failed_outcome_keeps_its_failure_kind(
    monkeypatch: Any, tmp_path: Path
) -> None:
    run = FakeRun(
        summary={"goal": {"outcome": "failed(no_block)"}, "ticks": 12},
        inventory={},
        events=[],
        run_dir=tmp_path / "run",
    )
    monkeypatch.setattr(
        "praxion_brain.practice.start_body", fake_start_body(run=run)
    )

    result = practice("get_furnace", GOAL, tmp_path, seed=10)

    assert result.passed is False
    assert result.outcome == "failed(no_block)"
    assert result.failure_kind == "no_block"
    assert "failure kind: no_block" in result.digest
    assert "referee: furnace 0/1 (body outcome failed(no_block))" in result.digest


def test_failure_kind_parses_failed_prefix_and_event_fallback() -> None:
    assert _failure_kind("failed(no_block)", []) == "no_block"
    assert _failure_kind("failed", []) == "failed"
    assert (
        _failure_kind(
            "failed",
            [
                {
                    "kind": "primitive_finished",
                    "outcome": "failed",
                    "failure_kind": "not_found",
                }
            ],
        )
        == "not_found"
    )
