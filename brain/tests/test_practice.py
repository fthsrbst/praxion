"""Unit tests for one practice run, with the body stack faked out."""

from __future__ import annotations

from pathlib import Path
from typing import Any, Optional

import pytest

from praxion_brain.cancel import AdmissionCancelled, CancelToken
from praxion_brain.goal import Goal
from praxion_brain.practice import (
    _failure_kind,
    classify_failure,
    practice,
    stop_live_runs,
)


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
        self.stopped = False
        self.stops = 0

    def wait_for_outcome(self, timeout: float) -> dict[str, Any]:
        if self._summary is None:
            raise TimeoutError("the body never wrote a summary")
        return self._summary

    def summary(self) -> Optional[dict[str, Any]]:
        return self._summary

    def events(self) -> list[dict[str, Any]]:
        return list(self._events)

    def stop(self) -> None:
        """The real run kills its body, forwarder and server; the fake counts."""
        if not self.stopped:
            self.stopped = True
            self.stops += 1


class FakeSession:
    def __init__(self, run: FakeRun) -> None:
        self._run = run

    def __enter__(self) -> FakeRun:
        return self._run

    def __exit__(self, *_exc: Any) -> None:
        self._run.stop()
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
    assert result.failure_class == "body"
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
    assert result.failure_class == "skill"
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
    assert result.failure_class == "body"
    assert "failure kind: no_block" in result.digest
    assert "referee: furnace 0/1 (body outcome failed(no_block))" in result.digest


def test_practice_reads_candidate_skill_names_for_classification(
    monkeypatch: Any, tmp_path: Path
) -> None:
    candidate_dir = tmp_path / "candidate"
    candidate_dir.mkdir()
    (candidate_dir / "get_furnace.skill").write_text(
        "skill get_furnace()\n  doc \"top\"\n  rules:\n    true -> do helper()\n"
        "skill helper()\n  doc \"inner\"\n  rules:\n    true -> done\n",
        encoding="utf-8",
    )
    events = [
        _frame("helper", 2, "no_rule"),
        _frame("get_furnace", 1, "no_rule"),
    ]
    run = FakeRun(
        summary={"goal": {"outcome": "failed(no_rule)"}, "ticks": 12},
        inventory={},
        events=events,
        run_dir=tmp_path / "run",
    )
    monkeypatch.setattr(
        "praxion_brain.practice.start_body", fake_start_body(run=run)
    )

    result = practice("get_furnace", GOAL, candidate_dir, seed=11)

    assert result.failure_class == "skill"


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


CANDIDATES = {"get_furnace"}


def _frame(skill: str, depth: int, kind: str) -> dict[str, Any]:
    return {
        "kind": "skill_frame",
        "skill": skill,
        "depth": depth,
        "outcome": "failed",
        "failure_kind": kind,
    }


def _rule(skill: str, rule: int = 0) -> dict[str, Any]:
    return {"kind": "rule_selected", "skill": skill, "rule": rule}


def test_classifier_referee_disagreement_is_a_skill_failure() -> None:
    assert (
        classify_failure([], CANDIDATES, "done", "goal_not_met") == "skill"
    )


def test_classifier_candidate_no_rule_is_a_skill_failure() -> None:
    events = [_frame("get_furnace", 1, "no_rule")]
    assert (
        classify_failure(events, CANDIDATES, "failed(no_rule)", "no_rule")
        == "skill"
    )


def test_classifier_core_no_rule_is_a_body_failure() -> None:
    events = [
        _frame("get_cobblestone", 2, "no_rule"),
        _frame("get_furnace", 1, "no_rule"),
    ]
    assert (
        classify_failure(events, CANDIDATES, "failed(no_rule)", "no_rule")
        == "body"
    )


def test_classifier_primitive_failure_is_a_body_failure() -> None:
    events = [
        {
            "kind": "primitive_finished",
            "primitive": "mine",
            "outcome": "failed",
            "failure_kind": "no_block",
        },
        _frame("get_furnace", 1, "no_block"),
    ]
    assert (
        classify_failure(events, CANDIDATES, "failed(no_block)", "no_block")
        == "body"
    )


def test_classifier_candidate_loop_on_max_ticks_is_a_skill_failure() -> None:
    events = [_rule("get_furnace", index % 2) for index in range(10)]
    assert (
        classify_failure(events, CANDIDATES, "max_ticks", "max_ticks") == "skill"
    )


def test_classifier_loop_on_timeout_is_a_skill_failure() -> None:
    events = [_rule("get_furnace") for _ in range(10)]
    assert classify_failure(events, CANDIDATES, "timeout", "timeout") == "skill"


def test_classifier_confirmed_primitive_breaks_the_loop() -> None:
    events = [_rule("get_furnace") for _ in range(10)]
    events.insert(
        4,
        {
            "kind": "primitive_finished",
            "primitive": "mine",
            "outcome": "confirmed",
        },
    )
    assert (
        classify_failure(events, CANDIDATES, "max_ticks", "max_ticks") == "body"
    )


def test_classifier_core_rule_selection_breaks_the_loop() -> None:
    events = [_rule("get_furnace") for _ in range(9)]
    events.insert(0, _rule("get_cobblestone"))
    assert (
        classify_failure(events, CANDIDATES, "max_ticks", "max_ticks") == "body"
    )


def test_classifier_max_ticks_inside_core_is_a_body_failure() -> None:
    events = [_rule("get_cobblestone") for _ in range(10)]
    assert (
        classify_failure(events, CANDIDATES, "max_ticks", "max_ticks") == "body"
    )


def test_classifier_body_error_is_a_body_failure() -> None:
    assert classify_failure([], CANDIDATES, "body_error", "body_error") == "body"


# --------------------------------------------------------------------------- #
# Cancellation: a live run, and the runs a process exit leaves behind
# --------------------------------------------------------------------------- #
def _done_run(tmp_path: Path) -> FakeRun:
    return FakeRun(
        summary={"goal": {"outcome": "done"}, "ticks": 20},
        inventory={"minecraft:furnace": 1},
        events=[],
        run_dir=tmp_path / "run",
    )


def test_a_cancel_kills_the_live_run_before_it_returns(
    monkeypatch: Any, tmp_path: Path
) -> None:
    cancel = CancelToken()
    run = _done_run(tmp_path)
    seen: list[bool] = []

    def wait_for_outcome(timeout: float) -> dict[str, Any]:
        cancel.set()  # the admission deadline expires while the body runs
        seen.append(run.stopped)
        return run.summary() or {}

    run.wait_for_outcome = wait_for_outcome  # type: ignore[method-assign]
    monkeypatch.setattr("praxion_brain.practice.start_body", fake_start_body(run=run))

    with pytest.raises(AdmissionCancelled):
        practice("get_furnace", GOAL, tmp_path, seed=12, cancel=cancel)

    assert seen == [True], "the server, forwarder and body must be killed on cancel"
    assert run.stops == 1, "the cancel stopped it once; the context exit repeats nothing"


def test_stop_live_runs_kills_a_run_still_in_flight(
    monkeypatch: Any, tmp_path: Path
) -> None:
    # What the interpreter's exit runs: the registry must have the live run.
    run = _done_run(tmp_path)
    seen: list[bool] = []

    def wait_for_outcome(timeout: float) -> dict[str, Any]:
        stop_live_runs()
        seen.append(run.stopped)
        return run.summary() or {}

    run.wait_for_outcome = wait_for_outcome  # type: ignore[method-assign]
    monkeypatch.setattr("praxion_brain.practice.start_body", fake_start_body(run=run))

    practice("get_furnace", GOAL, tmp_path, seed=13)

    assert seen == [True]
    assert run.stops == 1


def test_a_finished_run_is_unregistered(monkeypatch: Any, tmp_path: Path) -> None:
    run = _done_run(tmp_path)
    monkeypatch.setattr("praxion_brain.practice.start_body", fake_start_body(run=run))

    practice("get_furnace", GOAL, tmp_path, seed=14)
    stop_live_runs()  # a later exit must not stop a finished run again

    assert run.stops == 1


def test_a_cancel_before_the_run_starts_no_server(
    monkeypatch: Any, tmp_path: Path
) -> None:
    cancel = CancelToken()
    cancel.set()

    def start(*_args: Any, **_kwargs: Any) -> Any:
        raise AssertionError("a cancelled practice must not start a server")

    monkeypatch.setattr("praxion_brain.practice.start_body", start)

    with pytest.raises(AdmissionCancelled):
        practice("get_furnace", GOAL, tmp_path, seed=15, cancel=cancel)
