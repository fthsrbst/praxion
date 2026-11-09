"""commands.py: control words, the Turkish/English fast path, validation."""

from __future__ import annotations

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


CAT = catalog("get_cobblestone", "get_wood", "get_furnace")


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
        ("GET 3 RAW IRON", "count(raw_iron) >= 3", None),
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
