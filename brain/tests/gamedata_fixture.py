"""A small hand-written gamedata export for brain tests (no body, no jar)."""

from __future__ import annotations

from praxion_brain.needs import Gamedata, Recipe, Smelt

OAK = ["oak_planks"]
BIRCH = ["birch_planks"]


def gamedata(jar_sha256: str = "0" * 64) -> Gamedata:
    return Gamedata(
        version="26.3",
        jar_sha256=jar_sha256,
        recipes={
            "furnace": Recipe(out=1, table=True, inputs={"cobblestone": 8}),
            "crafting_table": Recipe(out=1, table=False, inputs={"#planks": 4}),
            "stick": Recipe(out=4, table=False, inputs={"#planks": 2}),
            "wooden_pickaxe": Recipe(out=1, table=True, inputs={"#planks": 3, "stick": 2}),
            "stone_pickaxe": Recipe(out=1, table=True, inputs={"cobblestone": 3, "stick": 2}),
            "stone_sword": Recipe(out=1, table=True, inputs={"cobblestone": 2, "stick": 1}),
            "oak_planks": Recipe(out=4, table=False, inputs={"#oak_logs": 1}),
            "birch_planks": Recipe(out=4, table=False, inputs={"#birch_logs": 1}),
            # An iron ingot also crafts from a block; the furnace route must win.
            "iron_ingot": Recipe(out=9, table=False, inputs={"iron_block": 1}),
        },
        smelting={
            "iron_ingot": Smelt(inputs=["raw_iron", "iron_ore"], seconds=10),
            # Smelting coal ore only gives the coal mining already gives.
            "coal": Smelt(inputs=["coal_ore", "deepslate_coal_ore"], seconds=10),
        },
        tags={
            "logs": ["oak_log", "birch_log"],
            "oak_logs": ["oak_log"],
            "birch_logs": ["birch_log"],
            "planks": ["birch_planks", "oak_planks"],
        },
    )
