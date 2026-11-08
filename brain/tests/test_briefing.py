from __future__ import annotations

from praxion_brain.briefing import build, estimate_tokens
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
