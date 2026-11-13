"""The autonomous brain loop: pick, route, write, play, settle.

A small state machine over one :class:`~praxion_brain.session.BodySession`:

```
IDLE --wake--> PICKING --plan step--> ROUTING --play----------> PLAYING
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
proposal that would need a second write is deferred with the ``nothing_to_do``
back-off, not recorded as a failure. When the job ends, an admitted skill is the next pick; a
rejection or an inconclusive run fails the goal with its admission status. A
console stop while the job runs marks it, so its goal is recorded
``interrupted``/``stopped_by_user`` when it ends and is never played, even if
the admission succeeded. Closing the loop reaps a job that already finished,
cancels one still running (killing its practice stack; nothing goes active) and
records that goal interrupted; the final state is published so the console
drops the learning badge.

The pick plays a plan queue (decision 2026-11-12 "Demo night"): one planner
call returns 1 to 5 goals and the loop plays them one by one, validating each in
code when its turn comes. The queue is dropped (evidence ``plan_dropped``) after a
failed or interrupted goal, a death, a survival goal, an operator task, a skill
write or a world desync, and the next pick plans again; a goal that finishes
``done`` keeps it.

At night a body that holds a pickaxe and is healthy above
``survival.SHELTER_HEALTH`` keeps working instead of sheltering (decision
2026-11-13 "night mining", run B): the survival module starts no night goal, the
pick plans again, and only ``survival.NIGHT_WORK_SKILLS`` pass the turn-time
validation (``night_unsafe`` otherwise, also for the skill Jev finally routed
to). Dusk drops the daytime plan and dawn drops the night one; when health falls
to ``survival.SHELTER_HEALTH`` the ordinary survival path interrupts the work.

A first failed goal does not delay the next pick (a new plan is the answer); the
second in a row waits :data:`FAILURE_GAP_TICKS` body ticks (or
:data:`FAILURE_STREAK_GAP_TICKS` after :data:`FAILURE_STREAK` failures in a row)
while the loop stays IDLE, so a broken world cannot spin. Code-only picks obey
the same failure memory as plan steps, so a blocked goal waits instead of
spinning. A pick that ends with no goal at all backs off as
``nothing_to_do`` (:data:`NOTHING_TO_DO_GAP_TICKS`, doubling) and picks again;
so does a route or a play that ends without a goal (router timeout or error, no
skill, no snapshot, a failed send); none leaves the loop IDLE with nothing to
wake it (run 8). A ``world_desync`` wake from the body stops picking for good; a
``died`` wake re-picks at once. A goal the body settles ``failed(died)`` or
``failed(world_desync)`` is recorded ``interrupted``: it blocks nothing, does not
count in the failure streak and is not narrated twice.

Every expensive step is injectable so the loop is tested against
:class:`FakeBody` with fake planner, router and admission callables.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` sections 3.1-3.7.
"""

from __future__ import annotations

import importlib
import re
import time
from collections import deque
from enum import Enum
from typing import Any, Callable, Mapping, Optional, Sequence

from . import commands, config, persona as personas, speech
from .admission import (
    AdmissionReport,
    admit,
    estimate as admission_estimate,
)
from .admission_job import AdmissionJob
from .briefing import build as build_briefing
from .cancel import CancelToken
from .conditions import ConditionError, count_of_condition, evaluate, item_of_condition
from .console.hub import Console
from .briefing import heartbeat_change
from . import survival
from .curriculum import GoalProposal, Validation, goal_of, validate
from .evidence import Evidence
from .goal import Goal
from .governor import CallTimeout, Governor
from .ladder import RUNGS, code_only_goal, first_unmet, progress as ladder_progress
from .ledger import Ledger, estimate_cost
from .memory import NOT_FAILURE_KINDS, GoalRecord, Memory, normalize_condition
from .needs import Gamedata, need_line, rung_need
from .planner import MAX_GOALS, Plan, PlannedGoal, estimate as plan_estimate, plan as make_plan, shape_problem
from .port import EventMessage, PortClient, PortError, Snapshot
from .proposer import DEFAULT_MODEL
from .records import GoalRecords, OpenGoal, PendingGoal, settled_status
from .router import DEFAULT_MODEL as ROUTER_MODEL
from .router import PUBLIC_EXCLUDE, Route
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
#: Minimum body ticks between a failed goal and the next pick (5 s at 20 TPS)
#: from the second failure in a row on: an always-failing goal must not spin the
#: loop (autonomous run 2, 2026-11-08). The first failure waits for nothing: a
#: fresh plan is the answer and the body must not idle (decision 2026-11-12).
FAILURE_GAP_TICKS = 100
#: The longer gap after :data:`FAILURE_STREAK` consecutive failures (10 s). It
#: was 30 s; in run B (2026-11-12) the body stood still for half a minute after
#: its fourth failed goal, which reads as a freeze. The governor's rate limit
#: and budget already bound the paid calls, so this gap only has to stop a
#: tight spin.
FAILURE_STREAK_GAP_TICKS = 200
#: Consecutive failed goals after which the longer gap applies.
FAILURE_STREAK = 3
#: Body ticks the pick waits after ending as ``nothing_to_do`` (10 s at 20 TPS).
#: It doubles with each consecutive one up to :data:`NOTHING_TO_DO_MAX_GAP_TICKS`
#: and is lifted at once by an admitted skill or a changed world (critique
#: finding 12), so waiting is quiet but not deaf.
NOTHING_TO_DO_GAP_TICKS = 200
#: The longest back-off after repeated ``nothing_to_do`` picks (1 min).
NOTHING_TO_DO_MAX_GAP_TICKS = 1200
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
    SKILL_ADMITTED = "skill_admitted"
    DIED = "died"


def skill_goal_text(skill: str, args: Sequence[str] = ()) -> str:
    """The text of a goal a skill's own ``done`` verifies: ``find_village()``.

    It is the goal's failure-memory key (it has no inventory condition) and what
    the records and the console show as its condition.
    """
    return f"{skill}({', '.join(str(arg) for arg in args)})"


# --------------------------------------------------------------------------- #
# The loop
# --------------------------------------------------------------------------- #
class Loop:
    """One autonomous brain loop against one body port."""

    #: Whether the pick gap is a ``nothing_to_do`` back-off (a class default so
    #: a partially built loop in a test reads False).
    _nothing_gap = False
    #: The last remembered proposal and what it replaced (``_forget_proposal``).
    _before_remember: Optional[tuple[GoalProposal, Optional[GoalProposal]]] = None
    #: The body's world desync reason; while set no new goal is picked.
    _world_desync: Optional[str] = None
    #: Set in ``__init__``; a class default so a partly built loop in a test
    #: simply does not talk.
    narrator: "speech.Narrator | None" = None

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
        plan_fn: Callable[..., Plan] = make_plan,
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
        gamedata: Gamedata | None = None,
        map_fn: Callable[..., Any] | None = None,
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
        self._plan_fn = plan_fn
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
        self._before_remember = None
        self._world_desync = None
        #: The recipe export; ``None`` leaves the ``## Needs`` line out.
        self.gamedata = gamedata
        #: The plan queue: goals the planner named and the loop has not played
        #: yet, in order (decision 2026-11-12 "Demo night").
        self._plan: deque[PlannedGoal] = deque()
        #: The newest plan's spoken thought, for the console.
        self._plan_thought: Optional[str] = None
        #: The body died since the last plan: the next plan escalates.
        self._after_death = False
        #: The run's play style, drawn once from the world id.
        self.persona = personas.pick(world_id)
        #: Consecutive ``nothing_to_do`` picks, and whether the gap is theirs.
        self._nothing_streak = 0
        self._nothing_gap = False
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
        #: Survival skill -> body tick before which it is not picked again: a
        #: failed survival goal backs off instead of being failure-blocked
        #: (decision 2026-11-10). In memory only, never persisted.
        self._survival_backoff: dict[str, int] = {}
        #: Consecutive failures per survival skill (the back-off doubles).
        self._survival_failures: dict[str, int] = {}
        #: Whether the body works instead of sheltering at night (decision
        #: 2026-11-13): ``None`` until the first snapshot says either way, so
        #: the first night of a run drops no plan.
        self._night_working: Optional[bool] = None
        #: Whether this night's night-work line has been said already.
        self._night_spoken = False
        #: The one admission job in flight; the loop starts no second one.
        self._job: Optional[AdmissionJob] = None
        #: An admitted skill whose goal waits for the next pick.
        self._written: Optional[PendingGoal] = None
        self._console = console
        if console is not None:
            # The loop maps console free text to goals (`_handle_user_text`).
            console.accepts_tasks = True
        self._paused = False
        self._user_goals: list[GoalProposal] = []
        #: Parallel to ``_user_goals``: whether a repeated queue entry is the
        #: operator asking again (true, played again) or the console's explicit
        #: ``new_goal`` (false, still checked against the last one).
        self._user_repeatable: list[bool] = []
        #: Parallel to ``_user_goals``: the known skill and explicit arguments
        #: the operator's sentence named (``None`` when it named none).
        self._user_routes: list[tuple[Optional[str], Optional[list[str]]]] = []
        self._last_route: Optional[dict[str, Any]] = None
        self._last_publish = -float("inf")
        #: Goals a newer command preempted, oldest first; the body resumes the
        #: newest when the current one settles (session 9, critique finding 3).
        self._suspended: list[OpenGoal] = []
        #: A `dur` the body will confirm with `interrupted` receipts. Stop pops
        #: the suspended goals too, so the next closed goal must not "resume"
        #: one that no longer exists (review finding 2).
        self._stop_requested = False
        self._map_fn = map_fn if map_fn is not None else commands.map_with_model
        self._last_reply: Optional[dict[str, Any]] = None

        self.narrator = speech.Narrator(
            say_fn=lambda text, priority: port.say(text, priority=priority),
            supported=lambda: self.session.connected and port.supports_say,
            evidence=self.evidence,
            clock=self._clock,
        )
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
        self.evidence.write("persona", id=self.persona.id, style=self.persona.style)
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

    @property
    def stopped(self) -> bool:
        """Whether the loop ended (stop, terminal bye or too many step errors)."""
        return self._stopped

    def run(self, timeout: float = 0.25) -> None:
        """Run until :meth:`stop`, a terminal bye, or repeated step errors.

        One bad step is recorded and survived (A6): the open record is closed,
        a lost port reconnects on the next step, and only
        :data:`MAX_STEP_ERRORS` consecutive errors end the run.
        """
        while not self._stopped:
            self.run_once(timeout)

    def run_once(self, timeout: float = 0.25) -> LoopState:
        """One step through the same error handling :meth:`run` uses.

        A lifecycle owner that must also watch its body process and a stop
        event drives the loop with this instead of :meth:`run`, which owns its
        thread until the loop ends. One bad step is recorded and survived (A6).
        """
        try:
            return self.step(timeout)
        except Exception as error:  # noqa: BLE001 - one step must not kill the loop
            self._on_step_error(error)
            return self.state

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
        self._sync_night_work()
        self._advance()
        self._survival_check()
        if self.narrator is not None:
            self.narrator.pump()
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

    def on_snapshot(self, snapshot: Snapshot, *, cause: Optional[str] = None) -> None:
        """A snapshot arrived: diff the facts, then recover from a fresh connection.

        After a ``dropped(n)`` the snapshot that follows also reconciles the
        open goals against its stack (:meth:`_reconcile_dropped`).
        """
        facts = (self.session.snapshot or {}).get("facts") or {}
        playing = any(entry.state == "running" for entry in snapshot.stack)
        self._check_heartbeat(
            facts,
            is_playing=playing,
            tick=snapshot.tick,
            heartbeat_pending=cause == "heartbeat",
        )
        recovery = self.session.take_recovery()
        if recovery is not None:
            # A recovery adopts or settles every open record already.
            self.session.take_dropped()
            self._recover(snapshot, WakeCause(recovery))
        elif self.session.take_dropped():
            self._reconcile_dropped(snapshot)

    def _reconcile_dropped(self, snapshot: Snapshot) -> None:
        """Close the goals whose receipt a real overflow may have lost.

        The body announced ``dropped(n)`` and this snapshot covers the dropped
        events. An open or suspended goal whose accepted intent is no longer
        live on the stack has settled (or was popped) and its receipt is gone:
        it is closed now from the snapshot, ``done`` when its condition holds
        and ``interrupted`` (``receipt_lost``, not a failure) otherwise, so the
        loop does not wait for it forever. A goal whose ``accepted`` receipt
        never arrived has no intent id to check and is left to its expiry.
        """
        live = {
            entry.intent_id
            for entry in snapshot.stack
            if entry.state in survival.LIVE_STATES
        }
        goals = [*self._suspended]
        if self._open_goal is not None:
            goals.append(self._open_goal)
        facts = snapshot.facts.model_dump(mode="python")
        inventory = {str(k): int(v) for k, v in (facts.get("inventory") or {}).items()}
        for goal in goals:
            if goal.intent_id is None or goal.intent_id in live:
                continue
            if goal.record.status != "open":
                continue
            if goal.stop_pending:
                cause = goal.stop_source
                status = "interrupted"
                result = None
                failure_kind = (
                    "stopped_by_user" if cause == "console" else f"preempted_by_{cause}"
                )
            else:
                try:
                    value = evaluate(goal.proposal.condition, inventory, facts)
                except ConditionError:
                    value = None
                result = "unknown" if value is None else ("true" if value else "false")
                status = "done" if result == "true" else "interrupted"
                failure_kind = None if status == "done" else "receipt_lost"
            self.evidence.write(
                "receipt_lost",
                goal_id=goal.record.goal_id,
                command_id=goal.command_id,
                intent_id=goal.intent_id,
                status=status,
                tick=snapshot.tick,
            )
            self.records.finish(
                goal.record,
                status=status,
                failure_kind=failure_kind,
                condition_result=result,
                ticks=goal.ticks_since(snapshot.tick),
                source="recovery",
            )

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
            if self._nothing_gap:
                # The world moved: a waiting pick may find work now.
                self._next_pick_tick = 0
            if self.state == LoopState.IDLE:
                self._set_wake(WakeCause.HEARTBEAT)
        elif heartbeat_pending:
            self.evidence.write("heartbeat_quiet", tick=tick)

    def _recover(self, snapshot: Snapshot, cause: WakeCause) -> None:
        """A1/A8: adopt every live intent, then settle what is left open."""
        # The back-off deadlines are ticks of the body we lost: a new body (a
        # restarted process, a new session) counts its ticks afresh, and an
        # old deadline could hold a survival skill for hours.
        self._survival_backoff.clear()
        self._survival_failures.clear()
        recovery = self.records.recover(
            snapshot.stack,
            tick=snapshot.tick,
            cause=cause.value,
            keep=self._job_goal_ids(),
        )
        if recovery.adopted is not None:
            self._remember_proposal(recovery.adopted.proposal)
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
        elif kind in ("chat_sent", "chat_dropped"):
            if self.narrator is not None:
                self.narrator.on_event(event)
        elif kind == "chat_received":
            self._on_chat_received(event)

    def _on_body_wake(self, event: Mapping[str, Any]) -> None:
        cause = event.get("cause")
        if cause == "died":
            self._on_died(event)
        elif cause == "world_desync":
            self._on_world_desync(str(event.get("detail") or "") or None)
        if cause == "hunger_no_food" and self.state == LoopState.IDLE:
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

    def _on_died(self, event: Mapping[str, Any]) -> None:
        """The body died (decision 2026-11-10 "Death is an event").

        The running command settles as an ordinary ``failed(died)`` receipt; this
        only records the death, says it, and makes the next pick read fresh facts
        (the inventory is gone): a ``nothing_to_do`` wait is lifted and an idle
        loop wakes at once.
        """
        detail = str(event.get("detail") or "") or None
        self.evidence.write("died", detail=detail, tick=self.session.tick, state=self.state.value)
        # The inventory is gone and so is the plan built on it; the next plan
        # is the first one after a death and escalates to the stronger model.
        self._drop_plan("died")
        self._after_death = True
        self._say("died", speech.died(detail), priority="urgent")
        if self._nothing_gap:
            self._next_pick_tick = 0
        if self.state == LoopState.IDLE:
            self._set_wake(WakeCause.DIED)

    def _on_world_desync(self, reason: Optional[str]) -> None:
        """The body's world mirror desynchronised: stop picking, say it once.

        Decision 2026-11-10 "A world desync stops the run loudly": every fact the
        body serves from now on is stale, so no new goal starts; the autonomy
        harness ends the run. Idempotent: the event and a ``world_valid: false``
        snapshot both lead here.
        """
        if self._world_desync is not None:
            return
        self._world_desync = reason or "unknown"
        self._drop_plan("world_desync")
        self.evidence.write(
            "world_desync", reason=self._world_desync, tick=self.session.tick, state=self.state.value
        )
        self._say("world_desync", speech.world_desync(), priority="urgent")
        self._wake = None

    def _on_receipt(self, event: Mapping[str, Any]) -> None:
        command_id = str(event.get("command_id") or "")
        state = event.get("state")
        open_goal = self._open_goal
        if open_goal is None or open_goal.command_id != command_id:
            # A preempted goal's later receipts (it settles after being resumed)
            # still belong to it.
            open_goal = next(
                (goal for goal in self._suspended if goal.command_id == command_id),
                None,
            )
            if open_goal is None:
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
                    # `dur` covers the preempted goals too.
                    self._pop_suspended()
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
                # A command refused because the body went blind is not the
                # goal's failure (decision 2026-11-10), as in `settled_status`.
                status="interrupted" if failure in NOT_FAILURE_KINDS else "failed",
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
                open_goal.skill,
                open_goal.proposal.condition,
                open_goal.budget_ticks,
                open_goal.args,
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
        if (
            self.state == LoopState.IDLE
            and self._wake is None
            and self._nothing_gap
            and not self._paused
            and self._pick_ready()
        ):
            # The back-off after a nothing_to_do pick is over: look again.
            self._set_wake(WakeCause.HEARTBEAT)
        if self._world_desync is not None:
            # A blind body: no new goal on stale facts (decision 2026-11-10).
            if self.state in (LoopState.PICKING, LoopState.ROUTING):
                pending, self._pending = self._pending, None
                if pending is not None:
                    self._drop_unplayed(pending, "world_desync")
                self._transition(LoopState.IDLE, reason="world_desync")
            return
        if self.state == LoopState.IDLE and self._wake is not None:
            if self._paused:
                return
            if not self._user_goals and not self._pick_ready():
                # The gap after a failure has not passed yet; keep the wake
                # pending and let the next step (heartbeat/poll) try again.
                return
            wake = self._wake
            self._wake = None
            self._nothing_gap = False
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
        self._back_off_survival(record)
        was_suspended = any(g.record.goal_id == record.goal_id for g in self._suspended)
        self._suspended = [g for g in self._suspended if g.record.goal_id != record.goal_id]
        self._narrate_finished(record)
        if was_suspended:
            # A preempted goal ended on its own (popped, expired): the goal being
            # played is untouched.
            if not self._suspended and record.failure_kind == "stopped_by_user":
                # The stop's pops have all been confirmed: nothing left to say.
                self._stop_requested = False
            return
        self._open_goal = None
        if record.status in ("failed", "interrupted"):
            # The steps behind a goal that did not finish were planned for a
            # world where it had; the next pick plans again. A goal that ends
            # ``done`` keeps the queue.
            self._drop_plan(f"goal_{record.status}")
        stopped = (
            self._stop_requested
            and record.status == "interrupted"
            and record.failure_kind == "stopped_by_user"
        )
        # The flag is for the first goal a `dur` closes; never let it linger.
        self._stop_requested = False
        if stopped:
            # `dur` pops every command, the suspended ones included, so nothing
            # is on the body's stack to hand back. Their own `popped` receipts
            # close them; saying "devam ediyorum" for a goal that no longer
            # exists was review finding 2's contradiction.
            self._transition(LoopState.IDLE, reason="stopped_by_user")
            self._set_wake(WakeCause.GOAL_FAILED)
            return
        if self._suspended:
            # The body hands the body back to the newest preempted goal and
            # restarts its skill from the top (driver.rs restart_resumed): the
            # loop keeps playing and now waits for that goal's receipt.
            resumed = self._suspended.pop()
            self._open_goal = resumed
            self.evidence.write(
                "goal_resumed",
                goal_id=resumed.record.goal_id,
                command_id=resumed.command_id,
                after=record.goal_id,
                restart=True,
            )
            self._say("goal_resumed", speech.goal_resumed(resumed.record.text), key=resumed.record.goal_id)
            self._transition(LoopState.PLAYING, reason="goal_resumed")
            return
        self._transition(LoopState.IDLE, reason=f"goal_{record.status}")
        if record.status == "failed":
            self._note_failure(record.failure_kind)
        elif record.failure_kind in NOT_FAILURE_KINDS:
            # A death or a desync says nothing about the goal: the failure
            # streak and its gap stay as they were (review of run 8 fixes).
            pass
        else:
            self._consecutive_failures = 0
            self._next_pick_tick = 0
        self._nothing_streak = 0
        self._set_wake(
            WakeCause.GOAL_SETTLED if record.status == "done" else WakeCause.GOAL_FAILED
        )

    def _note_failure(self, failure_kind: Optional[str]) -> None:
        """One failed goal: count the streak and hold the pick for the gap.

        The first failure in a row holds nothing: the next pick plans again at
        once. The gap starts with the second, so a broken world cannot spin.
        """
        self._consecutive_failures += 1
        if self._consecutive_failures >= FAILURE_STREAK:
            gap = FAILURE_STREAK_GAP_TICKS
        elif self._consecutive_failures >= 2:
            gap = FAILURE_GAP_TICKS
        else:
            gap = 0
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
        if self._facts().get("world_valid") is False:
            # A snapshot can say it before (or without) the wake event.
            self._on_world_desync(None)
            self._transition(LoopState.IDLE, reason="world_desync")
            return
        self._sync_night_work()
        inventory = self._inventory()
        # The user's goal first, then survival, then the rest: the user's
        # projects come before Praxion's own ideas (architecture §1), so a
        # survival goal is exempt from the need gate but never above the user.
        if self._pick_user_goal(inventory):
            return
        if self._pick_survival(inventory):
            return
        hold = self._night_hold()
        if hold is not None:
            # Night with no survival goal to start (unarmed, backing off, or
            # the body's own fallback running), or a body still sealed in by
            # day whose ``take_shelter`` cannot start now: wait, start no
            # ladder work.
            # An admitted skill stays in ``_written`` for the morning.
            self._pending = None
            gap = self._nothing_backoff()
            self.evidence.write(
                "night_wait",
                reason=hold,
                gap_ticks=gap,
                next_pick_tick=self._next_pick_tick,
            )
            self._transition(LoopState.IDLE, reason="night_wait")
            return
        if self._pick_written_goal(inventory):
            return
        if self.governor.code_only:
            self._end_pick_without_goal("governor_code_only")
            return

        self._pick_planned(inventory)

    # -- the plan queue (decision 2026-11-12 "Demo night") ---------------- #
    def _pick_planned(self, inventory: dict[str, int]) -> None:
        """Play the next queued goal, or plan again when the queue is empty.

        A queued goal costs no model call: only its turn-time validation. An
        empty queue is one planner call (through the governor, so the budget and
        the breaker still apply) whose goals fill it. A plan with no valid goal
        counts as an invalid plan and is asked again, up to
        :attr:`max_proposals` times, then the pick ends as ``plans_invalid``.
        """
        if self._next_plan_step(inventory)[0]:
            return
        briefing = self._briefing(self._needs_line(inventory), knowledge=self._knowledge())
        excluded = False
        for _ in range(max(1, self.max_proposals)):
            denied = self._gate(
                "propose", plan_estimate(self._invalid_proposals, self._after_death)
            )
            if denied:
                self._end_pick_without_goal(denied)
                return
            fresh, failure = self._plan_call(briefing)
            if fresh is None:
                self._end_pick_without_goal(failure)
                return
            problem = shape_problem(fresh)
            if problem is not None:
                self._invalid_plan(problem)
                continue
            self._plan = deque(fresh.goals[:MAX_GOALS])
            self._plan_thought = fresh.thought.strip() or None
            self.evidence.write(
                "plan",
                thought=fresh.thought,
                goals=[goal.model_dump() for goal in fresh.goals],
                kept=len(self._plan),
            )
            if self._plan_thought is not None:
                self._say("plan", speech.plan_thought(self._plan_thought), key="plan")
            played, reasons = self._next_plan_step(inventory)
            if played:
                return
            if reasons and set(reasons) == {"writing"}:
                # Every goal is the one being written: not the planner's
                # mistake, so no escalation to the dearer model (run 5).
                excluded = True
                continue
            self._invalid_plan("no_valid_step", reasons=reasons)

        self._end_pick_without_goal("plans_excluded" if excluded else "plans_invalid")

    def _invalid_plan(self, reason: str, **fields: Any) -> None:
        """One plan the code cannot use: count it (two in a row escalate)."""
        self._invalid_proposals += 1
        self.evidence.write(
            "plan_invalid",
            reason=reason,
            invalid_count=self._invalid_proposals,
            **fields,
        )

    def _next_plan_step(self, inventory: dict[str, int]) -> tuple[bool, list[str]]:
        """Pop queued goals until one validates and send it to the router.

        Returns whether a goal was started and the reason of every dropped one
        (each is also an evidence line ``plan_step_skipped``). ``same_as_last``
        does not apply: a plan may legitimately repeat a condition (more wood
        for the pickaxe after the planks). While the body works at night only
        ``survival.NIGHT_WORK_SKILLS`` pass (``night_unsafe``, decision
        2026-11-13); a step without a named skill is one of them.
        """
        reasons: list[str] = []
        night = self._night_work()
        while self._plan:
            step = self._plan.popleft()
            proposal, result = self._validate_step(step, inventory)
            reason = result.reason if not result.ok else None
            detail = result.detail
            if reason is None and night and not self._night_safe(step.skill):
                reason = "night_unsafe"
                detail = f"{step.skill or 'no skill'} is not a night skill"
            if reason is None and self._excluded_by_job(proposal):
                # The skill being written is not ready: this goal cannot play.
                reason = "writing"
            if reason is not None:
                reasons.append(reason)
                self.evidence.write(
                    "plan_step_skipped",
                    step=step.model_dump(),
                    reason=reason,
                    detail=detail,
                    remaining=len(self._plan),
                )
                continue
            verified = self._skill_verified(step)
            skill = step.skill if self._public_skill(step.skill) else None
            self._pending = PendingGoal(
                proposal=proposal,
                validation=result,
                goal=goal_of(proposal, result),
                source="skill_goal" if verified else "plan",
                forced_skill=skill if verified else None,
                args=list(step.args) or None,
                hint=None if verified else skill,
            )
            self._remember_proposal(proposal)
            self._invalid_proposals = 0
            self.evidence.write(
                "plan_step",
                proposal=proposal.model_dump(),
                validation=result.model_dump(),
                skill=skill,
                args=step.args,
                remaining=len(self._plan),
            )
            self._transition(LoopState.ROUTING, reason="plan_step")
            return True, reasons
        return False, reasons

    def _skill_verified(self, step: PlannedGoal) -> bool:
        """A goal with no condition whose known public skill's own ``done`` settles it."""
        return not step.condition.strip() and self._public_skill(step.skill)

    def _public_skill(self, name: Optional[str]) -> bool:
        """Whether ``name`` is a known skill that is not a helper (``PUBLIC_EXCLUDE``)."""
        return (
            bool(name)
            and name not in PUBLIC_EXCLUDE
            and self.catalog.by_name(str(name)) is not None
        )

    def _validate_step(
        self, step: PlannedGoal, inventory: dict[str, int]
    ) -> tuple[GoalProposal, Validation]:
        """The goal of one plan step and the code's verdict on it.

        A skill-verified step has no inventory condition, so its condition is the
        skill call text (``find_village()``): it keys failure memory, and the body
        gets no condition (it is not verifiable), so the skill's own ``done``
        settles the goal. Every other step is validated like any goal.
        """
        rationale = step.rationale[:200]
        problem = self._args_problem(step)
        if self._skill_verified(step):
            proposal = GoalProposal(
                text=step.text,
                condition=skill_goal_text(str(step.skill), step.args),
                rationale=rationale,
            )
            if problem is not None:
                return proposal, Validation(
                    ok=False, reason="bad_args", detail=problem, unverifiable=True
                )
            result = validate(proposal, inventory, self.memory, None)
            return proposal, result.model_copy(update={"skill_name": step.skill})
        proposal = GoalProposal(
            text=step.text,
            condition=step.condition,
            rung=self._rung_of(step.condition),
            rationale=rationale,
        )
        if not step.condition.strip() and step.skill:
            return proposal, Validation(
                ok=False, reason="unknown_skill", detail=str(step.skill)
            )
        if problem is not None:
            return proposal, Validation(ok=False, reason="bad_args", detail=problem)
        return proposal, validate(proposal, inventory, self.memory, None)

    def _args_problem(self, step: PlannedGoal) -> Optional[str]:
        """Why the step's ``args`` cannot be the named skill's arguments.

        The planner names the skill its arguments were written for; code types
        them the way the body does (``body/crates/body/src/cli.rs``), so a bad
        argument skips the step here instead of reaching the body and recording
        a false failure under the goal's condition (review finding 2).
        """
        if not step.args:
            return None
        info = self.catalog.by_name(str(step.skill)) if self._public_skill(step.skill) else None
        if info is None:
            return f"the arguments {list(step.args)!r} name no public skill"
        params = [str(param) for param in info.params]
        if len(step.args) > len(params):
            return (
                f"{info.name} takes at most {len(params)} arguments, "
                f"got {len(step.args)}"
            )
        for arg, param in zip(step.args, params):
            name, _, spec = param.partition(":")
            kind = spec.split("=")[0].strip().lower()
            if kind == "int" and not re.fullmatch(r"[+-]?\d+", str(arg)):
                return f"{name.strip() or 'argument'} wants an int, got {arg!r}"
        return None

    @staticmethod
    def _rung_of(condition: str) -> Optional[str]:
        """The ladder rung whose condition this is, for the speech and the record."""
        key = normalize_condition(condition)
        for rung in RUNGS:
            if normalize_condition(rung.condition) == key:
                return rung.id
        return None

    def _remember_proposal(self, proposal: GoalProposal) -> None:
        """The newest accepted goal; what it replaced is kept.

        A goal that is never played (a deferred write) can be forgotten again by
        :meth:`_forget_proposal`.
        """
        self._before_remember = (proposal, self._last_proposal)
        self._last_proposal = proposal

    def _forget_proposal(self, proposal: GoalProposal) -> None:
        """Undo :meth:`_remember_proposal` for a goal that was never played.

        A write deferred while another skill is written plays nothing, so it
        must not become the "last" goal. Only the matching, most recent remember
        is undone.
        """
        before = self._before_remember
        if before is None or before[0] is not proposal or self._last_proposal is not proposal:
            return
        self._before_remember = None
        self._last_proposal = before[1]

    def _drop_plan(self, reason: str) -> None:
        """Forget the queued goals; the next pick plans again.

        Called when the world the plan was made for is gone: a goal that did not
        finish, a death, a survival goal, an operator task, a skill write, a
        world desync, or night falling (``night``) and morning coming
        (``morning``). Silent when the queue is already empty.
        """
        if not self._plan:
            return
        self.evidence.write(
            "plan_dropped",
            reason=reason,
            goals=[goal.text for goal in self._plan],
        )
        self._plan.clear()

    def _needs_line(self, inventory: dict[str, int]) -> Optional[str]:
        """The ``## Needs`` fact for the next ladder rung; ``None`` without the export."""
        if self.gamedata is None:
            return None
        rung = first_unmet(inventory)
        if rung is None:
            return None
        return need_line(
            rung_need(rung.id, rung.condition, inventory, self.gamedata),
            inventory,
            self.gamedata,
        )

    def _knowledge(self) -> list[str]:
        """Wiki lines about the next ladder rung's item, when the wiki exists.

        The wiki package is optional: without it, or without an index, the
        planner simply gets no ``## Knowledge`` section. A search error is
        evidence (``wiki_error``) and never stops the pick.
        """
        rung = first_unmet(self._inventory())
        if rung is None:
            return []
        item = item_of_condition(rung.condition)
        name = rung.id if item is None or item.startswith("#") else item
        query = f"how to get {name.replace('_', ' ')} in Minecraft survival"
        try:
            search = importlib.import_module(".wiki", __package__).search
        except (ImportError, AttributeError):
            return []
        try:
            snippets = search(query, k=2)
        except Exception as error:  # noqa: BLE001 - the wiki is a bonus, never a stop
            self.evidence.write(
                "wiki_error", query=query, error=f"{type(error).__name__}: {error}"
            )
            return []
        return [
            f"{snippet.title}: {' '.join(str(snippet.text).split())[:300]}"
            for snippet in snippets
        ]

    def _plan_call(self, briefing: str) -> tuple[Optional[Plan], str]:
        """One planner call with its deadline; ``(None, reason)`` when it fails."""
        try:
            fresh = self.governor.call(
                "propose",
                lambda: self._plan_fn(
                    briefing=briefing,
                    catalog=self.catalog,
                    invalid_count=self._invalid_proposals,
                    after_death=self._after_death,
                    ledger=self.ledger,
                    evidence=self.evidence,
                ),
            )
        except CallTimeout as error:
            self.governor.record_error()
            self.evidence.write(
                "plan_timeout",
                seconds=error.seconds,
                invalid_count=self._invalid_proposals,
            )
            return None, "plan_timeout"
        except Exception as error:  # noqa: BLE001 - provider errors feed the breaker
            self.governor.record_error()
            self.evidence.write(
                "plan_error",
                error=f"{type(error).__name__}: {error}",
                invalid_count=self._invalid_proposals,
            )
            return None, "plan_error"
        self.governor.record_success()
        # The first plan after a death is the escalated one; the next is not.
        self._after_death = False
        return fresh, ""

    def _end_pick_without_goal(self, reason: str, detail: Optional[str] = None) -> bool:
        """The pick found no goal to play: play the rung's known skill, else wait.

        This is the existing code-only path, so a skill that already serves the
        next rung is still played (``True``). When it finds nothing, the pick
        ends as one visible ``nothing_to_do`` and the next pick waits a bounded,
        doubling gap (a skill admission or a changed world lifts it at once), so
        the loop is never left IDLE with nothing to wake it (run 8, decision
        2026-11-10 "Survival goals start during a skill admission").
        """
        if self._pick_code_only(reason):
            return True
        gap = self._nothing_backoff()
        self.evidence.write(
            "nothing_to_do",
            reason=reason,
            detail=detail,
            writing=self._writing_skill(),
            gap_ticks=gap,
            next_pick_tick=self._next_pick_tick,
        )
        return False

    def _nothing_backoff(self) -> int:
        """Hold the next pick for the doubling ``nothing_to_do`` gap; return it.

        The one back-off for every pick that ends without a goal. The gap is
        lifted by a changed world, an admitted skill, a death or a due survival
        goal; when it simply runs out, ``_advance`` wakes the loop. The streak
        resets when a goal settles.
        """
        gap = min(
            NOTHING_TO_DO_GAP_TICKS * (2**self._nothing_streak),
            NOTHING_TO_DO_MAX_GAP_TICKS,
        )
        self._nothing_streak += 1
        now = int(self.session.tick or 0)
        self._next_pick_tick = max(self._next_pick_tick, now + gap)
        self._nothing_gap = True
        return gap

    def _idle_backoff(self, reason: str) -> None:
        """A route or a play ended without a goal: IDLE with the back-off.

        A router timeout or a failed send is not the goal's failure, so it is
        no failure gap; but IDLE with no wake and no gap would wait for the
        world to change (run 8). The caller writes its own evidence line first.
        """
        pending, self._pending = self._pending, None
        if pending is not None:
            # Nothing was played, so the proposal must not become the "last"
            # goal: re-proposing it after the gap is not `same_as_last`. The
            # pending goal is gone too: a console Stop in the idle window must
            # not report a goal that was already abandoned (review finding 3).
            self._forget_proposal(pending.proposal)
        self._transition(LoopState.IDLE, reason=reason)
        gap = self._nothing_backoff()
        self.evidence.write(
            "pick_backoff",
            reason=reason,
            gap_ticks=gap,
            next_pick_tick=self._next_pick_tick,
        )

    # -- survival (decision 2026-11-09 "Five next areas") ------------------ #
    def _facts(self) -> dict[str, Any]:
        return dict((self.session.snapshot or {}).get("facts") or {})

    def _night_work(self) -> bool:
        """Whether the body works underground instead of sheltering now.

        Decision 2026-11-13 "night mining": a pickaxe and health above
        ``survival.SHELTER_HEALTH`` at night. The planner keeps planning then,
        restricted to ``survival.NIGHT_WORK_SKILLS``.
        """
        return survival.night_work(self._facts(), self._inventory())

    @staticmethod
    def _night_safe(skill: Optional[str]) -> bool:
        """Whether ``skill`` may play at night while the body works.

        A step without a named skill is never safe: the loop would have to write
        a skill first, and nothing is written in the dark (decision 2026-11-13).
        """
        return bool(skill) and str(skill) in survival.NIGHT_WORK_SKILLS

    def _safe_to_play(self, skill: Optional[str]) -> bool:
        """Whether the skill finally routed may play now: any skill by day, a
        night skill while the body works at night (decision 2026-11-13)."""
        return not self._night_work() or self._night_safe(skill)

    def _sync_night_work(self) -> None:
        """Track night work across the night; drop the plan when it turns.

        Dusk drops the daytime plan (evidence ``plan_dropped`` reason ``night``)
        so the next plan is made for the night, and dawn drops the night plan
        (reason ``morning``) the same way. The first sighting of a night says
        one line, once per night (decision 2026-11-13): a body hurt and healed
        again during the same night does not repeat it.
        """
        facts = self._facts()
        if facts.get("world_valid") is False:
            # Stale facts decide nothing; a desync drops the plan itself.
            return
        working = survival.night_work(facts, self._inventory())
        if facts.get("is_night") is not True:
            self._night_spoken = False
        if working != self._night_working:
            if self._night_working is not None:
                self._drop_plan("night" if working else "morning")
            self._night_working = working
        if working and not self._night_spoken:
            self._night_spoken = True
            self._say("night_work", speech.night_work(), key="night_work")

    def _night_allowed(self) -> Optional[list[str]]:
        """The ``## Night`` skills the catalog has, or ``None`` off night work."""
        if not self._night_work():
            return None
        return sorted(
            name
            for name in survival.NIGHT_WORK_SKILLS
            if self.catalog.by_name(name) is not None
        )

    def _due_survival(self) -> Optional["survival.SurvivalGoal"]:
        return survival.choose(
            self._facts(),
            self._inventory(),
            self.catalog,
            stack=(self.session.snapshot or {}).get("stack") or (),
            backoff=self._survival_backoff,
            tick=int(self.session.tick or 0),
        )

    def _night_hold(self) -> Optional[str]:
        """Why the pick must wait instead of starting ladder work at night."""
        return survival.night_hold(
            self._facts(),
            self._inventory(),
            stack=(self.session.snapshot or {}).get("stack") or (),
            backoff=self._survival_backoff,
            tick=int(self.session.tick or 0),
            failures=self._survival_failures,
        )

    def _back_off_survival(self, record: GoalRecord) -> None:
        """A failed survival goal: its skill waits, it is never blocked."""
        if record.origin != "survival" or not record.skill:
            return
        if record.status == "done":
            self._survival_failures.pop(record.skill, None)
            return
        if record.status != "failed":
            return
        failures = self._survival_failures.get(record.skill, 0) + 1
        self._survival_failures[record.skill] = failures
        gap = min(
            survival.SURVIVAL_BACKOFF_TICKS * 2 ** (failures - 1),
            survival.SURVIVAL_BACKOFF_MAX_TICKS,
        )
        until = int(self.session.tick or 0) + gap
        self._survival_backoff[record.skill] = until
        self.evidence.write(
            "survival_backoff",
            skill=record.skill,
            goal_id=record.goal_id,
            failure_kind=record.failure_kind,
            until_tick=until,
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
        self._drop_plan("survival")
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
        fallback takes the night again when their goal settles. A night goal of
        ours yields to ``get_food`` only when food is critical.
        """
        if self._paused or self.session.snapshot is None or self._world_desync is not None:
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
                # A survival goal is never interrupted by another survival goal,
                # with one exception: critical food stops a night goal so
                # ``get_food`` can hunt (decision 2026-11-10 "Hungry at night:
                # hunt only when it is critical"). ``get_food`` opens a sealed
                # shelter itself (its own ``sheltered() -> leave_shelter()``
                # rule; ``hunt_food`` only hunts). Food above critical waits
                # for dawn. ``due`` honours the back-off, which doubles on each
                # failure (600, 1200, 2400 ... ticks), so a failed night hunt
                # does not reopen the shelter every 600 ticks.
                if opened.skill in survival.NIGHT_SKILLS and survival.critical_food(
                    self._facts()
                ):
                    due = self._due_survival()
                    if due is not None and due.kind == "food":
                        self._interrupt_goal(
                            "survival",
                            survival=due.kind,
                            reason=due.reason,
                            interrupted_skill=opened.skill,
                        )
                return
            due = self._due_survival()
            if due is not None:
                self._interrupt_goal("survival", survival=due.kind, reason=due.reason)
        elif self.state == LoopState.IDLE and self._wake is None:
            # Also while a skill is written in the background (decision
            # 2026-11-10 "Survival goals start during a skill admission"): the
            # pick plays survival before the written goal and the proposer, and
            # an admitted skill waits in ``_written`` until survival is done.
            if self._due_survival() is not None:
                if self._nothing_gap:
                    # "Nothing to do" is no longer true: night or hunger is.
                    # A failure gap still holds, so a failing survival goal
                    # cannot spin.
                    self._next_pick_tick = 0
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
            self._remember_proposal(pending.proposal)
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
            repeatable = self._user_repeatable.pop(0) if self._user_repeatable else False
            skill, args = self._user_routes.pop(0) if self._user_routes else (None, None)
            verified = skill is not None and proposal.condition == skill_goal_text(
                skill, args or ()
            )
            # Same-as-last does not apply to the operator's own sentence, who
            # may repeat a request (the command path already decided that);
            # checking it again here refused the repeat with a raw key (review
            # finding 5). The console's explicit `new_goal` keeps the guard.
            result = validate(
                proposal, inventory, self.memory, None if repeatable else self._last_proposal
            )
            self.evidence.write(
                "user_goal_result",
                proposal=proposal.model_dump(),
                validation=result.model_dump(),
            )
            if not result.ok:
                self._say(
                    "refused",
                    speech.refused(str(result.reason), result.detail),
                    priority="urgent",
                )
            if result.ok:
                self._pending = PendingGoal(
                    proposal=proposal,
                    validation=(
                        result.model_copy(update={"skill_name": skill}) if verified else result
                    ),
                    goal=goal_of(proposal, result),
                    source="skill_goal" if verified else "llm",
                    forced_skill=skill if verified else None,
                    origin="user",
                    args=args,
                    hint=None if verified else skill,
                )
                self._remember_proposal(proposal)
                self._transition(LoopState.ROUTING, reason="user_goal")
                return True
        return False

    def _pick_code_only(self, reason: str) -> bool:
        """Route the rung's known skill, or say why not and go IDLE (``False``).

        Only :meth:`_end_pick_without_goal` calls this: a ``False`` here is
        always followed by the ``nothing_to_do`` back-off, never a dead IDLE.
        """
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
        if not self._safe_to_play(fallback.skill):
            # The rung's skill gathers on the surface; at night the work is
            # underground (decision 2026-11-13), so the fallback idles instead.
            self.evidence.write(
                "code_only_idle",
                reason="night_unsafe",
                detail=f"{fallback.skill} is not a night skill",
                requested=reason,
                inventory=inventory,
            )
            self._transition(LoopState.IDLE, reason="code_only_idle")
            return False
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
        self._remember_proposal(proposal)
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

    def _briefing(
        self, needs: Optional[str] = None, knowledge: Sequence[str] = ()
    ) -> str:
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
            needs=needs,
            persona=self.persona.style,
            knowledge=knowledge,
            night=self._night_allowed(),
        )

    # -- ROUTING -------------------------------------------------------- #
    def _route(self) -> None:
        pending = self._pending
        if pending is None:
            self._idle_backoff("route_without_goal")
            return

        if pending.source in ("code_only", "survival", "skill_goal") and pending.forced_skill:
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
            if self._end_pick_without_goal(denied):
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
                    hint=pending.hint,
                ),
            )
        except CallTimeout as error:
            self.governor.record_error()
            self.evidence.write("route_timeout", seconds=error.seconds)
            self._idle_backoff("route_timeout")
            return
        except Exception as error:  # noqa: BLE001 - a Jev error is never "no"
            self.governor.record_error()
            self.evidence.write(
                "route_error",
                error=f"{type(error).__name__}: {error}",
            )
            self._idle_backoff("route_error")
            return
        self.governor.record_success()

        self.evidence.write(
            "loop_route",
            decision=routed.kind,
            skill=routed.skill,
            scores=routed.scores,
            condition=pending.proposal.condition,
            hint=pending.hint,
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
                self._idle_backoff("route_no_skill")
                return
            if pending.origin != "user" and not self._safe_to_play(skill):
                # The router named a surface skill for this step; at night the
                # step is skipped exactly like a planner-named one (decision
                # 2026-11-13). Nothing is played and the goal is not a failure.
                # An operator task is the user's: it still plays.
                self.evidence.write(
                    "plan_step_skipped",
                    step=PlannedGoal(
                        text=pending.proposal.text,
                        condition=pending.proposal.condition,
                        skill=str(skill),
                        args=list(pending.args or []),
                        rationale=pending.proposal.rationale[:200],
                    ).model_dump(),
                    reason="night_unsafe",
                    detail=f"{skill} is not a night skill",
                    remaining=len(self._plan),
                )
                self._idle_backoff("night_unsafe")
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
        # The goals behind this one were planned for a world with its skill.
        self._drop_plan("skill_write")
        if self._job is not None:
            self._defer_write(pending)
            return

        denied = self._gate("write", admission_estimate(self.writer_model))
        if denied:
            if self._end_pick_without_goal(denied):
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
        self._say("learning", speech.learning_started(skill), key=f"learn:{skill}")
        self._transition(LoopState.IDLE, reason="write_started")
        self._set_wake(WakeCause.WRITE_STARTED)

    def _defer_write(self, pending: PendingGoal) -> None:
        """A write route while a job runs: back to IDLE with the back-off, no failure.

        The ``nothing_to_do`` back-off, not the failure gap: while an admission
        runs for minutes the model must not re-propose (and pay for) the same
        deferred goal every few seconds. No wake is set, so a due survival goal
        still lifts the gap at once; an admitted skill lifts it too.
        """
        writing = self._writing_skill()
        self._pending = None
        # Nothing was played: the model may name this goal again later.
        self._forget_proposal(pending.proposal)
        self.evidence.write(
            "write_deferred",
            condition=pending.proposal.condition,
            skill=pending.validation.skill_name,
            writing=writing,
        )
        self._idle_backoff("write_deferred")

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
        if self._nothing_gap:
            self._next_pick_tick = 0
        self.evidence.write(
            "admission_admitted",
            goal_id=job.record.goal_id,
            skill=report.skill_name,
            repairs=report.repairs_used,
        )
        self._say(
            "learned",
            speech.learning_done(str(report.skill_name)),
            key=f"learn:{report.skill_name}",
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
    def _args_skill(self, pending: PendingGoal) -> Optional[str]:
        """The skill the pending goal's explicit arguments were written for."""
        return pending.hint or pending.validation.skill_name

    def _play(self) -> None:
        pending = self._pending
        if pending is None:
            self._idle_backoff("play_without_goal")
            return
        skill = pending.forced_skill or pending.validation.skill_name
        if not skill:
            self.evidence.write("play_no_skill", condition=pending.proposal.condition)
            self._drop_unplayed(pending, "no_skill")
            self._idle_backoff("no_skill")
            return
        # Section 2.4: refresh the tick and generation from a snapshot right
        # before sending, so a long proposal/Jev/admission call cannot make the
        # command expire or carry a stale generation.
        self.session.refresh()
        if self.session.generation is None or self.session.tick is None:
            self.evidence.write("play_no_snapshot", condition=pending.proposal.condition)
            self._drop_unplayed(pending, "no_snapshot")
            self._idle_backoff("no_snapshot")
            return

        # Explicit arguments belong to the skill they were written for: a skill
        # Jev routed to instead takes its own shape, never another skill's
        # arguments (review finding 2).
        args = pending.args
        if args is not None and skill != self._args_skill(pending):
            self.evidence.write(
                "args_dropped",
                skill=skill,
                written_for=self._args_skill(pending),
                args=list(args),
            )
            args = None

        try:
            command_id = self._send_goal(
                skill, pending.proposal.condition, pending.budget_ticks, args
            )
        except PortError as error:
            self.evidence.write("command_send_failed", error=str(error), skill=skill)
            self._drop_unplayed(pending, "send_failed")
            self._idle_backoff("send_failed")
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
        if self._open_goal is not None:
            # A user task preempts the goal in progress: the body suspends it
            # (session 9) and the loop remembers it for the resume.
            self._suspended.append(self._open_goal)
            self.evidence.write(
                "goal_preempted",
                goal_id=self._open_goal.record.goal_id,
                by=record.goal_id,
            )
        self._open_goal = OpenGoal(
            record=record,
            proposal=pending.proposal,
            skill=skill,
            command_id=command_id,
            sent_tick=int(self.session.tick or 0),
            survival=pending.source == "survival",
            budget_ticks=pending.budget_ticks,
            args=args,
        )
        if pending.origin != "user":
            rung = pending.proposal.rung
            reason = (
                f"{rung} basamağı"
                if rung in {item.id for item in RUNGS}
                else ("kod yedeği" if pending.source == "code_only" else None)
            )
            self._say(
                "goal_start",
                speech.goal_started(
                    pending.proposal.text, pending.proposal.condition, reason=reason
                ),
                key=record.goal_id,
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
            elif action == "task":
                result = self._handle_user_text(str(request.get("text") or ""), source="console")
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
                # The preempted goals are stopped too; the unaccepted one is
                # marked the same way.
                self._pop_suspended()
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
            # Stop means everything the user can see, the preempted goals
            # included.
            self._pop_suspended()
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
        self._clear_user_goals()
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

    def _pop_suspended(self) -> int:
        """`dur` includes the preempted goals: pop every one the body holds.

        A goal whose command the body has not accepted yet has no intent id; it
        is marked ``stop_pending`` so its `accepted` receipt pops it at once.
        """
        popped = 0
        for older in self._suspended:
            if older.intent_id is None:
                older.stop_pending = True
                popped += 1
                continue
            try:
                self.session.port.pop(older.intent_id)
                popped += 1
            except PortError as error:
                self.evidence.write("stop_failed", error=str(error))
        if popped:
            self._stop_requested = True
        return popped

    def _queue_user_goal(self, request: Mapping[str, Any]) -> str:
        """Queue the console's goal; the condition is what can be verified."""
        condition = str(request.get("condition") or "").strip()
        text = str(request.get("text") or "").strip()
        if not condition:
            return "no_condition"
        self._enqueue_user(
            GoalProposal(text=text or condition, condition=condition, rationale="user goal"),
            repeatable=False,
        )
        if self.state == LoopState.IDLE:
            self._set_wake(WakeCause.USER)
        return "queued"

    def _enqueue_user(
        self,
        proposal: GoalProposal,
        *,
        repeatable: bool,
        skill: Optional[str] = None,
        args: Optional[list[str]] = None,
    ) -> None:
        """Queue one operator goal; ``repeatable`` says a repeat may be played
        again (a chat/console sentence) or is still checked against the last
        goal (the console's explicit ``new_goal``). ``skill`` and ``args`` are
        what the sentence named. A queued operator task drops the plan."""
        self._user_goals.append(proposal)
        self._user_repeatable.append(repeatable)
        self._user_routes.append((skill, args))
        self._drop_plan("user_task")

    def _clear_user_goals(self) -> None:
        self._user_goals.clear()
        self._user_repeatable.clear()
        self._user_routes.clear()

    # -- talking ---------------------------------------------------------- #
    def _say(
        self,
        topic: str,
        text: str,
        *,
        priority: str = "normal",
        key: Optional[str] = None,
    ) -> None:
        """Narrate one line (chat, when the body can speak; always the feed)."""
        if self.narrator is not None:
            self.narrator.say(topic, text, priority=priority, key=key)

    def _reply(self, text: str, *, topic: str = "reply") -> None:
        """An answer to the operator: spoken, and shown in the console's dock."""
        self._last_reply = {"text": text, "at": self._wall_clock()}
        self._say(topic, text, priority="urgent")

    def _narrate_finished(self, record: GoalRecord) -> None:
        """One line per closed goal, from the record's own fields."""
        if record.status == "done":
            self._say("done", speech.goal_done(record.text, record.ticks), key=record.goal_id)
        elif record.status == "failed" and record.failure_kind not in NOT_FAILURE_KINDS:
            # A death or a desync has its own line (``_on_died``,
            # ``_on_world_desync``); a second "olmadı" would say it twice.
            self._say(
                "failed",
                speech.goal_failed(record.text, record.failure_kind, record.ticks),
                key=record.goal_id,
            )
        elif record.status == "interrupted" and record.failure_kind == "stopped_by_user":
            self._say(
                "stopped", speech.goal_stopped(record.text), priority="urgent", key=record.goal_id
            )
        elif record.status == "unverified":
            self._say(
                "done",
                speech.goal_done(record.text, record.ticks) + " (doğrulayamadım)",
                key=record.goal_id,
            )

    def _on_chat_received(self, event: Mapping[str, Any]) -> None:
        """The operator's chat line, already filtered by sender UUID in the body.

        A control verb was acted on by the body (it works with the brain down);
        the brain only keeps its own picking in step. Anything else is mapped.
        """
        text = str(event.get("text") or "")
        control = event.get("control")
        self.evidence.write("chat_received", text=text[:200], control=control)
        if control:
            self._apply_control_word(str(control), source="chat")
            return
        self._handle_user_text(text, source="chat")

    def _apply_control_word(self, control: str, *, source: str) -> str:
        """``stop``/``cancel``/``status``/``resume`` from chat or the console.

        From chat the body already cancelled the running command (and, for
        stop, popped the suspended ones); from the console the brain pops.
        """
        if control == "stop":
            self._clear_user_goals()
            self._paused = True
            # Chat: the body has already popped every command. Console: the stop
            # below pops. Either way no suspended goal survives to be resumed,
            # so the next closed goal must not say "devam ediyorum" (review
            # finding 2).
            self._stop_requested = bool(self._suspended) or self._stop_requested
            result = "paused"
            if source == "console":
                result, _ = self._stop_goal()
            self._say("paused", speech.paused(), priority="urgent")
            return result
        if control == "cancel":
            if source == "console":
                return self._stop_goal()[0]
            return "body_cancelled"
        if control == "status":
            if source == "console":
                self._reply(self._status_line(), topic="status")
            return "status"
        if control == "resume":
            self._paused = False
            if self.state == LoopState.IDLE:
                self._set_wake(WakeCause.USER)
            self._reply(speech.resumed(), topic="resumed")
            return "resumed"
        return "unknown_control"

    def _status_line(self) -> str:
        """The brain's own status: the goal it plays and the body's facts."""
        facts = (self.session.snapshot or {}).get("facts") or {}
        health = facts.get("health")
        vitals = f", can {health:.0f}/20" if isinstance(health, (int, float)) else ""
        opened = self._open_goal
        if opened is None:
            return f"durum: boştayım{vitals}"
        took = speech.seconds(opened.ticks_since(self.session.tick))
        return speech.clean(
            f"durum: {opened.record.text} ({opened.record.condition})"
            + (f", {took} sn" if took is not None else "")
            + vitals
        )

    def _map_command(self, text: str) -> Optional[commands.CommandPlan]:
        """The one paid call of the command path; ``None`` when it may not run."""
        # The budget is reserved for the model the call actually uses (review
        # finding 13).
        denied = self._gate("map", estimate_cost(commands.MAP_MODEL, 800, 200))
        if denied:
            return None
        try:
            plan = self.governor.call(
                "map",
                lambda: self._map_fn(
                    text,
                    catalog=self.catalog,
                    inventory=self._inventory(),
                    ledger=self.ledger,
                    evidence=self.evidence,
                ),
            )
        except Exception as error:  # noqa: BLE001 - a failed map is "cannot understand"
            self.governor.record_error()
            self.evidence.write("command_map_error", error=f"{type(error).__name__}: {error}")
            return None
        self.governor.record_success()
        return plan

    def _handle_user_text(self, text: str, *, source: str) -> str:
        """One sentence from chat or the console: resolve it, answer, play it."""
        self.evidence.write("command_received", source=source, text=text[:200])
        resolved = commands.resolve(
            text,
            inventory=self._inventory(),
            catalog=self.catalog,
            memory=self.memory,
            map_fn=self._map_command,
        )
        self.evidence.write(
            "command_resolved",
            source=source,
            resolution=resolved.kind,
            reason=resolved.reason,
            via=resolved.via,
            condition=resolved.proposal.condition if resolved.proposal else None,
            skill=resolved.skill,
        )
        if resolved.kind == "control":
            return self._apply_control_word(str(resolved.control), source=source)
        if resolved.kind == "unsupported":
            self._reply(
                speech.cannot_understand(text, resolved.detail),
                topic="cannot_understand",
            )
            return "unsupported"
        if resolved.kind == "refused":
            self._reply(speech.refused(str(resolved.reason), resolved.detail), topic="refused")
            return f"refused:{resolved.reason}"
        proposal = resolved.proposal
        validation = resolved.validation
        assert proposal is not None and validation is not None
        known = resolved.skill is not None and self.catalog.by_name(resolved.skill) is not None
        # The arguments the sentence named (`commands.Resolution.args`, when the
        # command map has them); ``None`` derives the count from the condition.
        args = list(getattr(resolved, "args", None) or []) or None
        if known and not proposal.condition.strip():
            # A skill-verified task: no inventory condition, the skill's own
            # `done` settles it (same shape as a plan step).
            proposal = proposal.model_copy(
                update={"condition": skill_goal_text(str(resolved.skill), args or ())}
            )
            validation = Validation(
                ok=True, skill_name=resolved.skill, unverifiable=True
            )
        self._reply(
            speech.task_accepted(proposal.text, proposal.condition, learning=not known),
            topic="task_accepted",
        )
        if self.state == LoopState.PLAYING and self._open_goal is not None and known:
            self._drop_plan("user_task")
            self._pending = PendingGoal(
                proposal=proposal,
                validation=validation,
                goal=goal_of(proposal, validation),
                source="user",
                forced_skill=resolved.skill,
                origin="user",
                args=args,
            )
            self._last_route = {
                "decision": "play",
                "skill": resolved.skill,
                "scores": None,
                "condition": proposal.condition,
                "source": "user_task",
            }
            self._remember_user_task(proposal)
            self._play()
            return "preempted"
        # A sentence from the operator may repeat a request (review finding 5).
        self._enqueue_user(
            proposal,
            repeatable=True,
            skill=resolved.skill if known else None,
            args=args,
        )
        if self.state == LoopState.IDLE:
            self._set_wake(WakeCause.USER)
        return "queued"

    def _remember_user_task(self, proposal: GoalProposal) -> None:
        self._last_proposal = proposal

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
                "snapshot_at": self.session.snapshot_at,
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
                    "rationale": opened.proposal.rationale,
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
                "persona": {"id": self.persona.id, "style": self.persona.style},
                "plan": [
                    {"text": goal.text, "condition": goal.condition, "skill": goal.skill}
                    for goal in self._plan
                ],
                "thought": self._plan_thought,
                "user_goals": len(self._user_goals),
                "suspended": len(self._suspended),
                "reply": self._last_reply,
                "updated": self._wall_clock(),
            }
        )

    # -- helpers -------------------------------------------------------- #
    def _send_goal(
        self,
        skill: str,
        condition: str,
        budget_ticks: Optional[int] = None,
        args: Optional[list[str]] = None,
    ) -> str:
        """Send one command; the one send path, so the count always travels.

        Code-only, plan, survival and stale-retry sends all come through
        here, which is why :meth:`_goal_args` cannot be forgotten on one of
        them. Explicit ``args`` (a plan step, an operator task) win over the
        derived count. ``budget_ticks`` overrides the default per-goal budget: a
        night goal needs longer than the default or the body fails it mid-night.
        """
        return self.session.send_goal(
            skill,
            condition,
            self._goal_args(skill, condition) if args is None else list(args),
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
        """Ask for a snapshot on the 60 s heartbeat clock.

        A watching console asks for nothing: the body pushes a snapshot every
        20 ticks and the console shows ``session.snapshot`` (run 9: the 1 Hz
        watch request was the source of every ``dropped`` notice, and one of
        them lost a goal's settled receipt).
        """
        if self.session.awaiting_snapshot:
            return
        now = self._clock()
        if now - self._last_heartbeat < self.heartbeat_seconds:
            return
        self._last_heartbeat = now
        if not self.session.request_snapshot(cause="heartbeat"):
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
    "NOTHING_TO_DO_GAP_TICKS",
    "NOTHING_TO_DO_MAX_GAP_TICKS",
    "Loop",
    "LoopState",
    "WakeCause",
]
