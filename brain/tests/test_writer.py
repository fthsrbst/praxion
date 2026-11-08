from __future__ import annotations

import json

import pytest

from praxion_brain.evidence import Evidence
from praxion_brain.goal import Goal
from praxion_brain.ledger import BudgetExceeded, Ledger
from praxion_brain.skills import load_catalog
from praxion_brain.writer import FailureContext, SkillDraft, write


class FakeUsage:
    input_tokens = 1_234
    output_tokens = 56


class FakeResponse:
    def __init__(self, parsed: SkillDraft | None) -> None:
        self.output_parsed = parsed
        self.usage = FakeUsage()
        self.id = "resp_test"


class FakeResponses:
    def __init__(self, parsed: SkillDraft | None) -> None:
        self.parsed = parsed
        self.calls: list[dict] = []

    def parse(self, **kwargs):
        self.calls.append(kwargs)
        return FakeResponse(self.parsed)


class FakeClient:
    def __init__(self, parsed: SkillDraft | None) -> None:
        self.responses = FakeResponses(parsed)


DRAFT = SkillDraft(
    skill_source='skill get_furnace()\n  doc "Have a furnace."\n  rules:\n    count(furnace) >= 1 -> done\n',
    rationale="Craft a furnace from cobblestone at a table.",
)


def _goal() -> Goal:
    return Goal(text="have a furnace", item="furnace")


def test_prompt_is_the_full_catalog_and_the_task(tmp_path) -> None:
    catalog = load_catalog()
    fake = FakeClient(DRAFT)
    ledger = Ledger(tmp_path / "ledger.jsonl")
    evidence = Evidence(tmp_path / "evidence")

    parsed = write(_goal(), catalog, client=fake, ledger=ledger, evidence=evidence)

    assert parsed == DRAFT
    kwargs = fake.responses.calls[0]
    system = kwargs["instructions"]
    task = kwargs["input"][0]["content"]
    assert kwargs["model"] == "gpt-6-luna"
    assert kwargs["text_format"] is SkillDraft
    assert kwargs["reasoning"] == {"effort": "low"}
    for skill in catalog.skills:
        if skill.origin == "core":
            assert skill.source.strip() in system
        else:
            # Learned skills never reach the writer: runs do not load them.
            assert skill.source.strip() not in system
    assert "have a furnace" in task
    assert "get_furnace" in task
    assert "1 furnace" in task

    entries = ledger.entries()
    assert len(entries) == 1
    assert entries[0]["purpose"] == "write_skill"
    assert entries[0]["input_tokens"] == 1_234
    assert entries[0]["output_tokens"] == 56

    kinds = [
        json.loads(line)["kind"]
        for line in (tmp_path / "evidence" / "brain.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert kinds == ["writer_request", "writer_response"]


def test_repair_task_carries_the_previous_source_and_failure(tmp_path) -> None:
    catalog = load_catalog()
    fake = FakeClient(DRAFT)
    failure = FailureContext(
        kind="no_rule",
        digest="failure kind: no_rule\nrule_selected get_furnace rule=2",
        previous_source="skill get_furnace()\n  rules:\n    true -> done\n",
    )

    write(
        _goal(),
        catalog,
        failure=failure,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )

    task = fake.responses.calls[0]["input"][0]["content"]
    assert "skill get_furnace()" in task
    assert "no_rule" in task
    assert "rule_selected get_furnace rule=2" in task
    assert "get_furnace" in task
    entries = Ledger(tmp_path / "ledger.jsonl").entries()
    assert entries[0]["purpose"] == "repair_skill"


def test_repair_can_choose_a_stronger_model(tmp_path) -> None:
    fake = FakeClient(DRAFT)
    write(
        _goal(),
        load_catalog(),
        model="gpt-6.1-terra",
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    assert fake.responses.calls[0]["model"] == "gpt-6.1-terra"


def test_budget_refusal_happens_before_the_api_call(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("PRAXION_BUDGET_OPENAI", "0")
    fake = FakeClient(DRAFT)
    with pytest.raises(BudgetExceeded):
        write(
            _goal(),
            load_catalog(),
            client=fake,
            ledger=Ledger(tmp_path / "ledger.jsonl"),
            evidence=Evidence(tmp_path / "evidence"),
        )
    assert fake.responses.calls == []


def test_missing_parsed_output_is_an_error_but_the_cost_is_recorded(tmp_path) -> None:
    fake = FakeClient(None)
    ledger = Ledger(tmp_path / "ledger.jsonl")
    with pytest.raises(RuntimeError):
        write(
            _goal(),
            load_catalog(),
            client=fake,
            ledger=ledger,
            evidence=Evidence(tmp_path / "evidence"),
        )
    assert len(ledger.entries()) == 1
