"""Unit tests of the referee's SNBT inventory parser.

The real 26.3 replies live in ``fixtures/observed-26.3.txt``; the crafted ones
cover what the server can send but the captured inventory happened not to:
nested item components with their own ``id``/``count``, names with commas and
braces, missing counts and several stacks of the same item.
"""

from __future__ import annotations

import ast
from pathlib import Path

from praxion_harness.referee import Referee

FIXTURE = Path(__file__).parent / "fixtures" / "observed-26.3.txt"


class FakeRcon:
    def __init__(self, response: str):
        self.response = response
        self.commands = []

    def command(self, command: str) -> str:
        self.commands.append(command)
        return self.response


def inventory_of(out: str) -> dict:
    rcon = FakeRcon(out)
    ref = Referee(rcon, player="Bot1")
    assert rcon.commands == []  # The parser is pure; it only reads the reply.
    result = ref.player_inventory()
    assert rcon.commands == ["data get entity Bot1 Inventory"]
    return result


def observed_inventory_outputs() -> list:
    """Every ``data get entity ... Inventory`` reply in the 26.3 fixture."""
    lines = FIXTURE.read_text(encoding="utf-8").splitlines()
    outputs = []
    for index, line in enumerate(lines):
        if line.startswith("CMD: data get entity") and line.rstrip().endswith("Inventory"):
            value_line = lines[index + 1]
            assert value_line.startswith("OUT: "), value_line
            outputs.append(ast.literal_eval(value_line[len("OUT: "):]))
    return outputs


def test_player_inventory_reads_the_real_26_3_replies():
    outputs = observed_inventory_outputs()
    assert len(outputs) == 2
    assert inventory_of(outputs[0]) == {
        "minecraft:oak_log": 5,
        "minecraft:stone_pickaxe": 1,
    }
    # Components may nest braces, commas and quoted strings; only the
    # top-level id names the item.
    assert inventory_of(outputs[1]) == {"minecraft:diamond_sword": 1}


def test_player_inventory_accepts_id_before_count_and_missing_count():
    out = (
        'Bot1 has the following entity data: [{id: "minecraft:stone", count: 3}, '
        '{id: "minecraft:dirt"}]'
    )
    assert inventory_of(out) == {"minecraft:stone": 3, "minecraft:dirt": 1}


def test_player_inventory_sums_stacks_of_the_same_item():
    out = (
        'Bot1 has the following entity data: [{count: 2, Slot: 0b, id: "minecraft:oak_log"}, '
        '{count: 3, Slot: 1b, id: "minecraft:oak_log"}, '
        '{count: 4, Slot: 2b, id: "minecraft:stick"}]'
    )
    assert inventory_of(out) == {"minecraft:oak_log": 5, "minecraft:stick": 4}


def test_player_inventory_ignores_nested_component_fields():
    out = (
        'Bot1 has the following entity data: [{components: {"minecraft:enchantments": '
        '{"minecraft:levels": {id: "minecraft:fake", count: 9}}}, count: 1, Slot: 0b, '
        'id: "minecraft:diamond_sword"}]'
    )
    assert inventory_of(out) == {"minecraft:diamond_sword": 1}


def test_player_inventory_handles_a_name_with_a_comma_and_a_brace():
    # The custom name is a single-quoted SNBT string that happens to contain a
    # comma, braces and an `id:` token; none of it may split or rename the item.
    out = (
        'Bot1 has the following entity data: [{components: {"minecraft:custom_name": '
        '\'"Pick, {Axe} id: minecraft:dirt"\'}, count: 4, Slot: 3b, '
        'id: "minecraft:iron_axe"}]'
    )
    assert inventory_of(out) == {"minecraft:iron_axe": 4}


def test_player_inventory_is_empty_for_an_empty_or_missing_list():
    assert inventory_of("Bot1 has the following entity data: []") == {}
    assert inventory_of("No entity was found") == {}
    assert inventory_of('Bot1 has the following entity data: [{id: "minecraft:stone"') == {}
