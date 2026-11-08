from __future__ import annotations

from praxion_brain.ladder import RUNGS, first_unmet, progress

FULL = {
    "oak_log": 3,
    "crafting_table": 1,
    "wooden_pickaxe": 1,
    "cobblestone": 8,
    "stone_pickaxe": 1,
    "stone_sword": 1,
    "furnace": 1,
    "coal": 2,
    "raw_iron": 3,
    "iron_ingot": 4,
    "iron_pickaxe": 1,
    "diamond": 1,
}


def test_the_table_is_the_twelve_rungs_in_order() -> None:
    assert [rung.id for rung in RUNGS] == [
        "wood",
        "crafting_table",
        "wooden_pickaxe",
        "cobblestone",
        "stone_pickaxe",
        "stone_sword",
        "furnace",
        "coal",
        "iron_ore",
        "iron_ingot",
        "iron_pickaxe",
        "diamond",
    ]
    assert RUNGS[0].condition == "count(#logs) >= 1"
    assert RUNGS[3].condition == "count(cobblestone) >= 8"
    assert RUNGS[8].condition == "has(raw_iron)"


def test_empty_inventory_starts_at_wood() -> None:
    meeting = first_unmet({})
    assert meeting is not None and meeting.id == "wood"
    state = progress({})
    assert state.done == []
    assert state.next == "wood"
    assert state.met == 0
    assert state.total == 12
    assert state.next_index == 1


def test_full_inventory_completes_the_ladder() -> None:
    assert first_unmet(FULL) is None
    state = progress(FULL)
    assert state.done == [rung.id for rung in RUNGS]
    assert state.next is None
    assert state.met == 12
    assert state.next_index == 12


def test_a_later_met_rung_meets_every_earlier_one() -> None:
    # A furnace in hand means the rungs below it were climbed, even if the
    # pickaxe broke since; the skills re-make their own prerequisites.
    inventory = {"oak_log": 1, "crafting_table": 1, "furnace": 1}
    state = progress(inventory)
    assert state.done == [rung.id for rung in RUNGS[: RUNGS.index(next(r for r in RUNGS if r.id == "furnace")) + 1]]
    assert state.next == "coal"


def test_eight_cobblestone_is_the_rung_threshold() -> None:
    assert first_unmet({"oak_log": 1, "crafting_table": 1, "wooden_pickaxe": 1}).id == "cobblestone"
    meeting = first_unmet(
        {"oak_log": 1, "crafting_table": 1, "wooden_pickaxe": 1, "cobblestone": 7}
    )
    assert meeting is not None and meeting.id == "cobblestone"
    meeting = first_unmet(
        {"oak_log": 1, "crafting_table": 1, "wooden_pickaxe": 1, "cobblestone": 8}
    )
    assert meeting is not None and meeting.id == "stone_pickaxe"


def test_the_ladder_is_cumulative() -> None:
    # The logs went into the table: wood still counts as met.
    inventory = {"crafting_table": 1}
    assert first_unmet(inventory).id == "wooden_pickaxe"
    assert progress(inventory).done[:2] == ["wood", "crafting_table"]
