"""The autonomous brain loop: pick, route, write, play, settle.

A small state machine over one :class:`~praxion_brain.session.BodySession`:

```
IDLE --wake--> PICKING --proposal--> ROUTING --play----------> PLAYING
                                             \\--write--> WRITING --admit--> PLAYING
PLAYING --settled/failed--> IDLE (next pick on the wake)
```

It decides *when* to act: wake causes, the heartbeat, the code-only fallback,
the pick, the route, the write, the play and every receipt-driven transition.
The connection is :mod:`~praxion_brain.session`, the goal history is
:mod:`~praxion_brain.records`; the loop only composes them. Every paid call runs
through :meth:`~praxion_brain.governor.Governor.call` with the deadline of its
class, so no provider can hang the loop (section 3.7). A failing step is
recorded and the loop reconnects or stops cleanly; it never dies on one bad
frame.

A failed goal is not retried at once: the pick waits
:data:`FAILURE_GAP_TICKS` body ticks (or :data:`FAILURE_STREAK_GAP_TICKS`
after :data:`FAILURE_STREAK` failures in a row) while the loop stays IDLE.
Code-only picks obey the same failure memory as proposals, so a blocked goal
idles instead of spinning.

Every expensive step is injectable so the loop is tested against
:class:`FakeBody` with fake proposal, router and admission callables.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` sections 3.1-3.7.
"""

from __future__ import annotations

import time
from enum import Enum
from typing import Any, Callable, Mapping, Optional

from . import config
from .admission import (
    AdmissionReport,
    admit,
    estimate as admission_estimate,
)
from .briefing import build as build_briefing
from .conditions import ConditionError, count_of_condition
from .briefing import heartbeat_change
from .curriculum import GoalProposal, Validation, goal_of, validate
from .evidence import Evidence
from .goal import Goal
from .governor import CallTimeout, Governor
from .ladder import code_only_goal, progress as ladder_progress
from .ledger import Ledger, estimate_cost
from .memory import GoalRecord, Memory, normalize_condition
from .port import EventMessage, PortClient, PortError, Snapshot
from .proposer import DEFAULT_MODEL, estimate as propose_estimate, propose
from .records import GoalRecords, OpenGoal, PendingGoal, settled_status
from .router import DEFAULT_MODEL as ROUTER_MODEL
from .router import Route
from .router import route as router_route
from .session import COMMAND_BUDGET_TICKS, BodySession
from .skills import SkillCatalog, load_catalog
from .writer import write as write_skill

#: 60 s heartbeat (proposal sections 3.2 and A 5.7).
HEARTBEAT_SECONDS = 60.0
#: ``body_wake`` events are recorded at most once per 30 s.
BODY_WAKE_LIMIT_SECONDS = 30.0
#: Proposal calls per pick before the code-only fallback.
MAX_PROPOSALS = 3
#: Consecutive step errors before the loop stops cleanly (A6).
MAX_STEP_ERRORS = 5
#: Minimum body ticks between a failed goal and the next pick (5 s at 20 TPS):
#: an always-failing goal must not spin the loop (autonomous run 2, 2026-11-08).
FAILURE_GAP_TICKS = 100
#: The longer gap after :data:`FAILURE_STREAK` consecutive failures (30 s).
FAILURE_STREAK_GAP_TICKS = 600
#: Consecutive failed goals after which the longer gap applies.
FAILURE_STREAK = 3


class LoopState(str, Enum):
    IDLE = "idle"
    PICKING = "picking"
    ROUTING = "routing"
    WRITING = "writing"
    PLAYING = "playing"


class WakeCause(str, Enum):
    START = "start"
    GOAL_SETTLED = "goal_settled"
    GOAL_FAILED = "goal_failed"
    BODY_WAKE = "body_wake"
    HEARTBEAT = "heartbeat"
    RECONNECT = "reconnect"


# --------------------------------------------------------------------------- #
# The loop
# --------------------------------------------------------------------------- #
class Loop:
    """One autonomous brain loop against one body port."""

    def __init__(
        self,
        port: PortClient,
        *,
        world_id: str,
        memory: Memory,
        catalog: SkillCatalog,
        ledger: Ledger | None = None,
        evidence: Evidence | None = None,
        governor: Governor | None = None,
        propose_fn: Callable[..., GoalProposal] = propose,
        route_fn: Callable[..., Route] = router_route,
        admission_options: Optional[dict[str, Any]] = None,
        catalog_loader: Callable[[], SkillCatalog] = load_catalog,
        clock: Callable[[], float] | None = None,
        wall_clock: Callable[[], float] | None = None,
        logger: Callable[[str], None] | None = None,
        heartbeat_seconds: float = HEARTBEAT_SECONDS,
        body_wake_limit_seconds: float = BODY_WAKE_LIMIT_SECONDS,
        command_budget_ticks: int = COMMAND_BUDGET_TICKS,
        max_proposals: int = MAX_PROPOSALS,
        writer_model: str = DEFAULT_MODEL,
    ) -> None:
        self.world_id = world_id
        self.memory = memory
        self.catalog = catalog
        self.ledger = ledger if ledger is not None else Ledger()
        self.evidence = evidence if evidence is not None else Evidence(config.local_brain_dir())
        self.governor = (
            governor
            if governor is not None
            else Governor(self.ledger, evidence=self.evidence)
        )
        self._propose_fn = propose_fn
        self._route_fn = route_fn
        self._admission_options = dict(admission_options or {})
        self._catalog_loader = catalog_loader
        self._clock = clock if clock is not None else time.monotonic
        self._wall_clock = wall_clock if wall_clock is not None else time.time
        self._log = logger if logger is not None else print

        self.heartbeat_seconds = float(heartbeat_seconds)
        self.body_wake_limit_seconds = float(body_wake_limit_seconds)
        self.command_budget_ticks = int(command_budget_ticks)
        self.max_proposals = int(max_proposals)
        self.writer_model = writer_model

        self.state = LoopState.IDLE
        self._stopped = False
        self._wake: Optional[WakeCause] = None
        self._pending: Optional[PendingGoal] = None
        self._open_goal: Optional[OpenGoal] = None
        self._last_proposal: Optional[GoalProposal] = None
        self._invalid_proposals = 0
        self._last_heartbeat = self._clock()
        self._heartbeat_facts: Optional[dict[str, Any]] = None
        self._was_playing = False
        self._blocked_goals: set[str] = set()
        self._last_body_wake = -float("inf")
        self._step_errors = 0
        self._step_failed = False
        self._consecutive_failures = 0
        self._next_pick_tick = 0

        self.session = BodySession(
            port,
            evidence=self.evidence,
            listener=self,
            clock=self._clock,
            state=lambda: self.state.value,
            log=self._log,
            command_budget_ticks=self.command_budget_ticks,
        )
        self.records = GoalRecords(
            memory,
            self.evidence,
            inventory=self._inventory,
            on_finished=self._goal_finished,
        )

    # -- lifecycle ------------------------------------------------------ #
    def start(self) -> "Loop":
        """Connect to the body and start the reader; the snapshot recovers state."""
        self._stopped = False
        self._step_errors = 0
        self.session.start()
        self._last_heartbeat = self._clock()
        self.evidence.write(
            "loop_started",
            world_id=self.world_id,
            port=self.session.port_number,
        )
        return self

    def close(self) -> None:
        self._stopped = True
        self.session.close()
        self.evidence.write("loop_stopped", world_id=self.world_id, state=self.state.value)

    def stop(self) -> None:
        self._stopped = True

    def run(self, timeout: float = 0.25) -> None:
        """Run until :meth:`stop`, a terminal bye, or repeated step errors.

        One bad step is recorded and survived (A6): the open record is closed,
        a lost port reconnects on the next step, and only
        :data:`MAX_STEP_ERRORS` consecutive errors end the run.
        """
        while not self._stopped:
            try:
                self.step(timeout)
            except Exception as error:  # noqa: BLE001 - one step must not kill the loop
                self._on_step_error(error)

    # -- one step ------------------------------------------------------- #
    def step(self, timeout: float = 0.0) -> LoopState:
        """Consume one message from the reader, then advance at most one phase.

        A reconnect only happens once the reader is gone and its last message
        has been handled, so the ``bye`` that closed the connection is read
        before anyone decides to dial again.
        """
        if self._stopped:
            return self.state

        self._maybe_heartbeat()
        message = self.session.poll(timeout if self.session.connected else 0.0)
        if message is not None:
            self._handle_message(message)
        if self.session.terminated:
            self._stopped = True

        if (
            not self._stopped
            and not self.session.connected
            and not self.session.reading
        ):
            self.session.try_reconnect()

        self._advance()
        if self.session.terminated:
            # A terminal bye can also arrive inside a snapshot refresh.
            self._stopped = True
        if not self._step_failed:
            self._step_errors = 0
        self._step_failed = False
        return self.state

    def _on_step_error(self, error: BaseException) -> None:
        """A6: record the error, close the open record, reconnect or stop."""
        self._step_errors += 1
        self._step_failed = True
        if isinstance(error, PortError):
            self.session.disconnect(str(error))
        try:
            self.evidence.write(
                "loop_error",
                error=f"{type(error).__name__}: {error}",
                state=self.state.value,
                consecutive=self._step_errors,
            )
            opened = self._open_goal
            if opened is not None:
                self.records.finish(
                    opened.record,
                    status="failed",
                    failure_kind="loop_error",
                    ticks=opened.ticks_since(self.session.tick),
                    detail=str(error),
                    source="loop_error",
                )
        except OSError as secondary:  # the memory or evidence store itself failed
            self._log(f"loop: could not record the step error: {secondary}")
        if self._step_errors >= MAX_STEP_ERRORS:
            self._log(
                f"loop: {self._step_errors} consecutive step errors, stopping: {error}"
            )
            self.stop()

    # -- what the session reports --------------------------------------- #
    @property
    def _recovering(self) -> Optional[str]:
        """The pending recovery cause; a fresh connection owns it (start/reconnect)."""
        return self.session.recovering

    def on_event(self, event: Mapping[str, Any]) -> None:
        """An event that arrived while the loop was blocked in a snapshot refresh."""
        self._on_event(event)

    def on_snapshot(self, snapshot: Snapshot, *, heartbeat_pending: bool) -> None:
        """A snapshot arrived: diff the facts, then recover from a fresh connection."""
        facts = (self.session.snapshot or {}).get("facts") or {}
        playing = any(entry.state == "running" for entry in snapshot.stack)
        self._check_heartbeat(
            facts,
            is_playing=playing,
            tick=snapshot.tick,
            heartbeat_pending=heartbeat_pending,
        )
        cause = self.session.take_recovery()
        if cause is not None:
            self._recover(snapshot, WakeCause(cause))

    def on_error(self, error: PortError) -> None:
        """A protocol error is a step error (A6)."""
        self._on_step_error(error)

    # -- port events ---------------------------------------------------- #
    def _handle_message(self, message: EventMessage) -> None:
        """The one place a body event enters the loop; the session sent it."""
        self._on_event(message.event)

    def _check_heartbeat(
        self,
        facts: Mapping[str, Any],
        *,
        is_playing: bool,
        tick: int,
        heartbeat_pending: bool,
    ) -> None:
        """Diff this snapshot against the last one; the baseline always moves (B8)."""
        was_playing, self._was_playing = self._was_playing, is_playing
        previous, self._heartbeat_facts = self._heartbeat_facts, dict(facts)
        if previous is None:
            return
        reasons = heartbeat_change(
            previous,
            facts,
            was_playing=was_playing,
            is_playing=is_playing,
        )
        # A goal block that has run out is a change too: an idle body with an
        # unchanged world would otherwise never pick again.
        blocked_now = set(self.memory.blocked())
        if self._blocked_goals - blocked_now:
            reasons.append("block_expired")
        self._blocked_goals = blocked_now
        if reasons:
            self.evidence.write("heartbeat_change", reasons=reasons, tick=tick)
            if self.state == LoopState.IDLE:
                self._set_wake(WakeCause.HEARTBEAT)
        elif heartbeat_pending:
            self.evidence.write("heartbeat_quiet", tick=tick)

    def _recover(self, snapshot: Snapshot, cause: WakeCause) -> None:
        """A1/A8: adopt every live intent, then settle what is left open."""
        recovery = self.records.recover(
            snapshot.stack, tick=snapshot.tick, cause=cause.value
        )
        if recovery.adopted is not None:
            self._last_proposal = recovery.adopted.proposal
            self._open_goal = recovery.adopted
            self._transition(LoopState.PLAYING, reason=f"recover:{cause.value}")
            return

        self._open_goal = None
        if recovery.unmatched:
            # Spec 3.6: let the stranger finish and do not adopt it. A suspended
            # one is not a reason to pick either: a new command would preempt it.
            self.evidence.write(
                "loop_unmatched_intent",
                skills=sorted(item.skill or "" for item in recovery.unmatched),
                states=sorted(item.state for item in recovery.unmatched),
                tick=snapshot.tick,
            )
            self._transition(LoopState.IDLE, reason=f"recover:{cause.value}")
            return
        self._transition(LoopState.IDLE, reason=f"recover:{cause.value}")
        self._set_wake(cause if cause is WakeCause.RECONNECT else WakeCause.START)

    # -- events --------------------------------------------------------- #
    def _on_event(self, event: Mapping[str, Any]) -> None:
        kind = event.get("kind")
        if kind == "command_receipt":
            self._on_receipt(event)
        elif kind == "wake_brain":
            self._on_body_wake(event)

    def _on_body_wake(self, event: Mapping[str, Any]) -> None:
        now = self._clock()
        if now - self._last_body_wake < self.body_wake_limit_seconds:
            return
        self._last_body_wake = now
        self.evidence.write(
            "body_wake",
            cause=event.get("cause"),
            detail=event.get("detail"),
            tick=self.session.tick,
        )

    def _on_receipt(self, event: Mapping[str, Any]) -> None:
        command_id = str(event.get("command_id") or "")
        state = event.get("state")
        open_goal = self._open_goal
        if open_goal is None or open_goal.command_id != command_id:
            self.evidence.write(
                "receipt_unmatched",
                command_id=command_id,
                state=state,
            )
            return

        if state == "pending":
            return
        if state == "accepted":
            self.records.accepted(open_goal, event.get("intent_id"))
            self.evidence.write(
                "command_accepted",
                command_id=command_id,
                intent_id=open_goal.intent_id,
                generation=event.get("generation"),
            )
            return
        if state in ("rejected", "expired"):
            if (
                state == "rejected"
                and event.get("reason") == "stale_generation"
                and not open_goal.retried
            ):
                self._retry_stale(open_goal)
                return
            failure = event.get("reason") or state
            self.records.finish(
                open_goal.record,
                status="failed",
                failure_kind=str(failure),
                ticks=open_goal.ticks_since(self.session.tick),
            )
            return
        if state == "settled":
            condition = event.get("condition")
            status, failure_kind = settled_status(
                event.get("outcome"),
                condition,
                open_goal.record.unverifiable,
                event.get("failure_kind"),
            )
            self.records.finish(
                open_goal.record,
                status=status,
                failure_kind=failure_kind,
                condition_result=str(condition) if condition is not None else None,
                ticks=open_goal.ticks_since(self.session.tick),
            )

    def _retry_stale(self, open_goal: OpenGoal) -> None:
        try:
            command_id = self._send_goal(
                open_goal.skill, open_goal.proposal.condition
            )
        except PortError as error:
            self.records.finish(
                open_goal.record,
                status="failed",
                failure_kind="stale_generation",
                ticks=open_goal.ticks_since(self.session.tick),
                detail=str(error),
            )
            return
        self.records.retried(open_goal, command_id, tick=int(self.session.tick or 0))
        self.evidence.write(
            "command_retried",
            command_id=command_id,
            goal_id=open_goal.record.goal_id,
        )

    # -- state machine -------------------------------------------------- #
    def _advance(self) -> None:
        if self.state == LoopState.IDLE and self._wake is not None:
            if not self._pick_ready():
                # The gap after a failure has not passed yet; keep the wake
                # pending and let the next step (heartbeat/poll) try again.
                return
            wake = self._wake
            self._wake = None
            self._transition(LoopState.PICKING, reason=wake.value)
        elif self.state == LoopState.PICKING:
            self._pick()
        elif self.state == LoopState.ROUTING:
            self._route()
        elif self.state == LoopState.WRITING:
            self._write()

    def _transition(self, state: LoopState, *, reason: str) -> None:
        if state == self.state:
            return
        previous = self.state
        self.state = state
        self.evidence.write(
            "loop_transition",
            from_state=previous.value,
            to_state=state.value,
            reason=reason,
            tick=self.session.tick,
        )

    def _set_wake(self, cause: WakeCause) -> None:
        if self._wake is not None:
            return
        self._wake = cause
        self.evidence.write("loop_wake", cause=cause.value, state=self.state.value)

    def _pick_ready(self) -> bool:
        """Whether the body tick has passed the gap a failed goal asked for."""
        return int(self.session.tick or 0) >= self._next_pick_tick

    def _goal_finished(self, record: GoalRecord) -> None:
        """Records closed a goal: idle, wait out a failed goal's gap, pick again."""
        self._open_goal = None
        self._transition(LoopState.IDLE, reason=f"goal_{record.status}")
        if record.status == "failed":
            self._consecutive_failures += 1
            gap = (
                FAILURE_STREAK_GAP_TICKS
                if self._consecutive_failures >= FAILURE_STREAK
                else FAILURE_GAP_TICKS
            )
            now = int(self.session.tick or 0)
            self._next_pick_tick = now + gap
            self.evidence.write(
                "pick_gap",
                ticks=gap,
                consecutive_failures=self._consecutive_failures,
                from_tick=now,
                next_pick_tick=self._next_pick_tick,
                failure_kind=record.failure_kind,
            )
        else:
            self._consecutive_failures = 0
            self._next_pick_tick = 0
        self._set_wake(
            WakeCause.GOAL_SETTLED if record.status == "done" else WakeCause.GOAL_FAILED
        )

    def _gate(self, call_class: str, estimate: float) -> Optional[str]:
        """Ask the governor before a paid call; ``None`` when it may run.

        A refusal is recorded once here and returned as the reason the caller
        falls back to code-only mode with.
        """
        decision = self.governor.allow(call_class, estimate)
        if decision.allowed:
            return None
        reason = str(decision.reason or "governor_denied")
        self.evidence.write("governor_denied", call=call_class, reason=reason)
        return reason

    # -- PICKING -------------------------------------------------------- #
    def _pick(self) -> None:
        if not self.session.connected:
            # No body, no goal: the reconnect snapshot re-wakes the loop.
            self._transition(LoopState.IDLE, reason="offline")
            return
        # Facts and the generation may have moved while the body played or
        # while the brain was writing; read them before proposing or validating.
        self.session.refresh()
        inventory = self._inventory()
        if self.governor.code_only:
            self._pick_code_only("governor_code_only")
            return

        briefing = self._briefing()
        for _ in range(max(1, self.max_proposals)):
            denied = self._gate("propose", propose_estimate(self._invalid_proposals))
            if denied:
                self._pick_code_only(denied)
                return
            proposal, failure = self._proposal_call(briefing)
            if proposal is None:
                self._pick_code_only(failure)
                return

            result = validate(proposal, inventory, self.memory, self._last_proposal)
            self.evidence.write(
                "proposal_result",
                proposal=proposal.model_dump(),
                validation=result.model_dump(),
                invalid_count=self._invalid_proposals,
            )
            if result.ok:
                self._pending = PendingGoal(
                    proposal=proposal,
                    validation=result,
                    goal=goal_of(proposal, result),
                )
                self._last_proposal = proposal
                self._invalid_proposals = 0
                self._transition(LoopState.ROUTING, reason="proposal")
                return

            self._invalid_proposals += 1
            self.evidence.write(
                "proposal_invalid",
                reason=result.reason,
                detail=result.detail,
                invalid_count=self._invalid_proposals,
            )

        self._pick_code_only("proposals_invalid")

    def _proposal_call(self, briefing: str) -> tuple[Optional[GoalProposal], str]:
        """One proposal call with its deadline; ``(None, reason)`` when it fails."""
        try:
            proposal = self.governor.call(
                "propose",
                lambda: self._propose_fn(
                    briefing=briefing,
                    catalog=self.catalog,
                    invalid_count=self._invalid_proposals,
                    ledger=self.ledger,
                    evidence=self.evidence,
                ),
            )
        except CallTimeout as error:
            self.governor.record_error()
            self.evidence.write(
                "proposal_timeout",
                seconds=error.seconds,
                invalid_count=self._invalid_proposals,
            )
            return None, "proposal_timeout"
        except Exception as error:  # noqa: BLE001 - provider errors feed the breaker
            self.governor.record_error()
            self.evidence.write(
                "proposal_error",
                error=f"{type(error).__name__}: {error}",
                invalid_count=self._invalid_proposals,
            )
            return None, "proposal_error"
        self.governor.record_success()
        return proposal, ""

    def _pick_code_only(self, reason: str) -> bool:
        inventory = self._inventory()
        fallback = code_only_goal(inventory, self.catalog)
        self._pending = None
        if fallback is None:
            self.evidence.write(
                "code_only_idle",
                reason=reason,
                inventory=inventory,
            )
            self._transition(LoopState.IDLE, reason="code_only_idle")
            return False
        proposal = fallback.goal
        # The same failure memory the proposal path validates against: two
        # failures block a goal for two hours, so code-only must idle rather
        # than send the same doomed goal again (autonomous run 2).
        if self.memory.is_blocked(proposal.condition):
            entry = self.memory.failures().get(normalize_condition(proposal.condition))
            self.evidence.write(
                "code_only_blocked",
                reason=reason,
                condition=proposal.condition,
                skill=fallback.skill,
                attempts=entry.attempts if entry is not None else None,
                blocked_until=entry.blocked_until if entry is not None else None,
            )
            self._transition(LoopState.IDLE, reason="code_only_blocked")
            return False
        validation = Validation(ok=True, skill_name=fallback.skill)
        self._last_proposal = proposal
        self._pending = PendingGoal(
            proposal=proposal,
            validation=validation,
            goal=goal_of(proposal, validation),
            source="code_only",
            forced_skill=fallback.skill,
        )
        self.evidence.write(
            "code_only_goal",
            reason=reason,
            goal=proposal.model_dump(),
            skill=fallback.skill,
        )
        self._transition(LoopState.ROUTING, reason=f"code_only:{reason}")
        return True

    def _briefing(self) -> str:
        snapshot = self.session.snapshot or {"facts": {"inventory": {}}}
        failures = self.memory.failures()
        records = []
        for record in self.memory.recent(10):
            entry = record.model_dump(exclude_none=True)
            failure = failures.get(normalize_condition(record.condition))
            if failure is not None:
                entry["attempts"] = failure.attempts
                entry["blocked_until"] = failure.blocked_until
            records.append(entry)
        return build_briefing(
            snapshot,
            ladder_progress(self._inventory()),
            records,
            self.catalog,
            self.governor.budget_snapshot(),
            now=self._wall_clock(),
        )

    # -- ROUTING -------------------------------------------------------- #
    def _route(self) -> None:
        pending = self._pending
        if pending is None:
            self._transition(LoopState.IDLE, reason="route_without_goal")
            return

        if pending.source == "code_only" and pending.forced_skill:
            self.evidence.write(
                "route_code_only",
                skill=pending.forced_skill,
                condition=pending.proposal.condition,
            )
            self._play()
            return

        denied = self._gate("route", estimate_cost(ROUTER_MODEL, 1_000, 0))
        if denied:
            if self._pick_code_only(denied):
                self._play()
            return

        try:
            routed = self.governor.call(
                "route",
                lambda: self._route_fn(
                    pending.goal,
                    self.catalog,
                    ledger=self.ledger,
                    evidence=self.evidence,
                ),
            )
        except CallTimeout as error:
            self.governor.record_error()
            self.evidence.write("route_timeout", seconds=error.seconds)
            self._transition(LoopState.IDLE, reason="route_timeout")
            return
        except Exception as error:  # noqa: BLE001 - a Jev error is never "no"
            self.governor.record_error()
            self.evidence.write(
                "route_error",
                error=f"{type(error).__name__}: {error}",
            )
            self._transition(LoopState.IDLE, reason="route_error")
            return
        self.governor.record_success()

        self.evidence.write(
            "loop_route",
            decision=routed.kind,
            skill=routed.skill,
            scores=routed.scores,
            condition=pending.proposal.condition,
        )
        if routed.kind == "play":
            skill = routed.skill or pending.validation.skill_name
            if not skill:
                self.evidence.write("route_no_skill", condition=pending.proposal.condition)
                self._transition(LoopState.IDLE, reason="route_no_skill")
                return
            pending.forced_skill = skill
            self._play()
            return
        self._transition(LoopState.WRITING, reason=f"route:{routed.kind}")

    # -- WRITING -------------------------------------------------------- #
    def _write(self) -> None:
        pending = self._pending
        if pending is None:
            self._transition(LoopState.IDLE, reason="write_without_goal")
            return

        denied = self._gate("write", admission_estimate(self.writer_model))
        if denied:
            if self._pick_code_only(denied):
                self._play()
            return

        try:
            report = self.governor.call("admission", lambda: self._admit(pending.goal))
        except CallTimeout as error:
            self.governor.record_error()
            self.evidence.write(
                "write_timeout",
                call=error.call_class,
                seconds=error.seconds,
            )
            self._write_failed("timeout", pending)
            return
        except Exception as error:  # noqa: BLE001 - an admission crash is a goal failure
            self.governor.record_error()
            self.evidence.write(
                "admission_error",
                error=f"{type(error).__name__}: {error}",
            )
            self._write_failed("inconclusive_body", pending)
            return
        self.governor.record_success()

        if report.status == "admitted":
            self.catalog = self._catalog_loader()
            pending.forced_skill = report.skill_name
            pending.validation = pending.validation.model_copy(
                update={"skill_name": report.skill_name}
            )
            pending.source = "written"
            self.evidence.write(
                "admission_admitted",
                goal_id=None,
                skill=report.skill_name,
                repairs=report.repairs_used,
            )
            self._play()
            return

        self._write_failed(report.status, pending, report=report)

    def _admit(self, goal: Goal) -> AdmissionReport:
        """Run admission with this loop's evidence, ledger, writer model and deadlines.

        The writer call carries the ``write`` deadline and the whole run the
        ``admission`` deadline (A4); tests override the writer, checker and
        practice room through ``admission_options``.
        """
        options = dict(self._admission_options)
        options.setdefault("evidence", self.evidence)
        options.setdefault("ledger", self.ledger)
        options.setdefault("catalog", self.catalog)
        options.setdefault("writer_model", self.writer_model)
        write_fn = options.get("write_fn") or write_skill
        options["write_fn"] = lambda **kwargs: self.governor.call(
            "write", lambda: write_fn(**kwargs)
        )
        return admit(goal, **options)

    def _write_failed(
        self,
        failure_kind: str,
        pending: PendingGoal,
        *,
        report: Optional[AdmissionReport] = None,
    ) -> None:
        skill = report.skill_name if report is not None else pending.validation.skill_name
        record = self.records.new(
            pending,
            status="failed",
            skill=skill,
            origin="written" if skill else None,
        )
        self.records.finish(
            record,
            status="failed",
            failure_kind=str(failure_kind),
            detail=report.reason if report is not None else None,
            source="admission",
        )

    # -- PLAYING -------------------------------------------------------- #
    def _play(self) -> None:
        pending = self._pending
        if pending is None:
            self._transition(LoopState.IDLE, reason="play_without_goal")
            return
        skill = pending.forced_skill or pending.validation.skill_name
        if not skill:
            self.evidence.write("play_no_skill", condition=pending.proposal.condition)
            self._transition(LoopState.IDLE, reason="no_skill")
            return
        # Section 2.4: refresh the tick and generation from a snapshot right
        # before sending, so a long proposal/Jev/admission call cannot make the
        # command expire or carry a stale generation.
        self.session.refresh()
        if self.session.generation is None or self.session.tick is None:
            self.evidence.write("play_no_snapshot", condition=pending.proposal.condition)
            self._transition(LoopState.IDLE, reason="no_snapshot")
            return

        try:
            command_id = self._send_goal(skill, pending.proposal.condition)
        except PortError as error:
            self.evidence.write("command_send_failed", error=str(error), skill=skill)
            self._transition(LoopState.IDLE, reason="send_failed")
            return

        record = self.records.new(
            pending,
            status="open",
            skill=skill,
            origin=self._skill_origin(skill, pending),
            command_id=command_id,
        )
        self.memory.append_goal(record)
        self._open_goal = OpenGoal(
            record=record,
            proposal=pending.proposal,
            skill=skill,
            command_id=command_id,
            sent_tick=int(self.session.tick or 0),
        )
        self.evidence.write(
            "command_sent",
            goal_id=record.goal_id,
            command_id=command_id,
            skill=skill,
            condition=pending.proposal.condition,
            expires_at_tick=int(self.session.tick or 0) + self.session.command_ttl_ticks,
            based_on_generation=self.session.generation,
            budget_ticks=self.command_budget_ticks,
        )
        self._pending = None
        self._transition(LoopState.PLAYING, reason="command_sent")

    # -- helpers -------------------------------------------------------- #
    def _send_goal(self, skill: str, condition: str) -> str:
        """Send one command; the one send path, so the count always travels.

        Code-only, proposal and stale-retry sends all come through here, which
        is why :meth:`_goal_args` cannot be forgotten on one of them.
        """
        return self.session.send_goal(
            skill,
            condition,
            self._goal_args(skill, condition),
        )

    def _goal_args(self, skill: str, condition: str) -> list[str]:
        """The count a `count(X) >= n` goal asks for, as the skill's first
        argument when the skill takes one: `get_wood()` alone stops at its
        default and a "collect more wood" goal settled `skill_wrong` at once
        (autonomous run 2, 2026-11-08)."""
        info = next((s for s in self.catalog.skills if s.name == skill), None)
        if info is None or not info.params or "count(" not in condition:
            return []
        try:
            return [str(count_of_condition(condition))]
        except ConditionError:
            return []

    def _maybe_heartbeat(self) -> None:
        if self.session.awaiting_snapshot:
            return
        now = self._clock()
        if now - self._last_heartbeat < self.heartbeat_seconds:
            return
        self._last_heartbeat = now
        if not self.session.request_snapshot():
            return
        self.evidence.write("heartbeat_requested", tick=self.session.tick)

    def _inventory(self) -> dict[str, int]:
        facts = (self.session.snapshot or {}).get("facts") or {}
        inventory = facts.get("inventory") or {}
        return {str(name): int(amount) for name, amount in inventory.items()}

    def _skill_origin(self, skill: str, pending: PendingGoal) -> str:
        if pending.source == "written":
            return "written"
        info = self.catalog.by_name(skill)
        return info.origin if info is not None else "active"


__all__ = [
    "BODY_WAKE_LIMIT_SECONDS",
    "FAILURE_GAP_TICKS",
    "FAILURE_STREAK",
    "FAILURE_STREAK_GAP_TICKS",
    "HEARTBEAT_SECONDS",
    "MAX_PROPOSALS",
    "MAX_STEP_ERRORS",
    "Loop",
    "LoopState",
    "WakeCause",
]
