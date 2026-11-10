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

from .conditions import evaluate, item_of_condition
from .curriculum import GoalProposal
from .skills import SkillCatalog


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


def _met(inventory: Mapping[str, int]) -> list[bool]:
    """Which rungs are met. The ladder is cumulative: a later rung met means
    every earlier one is too (a crafting table made from the only logs still
    passed the wood rung; without this the loop went back to wood after the
    table, first autonomous run 2026-11-08)."""
    direct = [evaluate(rung.condition, inventory) is True for rung in RUNGS]
    met = [False] * len(RUNGS)
    later = False
    for index in range(len(RUNGS) - 1, -1, -1):
        later = later or direct[index]
        met[index] = later
    return met


def first_unmet(inventory: Mapping[str, int]) -> Optional[Rung]:
    """The first rung not met (cumulatively); ``None`` when all pass."""
    for rung, met in zip(RUNGS, _met(inventory)):
        if not met:
            return rung
    return None


def progress(inventory: Mapping[str, int]) -> LadderProgress:
    """Every met rung in ladder order, plus the first unmet one."""
    met = _met(inventory)
    done = [rung.id for rung, ok in zip(RUNGS, met) if ok]
    first_index = next((index for index, ok in enumerate(met, start=1) if not ok), None)
    return LadderProgress(
        done=done,
        next=RUNGS[first_index - 1].id if first_index is not None else None,
        met=len(done),
        total=len(RUNGS),
        next_index=first_index if first_index is not None else len(RUNGS),
    )


class CodeOnlyGoal(BaseModel):
    """The code-only fallback: a ladder goal plus the skill that covers it."""

    goal: GoalProposal
    skill: str


def code_only_goal(
    inventory: Mapping[str, int], catalog: SkillCatalog
) -> Optional[CodeOnlyGoal]:
    """The first unmet rung, if an active or core skill already covers it.

    This is the governor's code-only mode: no model is called, and when no
    known skill covers the next rung the pick ends as ``nothing_to_do`` and
    backs off.
    """
    rung = first_unmet(inventory)
    if rung is None:
        return None

    candidates = [f"get_{rung.id}"]
    item = item_of_condition(rung.condition)
    if item is not None and not item.startswith("#"):
        name = f"get_{item}"
        if name not in candidates:
            candidates.insert(0, name)
    for name in candidates:
        if catalog.by_name(name) is not None:
            return CodeOnlyGoal(
                goal=GoalProposal(
                    text=f"get {rung.id.replace('_', ' ')}",
                    condition=rung.condition,
                    rung=rung.id,
                    rationale="code-only fallback",
                ),
                skill=name,
            )
    return None


__all__ = [
    "CodeOnlyGoal",
    "LadderProgress",
    "RUNGS",
    "Rung",
    "code_only_goal",
    "first_unmet",
    "progress",
]
