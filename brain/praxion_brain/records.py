"""Goal records: what the brain tried, and how each attempt ended.

It owns PendingGoal, OpenGoal and settled_status, goal ids, record creation and
receipt updates, the one place a goal closes (record, skill use, rung, evidence),
settling open records and post-reconnect adoption. It reads Memory, not the port.
Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` sections A1, A7 and 5.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Callable, Optional, Protocol, Sequence

from .conditions import ConditionError, evaluate
from .curriculum import GoalProposal, Validation
from .goal import Goal
from .ladder import progress as ladder_progress
from .memory import EvidenceSink, GoalRecord, Memory, SkillUse

#: Stack states that mean "the body still owns a goal" (A1).
LIVE_INTENT_STATES = ("running", "suspended")


def settled_status(
    outcome: Any,
    condition: Any,
    unverifiable: bool,
    failure_kind: Any = None,
) -> tuple[str, Optional[str]]:
    """The record status and failure kind of one ``settled`` receipt.

    ``done`` is only a success when the body verified the condition. A ``done``
    with ``condition:false`` is a lying skill; a ``done`` the body could not
    decide (``unknown`` or absent) is ``unverified`` unless the goal was already
    known to be unverifiable (A7). Unverified goals are excluded from the
    success statistics: they are not ``done``.
    """
    if outcome != "done":
        return "failed", str(failure_kind or outcome or "failed")
    if condition == "false":
        return "failed", "skill_wrong"
    if condition != "true" and not unverifiable:
        return "unverified", None
    return "done", None


@dataclass
class PendingGoal:
    """A validated proposal on its way to the body."""

    proposal: GoalProposal
    validation: Validation
    goal: Goal
    source: str = "llm"
    forced_skill: Optional[str] = None


@dataclass
class OpenGoal:
    """The one command whose receipt the loop is waiting for."""

    record: GoalRecord
    proposal: GoalProposal
    skill: str
    command_id: str
    intent_id: Optional[int] = None
    sent_tick: int = 0
    retried: bool = False

    def ticks_since(self, tick: Optional[int]) -> Optional[int]:
        """Body ticks since the command was sent; ``None`` without a snapshot."""
        if tick is None:
            return None
        return max(0, int(tick) - int(self.sent_tick))


class Intent(Protocol):
    """One entry of the body's intent stack, as far as records care."""

    intent_id: int
    skill: Optional[str]
    state: str


@dataclass
class Recovery:
    """What one recovery snapshot left the owner with."""

    #: The record adopted from a live intent, if one matched.
    adopted: Optional[OpenGoal] = None
    #: Live intents that match no record; the owner stays idle for them (3.6).
    unmatched: list[Intent] = field(default_factory=list)


class GoalRecords:
    """The goal lifecycle on top of :class:`~praxion_brain.memory.Memory`."""

    def __init__(
        self,
        memory: Memory,
        evidence: EvidenceSink,
        *,
        inventory: Callable[[], dict[str, int]],
        on_finished: Optional[Callable[[GoalRecord], None]] = None,
    ) -> None:
        self.memory = memory
        self.evidence = evidence
        self._inventory = inventory
        self._on_finished = on_finished
        self._goal_number = self._initial_goal_number()

    # -- creating and updating ------------------------------------------ #
    def new(
        self,
        pending: PendingGoal,
        *,
        status: str,
        skill: Optional[str],
        origin: Optional[str],
        command_id: Optional[str] = None,
    ) -> GoalRecord:
        """A fresh record for ``pending``; the caller appends it when it means to."""
        return GoalRecord(
            goal_id=self._next_goal_id(),
            text=pending.proposal.text,
            condition=pending.proposal.condition,
            unverifiable=pending.validation.unverifiable,
            rung=pending.proposal.rung,
            status=status,
            command_id=command_id,
            skill=skill,
            skill_origin=origin,
            t_start=self.memory.now(),
        )

    def accepted(self, opened: OpenGoal, intent_id: Optional[int]) -> GoalRecord:
        """The body accepted the command: remember its intent on the record."""
        opened.intent_id = intent_id
        opened.record = opened.record.model_copy(update={"intent_id": intent_id})
        return self.memory.append_goal(opened.record)

    def retried(self, opened: OpenGoal, command_id: str, *, tick: int) -> GoalRecord:
        """A stale-generation rejection was resent as ``command_id``."""
        opened.command_id = command_id
        opened.intent_id = None
        opened.retried = True
        opened.sent_tick = int(tick)
        opened.record = opened.record.model_copy(
            update={"command_id": command_id, "intent_id": None}
        )
        return self.memory.append_goal(opened.record)

    # -- closing -------------------------------------------------------- #
    def finish(
        self,
        record: GoalRecord,
        *,
        status: str,
        failure_kind: Optional[str] = None,
        condition_result: Optional[str] = None,
        ticks: Optional[int] = None,
        detail: Optional[str] = None,
        source: Optional[str] = None,
        wake: bool = True,
    ) -> GoalRecord:
        """The one place a goal is closed: record, skill use, rung note, evidence.

        ``wake=False`` is for recovery, which closes several records at once and
        then sets its own wake (A1). With ``wake`` it tells the owner the goal
        is closed, which clears its open goal and hands it back to the pick.
        """
        stored = record.model_copy(
            update={
                "status": status,
                "ticks": ticks,
                "failure_kind": failure_kind,
                "condition_result": condition_result,
                "t_end": self.memory.now(),
            }
        )
        self.memory.append_goal(stored)
        if stored.skill:
            self.memory.append_skill_use(
                SkillUse(
                    skill=stored.skill,
                    goal_id=stored.goal_id,
                    outcome=status,
                    failure_kind=failure_kind,
                    ticks=ticks,
                )
            )
        progress = ladder_progress(self._inventory())
        self.memory.note_rung_progress(progress.met, stored.goal_id)
        self.evidence.write(
            "goal_finished",
            goal_id=stored.goal_id,
            command_id=stored.command_id,
            status=status,
            failure_kind=failure_kind,
            condition_result=condition_result,
            skill=stored.skill,
            ticks=ticks,
            rungs_met=progress.met,
            detail=detail,
            source=source,
        )
        if wake and self._on_finished is not None:
            self._on_finished(stored)
        return stored

    def settle_open(self, records: list[GoalRecord]) -> None:
        """A1: close records the body will never settle, by their own condition.

        With an empty stack no receipt is coming: a goal whose condition already
        holds in the snapshot is ``done``; any other is ``interrupted``, which is
        not a failure and does not block the goal.
        """
        inventory = self._inventory()
        for record in records:
            try:
                value = evaluate(record.condition, inventory)
            except ConditionError:
                value = None
            result = "unknown" if value is None else ("true" if value else "false")
            self.finish(
                record,
                status="done" if result == "true" else "interrupted",
                condition_result=result,
                source="recovery",
                wake=False,
            )
        if records:
            self.evidence.write(
                "loop_settled_open",
                goals=[record.goal_id for record in records],
            )

    # -- recovery ------------------------------------------------------- #
    def open_records(self) -> list[GoalRecord]:
        """Every record that still waits for a receipt."""
        return [
            record
            for record in self.memory.goals()
            if record.status == "open" and record.command_id
        ]

    def recover(self, stack: Sequence[Intent], *, tick: int, cause: str) -> Recovery:
        """A1/A8: adopt every live intent, then settle what is left open."""
        active = {
            entry.intent_id: entry
            for entry in stack
            if entry.state in LIVE_INTENT_STATES
        }
        open_records = self.open_records()

        adopted: Optional[GoalRecord] = None
        entry: Optional[Intent] = None
        for record in reversed(open_records):
            match = active.get(record.intent_id) if record.intent_id is not None else None
            if match is None and record.skill:
                # The command that started an intent may have been accepted
                # after the record was written, so the skill is the fallback key.
                match = next(
                    (item for item in active.values() if item.skill == record.skill),
                    None,
                )
            if match is not None:
                adopted, entry = record, match
                break

        if adopted is not None and entry is not None:
            proposal = GoalProposal(
                text=adopted.text,
                condition=adopted.condition,
                rung=adopted.rung,
                rationale="",
            )
            opened = OpenGoal(
                record=adopted,
                proposal=proposal,
                skill=entry.skill or adopted.skill or "",
                command_id=adopted.command_id or "",
                intent_id=entry.intent_id,
                sent_tick=tick,
                retried=True,
            )
            self.evidence.write(
                "loop_recovered",
                cause=cause,
                goal_id=adopted.goal_id,
                command_id=adopted.command_id,
                intent_id=entry.intent_id,
                intent_state=entry.state,
            )
            return Recovery(adopted=opened)

        self.settle_open(open_records)
        return Recovery(unmatched=list(active.values()))

    # -- goal ids ------------------------------------------------------- #
    def _initial_goal_number(self) -> int:
        highest = 0
        for record in self.memory.goals():
            if record.goal_id.startswith("g-"):
                try:
                    highest = max(highest, int(record.goal_id[2:]))
                except ValueError:
                    continue
        return highest

    def _next_goal_id(self) -> str:
        self._goal_number += 1
        return f"g-{self._goal_number:04d}"


__all__ = [
    "GoalRecords",
    "Intent",
    "LIVE_INTENT_STATES",
    "OpenGoal",
    "PendingGoal",
    "Recovery",
    "settled_status",
]
