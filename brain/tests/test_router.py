from __future__ import annotations

import json

import pytest

from praxion_brain.evidence import Evidence
from praxion_brain.goal import Goal
from praxion_brain.ledger import Ledger
from praxion_brain.router import Route, decide, route
from praxion_brain.skills import SkillCatalog, SkillInfo


def _catalog(*names: str) -> SkillCatalog:
    return SkillCatalog(
        skills=[
            SkillInfo(
                name=name,
                doc=f"doc for {name}",
                source=f"skill {name}()\n  rules:\n    true -> done\n",
                path=f"{name}.skill",
                origin="core",
            )
            for name in names
        ]
    )


class FakeNoul:
    def __init__(self, value: float) -> None:
        self.noul = value


class FakeUsage:
    def __init__(self, input_tokens: int, output_tokens: int) -> None:
        self.input_tokens = input_tokens
        self.output_tokens = output_tokens


class FakeResponse:
    def __init__(self, scores: dict[str, float]) -> None:
        self.nouls = {name: FakeNoul(value) for name, value in scores.items()}
        self.usage = FakeUsage(120, 6)
        self.model = "jev-latest"


class FakeClient:
    def __init__(self, scores: dict[str, float]) -> None:
        self.scores = scores
        self.calls: list[dict] = []

    def system_one(self, *, state, questions):
        self.calls.append({"state": state, "questions": questions})
        return FakeResponse(self.scores)


def _goal() -> Goal:
    return Goal(text="have a furnace", item="furnace")


@pytest.mark.parametrize(
    ("scores", "kind", "skill"),
    [
        ({"get_wood": 0.9}, "play", "get_wood"),
        ({"get_wood": 0.8, "get_furnace": 0.75}, "play", "get_wood"),
        ({"get_wood": 0.95, "get_furnace": 0.91}, "play", "get_wood"),
        ({"get_wood": 0.2, "get_furnace": 0.1}, "write", None),
        ({"get_wood": 0.5, "get_furnace": 0.1}, "ask", None),
        ({"get_wood": 0.3}, "ask", None),
        ({"get_wood": 0.7}, "ask", None),
        ({"get_wood": 0.29}, "write", None),
    ],
)
def test_decision_table(scores, kind, skill) -> None:
    assert decide(scores) == (kind, skill)


def test_route_asks_one_question_per_skill(tmp_path) -> None:
    catalog = _catalog("get_wood", "get_planks")
    fake = FakeClient({"get_wood": 0.1, "get_planks": 0.2})
    ledger = Ledger(tmp_path / "ledger.jsonl")
    evidence = Evidence(tmp_path / "evidence")

    result = route(_goal(), catalog, client=fake, ledger=ledger, evidence=evidence)

    assert result.kind == "write"
    assert result.skill is None
    assert result.scores == {"get_wood": 0.1, "get_planks": 0.2}

    call = fake.calls[0]
    assert call["state"] == {
        "goal": "have a furnace",
        "item": "furnace",
        "skills": {
            name: {"doc": f"doc for {name}", "rules": skill.source}
            for name, skill in ((s.name, s) for s in catalog.skills)
        },
    }
    assert call["questions"]["get_wood"].instructions == (
        "Does running skill `skills.get_wood` give the player `furnace`?"
    )
    assert ledger.spent("typesafe") > 0

    kinds = [
        json.loads(line)["kind"]
        for line in (tmp_path / "evidence" / "brain.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert kinds == ["router_request", "router_response", "router_decision"]


def test_no_known_skill_writes_without_a_jev_call(tmp_path) -> None:
    class ExplodingClient:
        def system_one(self, **kwargs):
            raise AssertionError("Jev must not be called with no questions")

    result = route(
        _goal(),
        SkillCatalog(),
        client=ExplodingClient(),
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    assert result == Route(kind="write", skill=None, scores={})


def test_a_jev_error_is_not_a_no(tmp_path) -> None:
    class BrokenClient:
        def system_one(self, **kwargs):
            raise RuntimeError("Jev is down")

    with pytest.raises(RuntimeError, match="Jev is down"):
        route(
            _goal(),
            _catalog("get_wood"),
            client=BrokenClient(),
            ledger=Ledger(tmp_path / "ledger.jsonl"),
            evidence=Evidence(tmp_path / "evidence"),
        )


def test_a_missing_answer_is_an_error(tmp_path) -> None:
    fake = FakeClient({})
    with pytest.raises(RuntimeError, match="did not answer"):
        route(
            _goal(),
            _catalog("get_wood"),
            client=fake,
            ledger=Ledger(tmp_path / "ledger.jsonl"),
            evidence=Evidence(tmp_path / "evidence"),
        )


def test_an_exact_skill_name_plays_without_asking_jev(tmp_path) -> None:
    catalog = _catalog("get_wood", "get_furnace")
    fake = FakeClient({})
    result = route(
        _goal(),
        catalog,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    assert (result.kind, result.skill) == ("play", "get_furnace")
    assert fake.calls == []


def test_a_variant_item_plays_the_skill_for_its_last_word(tmp_path) -> None:
    catalog = _catalog("get_wood", "get_planks")
    fake = FakeClient({})
    goal = Goal(text="get four oak planks", item="oak_planks", count=4)
    result = route(
        goal,
        catalog,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    assert (result.kind, result.skill) == ("play", "get_planks")
    assert fake.calls == []


def _decision(tmp_path) -> dict:
    lines = (tmp_path / "evidence" / "brain.jsonl").read_text(encoding="utf-8").splitlines()
    return [json.loads(line) for line in lines if json.loads(line)["kind"] == "router_decision"][-1]


def test_helper_skills_are_hidden_from_the_jev_questions(tmp_path) -> None:
    catalog = _catalog("get_wood", "cut_log", "hunt_food", "find_stone", "get_planks")
    fake = FakeClient({"get_wood": 0.1, "get_planks": 0.2})

    result = route(
        _goal(),
        catalog,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )

    assert set(fake.calls[0]["questions"]) == {"get_wood", "get_planks"}
    assert set(fake.calls[0]["state"]["skills"]) == {"get_wood", "get_planks"}
    assert result.kind == "write"


def test_a_helper_is_not_played_by_the_exact_name_path(tmp_path) -> None:
    fake = FakeClient({"get_wood": 0.1})
    goal = Goal(text="get a crafting table near", item="crafting_table_near")
    result = route(
        goal,
        _catalog("get_wood", "get_crafting_table_near"),
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    assert result.skill is None
    assert fake.calls, "Jev was asked: the helper is no exact-name shortcut"


def test_the_hint_plays_at_half_a_point_even_when_another_skill_wins(tmp_path) -> None:
    catalog = _catalog("get_wood", "get_planks")
    fake = FakeClient({"get_wood": 0.5, "get_planks": 0.95})

    result = route(
        _goal(),
        catalog,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
        hint="get_wood",
    )

    assert (result.kind, result.skill) == ("play", "get_wood")
    decision = _decision(tmp_path)
    assert decision["hint"] == "get_wood" and decision["source"] == "hint"


def test_a_hint_jev_rejects_falls_back_to_the_normal_decision(tmp_path) -> None:
    catalog = _catalog("get_wood", "get_planks")
    fake = FakeClient({"get_wood": 0.29, "get_planks": 0.95})

    result = route(
        _goal(),
        catalog,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
        hint="get_wood",
    )

    assert (result.kind, result.skill) == ("play", "get_planks")
    decision = _decision(tmp_path)
    assert decision["hint"] == "get_wood" and decision["source"] == "jev"


def test_a_hint_that_is_not_a_public_skill_is_ignored(tmp_path) -> None:
    fake = FakeClient({"get_wood": 0.1})
    result = route(
        _goal(),
        _catalog("get_wood", "cut_log"),
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
        hint="cut_log",
    )
    assert result.kind == "write"


def test_the_exact_name_path_still_comes_first_with_a_hint(tmp_path) -> None:
    fake = FakeClient({})
    result = route(
        _goal(),
        _catalog("get_wood", "get_furnace"),
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
        hint="get_wood",
    )
    assert (result.kind, result.skill) == ("play", "get_furnace")
    assert fake.calls == []
    assert _decision(tmp_path)["source"] == "exact_name"


def test_a_hint_jev_does_not_reject_plays_even_when_it_is_not_the_best(tmp_path) -> None:
    # Run B, 2026-11-12: `hunt` scored 0.49 for a porkchop goal and was dropped.
    catalog = _catalog("hunt", "get_food")
    fake = FakeClient({"hunt": 0.49, "get_food": 0.38})

    result = route(
        _goal(),
        catalog,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
        hint="hunt",
    )

    assert (result.kind, result.skill) == ("play", "hunt")
    assert _decision(tmp_path)["source"] == "hint"
