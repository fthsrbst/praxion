"""When the body must eat or hide: goals decided in code, never by a model.

Decision 2026-11-09 "Five next areas" (survival): find food when hungry; at
night, armed and healthy hunt the hostiles that come near, else take shelter.
Design: ``logs/mac1109/design/survival.out.md`` (D10: the conditions are
verifiable) and its critique.

Decision 2026-11-13 "night mining" (run B, seed 37960): in run B the bot
survived the night sealed in its shelter for nine minutes, half of every
20-minute day. A real player with a pickaxe spends the night mining, so a body
that holds any pickaxe and is healthy above :data:`SHELTER_HEALTH` starts no
night goal at all: :func:`night_work` is true, :func:`choose` returns no night
goal and :func:`night_hold` drops its night reasons, and the loop plans again,
restricted to :data:`NIGHT_WORK_SKILLS` (work that digs down, tunnels, crafts or
smelts). Without a pickaxe, or hurt, the night behaviour above is unchanged.
A body that is already sheltered keeps its running ``take_shelter``: it is not
interrupted mid-night by the new rule.

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
#: Food at or below this lets ``get_food`` hunt at night even unarmed or
#: sheltered (decision 2026-11-10 "Hungry at night: hunt only when it is
#: critical"). Above it such a body eats what it carries (the body's arbiter
#: meal) and does not hunt in the dark.
CRITICAL_FOOD_AT = 3
#: Food items ``get_food`` collects.
FOOD_GOAL_COUNT = 2
#: Health at or below this, or no sword, sends the body into a shelter at night.
SHELTER_HEALTH = 10

FOOD_SKILL = "get_food"
NIGHT_SKILL = "night_watch"
SHELTER_SKILL = "take_shelter"

#: The public skills a body may play at night while it works instead of
#: sheltering (decision 2026-11-13 "night mining"): they dig down, tunnel to
#: ore, craft or smelt, so they need no surface light and hunt nothing. The
#: loop refuses every other plan step at night (``night_unsafe``), and the
#: briefing lists the ones the catalog has under ``## Night``.
NIGHT_WORK_SKILLS = frozenset(
    {
        "get_cobblestone",
        "get_coal",
        "get_raw_iron",
        "find_ore",
        "get_iron_ingot",
        "get_iron_pickaxe",
        "get_furnace",
        "get_torch",
        "get_stone_pickaxe",
        "get_stone_sword",
        "get_stone_axe",
        "get_sticks",
        "get_planks",
        "get_crafting_table",
        "get_chest",
        "make_camp",
        "cook_food",
        "eat_food",
    }
)

#: The survival skills, so a live one in the snapshot's stack means "already
#: running", never "start another".
SURVIVAL_SKILLS = frozenset({FOOD_SKILL, NIGHT_SKILL, SHELTER_SKILL})
#: The night survival skills: the only survival goals critical food interrupts.
NIGHT_SKILLS = frozenset({NIGHT_SKILL, SHELTER_SKILL})

#: The goal conditions: verifiable by the body at settle time.
FOOD_CONDITION = f"count_food() >= {FOOD_GOAL_COUNT}"
NIGHT_CONDITION = "not is_night()"
#: The morning goal of a body still sealed in by day: ``take_shelter`` waits
#: until sunrise, then ``leave_shelter()`` opens the cover and the skill ends.
#: ``not is_night()`` is already true by day, so it cannot be this goal's
#: condition.
LEAVE_CONDITION = "not sheltered()"

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

#: Ticks a survival skill is not picked again after its goal failed (30 s).
#: Survival goals are never failure-blocked (decision 2026-11-10 "Survival goals
#: are never failure-blocked"): run 9's two failed shelters blocked the night
#: goal for two hours and the bot went gathering in the dark. The back-off is
#: kept by the loop in memory only, never persisted.
SURVIVAL_BACKOFF_TICKS = 600

#: The back-off doubles on each further failure of the same skill, up to this
#: (a failed night ``get_food`` must not reopen the shelter every 600 ticks all
#: night; fourth review of run 9, finding 2). A success resets it.
SURVIVAL_BACKOFF_MAX_TICKS = 6000

#: Consecutive failed ``take_shelter`` goals by day after which a body that
#: cannot get out stops holding ladder work (a ladder goal may dig out; the
#: hold must not last all day; fourth review, finding 1).
LEAVE_GIVE_UP = 3


@dataclass(frozen=True)
class SurvivalGoal:
    """One survival goal due now: the skill that plays it and why."""

    kind: str  # "food" | "night" | "leave"
    skill: str
    proposal: GoalProposal
    reason: str

    def validation(self) -> Validation:
        return Validation(ok=True, skill_name=self.skill, unverifiable=False, met=False)

    @property
    def budget_ticks(self) -> Optional[int]:
        """The per-goal budget override; ``None`` leaves the loop's default."""
        # A leave goal can start sealed at dusk and wait the whole night for
        # sunrise, so it gets the night budget too (fourth review, finding 3).
        return NIGHT_BUDGET_TICKS if self.kind in ("night", "leave") else None


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


def _holds_pickaxe(inventory: Mapping[str, int]) -> bool:
    """Whether any pickaxe is in the inventory (wooden to netherite)."""
    return any(
        normalize_item(str(key)).endswith("_pickaxe") and int(amount) > 0
        for key, amount in inventory.items()
    )


def night_work(
    facts: Optional[Mapping[str, Any]], inventory: Mapping[str, int]
) -> bool:
    """Whether the body should work underground instead of sheltering now.

    Decision 2026-11-13 "night mining" (run B, seed 37960): a pickaxe body
    spends the night mining. True when ``is_night`` is true, the world mirror is
    valid, a pickaxe is held and ``health`` is a number above
    :data:`SHELTER_HEALTH`; a hurt body shelters as it always did. Callers use
    it twice: :func:`choose` and :func:`night_hold` step aside for it, and the
    loop restricts the plan to :data:`NIGHT_WORK_SKILLS` while it holds.
    """
    if not facts or facts.get("world_valid") is False:
        return False
    if facts.get("is_night") is not True:
        return False
    if not _holds_pickaxe(inventory):
        return False
    health = facts.get("health")
    return (
        isinstance(health, (int, float))
        and not isinstance(health, bool)
        and health > SHELTER_HEALTH
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


def _backing_off(
    skill: str, backoff: Optional[Mapping[str, int]], tick: Optional[int]
) -> bool:
    """Whether ``skill`` failed less than :data:`SURVIVAL_BACKOFF_TICKS` ago."""
    until = (backoff or {}).get(skill)
    return until is not None and int(tick or 0) < until


def sealed_by_day(facts: Optional[Mapping[str, Any]]) -> bool:
    """Whether the body is sealed in its shelter while it is not night.

    Only ``take_shelter`` opens the shelter, so every night goal that ends
    early (a user goal, its budget, a recovery, a failed ``leave_shelter()``)
    leaves the body shut in, where every ladder goal fails. ``is_night()``
    false covers the hour before sunrise (23000 to 24000) too: there
    ``take_shelter`` waits sealed and opens at sunrise.
    """
    return (
        bool(facts)
        and facts.get("world_valid") is not False
        and facts.get("sheltered") is True
        and facts.get("is_night") is False
    )


def _sheltered(
    facts: Mapping[str, Any],
    stack: Optional[Sequence[Mapping[str, Any]]],
    inventory: Mapping[str, int],
) -> bool:
    """Whether the body is in its shelter: the ``sheltered`` fact when the
    facts carry it, else a live ``take_shelter`` in the stack.

    A snapshot read through :class:`~praxion_brain.port.Facts` always carries
    the key (the body sends it only when true, absent reads false); the stack
    fallback serves facts that lack the key altogether.

    The snapshot stack lists each intent's root skill only (the body's
    ``publish_state``), never the children a skill runs, so a ``take_shelter``
    that ``night_watch`` runs is invisible. ``night_watch`` runs it exactly when
    its own shelter rule holds (health 10 or less, or no sword): a live
    ``night_watch`` at night on that rule counts as sheltered too.
    """
    if "sheltered" in facts:
        return facts.get("sheltered") is True
    live = _live_skills(stack)
    if SHELTER_SKILL in live:
        return True
    return (
        NIGHT_SKILL in live
        and facts.get("is_night") is True
        and _night_skill(facts, inventory) == SHELTER_SKILL
    )


def critical_food(facts: Optional[Mapping[str, Any]]) -> bool:
    """Whether the food bar is at :data:`CRITICAL_FOOD_AT` or below.

    Only then may ``get_food`` interrupt an open night goal (decision
    2026-11-10 "Hungry at night: hunt only when it is critical").
    """
    food = (facts or {}).get("food")
    return (
        isinstance(food, (int, float))
        and not isinstance(food, bool)
        and food <= CRITICAL_FOOD_AT
    )


def _hunt_held_at_night(
    facts: Mapping[str, Any],
    food: float,
    inventory: Mapping[str, int],
    stack: Optional[Sequence[Mapping[str, Any]]],
) -> bool:
    """Whether a hungry body must not hunt now: night, unarmed or sheltered,
    and the food bar above :data:`CRITICAL_FOOD_AT`."""
    return (
        facts.get("is_night") is True
        and food > CRITICAL_FOOD_AT
        and (not _armed(inventory) or _sheltered(facts, stack, inventory))
    )


def _night_skill(facts: Mapping[str, Any], inventory: Mapping[str, int]) -> str:
    health = facts.get("health")
    hurt = isinstance(health, (int, float)) and health <= SHELTER_HEALTH
    return SHELTER_SKILL if hurt or not _armed(inventory) else NIGHT_SKILL


def choose(
    facts: Optional[Mapping[str, Any]],
    inventory: Mapping[str, int],
    catalog: SkillCatalog,
    stack: Optional[Sequence[Mapping[str, Any]]] = None,
    *,
    backoff: Optional[Mapping[str, int]] = None,
    tick: Optional[int] = None,
) -> Optional[SurvivalGoal]:
    """The survival goal due now, or ``None``.

    Hunger first: food at :data:`HUNGRY_AT` or below with no food held, except
    at night while unarmed or sheltered, where ``get_food`` (which hunts) waits
    until food is at :data:`CRITICAL_FOOD_AT` or below. Then
    night: ``night_watch`` when armed and healthy, ``take_shelter`` when hurt or
    unarmed -- but no night goal at all while :func:`night_work` holds (decision
    2026-11-13): a pickaxe body that is healthy above :data:`SHELTER_HEALTH`
    works instead, and a ``take_shelter`` already running keeps running (this
    function starts nothing, and the loop never interrupts its own night goal).
    By day a body still sealed in (:func:`sealed_by_day`) gets
    ``take_shelter`` again, which waits for sunrise and then leaves the
    shelter (its goal is ``not sheltered()``). A goal whose skill the catalog
    lacks is skipped (the body's own
    fallback covers the night). Failure memory never blocks a survival goal;
    a skill whose goal failed is skipped until ``backoff[skill]`` (a body tick,
    kept by the loop) has passed ``tick``. A skill already live in ``stack`` is
    not started twice. ``facts`` is the snapshot's body facts; without them
    nothing is decided, and neither with a desynchronised world (``world_valid``
    false, decision 2026-11-10): its facts are stale.
    """
    if not facts or facts.get("world_valid") is False:
        return None
    live = _live_skills(stack)

    def usable(skill: str, condition: str) -> bool:
        if skill in live:
            # Already running (the body's fallback, or a goal of ours the
            # snapshot still shows): a second copy only fights the first.
            return False
        if catalog.by_name(skill) is None:
            return False
        if _backing_off(skill, backoff, tick):
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
        and not _hunt_held_at_night(facts, food, inventory, stack)
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

    if sealed_by_day(facts) and usable(SHELTER_SKILL, LEAVE_CONDITION):
        return SurvivalGoal(
            kind="leave",
            skill=SHELTER_SKILL,
            proposal=GoalProposal(
                text="leave the shelter",
                condition=LEAVE_CONDITION,
                rationale="sealed in by day",
            ),
            reason="sealed in by day",
        )

    if facts.get("is_night") is True and not night_work(facts, inventory):
        skill = _night_skill(facts, inventory)
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


def night_hold(
    facts: Optional[Mapping[str, Any]],
    inventory: Mapping[str, int],
    stack: Optional[Sequence[Mapping[str, Any]]] = None,
    *,
    backoff: Optional[Mapping[str, int]] = None,
    tick: Optional[int] = None,
    failures: Optional[Mapping[str, int]] = None,
) -> Optional[str]:
    """Why no ladder or proposer goal may start now, or ``None``.

    At night the brain waits instead of working (decisions 2026-11-10
    "Unarmed at night: avoid contact" and "Survival goals are never
    failure-blocked"): while unarmed, while the night skill is backing off after
    a failure, and while a night skill is already live in ``stack`` (the body's
    own fallback: a ladder command would preempt it). While :func:`night_work`
    holds, none of those night reasons applies (decision 2026-11-13): the pick
    keeps planning, restricted to :data:`NIGHT_WORK_SKILLS`. By day a body still
    sealed in waits too (:func:`sealed_by_day`): every ladder goal would fail
    inside the shelter, so the pick waits until ``take_shelter`` has opened it
    (it is backing off, running, or missing from the catalog). Call it only
    after :func:`choose` found nothing to start; a user goal is picked before
    both.
    """
    if sealed_by_day(facts) and (failures or {}).get(SHELTER_SKILL, 0) < LEAVE_GIVE_UP:
        if _backing_off(SHELTER_SKILL, backoff, tick):
            return f"sealed in by day, {SHELTER_SKILL} backing off after a failure"
        if SHELTER_SKILL in _live_skills(stack):
            return f"sealed in by day, {SHELTER_SKILL} is running"
        return "sealed in by day"
    if not facts or facts.get("world_valid") is False or facts.get("is_night") is not True:
        return None
    if night_work(facts, inventory):
        return None
    if not _armed(inventory):
        return "unarmed at night"
    skill = _night_skill(facts, inventory)
    if _backing_off(skill, backoff, tick):
        return f"{skill} backing off after a failure"
    live = _live_skills(stack) & {NIGHT_SKILL, SHELTER_SKILL}
    if live:
        return f"{sorted(live)[0]} is running"
    return None


__all__ = [
    "CRITICAL_FOOD_AT",
    "FOOD_CONDITION",
    "FOOD_GOAL_COUNT",
    "FOOD_SKILL",
    "HUNGRY_AT",
    "LEAVE_CONDITION",
    "LIVE_STATES",
    "NIGHT_BUDGET_TICKS",
    "NIGHT_CONDITION",
    "NIGHT_SKILL",
    "NIGHT_SKILLS",
    "NIGHT_WORK_SKILLS",
    "SHELTER_HEALTH",
    "SHELTER_SKILL",
    "SURVIVAL_BACKOFF_TICKS",
    "SURVIVAL_BACKOFF_MAX_TICKS",
    "LEAVE_GIVE_UP",
    "SURVIVAL_SKILLS",
    "SurvivalGoal",
    "choose",
    "critical_food",
    "night_hold",
    "night_work",
    "sealed_by_day",
]
