from __future__ import annotations

import json

import pytest

from praxion_brain.evidence import Evidence
from praxion_brain.ledger import BudgetExceeded, Ledger, estimate_cost
from praxion_brain.planner import (
    ESCALATE_AFTER,
    INPUT_ESTIMATE,
    MAX_GOALS,
    OUTPUT_ESTIMATE,
    PLAN_RULES,
    Plan,
    PlannedGoal,
    estimate,
    model_for,
    plan,
    public_briefing,
    shape_problem,
    system_prompt,
)
from praxion_brain.router import PUBLIC_EXCLUDE
from praxion_brain.skills import SkillCatalog, SkillInfo, load_catalog


class FakeUsage:
    input_tokens = 2_600
    output_tokens = 310


class FakeResponse:
    def __init__(self, parsed: Plan | None) -> None:
        self.output_parsed = parsed
        self.usage = FakeUsage()
        self.id = "resp_plan"


class FakeResponses:
    def __init__(self, parsed: Plan | None) -> None:
        self.parsed = parsed
        self.calls: list[dict] = []

    def parse(self, **kwargs):
        self.calls.append(kwargs)
        return FakeResponse(self.parsed)


class FakeClient:
    def __init__(self, parsed: Plan | None) -> None:
        self.responses = FakeResponses(parsed)


PLAN = Plan(
    thought="Önce odun toplayacağım, çünkü her şey odunla başlıyor.",
    goals=[
        PlannedGoal(text="get wood", condition="count(#logs) >= 4", skill="get_wood", args=["4"]),
        PlannedGoal(text="find a village", condition="", skill="find_village"),
    ],
)


def _catalog() -> SkillCatalog:
    def skill(name: str, params: list[str], doc: str) -> SkillInfo:
        return SkillInfo(
            name=name,
            params=params,
            doc=doc,
            source=f'skill {name}()\n  rules:\n    true -> mine("stone")\n',
            path=f"{name}.skill",
            origin="core",
        )

    return SkillCatalog(
        skills=[
            skill("get_wood", ["n"], "get logs"),
            skill("cut_log", [], "cut one log"),  # a helper: never listed
        ]
    )


def test_system_prompt_is_stable_lists_public_skills_and_states_the_rules() -> None:
    catalog = _catalog()
    prompt = system_prompt(catalog)
    assert prompt == system_prompt(catalog)
    assert PLAN_RULES in prompt
    assert "get_wood(n) - get logs" in prompt
    assert "cut_log" not in prompt.split("## Known skills")[1]
    assert "mine" in prompt  # the primitive catalog still comes from every skill
    # The rules that are the product.
    assert "There is NO fixed order" in prompt
    assert "1 to 5 goals" in prompt
    assert "skill` is the exact name of a known skill" in prompt
    assert "`null`" in prompt
    assert "#logs" in prompt and "#planks" in prompt
    assert "Turkish" in prompt and "first person" in prompt
    assert "blocked" in prompt
    assert "persona" in prompt
    assert "iron pickaxe" in prompt and "torches" in prompt and "chest" in prompt


def test_briefing_is_the_user_message_and_the_planner_model_is_default(tmp_path) -> None:
    fake = FakeClient(PLAN)
    ledger = Ledger(tmp_path / "ledger.jsonl")
    evidence = Evidence(tmp_path / "evidence")

    parsed = plan("## Now\nhealth 20/20", _catalog(), client=fake, ledger=ledger, evidence=evidence)

    assert parsed == PLAN
    call = fake.responses.calls[0]
    assert call["model"] == "gpt-6.1-terra"
    assert call["text_format"] is Plan
    assert call["instructions"] == system_prompt(_catalog())
    assert call["input"] == [{"role": "user", "content": "## Now\nhealth 20/20"}]
    assert call["reasoning"] == {"effort": "none"}

    entries = ledger.entries()
    assert len(entries) == 1
    assert entries[0]["purpose"] == "plan"
    assert entries[0]["input_tokens"] == 2_600
    assert entries[0]["output_tokens"] == 310

    lines = [json.loads(line) for line in evidence.path.read_text().splitlines()]
    assert [line["kind"] for line in lines] == ["plan_request", "plan_response"]
    assert lines[0]["briefing"] == "## Now\nhealth 20/20"
    assert lines[1]["parsed"]["goals"][0]["skill"] == "get_wood"
    assert lines[1]["response_id"] == "resp_plan"


@pytest.mark.parametrize(
    ("invalid_count", "after_death", "model"),
    [
        (0, False, "gpt-6.1-terra"),
        (ESCALATE_AFTER - 1, False, "gpt-6.1-terra"),
        (ESCALATE_AFTER, False, "gpt-6.1-sol"),
        (0, True, "gpt-6.1-sol"),
    ],
)
def test_the_call_escalates_after_two_invalid_plans_or_a_death(
    tmp_path, invalid_count, after_death, model
) -> None:
    assert model_for(invalid_count, after_death, "gpt-6.1-terra", "gpt-6.1-sol") == model
    fake = FakeClient(PLAN)
    plan(
        "b",
        _catalog(),
        invalid_count=invalid_count,
        after_death=after_death,
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    assert fake.responses.calls[0]["model"] == model


def test_the_escalated_call_thinks_longer(tmp_path) -> None:
    from praxion_brain.planner import DEFAULT_EFFORT, ESCALATION_EFFORT

    cases = [
        ({}, DEFAULT_EFFORT),
        ({"invalid_count": ESCALATE_AFTER}, ESCALATION_EFFORT),
        ({"after_death": True}, ESCALATION_EFFORT),
    ]
    for index, (kwargs, effort) in enumerate(cases):
        fake = FakeClient(PLAN)
        plan(
            "b",
            _catalog(),
            client=fake,
            ledger=Ledger(tmp_path / f"case{index}" / "ledger.jsonl"),
            evidence=Evidence(tmp_path / f"case{index}" / "evidence"),
            **kwargs,
        )
        assert fake.responses.calls[0]["reasoning"] == {"effort": effort}


def test_estimate_prices_the_model_the_call_will_use() -> None:
    cheap = estimate(0)
    assert cheap == pytest.approx(estimate_cost("gpt-6.1-terra", INPUT_ESTIMATE, OUTPUT_ESTIMATE))
    assert estimate(ESCALATE_AFTER) == pytest.approx(
        estimate_cost("gpt-6.1-sol", INPUT_ESTIMATE, OUTPUT_ESTIMATE)
    )
    assert estimate(0, after_death=True) > cheap


def test_a_response_without_a_parsed_plan_is_an_error(tmp_path) -> None:
    with pytest.raises(RuntimeError, match="no parsed Plan"):
        plan(
            "b",
            _catalog(),
            client=FakeClient(None),
            ledger=Ledger(tmp_path / "ledger.jsonl"),
            evidence=Evidence(tmp_path / "evidence"),
        )


def test_the_budget_stops_the_call_before_any_request(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("PRAXION_BUDGET_OPENAI", "0")
    fake = FakeClient(PLAN)
    ledger = Ledger(tmp_path / "ledger.jsonl")
    with pytest.raises(BudgetExceeded):
        plan("b", _catalog(), client=fake, ledger=ledger, evidence=Evidence(tmp_path / "evidence"))
    assert fake.responses.calls == []


def test_shape_problem() -> None:
    assert shape_problem(PLAN) is None
    assert shape_problem(Plan(thought="", goals=[])) == "no_goals"
    blank = Plan(thought="", goals=[PlannedGoal(text="  ", condition="has(x)")])
    assert shape_problem(blank) == "empty_text"
    long = Plan(
        thought="", goals=[PlannedGoal(text=f"g{n}", condition="has(x)") for n in range(MAX_GOALS + 2)]
    )
    assert shape_problem(long) is None  # the loop plays the first five


def test_the_rules_limit_the_plan_to_the_night_section_when_present() -> None:
    """Decision 2026-11-13: the briefing's ``## Night`` section restricts the
    plan, and the rule that says so is part of the stable prompt."""
    assert "`## Night` section" in PLAN_RULES
    assert "limits the plan to the\n  skills it lists" in PLAN_RULES
    assert "`## Night` section" in system_prompt(_catalog())


def test_the_rules_name_the_helper_skills_never_and_the_underground_rule() -> None:
    prompt = system_prompt(_catalog())
    # Only listed skills may be named; the helper rule must not leak a name.
    assert "is an internal helper, not a goal" in prompt
    for helper in PUBLIC_EXCLUDE:
        assert helper not in PLAN_RULES, helper
    # Underground: the first goal of a plan that needs the surface.
    assert "## Underground" in prompt
    assert "y is under 55" in prompt
    assert "no\nlogs or animals" in prompt
    assert "`go_to_surface`" in prompt
    assert "At night the `## Night` section decides" in prompt


def test_the_real_prompt_lists_go_to_surface_and_no_helper_anywhere(tmp_path) -> None:
    # An empty active directory: the machine's learned skills must not leak in.
    prompt = system_prompt(load_catalog(active_dir=tmp_path))
    known = prompt.split("## Known skills")[1]
    assert (
        "go_to_surface() - Return to the surface from underground (a tunnel, a pit or a "
        "cave): climbs until the body stands under open sky. Use it before any goal "
        "that needs trees, animals or exploring when the body is below ground."
    ) in known
    # The helpers are hidden from the skill list AND from the primitive and fact
    # sections the planner sees.
    for helper in PUBLIC_EXCLUDE:
        assert helper not in prompt, helper
    # The new primitive and fact come from the core skill like every other.
    assert "climb_out" in prompt.split("## Known skills")[0]
    assert "sky_above" in prompt.split("## Known skills")[0]


def test_public_briefing_cuts_the_helper_lines_of_the_skills_section_only() -> None:
    briefing = (
        "## Now  [live]\nhealth 20/20\n"
        "## Recent goals  [goals.jsonl]\n"
        "- cut_log(): done, 5 s ago\n"
        "## Skills  [catalog]\n"
        "get_wood(n: int = 1) - get logs\n"
        "cut_log() - cut one log\n"
        "search_surface(block: block) - walk\n"
        "go_to_surface() - climb\n"
        "## Budget  [ledger]\n"
    )
    cleaned = public_briefing(briefing)
    assert "cut_log() - cut one log" not in cleaned
    assert "search_surface(" not in cleaned
    assert "get_wood(n: int = 1) - get logs" in cleaned
    assert "go_to_surface() - climb" in cleaned
    # A helper named outside the Skills section (a goal's history) stays.
    assert "- cut_log(): done, 5 s ago" in cleaned
    assert cleaned.endswith("## Budget  [ledger]\n")
    assert public_briefing("## Now\nx") == "## Now\nx"


def test_the_planner_call_sends_the_briefing_without_helper_skill_lines(tmp_path) -> None:
    fake = FakeClient(PLAN)
    briefing = "## Skills  [catalog]\nget_wood(n) - logs\ncut_log() - one log\n"
    plan(
        briefing,
        _catalog(),
        client=fake,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    sent = fake.responses.calls[0]["input"][0]["content"]
    assert "cut_log" not in sent
    assert "get_wood(n) - logs" in sent


def test_go_to_surface_is_not_a_night_work_skill() -> None:
    # At night the body stays down: the surface is where the mobs are.
    from praxion_brain.survival import NIGHT_WORK_SKILLS

    assert "go_to_surface" not in NIGHT_WORK_SKILLS
