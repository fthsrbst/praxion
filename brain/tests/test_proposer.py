from __future__ import annotations

import json

import pytest

from praxion_brain.curriculum import GoalProposal
from praxion_brain.evidence import Evidence
from praxion_brain.ledger import BudgetExceeded, Ledger, estimate_cost
from praxion_brain.proposer import (
    ESCALATE_AFTER,
    INPUT_ESTIMATE,
    OUTPUT_ESTIMATE,
    PROPOSAL_RULES,
    estimate,
    model_for,
    propose,
    system_prompt,
)
from praxion_brain.skills import SkillCatalog, SkillInfo


class FakeUsage:
    input_tokens = 2_100
    output_tokens = 140


class FakeResponse:
    def __init__(self, parsed: GoalProposal | None) -> None:
        self.output_parsed = parsed
        self.usage = FakeUsage()
        self.id = "resp_proposal"


class FakeResponses:
    def __init__(self, parsed: GoalProposal | None) -> None:
        self.parsed = parsed
        self.calls: list[dict] = []

    def parse(self, **kwargs):
        self.calls.append(kwargs)
        return FakeResponse(self.parsed)


class FakeClient:
    def __init__(self, parsed: GoalProposal | None) -> None:
        self.responses = FakeResponses(parsed)


PROPOSAL = GoalProposal(
    text="get a furnace",
    condition="has(furnace)",
    rung="furnace",
    rationale="The ladder needs a furnace.",
)


def _catalog() -> SkillCatalog:
    return SkillCatalog(
        skills=[
            SkillInfo(
                name="get_wood",
                params=["n"],
                doc="get logs",
                source='skill get_wood(n)\n  rules:\n    true -> mine("stone")\n',
                path="get_wood.skill",
                origin="core",
            )
        ]
    )


def test_system_prompt_is_stable_and_names_the_skills() -> None:
    catalog = _catalog()
    first = system_prompt(catalog)
    assert first == system_prompt(catalog)
    assert PROPOSAL_RULES in first
    assert "get_wood(n) - get logs" in first
    assert "mine" in first  # primitive catalog from the core source


def test_briefing_is_the_user_message_and_the_cheap_model_is_default(tmp_path) -> None:
    fake = FakeClient(PROPOSAL)
    ledger = Ledger(tmp_path / "ledger.jsonl")
    evidence = Evidence(tmp_path / "evidence")

    parsed = propose(
        "## Now\nhealth 20/20",
        _catalog(),
        client=fake,
        ledger=ledger,
        evidence=evidence,
    )

    assert parsed == PROPOSAL
    call = fake.responses.calls[0]
    assert call["model"] == "gpt-6-luna"
    assert call["text_format"] is GoalProposal
    assert call["instructions"] == system_prompt(_catalog())
    assert call["input"] == [{"role": "user", "content": "## Now\nhealth 20/20"}]
    assert call["reasoning"] == {"effort": "low"}

    entries = ledger.entries()
    assert len(entries) == 1
    assert entries[0]["purpose"] == "propose_goal"
    assert entries[0]["input_tokens"] == 2_100
    assert entries[0]["output_tokens"] == 140

    kinds = [
        json.loads(line)["kind"]
        for line in (tmp_path / "evidence" / "brain.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert kinds == ["proposal_request", "proposal_response"]


def test_escalates_after_two_invalid_proposals(tmp_path) -> None:
    assert model_for(0, "gpt-6-luna", "gpt-6.1-terra") == "gpt-6-luna"
    assert model_for(ESCALATE_AFTER - 1, "gpt-6-luna", "gpt-6.1-terra") == "gpt-6-luna"
    assert model_for(ESCALATE_AFTER, "gpt-6-luna", "gpt-6.1-terra") == "gpt-6.1-terra"

    fake = FakeClient(PROPOSAL)
    propose(
        "briefing",
        _catalog(),
        invalid_count=2,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    assert fake.responses.calls[0]["model"] == "gpt-6.1-terra"


def test_budget_refusal_happens_before_the_api_call(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("PRAXION_BUDGET_OPENAI", "0")
    fake = FakeClient(PROPOSAL)
    with pytest.raises(BudgetExceeded):
        propose(
            "briefing",
            _catalog(),
            client=fake,
            ledger=Ledger(tmp_path / "ledger.jsonl"),
            evidence=Evidence(tmp_path / "evidence"),
        )
    assert fake.responses.calls == []


def test_missing_parsed_output_is_an_error_but_the_cost_is_recorded(tmp_path) -> None:
    fake = FakeClient(None)
    ledger = Ledger(tmp_path / "ledger.jsonl")
    with pytest.raises(RuntimeError):
        propose(
            "briefing",
            _catalog(),
            client=fake,
            ledger=ledger,
            evidence=Evidence(tmp_path / "evidence"),
        )
    assert len(ledger.entries()) == 1

def test_the_proposal_estimate_uses_the_model_the_call_will_use() -> None:
    cheap = estimate(0)
    dear = estimate(ESCALATE_AFTER)
    assert cheap == pytest.approx(
        estimate_cost("gpt-6-luna", INPUT_ESTIMATE, OUTPUT_ESTIMATE)
    )
    assert dear == pytest.approx(
        estimate_cost("gpt-6.1-terra", INPUT_ESTIMATE, OUTPUT_ESTIMATE)
    )
    assert dear > cheap


def test_the_rules_no_longer_ask_for_stock_and_name_the_needs_section() -> None:
    from praxion_brain.proposer import PROPOSAL_RULES

    assert "stocks what the next rungs need" not in PROPOSAL_RULES
    assert "more cobblestone" not in PROPOSAL_RULES
    assert "## Needs" in PROPOSAL_RULES
    assert "serve the next rung" in PROPOSAL_RULES
