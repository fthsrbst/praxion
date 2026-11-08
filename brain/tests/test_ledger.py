from __future__ import annotations

import json
import time

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

def test_spent_since_counts_only_lines_after_the_cutoff(tmp_path) -> None:
    ledger = Ledger(tmp_path / "ledger.jsonl")
    old = {
        "timestamp": "2020-01-01T00:00:00+00:00",
        "provider": "openai",
        "model": "gpt-6-luna",
        "input_tokens": 1,
        "output_tokens": 0,
        "cost": 1.50,
        "purpose": "write_skill",
    }
    ledger.path.write_text(json.dumps(old) + "\n", encoding="utf-8")
    ledger.record("openai", "gpt-6-luna", 1_000_000, 0, "write")  # $0.10 now

    assert ledger.spent_since(0.0) == pytest.approx(1.60)
    assert ledger.spent_since(time.time() - 60) == pytest.approx(0.10)
    assert ledger.spent_since(time.time() + 60) == 0.0


def test_entries_are_cached_until_the_file_changes(tmp_path) -> None:
    ledger = Ledger(tmp_path / "ledger.jsonl")
    assert ledger.entries() == []
    ledger.record("openai", "gpt-6-luna", 1_000_000, 0, "write")
    assert len(ledger.entries()) == 1
    # A line appended outside record() is still seen (the file changed).
    ledger.path.write_text(ledger.path.read_text(encoding="utf-8") + "\n", encoding="utf-8")
    assert len(ledger.entries()) == 1
    with ledger.path.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps({"provider": "typesafe", "cost": 0.02}) + "\n")
    assert len(ledger.entries()) == 2
    assert ledger.spent("typesafe") == pytest.approx(0.02)


def test_an_unreadable_timestamp_is_not_guessed(tmp_path) -> None:
    ledger = Ledger(tmp_path / "ledger.jsonl")
    ledger.path.write_text(
        json.dumps({"provider": "openai", "cost": 1.0, "timestamp": "not a date"}) + "\n",
        encoding="utf-8",
    )
    assert ledger.spent("openai") == pytest.approx(1.0)
    assert ledger.spent_since(0.0) == 0.0
