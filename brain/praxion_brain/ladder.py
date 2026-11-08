"""The ladder: a fixed yardstick of twelve rungs over inventory facts.

The rungs come from section 4.1 of the Demo-2 proposal, in the table's
row-major order. A rung is met when its guard condition is ``True`` in the
current snapshot; an unverifiable condition counts as unmet. Progress is always
recomputed from the world, never from what the brain did (decision S10).

``progress`` reports every rung that is met, in ladder order, plus the first
unmet rung. The briefing prints ``(rung N/12)`` where ``N`` is that first unmet
rung's 1-based position (or 12 when the ladder is complete).

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` section 4.1.
"""

from __future__ import annotations

from typing import Mapping, Optional

from pydantic import BaseModel, Field

from .conditions import evaluate


class Rung(BaseModel):
    """One step of the ladder: a stable id and its inventory condition."""

    id: str
    condition: str


#: The twelve rungs, in dependency order. Data, not code: change it here.
RUNGS: tuple[Rung, ...] = (
    Rung(id="wood", condition="count(#logs) >= 1"),
    Rung(id="crafting_table", condition="has(crafting_table)"),
    Rung(id="wooden_pickaxe", condition="has(wooden_pickaxe)"),
    Rung(id="cobblestone", condition="count(cobblestone) >= 8"),
    Rung(id="stone_pickaxe", condition="has(stone_pickaxe)"),
    Rung(id="stone_sword", condition="has(stone_sword)"),
    Rung(id="furnace", condition="has(furnace)"),
    Rung(id="coal", condition="has(coal)"),
    Rung(id="iron_ore", condition="has(raw_iron)"),
    Rung(id="iron_ingot", condition="has(iron_ingot)"),
    Rung(id="iron_pickaxe", condition="has(iron_pickaxe)"),
    Rung(id="diamond", condition="has(diamond)"),
)


class LadderProgress(BaseModel):
    """What the ladder looks like in one inventory."""

    done: list[str] = Field(default_factory=list)
    next: Optional[str] = None
    met: int = 0
    total: int = len(RUNGS)
    #: 1-based ladder position of ``next``; ``total`` when the ladder is done.
    next_index: int = 0


def first_unmet(inventory: Mapping[str, int]) -> Optional[Rung]:
    """The first rung whose condition is not ``True``; ``None`` when all pass."""
    for rung in RUNGS:
        if evaluate(rung.condition, inventory) is not True:
            return rung
    return None


def progress(inventory: Mapping[str, int]) -> LadderProgress:
    """Every met rung in ladder order, plus the first unmet one."""
    done: list[str] = []
    first_index: Optional[int] = None
    for index, rung in enumerate(RUNGS, start=1):
        if evaluate(rung.condition, inventory) is True:
            done.append(rung.id)
        elif first_index is None:
            first_index = index
    return LadderProgress(
        done=done,
        next=RUNGS[first_index - 1].id if first_index is not None else None,
        met=len(done),
        total=len(RUNGS),
        next_index=first_index if first_index is not None else len(RUNGS),
    )


__all__ = ["LadderProgress", "RUNGS", "Rung", "first_unmet", "progress"]
