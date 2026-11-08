from __future__ import annotations

import pytest

from praxion_brain.ledger import (
    BudgetExceeded,
    Ledger,
    cost,
    estimate_cost,
)


def test_cost_matches_the_price_table() -> None:
    assert cost("gpt-6-luna", 1_000_000, 0) == pytest.approx(0.10)
    assert cost("gpt-6-luna", 0, 1_000_000) == pytest.approx(0.50)
    assert cost("gpt-6.1-terra", 1_000_000, 1_000_000) == pytest.approx(6.00)
    assert cost("jev-latest", 1_000_000, 100) == pytest.approx(0.042)
    assert estimate_cost("gpt-6.1-sol", 500_000, 0) == pytest.approx(1.00)


def test_record_appends_one_line_per_request(tmp_path) -> None:
    ledger = Ledger(tmp_path / "ledger.jsonl")
    entry = ledger.record("openai", "gpt-6-luna", 1_000, 500, "write_skill")

    assert entry["cost"] == pytest.approx(0.00010 + 0.00025)
    assert entry["purpose"] == "write_skill"
    lines = (tmp_path / "ledger.jsonl").read_text(encoding="utf-8").splitlines()
    assert len(lines) == 1
    assert ledger.spent("openai") == pytest.approx(0.00035)
    assert ledger.spent("typesafe") == 0.0


def test_totals_groups_by_provider(tmp_path) -> None:
    ledger = Ledger(tmp_path / "ledger.jsonl")
    ledger.record("openai", "gpt-6-luna", 1_000_000, 0, "write")
    ledger.record("typesafe", "jev-latest", 1_000_000, 0, "route")
    totals = ledger.totals()
    assert totals["openai"] == pytest.approx(0.10)
    assert totals["typesafe"] == pytest.approx(0.042)


def test_default_budget_and_env_override(tmp_path, monkeypatch) -> None:
    monkeypatch.delenv("PRAXION_BUDGET_OPENAI", raising=False)
    ledger = Ledger(tmp_path / "ledger.jsonl")
    assert ledger.budget("openai") == pytest.approx(4.50)

    monkeypatch.setenv("PRAXION_BUDGET_OPENAI", "0.25")
    assert ledger.budget("openai") == pytest.approx(0.25)


def test_ensure_refuses_a_call_that_crosses_the_budget(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("PRAXION_BUDGET_OPENAI", "0.50")
    ledger = Ledger(tmp_path / "ledger.jsonl")
    ledger.record("openai", "gpt-6-luna", 1_000_000, 0, "write")  # $0.10

    ledger.ensure("openai", 0.40)  # spent + estimate == budget
    with pytest.raises(BudgetExceeded):
        ledger.ensure("openai", 0.41)


def test_invalid_budget_env_is_an_error(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("PRAXION_BUDGET_TYPESAFE", "not-a-number")
    ledger = Ledger(tmp_path / "ledger.jsonl")
    with pytest.raises(ValueError):
        ledger.budget("typesafe")


def test_unknown_model_has_no_price(tmp_path) -> None:
    ledger = Ledger(tmp_path / "ledger.jsonl")
    with pytest.raises(ValueError):
        ledger.record("openai", "no-such-model", 1, 1, "write")


def test_corrupt_ledger_line_is_reported(tmp_path) -> None:
    path = tmp_path / "ledger.jsonl"
    path.write_text("{not json}\n", encoding="utf-8")
    with pytest.raises(ValueError):
        Ledger(path).entries()
