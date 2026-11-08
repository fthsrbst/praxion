"""Validate one curriculum goal proposal before anything spends money.

A proposal is guard text plus prose (proposal section 4.2). Code checks, in the
proposal's order:

1. the condition parses. Malformed text rejects; a well-formed condition outside
   the inventory subset still runs but is marked ``unverifiable``;
3. the condition is not already true in the snapshot;
4. the goal is not blocked by failure memory and is not the same as the last
   goal;
5. a simple ``has(X)`` / ``count(X) OP n`` shape implies the skill name
   ``get_<X>``; other shapes get no name and the writer picks one.

Check 2 (items exist in game data) is deliberately skipped for now: no game-data
reader exists in the brain, and unknown items simply count as zero.

The LLM proposal call itself is not here.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` sections 4.2 and 4.4.
"""

from __future__ import annotations

import re
from typing import Any, Mapping, Optional

from pydantic import BaseModel, Field

from .conditions import ConditionError, evaluate, is_verifiable, item_of_condition
from .memory import normalize_condition

_SKILL_ITEM = re.compile(r"[A-Za-z0-9_]+")


class GoalProposal(BaseModel):
    """Structured output of the proposal call (section 4.2)."""

    text: str
    condition: str
    rung: Optional[str] = None
    rationale: str = Field(default="", max_length=200)


class Validation(BaseModel):
    """The code's verdict on one proposal."""

    ok: bool
    reason: Optional[str] = None
    detail: Optional[str] = None
    skill_name: Optional[str] = None
    unverifiable: bool = False
    #: Three-valued result of the condition in this snapshot (``None`` unknown).
    met: Optional[bool] = None


def _last_key(last: Any) -> Optional[str]:
    if last is None:
        return None
    if isinstance(last, str):
        return normalize_condition(last)
    condition = getattr(last, "condition", None)
    if condition is None and isinstance(last, Mapping):
        condition = last.get("condition")
    if condition is None:
        text = getattr(last, "text", None)
        if text is None and isinstance(last, Mapping):
            text = last.get("text")
        return normalize_condition(text) if isinstance(text, str) else None
    return normalize_condition(str(condition))


def validate(
    proposal: GoalProposal,
    inventory: Mapping[str, int],
    memory: Any,
    last: Any = None,
) -> Validation:
    """Run checks 1, 3, 4 and 5 from section 4.2."""
    try:
        met = evaluate(proposal.condition, inventory)
        verifiable = is_verifiable(proposal.condition)
    except ConditionError as error:
        return Validation(ok=False, reason="malformed_condition", detail=str(error))

    # Check 3: a condition already true needs no goal.
    if met is True:
        return Validation(
            ok=False,
            reason="already_met",
            skill_name=None,
            unverifiable=not verifiable,
            met=True,
        )

    condition_key = normalize_condition(proposal.condition)

    # Check 4: failure memory and repetition.
    if memory is not None and memory.is_blocked(proposal.condition):
        return Validation(
            ok=False,
            reason="blocked",
            skill_name=None,
            unverifiable=not verifiable,
            met=met,
        )
    if condition_key == _last_key(last):
        return Validation(
            ok=False,
            reason="same_as_last",
            skill_name=None,
            unverifiable=not verifiable,
            met=met,
        )

    # Check 5: the naming convention, when the shape has one.
    skill_name: Optional[str] = None
    item = item_of_condition(proposal.condition)
    if item is not None and _SKILL_ITEM.fullmatch(item):
        skill_name = f"get_{item}"

    return Validation(
        ok=True,
        skill_name=skill_name,
        unverifiable=not verifiable,
        met=met,
    )


__all__ = ["GoalProposal", "Validation", "validate"]
