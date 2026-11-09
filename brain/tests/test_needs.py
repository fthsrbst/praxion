"""needs.py: expansion, coverage, loader with the jar hash, and the gate."""

from __future__ import annotations

import json

import pytest

from praxion_brain import needs
from praxion_brain.needs import Gate

from gamedata_fixture import gamedata

DATA = gamedata()


def test_a_furnace_is_covered_by_eight_cobblestone() -> None:
    inventory = {"cobblestone": 96, "crafting_table": 1}
    assert needs.covered("furnace", inventory, DATA)
    # The table is not counted as an inventory need: the body places its own.
    assert needs.covered("furnace", {"cobblestone": 96}, DATA)


def test_missing_for_names_the_shortfall() -> None:
    short = needs.missing_for("furnace", {"cobblestone": 3}, DATA)
    assert [(n.item, n.missing) for n in short] == [("cobblestone", 5)]
    assert needs.missing_for("furnace", {"minecraft:cobblestone": 8}, DATA) == []


def test_a_tag_need_counts_every_member_and_spends_the_inventory_once() -> None:
    # wooden_pickaxe: 3 planks + 2 sticks; the sticks (4 from 2 planks) are held.
    held = {"birch_planks": 13, "stick": 11}
    assert needs.covered("wooden_pickaxe", held, DATA)
    # Without sticks they are crafted from the same planks: 3 + 2 = 5 needed.
    assert needs.covered("wooden_pickaxe", {"birch_planks": 5}, DATA)
    assert not needs.covered("wooden_pickaxe", {"birch_planks": 4}, DATA)
    # Planks mix over wood types.
    assert needs.covered("wooden_pickaxe", {"birch_planks": 2, "oak_planks": 3}, DATA)


def test_a_multi_output_recipe_is_not_over_ordered() -> None:
    one = needs.explain("stick", {}, DATA, count=1, depth=0)
    four = needs.explain("stick", {}, DATA, count=4, depth=0)
    five = needs.explain("stick", {}, DATA, count=5, depth=0)
    assert one.unmet[0].missing == 2 and four.unmet[0].missing == 2
    assert five.unmet[0].missing == 4, "five sticks need two crafts"


def test_planks_are_crafted_from_logs_within_the_depth() -> None:
    # pickaxe -> #planks (short) -> oak_planks -> #oak_logs
    plan = needs.explain("wooden_pickaxe", {"oak_log": 2}, DATA)
    assert plan.covered, plan.unmet
    assert not needs.explain("wooden_pickaxe", {}, DATA).covered
    assert needs.explain("wooden_pickaxe", {}, DATA).unmet[0].item.endswith("_logs")


def test_smelting_is_a_second_source_and_wins_for_the_ingot() -> None:
    plan = needs.explain("iron_ingot", {"raw_iron": 1}, DATA)
    assert plan.mode == needs.MODE_SMELTING and plan.covered
    short = needs.explain("iron_ingot", {}, DATA)
    assert [n.item for n in short.unmet] == ["raw_iron"], "ore blocks are no input"


def test_mined_and_unknown_items_are_inert_or_unknown() -> None:
    coal = needs.explain("coal", {"cobblestone": 90}, DATA)
    assert coal.mode == needs.MODE_INERT and not coal.covered and not coal.gating
    assert needs.explain("raw_iron", {}, DATA).mode == needs.MODE_INERT
    assert needs.explain("#nothing", {}, DATA).mode == needs.MODE_UNKNOWN
    odd = gamedata()
    odd.recipes["widget"] = needs.Recipe(inputs={"#missing_tag": 1})
    assert needs.explain("widget", {}, odd).mode == needs.MODE_UNKNOWN


def test_the_need_line_reads_like_the_design() -> None:
    inventory = {"cobblestone": 96}
    rung = needs.rung_need("furnace", "has(furnace)", inventory, DATA)
    assert needs.need_line(rung, inventory, DATA) == (
        "furnace: 0 of 1; needs cobblestone 8 (have 96) at a crafting table -> covered"
    )
    inventory = {"cobblestone": 3}
    rung = needs.rung_need("furnace", "has(furnace)", inventory, DATA)
    assert needs.need_line(rung, inventory, DATA).endswith(
        "-> missing cobblestone 5 more"
    )
    coal = needs.rung_need("coal", "has(coal)", inventory, DATA)
    assert "gathered, not crafted" in needs.need_line(coal, inventory, DATA)


def test_the_need_line_names_a_concrete_member_for_a_tag() -> None:
    """The briefing must name an item the brain can evaluate (finding 6).

    The real jar's stone tools use ``#stone_tool_materials``; the evaluator
    knows only ``#logs``/``#planks``, so a goal spelled with the tag cannot be
    judged and the writer is left to guess. Name the member the run holds.
    """
    data = gamedata()
    data.tags["stone_tool_materials"] = [
        "blackstone",
        "cobbled_deepslate",
        "cobblestone",
    ]
    data.recipes["stone_pickaxe"] = needs.Recipe(
        out=1, table=True, inputs={"#stone_tool_materials": 3, "stick": 2}
    )
    holding_stone = {"cobblestone": 1, "stick": 2}
    rung = needs.rung_need("stone_pickaxe", "has(stone_pickaxe)", holding_stone, data)
    assert needs.need_line(rung, holding_stone, data).endswith(
        "-> missing cobblestone (or #stone_tool_materials) 2 more"
    )
    # Nothing held: every member would be a guess, so the tag stays.
    empty: dict[str, int] = {}
    rung = needs.rung_need("stone_pickaxe", "has(stone_pickaxe)", empty, data)
    assert "-> missing #stone_tool_materials 3 more" in needs.need_line(
        rung, empty, data
    )


def _gate(inventory, *, recent=(), brake=False, rung=("furnace", "has(furnace)")):
    return Gate(
        data=DATA,
        need=needs.rung_need(rung[0], rung[1], inventory, DATA),
        recent_items=tuple(recent),
        brake=brake,
    )


def test_check_6_rejects_a_covered_input() -> None:
    gate = _gate({"cobblestone": 96})
    rejection = needs.check(gate, "count(cobblestone) >= 96")
    assert rejection is not None and rejection.reason == "not_needed"
    assert needs.check(gate, "has(furnace)") is None, "the rung's own goal passes"
    assert needs.check(gate, "has(coal)") is None
    # Short of the input, gathering it is the right move.
    assert needs.check(_gate({"cobblestone": 3}), "count(cobblestone) >= 8") is None


def test_check_7_only_fires_while_the_rung_is_covered() -> None:
    covered = _gate({"cobblestone": 96}, recent=["dirt"])
    assert needs.check(covered, "count(dirt) >= 30").reason == "repeat_item"
    uncovered = _gate({"cobblestone": 3}, recent=["cobblestone"])
    assert needs.check(uncovered, "count(cobblestone) >= 8") is None, (
        "a second harvest after a craft used the stock is legitimate"
    )


def test_the_brake_allows_only_the_rung_and_its_direct_inputs() -> None:
    gate = _gate({"cobblestone": 3}, brake=True)
    assert needs.check(gate, "count(dirt) >= 30").reason == "not_needed"
    assert needs.check(gate, "count(cobblestone) >= 8") is None
    assert needs.check(gate, "has(furnace)") is None


def test_an_inert_rung_switches_the_gate_off() -> None:
    gate = _gate({"cobblestone": 96}, rung=("coal", "has(coal)"), brake=True, recent=["dirt"])
    assert needs.check(gate, "count(dirt) >= 30") is None
    assert needs.check(gate, "count(cobblestone) >= 96") is None


def test_load_is_none_without_a_file_and_checks_the_jar_hash(tmp_path) -> None:
    assert needs.load(tmp_path / "absent.json", jar=tmp_path / "no.jar") is None
    jar = tmp_path / "server.jar"
    jar.write_bytes(b"pinned jar")
    sha = needs.jar_sha256(jar)
    export = tmp_path / "26.3.json"
    export.write_text(json.dumps(gamedata(sha).model_dump()))
    assert needs.load(export, jar=jar).jar_sha256 == sha
    jar.write_bytes(b"a repinned jar")
    with pytest.raises(needs.StaleGamedata):
        needs.load(export, jar=jar)
    # No jar on this machine: the hash cannot be checked, the export is used.
    assert needs.load(export, jar=tmp_path / "gone.jar") is not None
    export.write_text("{not json")
    with pytest.raises(needs.GamedataError):
        needs.load(export, jar=jar)


def test_a_waiting_covered_rung_refuses_everything_but_a_later_rung() -> None:
    inventory = {"cobblestone": 96}
    gate = Gate(
        data=DATA,
        need=needs.rung_need("furnace", "has(furnace)", inventory, DATA),
        waiting=True,
        later_items=frozenset({"coal", "raw_iron"}),
    )
    assert needs.check(gate, "count(#logs) >= 4").reason == "not_needed"
    assert needs.check(gate, "count(stick) >= 12").reason == "not_needed"
    assert needs.check(gate, "has(coal)") is None, "a later rung is still worth playing"
    assert needs.check(gate, "has(raw_iron)") is None
    # Not waiting (the skill exists): the same logs goal is not refused here.
    open_gate = Gate(data=DATA, need=gate.need)
    assert needs.check(open_gate, "count(#logs) >= 4") is None


def test_a_compound_stock_goal_cannot_slip_past_the_waiting_rule() -> None:
    """One ``and`` used to take a stock goal past every rule (finding 3)."""
    inventory = {"cobblestone": 96}
    gate = Gate(
        data=DATA,
        need=needs.rung_need("furnace", "has(furnace)", inventory, DATA),
        waiting=True,
        later_items=frozenset({"coal", "raw_iron"}),
    )
    rejection = needs.check(gate, "count(#logs) >= 4 and count(stick) >= 12")
    assert rejection is not None and rejection.reason == "not_needed"
    assert "serves a later rung" in rejection.detail
    # A part that names a later rung keeps the compound worth playing.
    assert needs.check(gate, "has(coal) and count(#logs) >= 4") is None
    assert needs.check(gate, "has(furnace) and count(stick) >= 12") is None


def test_a_compound_goal_is_judged_by_the_brake_and_by_check_6() -> None:
    brake = _gate({"cobblestone": 3}, brake=True)
    rejection = needs.check(brake, "count(dirt) >= 30 and count(stick) >= 12")
    assert rejection is not None and rejection.reason == "not_needed"
    assert "drift brake" in rejection.detail
    assert needs.check(brake, "count(dirt) >= 30 and has(furnace)") is None
    assert needs.check(brake, "count(dirt) >= 30 and count(cobblestone) >= 8") is None

    covered = _gate({"cobblestone": 96})
    rejection = needs.check(covered, "has(cobblestone) and count(cobblestone) >= 8")
    assert rejection is not None and rejection.reason == "not_needed"
    assert "already covers it" in rejection.detail
