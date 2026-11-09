"""When the body must eat or hide: goals decided in code, never by a model.

Decision 2026-11-09 "Five next areas" (survival): find food when hungry; at
night, armed and healthy hunt the hostiles that come near, else take shelter.
Design: ``logs/mac1109/design/survival.out.md`` (D10: the conditions are
verifiable) and its critique.

The body already eats what it holds (the arbiter's meal) and wakes the brain with
``hunger_no_food`` when it has nothing to eat. This module is the brain half: it
reads the snapshot facts and names the survival goal that is due, if any. The
loop plays it ahead of every ladder goal. Survival goals are built here, not
proposed, so they never pass through the need gate's ``waiting``/``not_needed``
rules (backlog "Survival exemption in the need gate"): a hungry body must not be
refused a food goal because the furnace is covered.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Mapping, Optional, Sequence

from .conditions import FOOD_ITEMS, evaluate, normalize_item
from .curriculum import GoalProposal, Validation
from .skills import SkillCatalog

#: Food at or below this with no food held starts ``get_food``. It is the body's
#: own ``eat_hard_threshold`` (``body/config/reflexes.toml``): the point where
#: the arbiter eats at once and, with nothing to eat, wakes the brain.
HUNGRY_AT = 6
#: Food items ``get_food`` collects.
FOOD_GOAL_COUNT = 2
#: Health at or below this, or no sword, sends the body into a shelter at night.
SHELTER_HEALTH = 10

FOOD_SKILL = "get_food"
NIGHT_SKILL = "night_watch"
SHELTER_SKILL = "take_shelter"

#: The survival skills, so a live one in the snapshot's stack means "already
#: running", never "start another".
SURVIVAL_SKILLS = frozenset({FOOD_SKILL, NIGHT_SKILL, SHELTER_SKILL})

#: The goal conditions: verifiable by the body at settle time.
FOOD_CONDITION = f"count_food() >= {FOOD_GOAL_COUNT}"
NIGHT_CONDITION = "not is_night()"

#: Stack states a live intent can be in (``IntentState`` minus ``done``).
LIVE_STATES = frozenset({"running", "suspended", "frozen_for_carry_on"})

#: Ticks a night goal may run. Night lasts 10 000 ticks (``day_time`` 13 000 to
#: 23 000, ``praxion_facts::is_night``) and the body's default per-goal budget is
#: 6 000, so a night goal sent with the default would settle ``failed(budget)``
#: mid-night and the loop would pick the very same goal again before dawn: two
#: failed goals every night, a failure streak, and no ladder work in between
#: (review of slice 3). Food keeps the default budget: a food goal that never
#: ends is a bug, a night goal that ends at dawn is the design.
NIGHT_BUDGET_TICKS = 12_000


@dataclass(frozen=True)
class SurvivalGoal:
    """One survival goal due now: the skill that plays it and why."""

    kind: str  # "food" | "night"
    skill: str
    proposal: GoalProposal
    reason: str

    def validation(self) -> Validation:
        return Validation(ok=True, skill_name=self.skill, unverifiable=False, met=False)

    @property
    def budget_ticks(self) -> Optional[int]:
        """The per-goal budget override; ``None`` leaves the loop's default."""
        return NIGHT_BUDGET_TICKS if self.kind == "night" else None


def _held_food(inventory: Mapping[str, int]) -> int:
    return sum(
        int(amount)
        for key, amount in inventory.items()
        if normalize_item(str(key)) in FOOD_ITEMS
    )


def _armed(inventory: Mapping[str, int]) -> bool:
    return any(
        normalize_item(str(key)).endswith("_sword") and int(amount) > 0
        for key, amount in inventory.items()
    )


def _live_skills(stack: Optional[Sequence[Mapping[str, Any]]]) -> frozenset[str]:
    """The survival skills the body is already running or holding.

    The body's own night fallback pushes ``night_watch`` at the survival tier;
    a slow but connected brain reads a snapshot taken before or while that
    intent exists, and without this check it would send a second one, which the
    body then plays as a command goal in front of its own fallback (review of
    slice 3). A suspended entry counts: a user command owns the body right now
    and the survival goal gets it back afterwards.
    """
    return frozenset(
        str(entry.get("skill") or "")
        for entry in stack or ()
        if entry.get("state") in LIVE_STATES
        and str(entry.get("skill") or "") in SURVIVAL_SKILLS
    )


def choose(
    facts: Optional[Mapping[str, Any]],
    inventory: Mapping[str, int],
    catalog: SkillCatalog,
    memory: Any = None,
    stack: Optional[Sequence[Mapping[str, Any]]] = None,
) -> Optional[SurvivalGoal]:
    """The survival goal due now, or ``None``.

    Hunger first: food at :data:`HUNGRY_AT` or below with no food held. Then
    night: ``night_watch`` when armed and healthy, ``take_shelter`` when hurt or
    unarmed. A goal whose skill the catalog lacks, or that failure memory blocks,
    is skipped (the body's own fallback covers the night). A skill already live
    in ``stack`` is not started twice. ``facts`` is the snapshot's body facts;
    without them nothing is decided.
    """
    if not facts:
        return None
    live = _live_skills(stack)

    def usable(skill: str, condition: str) -> bool:
        if skill in live:
            # Already running (the body's fallback, or a goal of ours the
            # snapshot still shows): a second copy only fights the first.
            return False
        if catalog.by_name(skill) is None:
            return False
        if memory is not None and memory.is_blocked(condition):
            return False
        # A goal already true needs nothing (the facts are the same ones the body
        # will evaluate at settle time).
        return evaluate(condition, inventory, facts) is not True

    food = facts.get("food")
    if (
        isinstance(food, (int, float))
        and not isinstance(food, bool)
        and food <= HUNGRY_AT
        and _held_food(inventory) == 0
        and usable(FOOD_SKILL, FOOD_CONDITION)
    ):
        return SurvivalGoal(
            kind="food",
            skill=FOOD_SKILL,
            proposal=GoalProposal(
                text="get food",
                condition=FOOD_CONDITION,
                rationale="hungry with no food held",
            ),
            reason=f"food {int(food)} and nothing to eat",
        )

    if facts.get("is_night") is True:
        health = facts.get("health")
        hurt = isinstance(health, (int, float)) and health <= SHELTER_HEALTH
        skill = SHELTER_SKILL if hurt or not _armed(inventory) else NIGHT_SKILL
        if usable(skill, NIGHT_CONDITION):
            return SurvivalGoal(
                kind="night",
                skill=skill,
                proposal=GoalProposal(
                    text="take shelter" if skill == SHELTER_SKILL else "watch the night",
                    condition=NIGHT_CONDITION,
                    rationale="night",
                ),
                reason="night, hurt or unarmed" if skill == SHELTER_SKILL else "night, armed and healthy",
            )
    return None


__all__ = [
    "FOOD_CONDITION",
    "FOOD_GOAL_COUNT",
    "FOOD_SKILL",
    "HUNGRY_AT",
    "LIVE_STATES",
    "NIGHT_BUDGET_TICKS",
    "NIGHT_CONDITION",
    "NIGHT_SKILL",
    "SHELTER_HEALTH",
    "SHELTER_SKILL",
    "SURVIVAL_SKILLS",
    "SurvivalGoal",
    "choose",
]
