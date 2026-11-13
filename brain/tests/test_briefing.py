from __future__ import annotations

import pytest

from praxion_brain.briefing import build, estimate_tokens, heartbeat_change
from praxion_brain.ladder import LadderProgress

SNAPSHOT = {
    "seq": 812,
    "tick": 4521,
    "generation": 7,
    "owner": "skill:3",
    "stack": [{"intent_id": 3, "tier": "command", "skill": "get_furnace"}],
    "facts": {
        "health": 20,
        "food": 18,
        "air": 300,
        "is_night": False,
        "day_time": 4521,
        "dimension": "overworld",
        "pos": [12, 64, -30],
        "inventory": {"oak_log": 3, "cobblestone": 8},
    },
}

LADDER = LadderProgress(
    done=["wood", "crafting_table", "wooden_pickaxe", "stone_pickaxe", "furnace"],
    next="coal",
    met=5,
    total=12,
    next_index=6,
)

RECENT = [
    {
        "goal_id": "g2",
        "text": "have a furnace",
        "condition": "has(furnace)",
        "status": "done",
        "skill": "get_furnace",
        "skill_origin": "written",
        "repairs": 2,
        "t_end": 1000.0,
    },
    {
        "goal_id": "g1",
        "text": "have coal",
        "condition": "has(coal)",
        "status": "failed",
        "failure_kind": "not_found",
        "attempts": 2,
        "blocked_until": 8241.0,
        "t_end": 900.0,
    },
]

CATALOG = [
    {"name": "get_furnace", "params": [], "doc": "get a furnace"},
    {"name": "get_wood", "params": ["n"], "doc": "get logs"},
]

LEDGER = {"last_hour": 0.31, "spent": 1.90, "budget": 4.50}

EXPECTED = """\
## Now  [live, tick 4521]
health 20/20, food 18/20, day_time 4521 (day), dimension overworld, pos 12,64,-30
inventory: cobblestone 8, oak_log 3
## Ladder  [computed from inventory now]
done: wood, crafting_table, wooden_pickaxe, stone_pickaxe, furnace   next: coal   (rung 6/12)
## Recent goals  [goals.jsonl]
- have a furnace: done, 41 s ago, skill get_furnace (written, 2 repairs)
- have coal: failed(not_found) x2 -> blocked for 2 h
## Skills  [catalog]
get_furnace() - get a furnace
get_wood(n) - get logs
## Budget  [ledger] $0.31 last hour, $1.90 of $4.50"""


def test_golden_briefing() -> None:
    text = build(SNAPSHOT, LADDER, RECENT, CATALOG, LEDGER, now=1041.0)
    assert text == EXPECTED


def test_empty_sections_are_explicit() -> None:
    empty_ladder = LadderProgress(done=[], next="wood", met=0, total=12, next_index=1)
    text = build(
        {"tick": 0, "facts": {"inventory": {}}},
        empty_ladder,
        [],
        [],
        {},
        now=0.0,
    )
    assert "get_" not in text
    assert "## Recent goals  [goals.jsonl]\n(none)" in text
    assert "## Skills  [catalog]\n(none)" in text
    assert "inventory: (empty)" in text
    assert "done: (none)   next: wood   (rung 1/12)" in text
    assert text.endswith("## Budget  [ledger] $0.00 last hour, $0.00 of $0.00")


def test_cap_trims_the_oldest_recent_goals_first() -> None:
    goals = [
        {
            "goal_id": f"g{index}",
            "text": f"goal number {index} " + "x" * 80,
            "condition": "has(coal)",
            "status": "done",
            "t_end": float(index),
        }
        for index in range(10)
    ]
    text = build(
        SNAPSHOT,
        LADDER,
        goals,
        CATALOG,
        LEDGER,
        now=1000.0,
        max_tokens=120,
    )

    assert estimate_tokens(text) <= 120
    assert "## Now  [live, tick 4521]" in text
    assert "health 20/20" in text
    assert "goal number 0 " not in text  # oldest goal trimmed
    assert "## Ladder" in text


def test_cap_never_trims_the_now_section() -> None:
    text = build(SNAPSHOT, LADDER, RECENT, CATALOG, LEDGER, now=1041.0, max_tokens=1)
    assert "## Now  [live, tick 4521]" in text
    assert "inventory: cobblestone 8, oak_log 3" in text
    assert estimate_tokens(text) > 1


def test_cap_is_a_hard_bound_for_one_huge_skill_line() -> None:
    catalog = [{"name": "get_huge", "params": [], "doc": "x" * 10000}]
    text = build(
        SNAPSHOT,
        LADDER,
        RECENT,
        catalog,
        LEDGER,
        now=1041.0,
        max_tokens=200,
    )
    assert estimate_tokens(text) <= 200
    assert "## Now  [live, tick 4521]" in text
    assert "inventory: cobblestone 8, oak_log 3" in text
    assert text.startswith("## Now")
    assert "get_huge()" in text  # the line is truncated, not dropped


def test_cap_is_a_hard_bound_for_a_huge_goal_line() -> None:
    goals = [
        {
            "goal_id": "g1",
            "text": "goal " + "y" * 3000,
            "condition": "has(coal)",
            "status": "done",
            "t_end": 1.0,
        }
    ]
    text = build(
        SNAPSHOT,
        LADDER,
        goals,
        CATALOG,
        LEDGER,
        now=1041.0,
        max_tokens=400,
    )
    assert estimate_tokens(text) <= 400
    assert "## Now  [live, tick 4521]" in text
    assert "goal y" not in text  # the huge goal was trimmed
    assert "## Recent goals  [goals.jsonl]\n(none)" in text

def test_heartbeat_change_is_a_diff_over_briefing_facts() -> None:
    before = {"health": 20, "food": 18, "is_night": False, "inventory": {"stone_pickaxe": 1}}
    after = {"health": 6, "food": 7, "is_night": True, "inventory": {"stone_pickaxe": 0}}
    reasons = heartbeat_change(before, after, was_playing=True, is_playing=False)
    assert reasons == [
        "no_running_intent",
        "low_health",
        "low_food",
        "night_began",
        "tool_lost:stone_pickaxe",
    ]


def test_heartbeat_change_is_empty_when_nothing_relevant_moved() -> None:
    facts = {"health": 20, "food": 18, "is_night": False, "inventory": {"oak_log": 3}}
    assert heartbeat_change(facts, dict(facts), was_playing=False, is_playing=False) == []
    # A non-tool item leaving the inventory is not a wake.
    assert heartbeat_change(
        {"inventory": {"oak_log": 3}}, {"inventory": {}}, was_playing=False, is_playing=False
    ) == []


def test_the_background_writing_line_names_the_skill() -> None:
    text = build(
        SNAPSHOT, LADDER, RECENT, CATALOG, LEDGER, now=1041.0, writing="get_furnace"
    )
    assert "## Background job  [admission]" in text
    assert "skill being written: get_furnace" in text
    # The line survives the cap: it is not one of the trimmable sections.
    capped = build(
        SNAPSHOT,
        LADDER,
        RECENT,
        CATALOG,
        LEDGER,
        now=1041.0,
        max_tokens=120,
        writing="get_furnace",
    )
    assert "skill being written: get_furnace" in capped
    assert estimate_tokens(capped) <= 120


def test_the_needs_line_is_its_own_section_before_the_budget() -> None:
    line = "furnace: 0 of 1; needs cobblestone 8 (have 96) -> covered"
    text = build(SNAPSHOT, LADDER, RECENT, CATALOG, LEDGER, now=1041.0, needs=line)
    lines = text.splitlines()
    header = lines.index("## Needs  [recipes + inventory]")
    assert lines[header + 1] == line
    assert lines[header + 2].startswith("## Budget"), lines[header + 2]
    assert "## Needs" not in build(SNAPSHOT, LADDER, RECENT, CATALOG, LEDGER, now=1041.0)


NEARBY = {
    "entities": {"cow": 2, "zombie": 1},
    "blocks": {"coal_ore": 3, "oak_log": 12, "lava": 0},
    "sounds": ["entity.creeper.primed", "block.stone.break"],
}


def _with_nearby(nearby) -> dict:
    facts = dict(SNAPSHOT["facts"])
    if nearby is not None:
        facts["nearby"] = nearby
    return {**SNAPSHOT, "facts": facts}


def test_surroundings_persona_and_knowledge_sections() -> None:
    text = build(
        _with_nearby(NEARBY),
        LADDER,
        RECENT,
        CATALOG,
        LEDGER,
        now=1041.0,
        persona="A bold miner.",
        knowledge=["Coal: found in stone layers.", "Torch: coal plus a stick."],
    )

    assert "## Persona  [this run]\nA bold miner." in text
    assert (
        "## Surroundings  [body perception]\n"
        "entities: cow 2, zombie 1\n"
        "blocks: coal_ore 3, oak_log 12\n"  # a zero count is omitted
        "sounds: entity.creeper.primed, block.stone.break"
    ) in text
    assert (
        "## Knowledge  [wiki]\nCoal: found in stone layers.\nTorch: coal plus a stick."
    ) in text
    assert text.index("## Now") < text.index("## Persona") < text.index("## Surroundings")
    assert text.index("## Skills") < text.index("## Knowledge") < text.index("## Budget")


@pytest.mark.parametrize("nearby", [None, {}, {"entities": {}, "blocks": {"lava": 0}, "sounds": []}])
def test_surroundings_are_omitted_when_there_is_nothing_to_say(nearby) -> None:
    text = build(_with_nearby(nearby), LADDER, RECENT, CATALOG, LEDGER, now=1041.0)
    assert "## Surroundings" not in text
    assert "## Persona" not in text and "## Knowledge" not in text
    assert text == EXPECTED


def test_cap_trims_knowledge_then_surroundings_after_the_skills_and_never_now() -> None:
    kwargs = dict(now=1041.0, persona="A bold miner.")
    knowledge = ["Coal: " + "y" * 300, "Torch: " + "y" * 300]
    full = build(
        _with_nearby(NEARBY), LADDER, RECENT, CATALOG, LEDGER, knowledge=knowledge, **kwargs
    )
    skills_gone = build(
        _with_nearby(NEARBY), LADDER, RECENT, [], LEDGER, knowledge=knowledge, **kwargs
    )

    # One token short of the full text: only the last recent goal and skills go
    # first, so the knowledge survives while there is room.
    text = build(
        _with_nearby(NEARBY),
        LADDER,
        RECENT,
        CATALOG,
        LEDGER,
        knowledge=knowledge,
        max_tokens=estimate_tokens(full) - 1,
        **kwargs,
    )
    assert estimate_tokens(text) <= estimate_tokens(full) - 1
    assert "## Knowledge" in text and "## Surroundings" in text

    # Far below the knowledge: the snippets go, then the surroundings, never Now.
    tight = build(
        _with_nearby(NEARBY),
        LADDER,
        RECENT,
        CATALOG,
        LEDGER,
        knowledge=knowledge,
        max_tokens=estimate_tokens(skills_gone) - 200,
        **kwargs,
    )
    assert "## Knowledge" not in tight
    assert "## Surroundings" not in tight
    assert "## Now  [live, tick 4521]" in tight
    assert "inventory: cobblestone 8, oak_log 3" in tight
    assert "## Persona" in tight


def test_the_night_section_limits_the_plan_to_the_listed_skills() -> None:
    text = build(
        SNAPSHOT,
        LADDER,
        RECENT,
        CATALOG,
        LEDGER,
        now=1041.0,
        night=["get_cobblestone", "get_coal"],
    )
    lines = text.splitlines()
    header = lines.index("## Night  [body facts + catalog]")
    assert lines[header + 1] == "it is night; work only underground or at the crafting table"
    assert lines[header + 2] == "allowed skills: get_coal, get_cobblestone"
    assert lines[header + 3] == (
        "never plan surface gathering, hunting or exploring before morning"
    )
    # A constraint belongs right after the live facts, before any wish list.
    assert text.index("## Now") < text.index("## Night") < text.index("## Ladder")
    # No night work, no section.
    assert "## Night" not in build(SNAPSHOT, LADDER, RECENT, CATALOG, LEDGER, now=1041.0)
    # A catalog with no night skill still says where the work is.
    empty = build(SNAPSHOT, LADDER, RECENT, CATALOG, LEDGER, now=1041.0, night=[])
    assert "allowed skills: (none available)" in empty


def test_the_night_section_survives_the_cap() -> None:
    text = build(
        SNAPSHOT,
        LADDER,
        RECENT,
        CATALOG,
        LEDGER,
        now=1041.0,
        max_tokens=120,
        night=["get_coal"],
    )
    assert "## Night  [body facts + catalog]" in text
    assert "allowed skills: get_coal" in text
    assert estimate_tokens(text) <= 120
