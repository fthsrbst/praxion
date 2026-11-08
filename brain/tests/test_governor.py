from __future__ import annotations

import json
import time

import pytest

from praxion_brain.cancel import CancelToken
from praxion_brain.evidence import Evidence
from praxion_brain.governor import (
    BREAKER_OPEN_SECONDS,
    CALL_TIMEOUTS,
    CallTimeout,
    Governor,
)
from praxion_brain.ladder import code_only_goal
from praxion_brain.ledger import Ledger
from praxion_brain.skills import SkillCatalog, SkillInfo


class FakeClock:
    def __init__(self, now: float = 1_000.0) -> None:
        self.now = now

    def __call__(self) -> float:
        return self.now


def _catalog(*names: str) -> SkillCatalog:
    return SkillCatalog(
        skills=[
            SkillInfo(
                name=name,
                doc="",
                source=f"skill {name}()\n  rules:\n    true -> done\n",
                path=f"{name}.skill",
                origin="core",
            )
            for name in names
        ]
    )


def _governor(tmp_path, **kwargs):
    clock = kwargs.pop("clock", FakeClock())
    evidence = kwargs.pop("evidence", Evidence(tmp_path / "evidence"))
    log = kwargs.pop("log", [].append)
    ledger = kwargs.pop("ledger", Ledger(tmp_path / "ledger.jsonl"))
    governor = Governor(
        ledger,
        clock=clock,
        evidence=evidence,
        log=log,
        **kwargs,
    )
    return governor, ledger, evidence


def _write_old_line(ledger: Ledger, cost: float, *, hour: int = 0) -> None:
    """One ledger line from an earlier session, with an old UTC timestamp."""
    entry = {
        "timestamp": f"2020-01-01T{hour:02d}:00:00+00:00",
        "provider": "openai",
        "model": "gpt-6-luna",
        "input_tokens": 1,
        "output_tokens": 0,
        "cost": cost,
        "purpose": "write_skill",
    }
    ledger.path.parent.mkdir(parents=True, exist_ok=True)
    with ledger.path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(entry) + "\n")


def test_hard_stop_denies_and_enters_code_only(tmp_path) -> None:
    governor, _ledger, _evidence = _governor(tmp_path, hard_stop_usd=0.0)

    decision = governor.allow("propose", 0.0)

    assert decision.allowed is False
    assert decision.reason == "hard_stop"
    assert governor.hard_stop_reached is True
    assert governor.code_only is True


def test_hard_stop_counts_this_session_not_the_whole_ledger(tmp_path, monkeypatch) -> None:
    # A2: an old session left $2.00; the user's $1.50 stop must not trip on it.
    monkeypatch.setenv("PRAXION_BUDGET_OPENAI", "4.50")
    ledger = Ledger(tmp_path / "ledger.jsonl")
    _write_old_line(ledger, 2.00)
    governor, ledger, _evidence = _governor(
        tmp_path,
        ledger=ledger,
        hard_stop_usd=1.50,
        wall_clock=time.time,
    )

    assert governor.spent_session() == 0.0
    assert governor.hard_stop_reached is False
    assert governor.code_only is False
    assert governor.allow("propose", 0.0).allowed is True

    # $1.00 of this session's spend still fits under the $1.50 session stop.
    ledger.record("openai", "gpt-6.1-sol", 500_000, 0, "propose_goal")  # $1.00
    assert governor.spent_session() == pytest.approx(1.00)
    assert governor.hard_stop_reached is False


def test_the_ledger_budget_stays_the_absolute_cap(tmp_path, monkeypatch) -> None:
    # A2: session scope on the stop, but the per-provider budget still caps all time.
    monkeypatch.setenv("PRAXION_BUDGET_OPENAI", "2.00")
    ledger = Ledger(tmp_path / "ledger.jsonl")
    _write_old_line(ledger, 2.10)
    governor, _ledger, _evidence = _governor(
        tmp_path,
        ledger=ledger,
        hard_stop_usd=100.0,
        wall_clock=time.time,
    )

    assert governor.hard_stop_reached is False
    decision = governor.allow("propose", 0.0)
    assert decision.allowed is False
    assert decision.reason == "budget"


def test_per_hour_warning_is_written_once(tmp_path) -> None:
    ledger = Ledger(tmp_path / "ledger.jsonl")
    ledger.record("openai", "gpt-6-luna", 1_000_000, 0, "write")  # $0.10
    warnings: list[str] = []
    governor = Governor(
        ledger,
        clock=time.monotonic,
        wall_clock=time.time,
        warn_usd_per_hour=0.05,
        evidence=Evidence(tmp_path / "evidence"),
        log=warnings.append,
    )

    assert governor.allow("propose", 0.0).allowed is True
    assert governor.allow("propose", 0.0).allowed is True

    assert ledger.spent_since(time.time() - 3600) >= 0.10
    assert len(warnings) == 1
    assert "cost warning" in warnings[0]
    kinds = [
        json.loads(line)["kind"]
        for line in (tmp_path / "evidence" / "brain.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert kinds.count("governor_warning") == 1


def test_breaker_opens_after_three_errors_and_probes_once(tmp_path) -> None:
    clock = FakeClock()
    governor, _ledger, _evidence = _governor(tmp_path, clock=clock)

    for _ in range(3):
        governor.record_error()
    assert governor.breaker_state == "open"

    denied = governor.allow("route", 0.0)
    assert denied.allowed is False
    assert denied.reason == "breaker_open"

    clock.now += BREAKER_OPEN_SECONDS
    probe = governor.allow("route", 0.0)
    assert probe.allowed is True
    second = governor.allow("route", 0.0)
    assert second.allowed is False
    assert second.reason == "breaker_open"

    governor.record_success()
    assert governor.breaker_state == "closed"
    assert governor.allow("route", 0.0).allowed is True


def test_rate_limit_per_call_class(tmp_path) -> None:
    governor, _ledger, _evidence = _governor(
        tmp_path, rate_limits={"propose": (2, 3600.0)}
    )

    assert governor.allow("propose", 0.0).allowed is True
    assert governor.allow("propose", 0.0).allowed is True
    denied = governor.allow("propose", 0.0)
    assert denied.allowed is False
    assert denied.reason == "rate_limit"
    # A different class has its own budget.
    assert governor.allow("route", 0.0).allowed is True


def test_call_timeouts(tmp_path) -> None:
    governor, _ledger, _evidence = _governor(tmp_path)
    assert governor.timeout("propose") == CALL_TIMEOUTS["propose"] == 60.0
    assert governor.timeout("route") == 5.0
    assert governor.timeout("write") == 180.0
    assert governor.timeout("admission") > governor.timeout("write")
    # Every call class the loop can gate has a provider and a deadline.
    for call_class in ("propose", "route", "write"):
        assert governor.allow(call_class, 0.0).allowed is True


def test_call_returns_the_value_and_reraises_the_error(tmp_path) -> None:
    governor, _ledger, _evidence = _governor(tmp_path)

    assert governor.call("route", lambda: 7) == 7

    def broken() -> None:
        raise ValueError("provider said no")

    with pytest.raises(ValueError, match="provider said no"):
        governor.call("route", broken)


def test_call_expires_with_a_typed_timeout(tmp_path) -> None:
    # A4: a provider that never answers cannot hold the loop.
    governor, _ledger, _evidence = _governor(tmp_path, call_timeouts={"route": 0.05})

    def stuck() -> None:
        time.sleep(1.0)

    started = time.monotonic()
    with pytest.raises(CallTimeout) as caught:
        governor.call("route", stuck)
    assert time.monotonic() - started < 0.9
    assert caught.value.call_class == "route"
    assert caught.value.seconds == 0.05


def test_a_timed_out_call_sets_the_cancel_token_it_was_given(tmp_path) -> None:
    # The abandoned worker of an admission must stop and never go active.
    governor, _ledger, _evidence = _governor(tmp_path, call_timeouts={"admission": 0.05})
    token = CancelToken()

    def stuck() -> None:
        time.sleep(1.0)

    with pytest.raises(CallTimeout):
        governor.call("admission", stuck, cancel=token)
    assert token.is_set()


def test_code_only_goal_is_the_first_unmet_rung_with_a_skill() -> None:
    catalog = _catalog("get_wood")
    goal = code_only_goal({}, catalog)
    assert goal is not None
    assert goal.skill == "get_wood"
    assert goal.goal.condition == "count(#logs) >= 1"
    assert goal.goal.rung == "wood"

    # The next rung has no skill in this catalog: stay idle.
    assert code_only_goal({"oak_log": 1}, catalog) is None

    catalog = _catalog("get_wood", "get_crafting_table")
    next_goal = code_only_goal({"oak_log": 1}, catalog)
    assert next_goal is not None
    assert next_goal.skill == "get_crafting_table"
    assert next_goal.goal.rung == "crafting_table"


def test_budget_snapshot_reads_the_ledger(tmp_path) -> None:
    governor, ledger, _evidence = _governor(tmp_path)
    ledger.record("openai", "gpt-6-luna", 1_000_000, 0, "write")  # $0.10
    snapshot = governor.budget_snapshot()
    assert snapshot["spent"] == 0.10
    assert snapshot["last_hour"] == 0.10
    assert snapshot["budget"] == 9.00
