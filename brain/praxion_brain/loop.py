"""The autonomous brain loop: pick, route, write, play, settle.

A small state machine over one :class:`~praxion_brain.session.BodySession`:

```
IDLE --wake--> PICKING --proposal--> ROUTING --play----------> PLAYING
                                       \\--write--> admission job (background)
                                                       \\--> IDLE at once
job admitted --> the next pick plays that goal
job rejected/inconclusive --> the goal is recorded failed, with the usual gap
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

Admission takes minutes, so it runs as a background job
(:class:`~praxion_brain.admission_job.AdmissionJob`): the loop records the goal
as open, starts the job and returns to IDLE at once, so the body keeps playing.
While the job runs, the pick excludes its goal (the briefing names it) and a
proposal that would need a second write is deferred with the failure gap, not
recorded as a failure. When the job ends, an admitted skill is the next pick; a
rejection or an inconclusive run fails the goal with its admission status. A
console stop while the job runs marks it, so its goal is recorded
``interrupted``/``stopped_by_user`` when it ends and is never played, even if
the admission succeeded. Closing the loop reaps a job that already finished,
cancels one still running (killing its practice stack; nothing goes active) and
records that goal interrupted; the final state is published so the console
drops the learning badge.

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
from .admission_job import AdmissionJob
from .briefing import build as build_briefing
from .cancel import CancelToken
from .conditions import ConditionError, count_of_condition
from .console.hub import Console
from .briefing import heartbeat_change
from . import survival
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
#: The console state is republished at most this often (seconds).
PUBLISH_SECONDS = 0.25


class LoopState(str, Enum):
    IDLE = "idle"
    PICKING = "picking"
    ROUTING = "routing"
    PLAYING = "playing"


class WakeCause(str, Enum):
    START = "start"
    GOAL_SETTLED = "goal_settled"
    GOAL_FAILED = "goal_failed"
    BODY_WAKE = "body_wake"
    HEARTBEAT = "heartbeat"
    RECONNECT = "reconnect"
    USER = "user"
    WRITE_STARTED = "write_started"
    WRITE_DEFERRED = "write_deferred"
    SKILL_ADMITTED = "skill_admitted"


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
        console: Console | None = None,
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
        #: The one admission job in flight; the loop starts no second one.
        self._job: Optional[AdmissionJob] = None
        #: An admitted skill whose goal waits for the next pick.
        self._written: Optional[PendingGoal] = None
        self._console = console
        self._paused = False
        self._user_goals: list[GoalProposal] = []
        self._last_route: Optional[dict[str, Any]] = None
        self._last_publish = -float("inf")

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
        """Stop for good: reap a finished job, cancel a running one, publish.

        A job that already finished is read here, so its real outcome reaches
        the record and the evidence (an admitted skill stays active); only a
        job still running is recorded ``interrupted``. The final publish clears
        the console's learning badge, which would otherwise outlive the loop.
        """
        self._stopped = True
        self._reap_job()
        self._abandon_job()
        self.session.close()
        self.evidence.write("loop_stopped", world_id=self.world_id, state=self.state.value)
        self._publish(force=True)

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

        self._apply_controls()
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

        self._reap_job()
        self._advance()
        self._survival_check()
        if self.session.terminated:
            # A terminal bye can also arrive inside a snapshot refresh.
            self._stopped = True
        if not self._step_failed:
            self._step_errors = 0
        self._step_failed = False
        self._publish()
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
            snapshot.stack,
            tick=snapshot.tick,
            cause=cause.value,
            keep=self._job_goal_ids(),
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
        if event.get("cause") == "hunger_no_food" and self.state == LoopState.IDLE:
            # The body is hungry and holds nothing: the pick starts get_food
            # (survival.choose) without waiting for a heartbeat.
            self._set_wake(WakeCause.BODY_WAKE)
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
            if open_goal.stop_pending and open_goal.intent_id is not None:
                try:
                    self.session.port.pop(open_goal.intent_id)
                    self.evidence.write("stop_sent_on_accept", intent_id=open_goal.intent_id)
                except PortError as error:
                    self.evidence.write("stop_failed", error=str(error))
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
            outcome = event.get("outcome")
            if outcome in ("cancelled", "popped"):
                # The body confirmed the stop of this goal: a pop of a running
                # intent settles `cancelled`, a pop of a done or suspended one
                # settles `popped`. Only now is the goal recorded: a pop on its
                # own is a request, not proof the skill stopped (§5.4). The
                # cause is the one that asked for the stop, never the console by
                # default: an interrupt by survival or by a queued user goal is
                # not something the user did (review of slice 3, finding 1).
                cause = open_goal.stop_source
                self.records.finish(
                    open_goal.record,
                    status="interrupted",
                    failure_kind=(
                        "stopped_by_user" if cause == "console" else f"preempted_by_{cause}"
                    ),
                    ticks=open_goal.ticks_since(self.session.tick),
                    source="survival" if cause == "survival" else "console",
                )
                return
            condition = event.get("condition")
            status, failure_kind = settled_status(
                outcome,
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
                open_goal.skill, open_goal.proposal.condition, open_goal.budget_ticks
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
            if self._paused:
                return
            if not self._user_goals and not self._pick_ready():
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
            self._note_failure(record.failure_kind)
        else:
            self._consecutive_failures = 0
            self._next_pick_tick = 0
        self._set_wake(
            WakeCause.GOAL_SETTLED if record.status == "done" else WakeCause.GOAL_FAILED
        )

    def _note_failure(self, failure_kind: Optional[str]) -> None:
        """One failed goal: count the streak and hold the pick for the gap."""
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
            failure_kind=failure_kind,
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
        # The user's goal first, then survival, then the rest: the user's
        # projects come before Praxion's own ideas (architecture §1), so a
        # survival goal is exempt from the need gate but never above the user.
        if self._pick_user_goal(inventory):
            return
        if self._pick_survival(inventory):
            return
        if self._pick_written_goal(inventory):
            return
        if self.governor.code_only:
            self._pick_code_only("governor_code_only")
            return

        briefing = self._briefing()
        excluded = False
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
            if self._excluded_by_job(proposal):
                # The skill being written is not ready: ask for another goal.
                # Not counted invalid, so the repeat of the goal that started
                # the job does not escalate to the dearer model (run 5).
                excluded = True
                self.evidence.write(
                    "proposal_writing_excluded",
                    proposal=proposal.model_dump(),
                    condition=proposal.condition,
                    validation_reason=result.reason,
                    writing=self._writing_skill(),
                )
                continue
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

        self._pick_code_only("proposals_excluded" if excluded else "proposals_invalid")

    # -- survival (decision 2026-11-09 "Five next areas") ------------------ #
    def _facts(self) -> dict[str, Any]:
        return dict((self.session.snapshot or {}).get("facts") or {})

    def _due_survival(self) -> Optional["survival.SurvivalGoal"]:
        return survival.choose(
            self._facts(),
            self._inventory(),
            self.catalog,
            self.memory,
            stack=(self.session.snapshot or {}).get("stack") or (),
        )

    def _pick_survival(self, inventory: dict[str, int]) -> bool:
        """Food or night comes before every autonomous goal, decided in code.

        No model call: the facts say it is due. The goal goes straight to the
        body (a known core skill), exempt from the need gate's rules. A queued
        user goal still goes first (``_pick``): the user's projects come before
        Praxion's own ideas (architecture §1).
        """
        due = self._due_survival()
        if due is None:
            return False
        validation = due.validation()
        self._pending = PendingGoal(
            proposal=due.proposal,
            validation=validation,
            goal=goal_of(due.proposal, validation),
            source="survival",
            origin="survival",
            forced_skill=due.skill,
            budget_ticks=due.budget_ticks,
        )
        self._last_proposal = due.proposal
        self.evidence.write(
            "survival_goal",
            skill=due.skill,
            survival=due.kind,
            condition=due.proposal.condition,
            reason=due.reason,
        )
        self._transition(LoopState.ROUTING, reason=f"survival:{due.kind}")
        return True

    def _survival_check(self) -> None:
        """Between picks: a due survival goal interrupts an autonomous goal and
        a queued user goal interrupts everything; an idle loop wakes.

        A goal in progress that is not a survival or user goal is stopped through
        the console's stop path (the body pops it and confirms ``cancelled``);
        the next pick then plays the survival goal. A user's task is never
        interrupted: the body's own arbiter still eats when it holds food and its
        night fallback covers a silent brain. A survival goal of ours does yield
        to a queued user goal, exactly as the body's fallback yields to a user
        command: the user's projects come first (architecture §1) and the night
        fallback takes the night again when their goal settles.
        """
        if self._paused or self.session.snapshot is None:
            return
        opened = self._open_goal
        if self.state == LoopState.PLAYING and opened is not None:
            if opened.stop_pending:
                # A pop is already out; that stop is the only one this goal gets
                # (a second pop per step would fight the first, review of
                # slice 3). The receipt settles it.
                return
            if opened.record.origin == "user":
                return
            if self._user_goals:
                # The user outranks our own survival goal (architecture §1).
                self._interrupt_goal("user", reason="user goal waiting")
                return
            if opened.survival:
                # A survival goal is never interrupted by another survival goal.
                return
            due = self._due_survival()
            if due is not None:
                self._interrupt_goal("survival", survival=due.kind, reason=due.reason)
        elif self.state == LoopState.IDLE and self._wake is None and self._job is None:
            if self._due_survival() is not None:
                self._set_wake(WakeCause.BODY_WAKE)

    def _interrupt_goal(self, cause: str, **fields: Any) -> None:
        """Stop the open goal with the reason it was stopped.

        The stop cause travels on the open goal, so the receipt that confirms
        the pop closes the record with ``preempted_by_<cause>`` and the matching
        source instead of the console's ``stopped_by_user``: nothing the user
        did must be written to the ledger as though they had (review of slice 3,
        finding 1). ``_stop_goal`` marks the pop as sent before the next step
        looks at the goal again, so one interrupt is one pop (finding 2).
        """
        opened = self._open_goal
        if opened is None or opened.stop_pending:
            return
        opened.stop_source = cause
        self.evidence.write(
            f"{cause}_interrupt",
            goal_id=opened.record.goal_id,
            **fields,
        )
        self._stop_goal()

    def _pick_written_goal(self, inventory: dict[str, int]) -> bool:
        """Play the goal whose skill the background admission just admitted."""
        pending, self._written = self._written, None
        if pending is None:
            return False
        # The written goal was never sent, so "same as last" does not apply: the
        # last proposal is usually this very goal.
        result = validate(pending.proposal, inventory, self.memory, None)
        if result.ok:
            pending.validation = result
            pending.goal = goal_of(pending.proposal, result)
            self._pending = pending
            self._last_proposal = pending.proposal
            self._invalid_proposals = 0
            self._play()
            return True

        # The world moved while the skill was written; the goal is not worth a
        # command now. Close the writing record without a failure.
        record = pending.record
        done = result.reason == "already_met"
        if record is not None:
            self.records.finish(
                record,
                status="done" if done else "interrupted",
                condition_result="true" if done else None,
                failure_kind=None if done else result.reason,
                source="written",
                wake=False,
            )
        self.evidence.write(
            "written_goal_skipped",
            goal_id=record.goal_id if record is not None else None,
            condition=pending.proposal.condition,
            reason=result.reason,
            detail=result.detail,
        )
        return False

    def _pick_user_goal(self, inventory: dict[str, int]) -> bool:
        """The user's goals come before the proposer; an invalid one is dropped."""
        while self._user_goals:
            proposal = self._user_goals.pop(0)
            result = validate(proposal, inventory, self.memory, self._last_proposal)
            self.evidence.write(
                "user_goal_result",
                proposal=proposal.model_dump(),
                validation=result.model_dump(),
            )
            if result.ok:
                self._pending = PendingGoal(
                    proposal=proposal,
                    validation=result,
                    goal=goal_of(proposal, result),
                    origin="user",
                )
                self._last_proposal = proposal
                self._transition(LoopState.ROUTING, reason="user_goal")
                return True
        return False

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
        if self._excluded_by_job(proposal):
            # The same goal is being written; a second command would preempt
            # the skill the loop is about to admit.
            self.evidence.write(
                "code_only_writing",
                reason=reason,
                condition=proposal.condition,
                skill=fallback.skill,
                writing=self._writing_skill(),
            )
            self._transition(LoopState.IDLE, reason="code_only_writing")
            return False
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
            writing=self._writing_skill(),
        )

    # -- ROUTING -------------------------------------------------------- #
    def _route(self) -> None:
        pending = self._pending
        if pending is None:
            self._transition(LoopState.IDLE, reason="route_without_goal")
            return

        if pending.source in ("code_only", "survival") and pending.forced_skill:
            self.evidence.write(
                "route_code_only",
                skill=pending.forced_skill,
                condition=pending.proposal.condition,
            )
            self._last_route = {
                "decision": "play",
                "skill": pending.forced_skill,
                "scores": None,
                "condition": pending.proposal.condition,
                "source": pending.source,
            }
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
        self._last_route = {
            "decision": routed.kind,
            "skill": routed.skill,
            "scores": routed.scores,
            "condition": pending.proposal.condition,
        }
        if routed.kind == "play":
            skill = routed.skill or pending.validation.skill_name
            if not skill:
                self.evidence.write("route_no_skill", condition=pending.proposal.condition)
                self._transition(LoopState.IDLE, reason="route_no_skill")
                return
            pending.forced_skill = skill
            self._play()
            return
        self._start_admission(pending)

    # -- WRITING (background) -------------------------------------------- #
    def _start_admission(self, pending: PendingGoal) -> None:
        """Start the one admission job and hand the body straight back to IDLE.

        The goal is recorded open (not failed) before the job starts, so the
        history and the console show what is being learned; the job's result is
        read in :meth:`_reap_job` on the loop thread. A second write while a job
        runs is deferred: the skill is not ready and a second admission would
        spend money twice for the same wait.
        """
        if self._job is not None:
            self._defer_write(pending)
            return

        denied = self._gate("write", admission_estimate(self.writer_model))
        if denied:
            if self._pick_code_only(denied):
                self._play()
            return

        skill = pending.validation.skill_name or pending.goal.skill_name
        record = self.records.new(
            pending,
            status="open",
            skill=skill,
            origin=None,
        )
        self.memory.append_goal(record)
        job = AdmissionJob(
            pending, record, skill=skill, wall_clock=self._wall_clock
        )
        self._job = job
        self._pending = None
        job.start(
            lambda: self.governor.call(
                "admission",
                lambda: self._admit(
                    pending.goal, progress=job.update, cancel=job.cancel
                ),
                cancel=job.cancel,
            )
        )
        self.evidence.write(
            "write_started",
            goal_id=record.goal_id,
            skill=skill,
            condition=pending.proposal.condition,
            writer_model=self.writer_model,
        )
        self._transition(LoopState.IDLE, reason="write_started")
        self._set_wake(WakeCause.WRITE_STARTED)

    def _defer_write(self, pending: PendingGoal) -> None:
        """A write route while a job runs: back to IDLE with the gap, no failure."""
        writing = self._writing_skill()
        self._pending = None
        self.evidence.write(
            "write_deferred",
            condition=pending.proposal.condition,
            skill=pending.validation.skill_name,
            writing=writing,
        )
        self._transition(LoopState.IDLE, reason="write_deferred")
        now = int(self.session.tick or 0)
        self._next_pick_tick = max(self._next_pick_tick, now + FAILURE_GAP_TICKS)
        self._set_wake(WakeCause.WRITE_DEFERRED)

    def _reap_job(self) -> None:
        """Read a finished admission job; the one place its result is applied.

        A job the console stopped while it ran still reports its result (an
        admitted skill stays active) but its goal is closed interrupted and is
        never played.
        """
        job = self._job
        if job is None or not job.finished:
            return
        self._job = None
        if job.abandoned:
            return  # the shutdown path already recorded the goal
        # The provider call itself counts for the breaker whether or not a
        # stop arrived, so the accounting happens before the result branches.
        if job.error is not None:
            self.governor.record_error()
        else:
            self.governor.record_success()
        if job.stop_requested:
            self._writing_stopped(job)
            return
        if job.error is not None:
            error = job.error
            if isinstance(error, CallTimeout):
                self.evidence.write(
                    "write_timeout",
                    call=error.call_class,
                    seconds=error.seconds,
                    goal_id=job.record.goal_id,
                    skill=job.record.skill,
                )
                self._writing_failed(job, "timeout", detail=str(error))
                return
            self.evidence.write(
                "admission_error",
                error=f"{type(error).__name__}: {error}",
                goal_id=job.record.goal_id,
                skill=job.record.skill,
            )
            self._writing_failed(job, "inconclusive_body", detail=str(error))
            return

        report = job.report
        if report is None:
            self.evidence.write(
                "admission_error",
                error="the admission job finished without a report",
                goal_id=job.record.goal_id,
                skill=job.record.skill,
            )
            self._writing_failed(job, "inconclusive_body")
            return
        if report.status == "admitted":
            self._writing_admitted(job, report)
            return
        self._writing_failed(job, report.status, detail=report.reason)

    def _writing_admitted(self, job: AdmissionJob, report: AdmissionReport) -> None:
        """The skill is active: the next pick plays the goal that asked for it."""
        pending = job.pending
        self.catalog = self._catalog_loader()
        pending.forced_skill = report.skill_name
        pending.validation = pending.validation.model_copy(
            update={"skill_name": report.skill_name}
        )
        pending.source = "written"
        pending.record = job.record
        self._written = pending
        self.evidence.write(
            "admission_admitted",
            goal_id=job.record.goal_id,
            skill=report.skill_name,
            repairs=report.repairs_used,
        )
        if self.state == LoopState.IDLE:
            self._set_wake(WakeCause.SKILL_ADMITTED)

    def _writing_failed(
        self,
        job: AdmissionJob,
        failure_kind: str,
        *,
        detail: Optional[str] = None,
    ) -> None:
        """Rejected or inconclusive: close the writing record as a real failure.

        The body may be playing another goal, so this never touches the
        playing record or the state: it only feeds failure memory and the gap.
        """
        self.records.finish(
            job.record,
            status="failed",
            failure_kind=str(failure_kind),
            detail=detail,
            source="admission",
            wake=False,
        )
        self._note_failure(str(failure_kind))
        if self.state == LoopState.IDLE:
            self._set_wake(WakeCause.GOAL_FAILED)

    def _writing_stopped(self, job: AdmissionJob) -> None:
        """The console stopped while the job ran: report it, never play it.

        The job ran to its own end (a practice series cannot be interrupted
        safely mid-run), so its result reaches the evidence and an admitted
        skill stays in ``active/``, where the body hot-loads it. Only the goal
        is closed: ``interrupted``/``stopped_by_user``, no failure memory and
        no play, because the user ended it.
        """
        report = job.report
        if report is not None and report.status == "admitted":
            # The file is on disk now; the catalog must know the skill even
            # though this pick will not be the one that plays it.
            self.catalog = self._catalog_loader()
        self.records.finish(
            job.record,
            status="interrupted",
            failure_kind="stopped_by_user",
            source="console",
            wake=False,
        )
        self.evidence.write(
            "write_stopped",
            goal_id=job.record.goal_id,
            skill=job.record.skill,
            admission_status=None if report is None else report.status,
            error=None
            if job.error is None
            else f"{type(job.error).__name__}: {job.error}",
        )

    def _abandon_job(self) -> None:
        """Stop with a job still running: cancel it, record the goal.

        The thread is a daemon and is never joined: a slow practice run must not
        hold up a shutdown. ``cancel`` kills the practice stack and stops the
        worker at its next boundary, so its late result cannot copy a skill
        active; ``_job`` is cleared and the record closed, so nothing reaps it.
        """
        job, self._job = self._job, None
        if job is not None:
            job.abandoned = True
            job.cancel.set()
            self.records.finish(
                job.record,
                status="interrupted",
                failure_kind="abandoned",
                source="shutdown",
                wake=False,
            )
            self.evidence.write(
                "write_abandoned",
                goal_id=job.record.goal_id,
                skill=job.record.skill,
            )
        written, self._written = self._written, None
        if written is not None and written.record is not None:
            self.records.finish(
                written.record,
                status="interrupted",
                failure_kind="not_played",
                source="shutdown",
                wake=False,
            )

    def _admit(
        self,
        goal: Goal,
        *,
        progress: Optional[Callable[[dict[str, object]], None]] = None,
        cancel: Optional[CancelToken] = None,
    ) -> AdmissionReport:
        """Run admission with this loop's evidence, ledger, writer model and deadlines.

        The writer call carries the ``write`` deadline and the whole run the
        ``admission`` deadline (A4); tests override the writer, checker and
        practice room through ``admission_options``. ``progress`` feeds the
        console's learning state from the job thread; ``cancel`` is the job's
        token, so a timeout or a shutdown stops the run before it goes active.
        """
        options = dict(self._admission_options)
        options.setdefault("evidence", self.evidence)
        options.setdefault("ledger", self.ledger)
        options.setdefault("catalog", self.catalog)
        options.setdefault("writer_model", self.writer_model)
        options.setdefault("on_progress", progress)
        # The job's token is the admission's token: the timeout and a shutdown
        # reach exactly the run that is being admitted.
        options["cancel"] = cancel
        write_fn = options.get("write_fn") or write_skill
        # The writer call is part of the same admission: a write deadline that
        # expires stops it just like the admission deadline does.
        options["write_fn"] = lambda **kwargs: self.governor.call(
            "write", lambda: write_fn(**kwargs), cancel=cancel
        )
        return admit(goal, **options)

    # -- PLAYING -------------------------------------------------------- #
    def _play(self) -> None:
        pending = self._pending
        if pending is None:
            self._transition(LoopState.IDLE, reason="play_without_goal")
            return
        skill = pending.forced_skill or pending.validation.skill_name
        if not skill:
            self.evidence.write("play_no_skill", condition=pending.proposal.condition)
            self._drop_unplayed(pending, "no_skill")
            self._transition(LoopState.IDLE, reason="no_skill")
            return
        # Section 2.4: refresh the tick and generation from a snapshot right
        # before sending, so a long proposal/Jev/admission call cannot make the
        # command expire or carry a stale generation.
        self.session.refresh()
        if self.session.generation is None or self.session.tick is None:
            self.evidence.write("play_no_snapshot", condition=pending.proposal.condition)
            self._drop_unplayed(pending, "no_snapshot")
            self._transition(LoopState.IDLE, reason="no_snapshot")
            return

        try:
            command_id = self._send_goal(
                skill, pending.proposal.condition, pending.budget_ticks
            )
        except PortError as error:
            self.evidence.write("command_send_failed", error=str(error), skill=skill)
            self._drop_unplayed(pending, "send_failed")
            self._transition(LoopState.IDLE, reason="send_failed")
            return

        if pending.record is not None:
            # The background admission opened this record; the command continues
            # it, so one goal is one line, not an open write plus a play.
            record = pending.record.model_copy(
                update={
                    "status": "open",
                    "command_id": command_id,
                    "skill": skill,
                    "skill_origin": self._skill_origin(skill, pending),
                }
            )
            record = self.memory.append_goal(record)
        else:
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
            survival=pending.source == "survival",
            budget_ticks=pending.budget_ticks,
        )
        self.evidence.write(
            "command_sent",
            goal_id=record.goal_id,
            command_id=command_id,
            skill=skill,
            condition=pending.proposal.condition,
            expires_at_tick=int(self.session.tick or 0) + self.session.command_ttl_ticks,
            based_on_generation=self.session.generation,
            budget_ticks=(
                self.command_budget_ticks
                if pending.budget_ticks is None
                else pending.budget_ticks
            ),
        )
        self._pending = None
        self._transition(LoopState.PLAYING, reason="command_sent")

    # -- the console ---------------------------------------------------- #
    def _apply_controls(self) -> None:
        """Apply what the console queued; each request is written to evidence."""
        if self._console is None:
            return
        for request in self._console.control.drain():
            action = request.get("action")
            result = "ok"
            extra: dict[str, Any] = {}
            if action == "pause":
                self._paused = True
            elif action == "resume":
                self._paused = False
                if self.state == LoopState.IDLE:
                    self._set_wake(WakeCause.USER)
            elif action == "stop_goal":
                result, extra = self._stop_goal()
            elif action == "new_goal":
                result = self._queue_user_goal(request)
            else:
                result = "unknown_action"
            self.evidence.write(
                "console_control",
                action=action,
                text=request.get("text"),
                condition=request.get("condition"),
                result=result,
                **extra,
            )

    def _stop_goal(self) -> tuple[str, dict[str, Any]]:
        """Stop the goal being played, or cancel the one being prepared.

        The body cancels a *running* intent when it is popped (stop and cancel
        always work, architecture §5.4). The pop is a request: the record of the
        running goal closes as interrupted only when the body's ``settled
        outcome=cancelled`` receipt arrives (see :meth:`_on_receipt`), so the
        console shows the stop as confirmed then. The intent state is reported
        too, for evidence. Once the pop is out the goal carries ``stop_pending``
        until its receipt, so no second stop path (a console stop, a survival or
        user interrupt) sends another pop for the same goal.

        While no goal is open, a stop cancels the pick, route or admission in
        flight and drops the goals waiting behind it. An admitted skill waiting
        for the next pick is dropped too. A running admission job cannot be
        stopped safely mid-practice, so it is *marked*: when it ends, its goal is
        recorded ``interrupted`` with the cause that asked for the stop and is
        never played, even if the admission succeeded (the skill may still go
        active).
        """
        opened = self._open_goal
        # Whoever asked for the stop: the open goal carries it (``console`` by
        # default). A survival or user interrupt closes the waiting write with
        # the same cause instead of inventing the user's stop (review of slice 3,
        # finding 1).
        cause = opened.stop_source if opened is not None else "console"
        written, self._written = self._written, None
        written_condition = written.proposal.condition if written is not None else None
        if written is not None and written.record is not None:
            self.records.finish(
                written.record,
                status="interrupted",
                failure_kind=(
                    "stopped_by_user" if cause == "console" else f"preempted_by_{cause}"
                ),
                source="survival" if cause == "survival" else "console",
                wake=False,
            )

        if opened is not None:
            if opened.stop_pending and opened.intent_id is not None:
                # The pop is already out; its receipt has not arrived yet. A
                # second stop (the console's Stop, another interrupt) must not
                # pop the same goal twice (review of slice 3, finding 2).
                return "pop_sent", {"goal_id": opened.record.goal_id, "already_sent": True}
            if opened.intent_id is None:
                # Sent but not yet accepted: the body has no intent to pop. Keep
                # the stop and pop on the accepted receipt, or it would be lost.
                opened.stop_pending = True
                return "stop_queued", {"goal_id": opened.record.goal_id}
            stack = (self.session.snapshot or {}).get("stack") or []
            entry = next((e for e in stack if e.get("intent_id") == opened.intent_id), None)
            extra = {
                "goal_id": opened.record.goal_id,
                "intent_id": opened.intent_id,
                "intent_state": entry.get("state") if entry else None,
            }
            if written_condition is not None:
                extra["written_cancelled"] = written_condition
            try:
                self.session.port.pop(opened.intent_id)
            except PortError as error:
                return f"pop_failed: {error}", extra
            # One goal, one stop: until the receipt confirms it, every other
            # stop path (the console, a survival interrupt) leaves it alone.
            opened.stop_pending = True
            return "pop_sent", extra

        pending = self._pending
        if (
            pending is None
            and not self._user_goals
            and self._job is None
            and written_condition is None
            and self.state not in (LoopState.PICKING, LoopState.ROUTING)
        ):
            return "no_goal", {}
        job = self._job
        if job is not None:
            # The job cannot stop mid-practice: mark it so the reap closes its
            # goal as the user's stop instead of playing it.
            job.stop_requested = True
        self._pending = None
        self._wake = None
        self._user_goals.clear()
        self._transition(LoopState.IDLE, reason="stop_goal")
        extra = {}
        if pending is not None:
            extra["cancelled_condition"] = pending.proposal.condition
            return "pick_cancelled", extra
        if job is not None:
            extra["writing"] = job.progress()["skill"]
            extra["stop_requested"] = True
            return "job_running", extra
        if written_condition is not None:
            extra["cancelled_condition"] = written_condition
        return "pick_cancelled", extra

    def _queue_user_goal(self, request: Mapping[str, Any]) -> str:
        """Queue the console's goal; the condition is what can be verified."""
        condition = str(request.get("condition") or "").strip()
        text = str(request.get("text") or "").strip()
        if not condition:
            return "no_condition"
        self._user_goals.append(
            GoalProposal(text=text or condition, condition=condition, rationale="user goal")
        )
        if self.state == LoopState.IDLE:
            self._set_wake(WakeCause.USER)
        return "queued"

    def _publish(self, *, force: bool = False) -> None:
        """Hand the console a fresh read-only state (copy on update, no lock).

        ``force`` skips the throttle: a shutdown must publish its final state,
        or the console keeps showing the learning badge of a dead loop.
        """
        if self._console is None:
            return
        now = time.monotonic()
        if not force and now - self._last_publish < PUBLISH_SECONDS:
            return
        self._last_publish = now
        snapshot = self.session.snapshot or {}
        opened = self._open_goal
        proposal = self._last_proposal
        learning = None
        if self._job is not None:
            progress = self._job.progress()
            learning = {
                "skill": progress["skill"],
                "attempt": progress["attempt"],
                "seed": progress["seed"],
                "started_at": progress["started_at"],
                "goal_id": self._job.record.goal_id,
            }
        self._console.publish(
            {
                "world_id": self.world_id,
                "state": self.state.value,
                "paused": self._paused,
                "connected": self.session.connected,
                "wake": self._wake.value if self._wake is not None else None,
                "tick": self.session.tick,
                "generation": self.session.generation,
                "owner": snapshot.get("owner"),
                "facts": snapshot.get("facts"),
                "stack": snapshot.get("stack"),
                "learning": learning,
                "goal": None
                if opened is None
                else {
                    "goal_id": opened.record.goal_id,
                    "text": opened.record.text,
                    "condition": opened.record.condition,
                    "skill": opened.skill,
                    "origin": opened.record.origin,
                    "sent_tick": opened.sent_tick,
                    "ticks": opened.ticks_since(self.session.tick),
                },
                "proposal": proposal.model_dump() if proposal is not None else None,
                "route": self._last_route,
                "ladder": ladder_progress(self._inventory()).model_dump(),
                "governor": {
                    "code_only": self.governor.code_only,
                    "breaker": self.governor.breaker_state,
                    "hard_stop_usd": self.governor.hard_stop_usd,
                    "spent_session": self.governor.spent_session(),
                },
                "user_goals": len(self._user_goals),
                "updated": self._wall_clock(),
            }
        )

    # -- helpers -------------------------------------------------------- #
    def _send_goal(
        self, skill: str, condition: str, budget_ticks: Optional[int] = None
    ) -> str:
        """Send one command; the one send path, so the count always travels.

        Code-only, proposal, survival and stale-retry sends all come through
        here, which is why :meth:`_goal_args` cannot be forgotten on one of
        them. ``budget_ticks`` overrides the default per-goal budget: a night
        goal needs longer than the default or the body fails it mid-night.
        """
        return self.session.send_goal(
            skill,
            condition,
            self._goal_args(skill, condition),
            budget_ticks=budget_ticks,
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

    def _drop_unplayed(self, pending: PendingGoal, reason: str) -> None:
        """Close the record a background admission opened for a goal never sent."""
        record = pending.record
        if record is None:
            return
        self.records.finish(
            record,
            status="interrupted",
            failure_kind=reason,
            source="play",
            wake=False,
        )

    def _writing_skill(self) -> Optional[str]:
        """The skill a background job is writing, or ``None``."""
        if self._job is None:
            return None
        return self._job.progress()["skill"]

    def _job_goal_ids(self) -> list[str]:
        """The records the loop still owns; recovery must not close them."""
        owned = []
        if self._job is not None and self._job.record is not None:
            owned.append(self._job.record.goal_id)
        if self._written is not None and self._written.record is not None:
            owned.append(self._written.record.goal_id)
        return owned

    def _excluded_by_job(self, proposal: GoalProposal) -> bool:
        """Whether this goal is the one being written right now."""
        job = self._job
        if job is None or job.record is None:
            return False
        if normalize_condition(proposal.condition) == normalize_condition(
            job.record.condition
        ):
            return True
        writing = job.progress()["skill"]
        return bool(writing) and goal_of(proposal).skill_name == writing

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
