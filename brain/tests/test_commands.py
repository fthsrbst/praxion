"""commands.py: control words, the Turkish/English fast path, validation."""

from __future__ import annotations

import types

import pytest

from praxion_brain import commands
from praxion_brain.commands import CommandPlan, resolve
from praxion_brain.skills import SkillCatalog, SkillInfo


def catalog(*names: str) -> SkillCatalog:
    return SkillCatalog(
        skills=[
            SkillInfo(name=n, doc="d", source=f"skill {n}()\n  rules:\n    true -> done\n", path=f"{n}.skill", origin="core")
            for n in names
        ]
    )


CAT = catalog(
    "get_cobblestone",
    "get_wood",
    "get_furnace",
    "get_coal",
    "get_raw_iron",
    "get_iron_ingot",
    "get_torch",
    "get_chest",
    "get_iron_pickaxe",
    "find_village",
    "make_camp",
    "go_to_surface",
    "take_shelter",
)


def catalog_of(skills: dict[str, list[str]]) -> SkillCatalog:
    """A catalog whose skills carry parameters, e.g. ``{"hunt": ["mob: entity_kind"]}``."""
    return SkillCatalog(
        skills=[
            SkillInfo(
                name=name,
                params=params,
                doc="d",
                source=f"skill {name}({', '.join(params)})\n  rules:\n    true -> done\n",
                path=f"{name}.skill",
                origin="core",
            )
            for name, params in skills.items()
        ]
    )


def run(text, inventory=None, map_fn=None):
    return resolve(text, inventory=inventory or {}, catalog=CAT, memory=None, map_fn=map_fn)


@pytest.mark.parametrize(
    "word,control",
    [("dur", "stop"), ("DUR!", "stop"), ("dur?", "stop"), ("iptal", "cancel"),
     ("İptal", "cancel"), ("İPTAL!", "cancel"), ("durum", "status"),
     ("devam", "resume"), ("devam et", "resume"), ("status", "status"), ("carry on", "resume")],
)
def test_control_words_are_turkish_with_an_english_table(word, control) -> None:
    result = run(word)
    assert result.kind == "control" and result.control == control


@pytest.mark.parametrize(
    "text,condition,skill",
    [
        ("8 taş getir", "count(cobblestone) >= 8", "get_cobblestone"),
        ("8 TAŞ getir", "count(cobblestone) >= 8", "get_cobblestone"),
        ("bana 8 taşı getir lütfen", "count(cobblestone) >= 8", "get_cobblestone"),
        ("get cobblestone 8", "count(cobblestone) >= 8", "get_cobblestone"),
        ("get 8 cobblestone", "count(cobblestone) >= 8", "get_cobblestone"),
        ("get_cobblestone 8", "count(cobblestone) >= 8", "get_cobblestone"),
        ("odun topla", "count(#logs) >= 4", "get_wood"),
        ("collect wood", "count(#logs) >= 4", "get_wood"),
        ("fırın yap", "has(furnace)", "get_furnace"),
        ("get a furnace", "has(furnace)", "get_furnace"),
        # English capitals keep their i; Turkish İ lowers to plain i (review
        # finding 15: `I -> ı` everywhere broke "GET RAW IRON").
        ("GET 3 RAW IRON", "count(raw_iron) >= 3", "get_raw_iron"),
        ("TAŞ GETİR 8", "count(cobblestone) >= 8", "get_cobblestone"),
    ],
)
def test_the_fast_path_maps_common_forms_without_a_model(text, condition, skill) -> None:
    called = []
    result = run(text, map_fn=lambda sentence: called.append(sentence))
    assert result.kind == "goal", result
    assert result.proposal.condition == condition
    assert result.skill == skill
    assert result.via == "fast" and not called


def test_a_count_is_absolute_and_a_bare_verb_asks_for_more_than_held() -> None:
    assert run("odun topla", {"oak_log": 3}).proposal.condition == "count(#logs) >= 7"
    refused = run("8 taş getir", {"cobblestone": 9})
    assert refused.kind == "refused" and refused.reason == "already_met"
    assert "9 cobblestone" in refused.detail


def test_anything_else_goes_to_the_model_and_is_validated_in_code() -> None:
    plan = CommandPlan(text="iki fırın", condition="count(furnace) >= 2", skill="get_furnace")
    result = run("bana iki tane fırın lazım", map_fn=lambda _text: plan)
    assert result.kind == "goal" and result.via == "model"
    assert result.skill == "get_furnace"

    # A malformed condition from the model is refused, not forwarded.
    bad = CommandPlan(text="x", condition="count(stone >= ")
    assert run("garip bir şey", map_fn=lambda _t: bad).reason == "malformed_condition"

    # An unknown skill name is dropped (the router decides), not trusted.
    odd = CommandPlan(text="x", condition="count(stick) >= 4", skill="get_nothing")
    assert run("dört çubuk", map_fn=lambda _t: odd).skill is None

    # The model says it cannot: unsupported, with its reason.
    no = CommandPlan(text="", condition="", unsupported="a house is not an inventory result")
    result = run("ev yap", map_fn=lambda _t: no)
    assert result.kind == "unsupported" and "house" in result.detail
    # No mapper at all: unsupported too.
    assert run("ev yap").kind == "unsupported"


def test_high_risk_goals_are_refused_whatever_the_model_says() -> None:
    plan = CommandPlan(text="elmas", condition="count(diamond) >= 3", risk="low")
    assert run("üç elmas bul", map_fn=lambda _t: plan).reason == "high_risk"
    assert run("elmas getir").reason == "high_risk"
    assert commands.risk_of("has(netherite_ingot)") == "high"
    assert commands.risk_of("count(cobblestone) >= 8") == "low"


def test_an_unverifiable_condition_is_refused() -> None:
    plan = CommandPlan(text="masaya yakın ol", condition="near(crafting_table, 3)")
    assert run("masaya git", map_fn=lambda _t: plan).reason == "not_verifiable"


def test_turkish_number_words_count() -> None:
    assert run("üç taş getir").proposal.condition == "count(cobblestone) >= 3"
    assert run("bir fırın yap").proposal.condition == "has(furnace)"


def test_a_counted_craftable_item_is_a_count_not_a_bare_has() -> None:
    # "2 fırın yap" with one furnace in the bag is not already met (review
    # finding 8: `has(furnace)` answered it "zaten var").
    result = run("2 fırın yap")
    assert result.proposal.condition == "count(furnace) >= 2"
    assert result.proposal.text == "2 furnace yap"
    assert run("iki fırın yap", {"furnace": 1}).kind == "goal"
    assert run("bir fırın yap").proposal.condition == "has(furnace)"


def test_a_noun_without_a_verb_is_not_a_command() -> None:
    # "taş kazma" alone is the stone pickaxe noun, not "mine stone": the phrase
    # table must not override the verb reading (review finding 15).
    assert commands.fast_path("taş kazma", {}) is None
    proposal, _skill = commands.fast_path("taş kazma yap", {})
    assert proposal.condition == "has(stone_pickaxe)"


@pytest.mark.parametrize(
    "text,condition,skill",
    [
        ("kömür bul", "count(coal) >= 4", "get_coal"),
        ("8 kömür getir", "count(coal) >= 8", "get_coal"),
        ("ham demir kaz", "count(raw_iron) >= 4", "get_raw_iron"),
        ("demir cevheri kaz", "count(raw_iron) >= 4", "get_raw_iron"),
        ("raw iron getir", "count(raw_iron) >= 4", "get_raw_iron"),
        ("3 demir külçe getir", "count(iron_ingot) >= 3", "get_iron_ingot"),
        ("demir getir", "count(iron_ingot) >= 4", "get_iron_ingot"),
        ("iron ingot getir", "count(iron_ingot) >= 4", "get_iron_ingot"),
        ("meşale yap", "has(torch)", "get_torch"),
        ("2 meşale yap", "count(torch) >= 2", "get_torch"),
        ("sandık yap", "has(chest)", "get_chest"),
        ("chest yap", "has(chest)", "get_chest"),
        ("demir kazma yap", "has(iron_pickaxe)", "get_iron_pickaxe"),
        ("fırın yap", "has(furnace)", "get_furnace"),
    ],
)
def test_the_fast_path_knows_the_new_items(text, condition, skill) -> None:
    called = []
    result = run(text, map_fn=lambda sentence: called.append(sentence))
    assert result.kind == "goal", result
    assert result.proposal.condition == condition
    assert result.skill == skill
    assert result.via == "fast" and not called


@pytest.mark.parametrize(
    "text,skill",
    [
        ("köy bul", "find_village"),
        ("köy bul!", "find_village"),
        ("find village", "find_village"),
        ("kamp kur", "make_camp"),
        ("base kur", "make_camp"),
        ("make camp", "make_camp"),
        ("sığınak yap", "take_shelter"),
        ("saklan", "take_shelter"),
        ("yüzeye çık", "go_to_surface"),
        ("yukarı çık!", "go_to_surface"),
        ("go to the surface", "go_to_surface"),
        ("Go to the surface, please", "go_to_surface"),
    ],
)
def test_a_known_skill_task_skips_the_model(text, skill) -> None:
    called = []
    result = run(text, map_fn=lambda sentence: called.append(sentence))
    assert result.kind == "goal", result
    assert result.skill == skill and result.args == []
    assert result.proposal is not None and result.proposal.condition == ""
    assert result.proposal.text
    assert result.validation is not None
    assert result.validation.ok and result.validation.unverifiable
    assert result.via == "fast" and not called


def test_a_skill_task_falls_through_when_the_skill_is_missing() -> None:
    called = []
    plan = CommandPlan(text="köy bul", condition="", skill="find_village")
    result = resolve(
        "köy bul",
        inventory={},
        catalog=catalog(),
        memory=None,
        map_fn=lambda text: called.append(text) or plan,
    )
    # The model named a skill the catalog does not have: unsupported, and the
    # Turkish detail names the missing skill.
    assert called == ["köy bul"]
    assert result.kind == "unsupported" and "find_village" in result.detail


def test_a_model_skill_task_carries_its_arguments() -> None:
    cat = catalog_of({"hunt": ["mob: entity_kind", "drop: item"]})
    plan = CommandPlan(text="2 inek avla", condition="", skill="hunt", args=["cow", "beef"])
    result = resolve("2 inek avla", inventory={}, catalog=cat, memory=None, map_fn=lambda _t: plan)
    assert result.kind == "goal" and result.via == "model"
    assert result.skill == "hunt" and result.args == ["cow", "beef"]
    assert result.proposal is not None and result.proposal.condition == ""
    assert result.validation is not None and result.validation.unverifiable


def test_argument_counts_follow_the_skills_parameters() -> None:
    cat = catalog_of({"hunt": ["mob: entity_kind", "drop: item"], "find_village": []})
    # More arguments than parameters: refused ("köy ara" misses the fast-path
    # pattern, so the model answers).
    too_many = CommandPlan(text="köy bul", condition="", skill="find_village", args=["x"])
    result = resolve("köy ara", inventory={}, catalog=cat, memory=None, map_fn=lambda _t: too_many)
    assert result.kind == "refused" and result.reason == "bad_args"
    # Fewer than the required arguments (no default): refused.
    hunt = CommandPlan(text="avla", condition="", skill="hunt", args=["cow"])
    result = resolve("avla", inventory={}, catalog=cat, memory=None, map_fn=lambda _t: hunt)
    assert result.kind == "refused" and result.reason == "bad_args"


def test_trailing_arguments_with_defaults_may_be_omitted() -> None:
    cat = catalog_of({"get_coal": ["n: int = 3"]})
    plan = CommandPlan(text="kömür bul", condition="", skill="get_coal")
    # "bana kömür lazım" has no verb, so the fast path leaves it to the model.
    result = resolve("bana kömür lazım", inventory={}, catalog=cat, memory=None, map_fn=lambda _t: plan)
    assert result.kind == "goal" and result.skill == "get_coal" and result.args == []


def test_an_unverifiable_condition_with_a_known_skill_becomes_a_skill_task() -> None:
    cat = catalog_of({"get_crafting_table": []})
    plan = CommandPlan(text="masaya yakın ol", condition="near(crafting_table, 3)", skill="get_crafting_table")
    result = resolve("masaya git", inventory={}, catalog=cat, memory=None, map_fn=lambda _t: plan)
    assert result.kind == "goal" and result.skill == "get_crafting_table"
    # The body would drop an unverifiable condition, so the goal is the skill's
    # own task: the raw guard text is never accepted, shown or sent.
    assert result.proposal is not None and result.proposal.condition == ""
    assert result.args == []
    assert result.validation is not None and result.validation.unverifiable

    # A guard text that is not an inventory condition is never a goal either,
    # even when the named skill is unrelated to it.
    odd = CommandPlan(text="çan", condition="count($p) >= 1", skill="get_torch")
    result = run("çan", map_fn=lambda _t: odd)
    assert result.kind == "goal" and result.skill == "get_torch"
    assert result.proposal.condition == ""
    # Without a known skill the same condition is still refused (as today).
    assert run("masaya git", map_fn=lambda _t: plan).reason == "not_verifiable"


def test_an_inventory_goal_never_carries_the_maps_arguments() -> None:
    # `args` belong to a skill task; an inventory goal derives them from the
    # condition (review finding 1).
    plan = CommandPlan(
        text="2 meşale", condition="count(torch) >= 2", skill="get_torch", args=["junk"]
    )
    result = run("bana iki meşale lazım", map_fn=lambda _t: plan)
    assert result.kind == "goal" and result.proposal.condition == "count(torch) >= 2"
    assert result.args == []


def test_a_high_risk_skill_task_is_refused_whatever_the_model_says() -> None:
    cat = catalog_of({"hunt": ["mob: entity_kind", "drop: item"]})
    plan = CommandPlan(
        text="ejderha avla",
        condition="",
        skill="hunt",
        args=["ender_dragon", "dragon_egg"],
        risk="low",
    )
    result = resolve("ejderha avla", inventory={}, catalog=cat, memory=None, map_fn=lambda _t: plan)
    # The skill's own arguments are on the same code list as a condition's item.
    assert result.kind == "refused" and result.reason == "high_risk"


def test_a_skill_task_argument_must_be_a_plain_word() -> None:
    cat = catalog_of({"hunt": ["mob: entity_kind", "drop: item"]})
    plan = CommandPlan(
        text="avla",
        condition="",
        skill="hunt",
        args=["count(diamond) >= 1", "beef"],
    )
    result = resolve("avla", inventory={}, catalog=cat, memory=None, map_fn=lambda _t: plan)
    # Guard syntax would turn the skill call into a condition: refused here
    # rather than accepted and then refused at pick time (review finding 3).
    assert result.kind == "refused" and result.reason == "bad_args"
    assert result.detail and "plain argument" in result.detail


def test_a_compound_is_not_the_item_its_first_word_names() -> None:
    # "taş-kazma" is the stone pickaxe, not a request for stone (finding 5).
    assert commands.fast_path("taş-kazma yap", {}) is None
    assert commands.fast_path("taş_kazma yap", {}) is None
    proposal, _skill = commands.fast_path("taş kazma yap", {})
    assert proposal.condition == "has(stone_pickaxe)"


def test_a_helper_named_task_is_unsupported() -> None:
    cat = catalog("cut_log", "get_wood")
    plan = CommandPlan(text="kütük kes", condition="", skill="cut_log")
    result = resolve("kütük kes", inventory={}, catalog=cat, memory=None, map_fn=lambda _t: plan)
    assert result.kind == "unsupported" and "cut_log" in result.detail


class _FakeMapResponses:
    def __init__(self) -> None:
        self.calls: list[dict] = []

    def parse(self, **kwargs):
        self.calls.append(kwargs)
        return types.SimpleNamespace(output_parsed=None, usage=None, id="resp_map")


class _FakeMapClient:
    def __init__(self) -> None:
        self.responses = _FakeMapResponses()


def test_the_map_prompt_lists_only_public_skills(tmp_path) -> None:
    from praxion_brain.evidence import Evidence
    from praxion_brain.ledger import Ledger

    client = _FakeMapClient()
    commands.map_with_model(
        "kütük kes",
        catalog=catalog("cut_log", "get_wood"),
        inventory={},
        client=client,
        ledger=Ledger(tmp_path / "ledger.jsonl"),
        evidence=Evidence(tmp_path / "evidence"),
    )
    known = client.responses.calls[0]["instructions"].split("## Known skills")[1]
    # A helper must never be mapped to; Jev once scored it level with get_wood
    # and it played instead (run A, `no_block`, review finding 1).
    assert "cut_log" not in known
    assert "get_wood" in known


def test_the_command_plan_limits_its_arguments() -> None:
    from pydantic import ValidationError

    assert CommandPlan(text="x", condition="").args == []
    with pytest.raises(ValidationError):
        CommandPlan(text="x", condition="", args=["a", "b", "c", "d", "e"])
    with pytest.raises(ValidationError):
        CommandPlan(text="x", condition="", args=["x" * 41])
    assert CommandPlan(text="x", condition="", args=["cow"]).args == ["cow"]


def test_the_map_prompt_shows_the_two_shapes_and_the_examples() -> None:
    rules = commands.MAP_RULES
    assert commands.MAP_MODEL == "gpt-6.1-terra"
    for needle in (
        "köy bul",
        "find_village",
        "count(iron_ingot) >= 3",
        "make_camp",
        "hunt",
        "unsupported",
        "dans et",
    ):
        assert needle in rules
    assert "condition" in rules and "args" in rules


def test_the_surface_task_needs_the_skill_in_the_catalog() -> None:
    # Without `go_to_surface` the sentence falls through to the model.
    called = []
    result = resolve(
        "yüzeye çık",
        inventory={},
        catalog=catalog("get_wood"),
        memory=None,
        map_fn=lambda sentence: called.append(sentence),
    )
    assert result.via != "fast"
    assert called == ["yüzeye çık"]
