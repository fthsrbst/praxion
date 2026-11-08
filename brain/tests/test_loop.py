from __future__ import annotations

import json
import threading
import time
from pathlib import Path

import pytest

from fake_body import FakeBody
from praxion_brain.admission import (
    WRITE_INPUT_ESTIMATE,
    WRITE_OUTPUT_ESTIMATE,
    AdmissionReport,
)
from praxion_brain.curriculum import GoalProposal
from praxion_brain.evidence import Evidence
from praxion_brain.goal import Goal
from praxion_brain.governor import Governor
from praxion_brain.ledger import Ledger, estimate_cost
from praxion_brain.loop import (
    FAILURE_GAP_TICKS,
    FAILURE_STREAK,
    FAILURE_STREAK_GAP_TICKS,
    MAX_STEP_ERRORS,
    Loop,
    LoopState,
)
from praxion_brain.memory import GoalRecord, Memory
from praxion_brain.practice import PracticeResult
from praxion_brain.port import Bye, PortClient, StackEntry
from praxion_brain.proposer import (
    ESCALATE_AFTER,
    INPUT_ESTIMATE,
    OUTPUT_ESTIMATE,
    estimate as propose_estimate,
)
from praxion_brain.router import Route
from praxion_brain.skills import (
    CandidateReport,
    CheckReport,
    SkillCatalog,
    SkillInfo,
)
from praxion_brain.writer import SkillDraft


class Clock:
    def __init__(self, now: float = 1_000.0) -> None:
        self.now = now

    def __call__(self) -> float:
        return self.now


class SpyGovernor(Governor):
    """A governor that records every ``allow`` decision a test wants to see."""

    def __init__(self, *args, **kwargs) -> None:
        super().__init__(*args, **kwargs)
        self.calls: list[tuple[str, float]] = []

    def allow(self, call_class, estimate=0.0, *, provider=None):
        self.calls.append((call_class, float(estimate)))
        return super().allow(call_class, estimate, provider=provider)


def catalog(*names: str) -> SkillCatalog:
    return SkillCatalog(
        skills=[
            SkillInfo(
                name=name,
                doc=f"doc for {name}",
                source=f"skill {name}()\n  rules:\n    true -> done\n",
                path=f"{name}.skill",
                origin="core",
            )
            for name in names
        ]
    )


def proposal(condition: str, text: str = "do the thing", rung: str | None = None):
    return GoalProposal(text=text, condition=condition, rung=rung, rationale="because")


def make_loop(
    body: FakeBody,
    tmp_path: Path,
    *,
    propose_fn,
    route_fn,
    skills: SkillCatalog | None = None,
    memory: Memory | None = None,
    clock: Clock | None = None,
    governor_kwargs: dict | None = None,
    governor_class: type[Governor] = Governor,
    **kwargs,
) -> Loop:
    clock = clock if clock is not None else Clock()
    evidence = Evidence(tmp_path / "evidence")
    ledger = Ledger(tmp_path / "ledger.jsonl")
    governor_options = {"warn_usd_per_hour": 100.0, "hard_stop_usd": 100.0}
    governor_options.update(governor_kwargs or {})
    governor = governor_class(
        ledger,
        evidence=evidence,
        clock=clock,
        log=lambda _message: None,
        **governor_options,
    )
    memory = memory if memory is not None else Memory(
        "w", root=tmp_path / "worlds", clock=clock, evidence=evidence
    )
    port = PortClient(body.port, handshake_timeout=2.0)
    logger = kwargs.pop("logger", lambda _message: None)
    return Loop(
        port,
        world_id="w",
        memory=memory,
        catalog=skills if skills is not None else SkillCatalog(),
        ledger=ledger,
        evidence=evidence,
        governor=governor,
        propose_fn=propose_fn,
        route_fn=route_fn,
        clock=clock,
        wall_clock=clock,
        logger=logger,
        **kwargs,
    )


def pump(loop: Loop, predicate, timeout: float = 4.0, what: str = "state") -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        loop.step(0.02)
    raise AssertionError(
        f"no {what} within {timeout}s; state={loop.state} "
        f"wake={loop._wake} open={loop._open_goal}"
    )


def set_tick(loop: Loop, body: FakeBody, target: int) -> None:
    """Move the body tick to exactly ``target`` and let the loop see it.

    The event carries the new tick, so no test has to sleep or poll pongs.
    """
    current = int(loop.session.tick or 0)
    if target <= current:
        raise AssertionError(f"target tick {target} is not past the body tick {current}")
    if target - current > 1:
        body.advance(target - current - 1)
    body.emit_event("note", text="tick")
    pump(loop, lambda: int(loop.session.tick or 0) >= target, what=f"tick {target}")


def wait_accepted(body: FakeBody, command_id: str, timeout: float = 3.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if any(
            receipt.get("command_id") == command_id
            and receipt.get("state") == "accepted"
            for receipt in body.receipts
        ):
            return
        time.sleep(0.01)
    raise AssertionError(f"command {command_id} was not accepted: {body.receipts!r}")


def evidence_kinds(loop: Loop) -> list[str]:
    path = loop.evidence.path
    if not path.is_file():
        return []
    return [
        json.loads(line)["kind"]
        for line in path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]


def evidence_entries(loop: Loop, kind: str) -> list[dict]:
    path = loop.evidence.path
    if not path.is_file():
        return []
    return [
        entry
        for entry in (
            json.loads(line)
            for line in path.read_text(encoding="utf-8").splitlines()
            if line.strip()
        )
        if entry["kind"] == kind
    ]


def open_record(
    memory: Memory,
    *,
    goal_id: str,
    command_id: str,
    condition: str,
    intent_id: int | None = None,
    skill: str = "get_furnace",
) -> GoalRecord:
    record = GoalRecord(
        goal_id=goal_id,
        text=f"goal {goal_id}",
        condition=condition,
        status="open",
        command_id=command_id,
        intent_id=intent_id,
        skill=skill,
        t_start=memory.now(),
    )
    memory.append_goal(record)
    return record


def admission_options(tmp_path: Path, **overrides) -> dict:
    options = {
        "seeds": (101,),
        "held_out": 202,
        "max_repairs": 0,
        "max_ticks": 10,
        "attempts_root": tmp_path / "attempts",
        "candidates_root": tmp_path / "candidates",
        "active_dir": tmp_path / "active",
        "body_failures_path": tmp_path / "body_failures.jsonl",
    }
    options.update(overrides)
    return options


def furnace_write_fn(*, goal, catalog, failure, model, **kwargs) -> SkillDraft:
    return SkillDraft(
        skill_source=(
            'skill get_furnace()\n  doc "furnace"\n  rules:\n'
            "    count(furnace) >= 1 -> done\n"
        ),
        rationale="craft it",
    )


def furnace_check_fn(directory) -> CheckReport:
    return CheckReport(
        ok=True,
        candidates=[
            CandidateReport(
                file="get_furnace.skill", skills=["get_furnace"], ok=True
            )
        ],
    )


def passing_practice(*, skill, goal, candidate_dir, seed, max_ticks) -> PracticeResult:
    return PracticeResult(
        passed=True,
        outcome="done",
        ticks=100,
        run_dir=f"runs/seed-{seed}",
        digest="ok",
        seed=seed,
    )


def write_loop(body: FakeBody, tmp_path: Path, **overrides):
    """A loop scripted to route one furnace goal into admission."""
    options = admission_options(tmp_path)
    options.update(overrides.pop("admission_overrides", {}))
    return make_loop(
        body,
        tmp_path,
        propose_fn=lambda *args, **kwargs: proposal(
            "has(furnace)", "get a furnace", "furnace"
        ),
        route_fn=lambda *args, **kwargs: Route(kind="write"),
        skills=catalog(),
        admission_options=options,
        **overrides,
    )


# --------------------------------------------------------------------------- #
# The happy path and the existing behaviour
# --------------------------------------------------------------------------- #
def test_full_happy_cycle_then_next_pick(tmp_path) -> None:
    proposals = [
        proposal("has(furnace)", "get a furnace", "furnace"),
        proposal("has(coal)", "get coal", "coal"),
    ]
    routes = [Route(kind="play", skill="get_furnace"), Route(kind="play", skill="get_coal")]
    propose_calls = []
    route_calls = []

    def propose_fn(briefing, catalog, **kwargs):
        propose_calls.append((briefing, kwargs.get("invalid_count")))
        return proposals.pop(0)

    def route_fn(goal, skills, **kwargs):
        route_calls.append(goal.text)
        return routes.pop(0)

    with FakeBody(skills={"get_furnace", "get_coal"}, facts={"inventory": {}}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=route_fn,
            skills=catalog("get_furnace", "get_coal"),
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            first = body.commands[0]
            assert first.skill == "get_furnace"
            assert first.condition == "has(furnace)"
            assert first.expires_at_tick > 0
            assert first.based_on_generation == 0

            wait_accepted(body, first.id)
            body.settle(first.id, outcome="done", condition="true")
            pump(loop, lambda: len(body.commands) >= 2, what="second command after settle")

            second = body.commands[1]
            assert second.skill == "get_coal"

            records = {record.goal_id: record for record in loop.memory.goals()}
            done = [record for record in records.values() if record.status == "done"]
            assert len(done) == 1
            assert done[0].condition == "has(furnace)"
            assert done[0].condition_result == "true"
            assert done[0].skill == "get_furnace"
            assert loop.memory.skill_uses()[0].outcome == "done"

            kinds = evidence_kinds(loop)
            assert "loop_transition" in kinds
            assert "command_sent" in kinds
            assert "goal_finished" in kinds
            assert len(propose_calls) == 2
            assert [goal_text for goal_text in route_calls] == [
                "get a furnace",
                "get coal",
            ]
        finally:
            loop.close()


def test_write_route_runs_admission_then_plays(tmp_path) -> None:
    practice_calls: list[int] = []

    def practice_fn(*, skill, goal, candidate_dir, seed, max_ticks):
        practice_calls.append(seed)
        return passing_practice(
            skill=skill, goal=goal, candidate_dir=candidate_dir, seed=seed, max_ticks=max_ticks
        )

    with FakeBody(skills={"get_furnace"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal(
                "has(furnace)", "get a furnace", "furnace"
            ),
            route_fn=lambda *args, **kwargs: Route(kind="write"),
            skills=catalog(),
            admission_options=admission_options(
                tmp_path,
                write_fn=furnace_write_fn,
                check_fn=furnace_check_fn,
                practice_fn=practice_fn,
            ),
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="command after admission")
            assert body.commands[0].skill == "get_furnace"
            assert practice_calls == [101, 202]
            assert loop.memory.goals()[0].skill == "get_furnace"
            assert loop.memory.goals()[0].skill_origin == "written"
            assert (tmp_path / "active" / "get_furnace.skill").is_file()
        finally:
            loop.close()


def test_budget_hard_stop_degrades_to_code_only(tmp_path) -> None:
    called: list[str] = []

    def propose_fn(*args, **kwargs):
        called.append("propose")
        raise AssertionError("the proposal model must not run under a hard stop")

    with FakeBody(skills={"get_wood"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_wood"),
            skills=catalog("get_wood"),
            governor_kwargs={"hard_stop_usd": 0.0},
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="code-only command")
            assert called == []
            assert body.commands[0].skill == "get_wood"
            assert body.commands[0].condition == "count(#logs) >= 1"
            assert "code_only_goal" in evidence_kinds(loop)
        finally:
            loop.close()


def test_two_failures_block_the_goal(tmp_path) -> None:
    clock = Clock(1_000.0)
    memory = Memory("w", root=tmp_path / "worlds", clock=clock)
    memory.append_goal(
        GoalRecord(
            goal_id="g-old",
            text="get coal",
            condition="has(coal)",
            status="failed",
            failure_kind="not_found",
            t_start=990.0,
            t_end=990.0,
        )
    )

    with FakeBody(skills={"get_coal"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal(
                "has(coal)", "get coal", "coal"
            ),
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_coal"),
            skills=catalog("get_coal"),
            memory=memory,
            clock=clock,
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first coal command")
            first = body.commands[0]
            wait_accepted(body, first.id)
            body.settle(
                first.id,
                outcome="failed(not_found)",
                failure_kind="not_found",
            )

            pump(
                loop,
                lambda: loop.state == LoopState.IDLE
                and loop._consecutive_failures == 1,
                what="failed goal",
            )
            # The failure gap holds the pick until the body tick moves on.
            set_tick(loop, body, loop._next_pick_tick)
            pump(
                loop,
                lambda: loop.state == LoopState.IDLE
                and loop._pending is None
                and loop._invalid_proposals >= 3,
                what="blocked pick",
            )

            assert memory.is_blocked("has(coal)") is True
            assert len(body.commands) == 1
            assert loop.memory.failures()["has(coal)"].attempts == 2
        finally:
            loop.close()


def test_reconnect_mid_goal_resyncs_and_continues(tmp_path) -> None:
    clock = Clock()
    with FakeBody(skills={"get_furnace"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal(
                "has(furnace)", "get a furnace", "furnace"
            ),
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_furnace"),
            skills=catalog("get_furnace"),
            clock=clock,
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            command = body.commands[0]
            wait_accepted(body, command.id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None)

            body.disconnect()
            deadline = time.monotonic() + 4.0
            while time.monotonic() < deadline:
                if body.connections >= 2 and loop.state == LoopState.PLAYING:
                    break
                loop.step(0.02)
                clock.now += 0.3
            else:
                raise AssertionError(
                    f"no reconnect recovery; state={loop.state} "
                    f"connections={body.connections}"
                )
            assert loop._open_goal is not None
            assert loop._open_goal.command_id == command.id

            body.settle(command.id, outcome="done", condition="true")
            pump(
                loop,
                lambda: any(
                    record.status == "done" for record in loop.memory.goals()
                ),
                what="settled after reconnect",
            )
            assert "loop_recovered" in evidence_kinds(loop)
        finally:
            loop.close()


def test_heartbeat_with_no_change_does_nothing(tmp_path) -> None:
    clock = Clock(0.0)

    with FakeBody() as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal("has(", "broken"),
            route_fn=lambda *args, **kwargs: Route(kind="play"),
            skills=catalog(),
            clock=clock,
        )
        try:
            loop.start()
            pump(
                loop,
                lambda: loop.state == LoopState.IDLE
                and loop._pending is None
                and loop._invalid_proposals >= 3,
                what="idle after invalid proposals",
            )
            commands_before = len(body.commands)

            clock.now += 61.0
            loop.step(0.05)
            pump(loop, lambda: "heartbeat_quiet" in evidence_kinds(loop), what="quiet heartbeat")

            kinds = evidence_kinds(loop)
            assert "heartbeat_requested" in kinds
            assert "heartbeat_change" not in kinds
            assert len(body.commands) == commands_before
            assert loop.state == LoopState.IDLE
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# A1: recovery adopts suspended intents and settles open goals by condition
# --------------------------------------------------------------------------- #
def test_recovery_adopts_a_suspended_intent(tmp_path) -> None:
    clock = Clock()
    memory = Memory("w", root=tmp_path / "worlds", clock=clock)
    open_record(
        memory,
        goal_id="g-0001",
        command_id="c-0001",
        condition="has(furnace)",
        intent_id=3,
    )

    with FakeBody(skills={"get_furnace"}) as body:
        # Plan B is live in the body: goal A is suspended while goal B runs.
        body.stack.append(
            StackEntry(
                intent_id=3, tier="command", skill="get_furnace", state="suspended"
            )
        )
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal("has(", "broken"),
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_furnace"),
            skills=catalog("get_furnace"),
            memory=memory,
            clock=clock,
        )
        try:
            loop.start()
            pump(loop, lambda: loop.state == LoopState.PLAYING, what="recovered playing")
            assert loop._open_goal is not None
            assert loop._open_goal.command_id == "c-0001"
            assert loop._open_goal.intent_id == 3
            recovered = evidence_entries(loop, "loop_recovered")
            assert recovered and recovered[0]["intent_state"] == "suspended"

            # The body resumes goal A and its (late) receipt must land on the
            # adopted record, not be dropped as unmatched.
            body.emit_event(
                "command_receipt",
                command_id="c-0001",
                state="settled",
                outcome="done",
                condition="true",
                intent_id=3,
            )
            pump(
                loop,
                lambda: any(record.status == "done" for record in loop.memory.goals()),
                what="late receipt",
            )
            records = {record.goal_id: record for record in loop.memory.goals()}
            assert records["g-0001"].status == "done"
            assert "receipt_unmatched" not in evidence_kinds(loop)
        finally:
            loop.close()


def test_recovery_settles_open_goals_from_the_snapshot(tmp_path) -> None:
    clock = Clock()
    memory = Memory("w", root=tmp_path / "worlds", clock=clock)
    open_record(
        memory, goal_id="g-0001", command_id="c-0001", condition="has(furnace)"
    )
    open_record(memory, goal_id="g-0002", command_id="c-0002", condition="has(coal)")

    with FakeBody(facts={"inventory": {"furnace": 1}}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal("has(", "broken"),
            route_fn=lambda *args, **kwargs: Route(kind="play"),
            skills=catalog(),
            memory=memory,
            clock=clock,
        )
        try:
            loop.start()
            pump(
                loop,
                lambda: all(record.status != "open" for record in memory.goals()),
                what="open goals settled",
            )
            records = {record.goal_id: record for record in memory.goals()}
            # The condition holds in the snapshot: the goal really is done.
            assert records["g-0001"].status == "done"
            assert records["g-0001"].condition_result == "true"
            # The body can no longer settle it: interrupted, not failed.
            assert records["g-0002"].status == "interrupted"
            assert records["g-0002"].condition_result == "false"
            assert memory.failures() == {}
            assert loop._open_goal is None
            assert loop.memory.is_blocked("has(coal)") is False
        finally:
            loop.close()


def test_unmatched_intent_closes_dead_open_records(tmp_path) -> None:
    # A8: a live intent that matches no record must not leave dead open lines.
    clock = Clock()
    memory = Memory("w", root=tmp_path / "worlds", clock=clock)
    open_record(
        memory,
        goal_id="g-0001",
        command_id="c-0001",
        condition="has(coal)",
        intent_id=9,
        skill="get_coal",
    )

    with FakeBody() as body:
        body.stack.append(
            StackEntry(
                intent_id=4, tier="command", skill="get_stranger", state="running"
            )
        )
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal("has(", "broken"),
            route_fn=lambda *args, **kwargs: Route(kind="play"),
            skills=catalog(),
            memory=memory,
            clock=clock,
        )
        try:
            loop.start()
            pump(
                loop,
                lambda: "loop_unmatched_intent" in evidence_kinds(loop),
                what="unmatched intent",
            )
            assert loop.state == LoopState.IDLE
            assert loop._open_goal is None
            records = {record.goal_id: record for record in memory.goals()}
            assert records["g-0001"].status == "interrupted"
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# A7: done with an undecided condition is unverified, not a success
# --------------------------------------------------------------------------- #
def test_done_with_an_unknown_condition_is_unverified(tmp_path) -> None:
    with FakeBody(skills={"get_furnace"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal(
                "has(furnace)", "get a furnace", "furnace"
            ),
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_furnace"),
            skills=catalog("get_furnace"),
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            command = body.commands[0]
            wait_accepted(body, command.id)
            body.settle(command.id, outcome="done", condition="unknown")

            pump(
                loop,
                lambda: any(
                    record.status == "unverified" for record in loop.memory.goals()
                ),
                what="unverified record",
            )
            records = loop.memory.goals()
            assert [record.status for record in records] == ["unverified"]
            assert records[0].condition_result == "unknown"
            assert [record for record in records if record.status == "done"] == []
            assert loop.memory.failures() == {}
            assert loop.memory.skill_uses()[-1].outcome == "unverified"
        finally:
            loop.close()


def test_an_unverifiable_goal_with_an_unknown_condition_stays_done(tmp_path) -> None:
    with FakeBody(skills={"get_furnace"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal(
                "near(furnace, 4)", "stand by a furnace", "furnace"
            ),
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_furnace"),
            skills=catalog("get_furnace"),
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")
            command = body.commands[0]
            wait_accepted(body, command.id)
            body.settle(command.id, outcome="done", condition="unknown")

            pump(
                loop,
                lambda: any(record.status == "done" for record in loop.memory.goals()),
                what="done record",
            )
            record = loop.memory.goals()[0]
            assert record.unverifiable is True
            assert record.status == "done"
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# A9: bye replaced is terminal
# --------------------------------------------------------------------------- #
def test_bye_replaced_is_terminal(tmp_path) -> None:
    logged: list[str] = []
    with FakeBody() as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal("has(", "broken"),
            route_fn=lambda *args, **kwargs: Route(kind="play"),
            skills=catalog(),
            logger=logged.append,
        )
        try:
            loop.start()
            pump(loop, lambda: loop._recovering is None, what="first snapshot")

            body.send(Bye(reason="replaced"))
            deadline = time.monotonic() + 3.0
            while time.monotonic() < deadline and not loop._stopped:
                loop.step(0.05)
            assert loop._stopped is True

            connections = body.connections
            loop.step(0.05)
            assert body.connections == connections  # no reconnect ping-pong
            assert any("replaced" in message for message in logged)
            kinds = evidence_kinds(loop)
            assert "loop_bye" in kinds
            assert "loop_reconnect" not in kinds
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# A3: the reader thread keeps the body from seeing a silent brain
# --------------------------------------------------------------------------- #
def test_body_sees_pings_while_the_loop_is_blocked_in_a_slow_call(tmp_path) -> None:
    started = threading.Event()
    release = threading.Event()

    def slow_propose(*args, **kwargs):
        started.set()
        release.wait(5.0)
        return proposal("has(furnace)", "get a furnace", "furnace")

    with FakeBody(skills={"get_furnace"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=slow_propose,
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_furnace"),
            skills=catalog("get_furnace"),
        )
        stop = threading.Event()

        def drive() -> None:
            while not stop.is_set():
                loop.step(0.02)

        driver = threading.Thread(target=drive, name="loop-driver", daemon=True)
        try:
            loop.start()
            driver.start()
            assert started.wait(3.0), "the loop never reached the slow call"
            before = body.pings
            time.sleep(2.2)
            # Section 3.1/2.5: one ping per second, even while the loop thinks.
            assert body.pings - before >= 2, (
                f"the body saw {body.pings - before} ping(s) during a 2.2 s "
                "blocking call"
            )
        finally:
            release.set()
            stop.set()
            driver.join(timeout=3.0)
            loop.close()


# --------------------------------------------------------------------------- #
# A4: every paid call has a deadline and fails typed
# --------------------------------------------------------------------------- #
def test_propose_timeout_falls_back_to_code_only(tmp_path) -> None:
    def stuck(*args, **kwargs):
        time.sleep(1.0)
        raise AssertionError("the timed-out proposal must not be awaited")

    with FakeBody(skills={"get_wood"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=stuck,
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_wood"),
            skills=catalog("get_wood"),
            governor_kwargs={"call_timeouts": {"propose": 0.1}},
        )
        try:
            loop.start()
            pump(
                loop,
                lambda: "proposal_timeout" in evidence_kinds(loop),
                what="propose timeout",
            )
            pump(loop, lambda: len(body.commands) >= 1, what="code-only command")
            assert body.commands[0].skill == "get_wood"
            assert "proposal_error" not in evidence_kinds(loop)
        finally:
            loop.close()


def test_route_timeout_returns_to_idle(tmp_path) -> None:
    def stuck_route(*args, **kwargs):
        time.sleep(1.0)
        return Route(kind="play", skill="get_furnace")

    with FakeBody(skills={"get_furnace"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal(
                "has(furnace)", "get a furnace", "furnace"
            ),
            route_fn=stuck_route,
            skills=catalog("get_furnace"),
            governor_kwargs={"call_timeouts": {"route": 0.1}},
        )
        try:
            loop.start()
            pump(
                loop,
                lambda: "route_timeout" in evidence_kinds(loop),
                what="route timeout",
            )
            assert loop.state == LoopState.IDLE
            assert loop._open_goal is None
            assert "route_error" not in evidence_kinds(loop)
        finally:
            loop.close()


def test_a_writer_call_that_hangs_times_out(tmp_path) -> None:
    def slow_write(**kwargs):
        time.sleep(1.0)
        raise AssertionError("the timed-out writer must not be awaited")

    def check_fn(directory):
        raise AssertionError("a timed-out write never reaches the checker")

    with FakeBody(skills=set()) as body:
        loop = write_loop(
            body,
            tmp_path,
            governor_kwargs={"call_timeouts": {"write": 0.1}},
            admission_overrides={"write_fn": slow_write, "check_fn": check_fn},
        )
        try:
            loop.start()
            pump(
                loop,
                lambda: any(
                    record.failure_kind == "timeout" for record in loop.memory.goals()
                ),
                what="writer timeout",
            )
            record = loop.memory.goals()[0]
            assert record.status == "failed"
            assert record.failure_kind == "timeout"
            entry = evidence_entries(loop, "write_timeout")[0]
            assert entry["call"] == "write"
        finally:
            loop.close()


def test_an_admission_run_that_hangs_times_out(tmp_path) -> None:
    def slow_practice(**kwargs):
        time.sleep(1.0)
        raise AssertionError("the timed-out admission must not be awaited")

    with FakeBody(skills=set()) as body:
        loop = write_loop(
            body,
            tmp_path,
            governor_kwargs={"call_timeouts": {"admission": 0.15, "write": 5.0}},
            admission_overrides={
                "write_fn": furnace_write_fn,
                "check_fn": furnace_check_fn,
                "practice_fn": slow_practice,
            },
        )
        try:
            loop.start()
            pump(
                loop,
                lambda: any(
                    record.failure_kind == "timeout" for record in loop.memory.goals()
                ),
                what="admission timeout",
            )
            entry = evidence_entries(loop, "write_timeout")[0]
            assert entry["call"] == "admission"
            assert entry["seconds"] == 0.15
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# A5: the estimates use the real model of each call
# --------------------------------------------------------------------------- #
def test_propose_estimate_uses_the_escalated_model(tmp_path) -> None:
    seen_invalid: list[int] = []

    def propose_fn(*args, **kwargs):
        seen_invalid.append(kwargs["invalid_count"])
        return proposal("has(furnace)", "get a furnace", "furnace")

    with FakeBody(skills={"get_furnace"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_furnace"),
            skills=catalog("get_furnace"),
            governor_class=SpyGovernor,
        )
        try:
            loop._invalid_proposals = ESCALATE_AFTER
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")

            assert seen_invalid == [ESCALATE_AFTER]
            estimates = [
                estimate for call, estimate in loop.governor.calls if call == "propose"
            ]
            assert estimates
            assert estimates[0] == pytest.approx(
                estimate_cost("gpt-6.1-terra", INPUT_ESTIMATE, OUTPUT_ESTIMATE)
            )
            assert estimates[0] > propose_estimate(0)
        finally:
            loop.close()


def test_admission_receives_the_writer_model(tmp_path, monkeypatch) -> None:
    seen: dict = {}

    def fake_admit(goal, **options):
        seen.update(options)
        seen["goal"] = goal
        return AdmissionReport(
            goal=goal, status="rejected", skill_name=goal.skill_name
        )

    monkeypatch.setattr("praxion_brain.loop.admit", fake_admit)

    with FakeBody(skills=set()) as body:
        loop = write_loop(
            body,
            tmp_path,
            writer_model="gpt-6.1-sol",
            governor_class=SpyGovernor,
        )
        try:
            loop.start()
            pump(loop, lambda: bool(seen), what="admission call")

            assert seen["writer_model"] == "gpt-6.1-sol"
            assert seen["goal"] == Goal(
                text="get a furnace", item="furnace", count=1
            )
            assert callable(seen["write_fn"])  # the deadline wrapper
            estimates = [
                estimate for call, estimate in loop.governor.calls if call == "write"
            ]
            assert estimates == [
                pytest.approx(
                    estimate_cost(
                        "gpt-6.1-sol", WRITE_INPUT_ESTIMATE, WRITE_OUTPUT_ESTIMATE
                    )
                )
            ]
            records = loop.memory.goals()
            assert records[0].status == "failed"
            assert records[0].failure_kind == "rejected"
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# A6: one bad step never kills the loop
# --------------------------------------------------------------------------- #
def test_run_survives_step_errors_and_stops_cleanly(tmp_path) -> None:
    logged: list[str] = []
    with FakeBody(skills={"get_furnace"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal(
                "has(furnace)", "get a furnace", "furnace"
            ),
            route_fn=lambda *args, **kwargs: Route(kind="play", skill="get_furnace"),
            skills=catalog("get_furnace"),
            logger=logged.append,
        )
        try:
            loop.start()
            pump(
                loop,
                lambda: loop._open_goal is not None
                and loop._open_goal.intent_id is not None,
                what="accepted command",
            )

            def boom(message):
                raise RuntimeError("bad message")

            loop._handle_message = boom
            for _ in range(MAX_STEP_ERRORS + 1):
                body.emit_event("note", text="x")

            loop.run(0.01)

            assert loop._stopped is True
            kinds = evidence_kinds(loop)
            assert kinds.count("loop_error") == MAX_STEP_ERRORS
            assert any("stopping" in message for message in logged)
            record = loop.memory.goals()[0]
            assert record.status == "failed"
            assert record.failure_kind == "loop_error"
            assert loop._open_goal is None
        finally:
            loop.close()


def test_protocol_error_is_recorded_and_the_loop_reconnects(tmp_path) -> None:
    clock = Clock()
    with FakeBody() as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: proposal("has(", "broken"),
            route_fn=lambda *args, **kwargs: Route(kind="play"),
            skills=catalog(),
            clock=clock,
        )
        try:
            loop.start()
            pump(loop, lambda: loop._recovering is None, what="first snapshot")
            # Settle into a plain idle loop, so the bad frame arrives through
            # step() and not inside a snapshot refresh.
            pump(
                loop,
                lambda: loop.state == LoopState.IDLE
                and loop._invalid_proposals >= 3,
                what="idle after invalid proposals",
            )

            body.send_raw({"t": "nonsense"})
            deadline = time.monotonic() + 4.0
            while time.monotonic() < deadline:
                loop.step(0.05)
                clock.now += 0.6
                if "loop_reconnect" in evidence_kinds(loop):
                    break
            else:
                raise AssertionError(f"no reconnect: {evidence_kinds(loop)}")

            kinds = evidence_kinds(loop)
            assert "loop_error" in kinds
            assert "loop_disconnected" in kinds
            assert body.connections >= 2
        finally:
            loop.close()


def test_a_count_goal_passes_its_count_to_a_skill_with_a_parameter() -> None:
    from praxion_brain.loop import Loop
    from praxion_brain.skills import SkillCatalog, SkillInfo

    loop = Loop.__new__(Loop)
    loop.catalog = SkillCatalog(
        skills=[
            SkillInfo(name="get_wood", params=["n: int = 1"], source="", path="", origin="core"),
            SkillInfo(name="get_crafting_table", source="", path="", origin="core"),
        ]
    )
    assert loop._goal_args("get_wood", "count(#logs) >= 5") == ["5"]
    assert loop._goal_args("get_wood", "has(oak_log)") == []
    assert loop._goal_args("get_crafting_table", "count(crafting_table) >= 2") == []


# --------------------------------------------------------------------------- #
# The never-spin rules: code-only obeys memory, count and the failure gap
# --------------------------------------------------------------------------- #
def test_code_only_passes_the_count_and_blocks_after_two_wrong_skills(tmp_path) -> None:
    clock = Clock()
    inventory = {"oak_log": 1, "crafting_table": 1, "wooden_pickaxe": 1}
    skills = SkillCatalog(
        skills=[
            SkillInfo(
                name="get_cobblestone",
                params=["n: int = 3"],
                source="",
                path="",
                origin="core",
            )
        ]
    )
    condition = "count(cobblestone) >= 8"

    with FakeBody(skills={"get_cobblestone"}, facts={"inventory": inventory}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *args, **kwargs: (_ for _ in ()).throw(
                AssertionError("code-only must not propose")
            ),
            route_fn=lambda *args, **kwargs: (_ for _ in ()).throw(
                AssertionError("code-only must not route")
            ),
            skills=skills,
            clock=clock,
            governor_kwargs={"hard_stop_usd": 0.0},
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first code-only command")
            first = body.commands[0]
            assert first.skill == "get_cobblestone"
            assert first.args == ["8"]  # the rung's count, not the default 3
            wait_accepted(body, first.id)
            body.settle(first.id, outcome="done", condition="false")  # skill_wrong

            pump(
                loop,
                lambda: loop.state == LoopState.IDLE
                and loop._consecutive_failures == 1,
                what="first failure",
            )
            deadline = loop._next_pick_tick
            assert deadline == int(loop.session.tick or 0) + FAILURE_GAP_TICKS

            set_tick(loop, body, deadline - 1)
            for _ in range(20):
                loop.step(0.0)
            assert len(body.commands) == 1  # still inside the gap
            assert loop.state == LoopState.IDLE

            set_tick(loop, body, deadline)
            pump(loop, lambda: len(body.commands) >= 2, what="second code-only command")
            second = body.commands[1]
            assert second.skill == "get_cobblestone"
            assert second.args == ["8"]
            wait_accepted(body, second.id)
            body.settle(second.id, outcome="done", condition="false")

            pump(
                loop,
                lambda: loop.memory.is_blocked(condition)
                and loop.state == LoopState.IDLE,
                what="blocked goal",
            )
            assert loop._consecutive_failures == 2

            set_tick(loop, body, loop._next_pick_tick)
            for _ in range(40):
                loop.step(0.0)
            assert len(body.commands) == 2  # blocked: never a third try
            blocked = evidence_entries(loop, "code_only_blocked")
            assert blocked and blocked[0]["condition"] == condition
            assert blocked[0]["skill"] == "get_cobblestone"
        finally:
            loop.close()


def test_a_failed_goal_waits_the_gap_and_three_failures_wait_longer(tmp_path) -> None:
    clock = Clock()
    proposals = [
        proposal("has(furnace)", "get a furnace", "furnace"),
        proposal("has(coal)", "get coal", "coal"),
        proposal("has(diamond)", "get diamond", "diamond"),
        proposal("has(furnace)", "get a furnace", "furnace"),
    ]
    def propose_fn(briefing, catalog, **kwargs):
        return proposals.pop(0)

    with FakeBody(skills={"get_furnace", "get_coal", "get_diamond"}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda goal, skills, **kwargs: Route(
                kind="play", skill=goal.skill_name
            ),
            skills=catalog("get_furnace", "get_coal", "get_diamond"),
            clock=clock,
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="first command")

            for attempt in range(FAILURE_STREAK):
                command = body.commands[attempt]
                wait_accepted(body, command.id)
                body.settle(command.id, outcome="done", condition="false")
                pump(
                    loop,
                    lambda expected=attempt + 1: loop.state == LoopState.IDLE
                    and loop._consecutive_failures == expected,
                    what=f"failure {attempt + 1}",
                )
                expected_gap = (
                    FAILURE_STREAK_GAP_TICKS
                    if loop._consecutive_failures >= FAILURE_STREAK
                    else FAILURE_GAP_TICKS
                )
                deadline = loop._next_pick_tick
                assert deadline == int(loop.session.tick or 0) + expected_gap

                set_tick(loop, body, deadline - 1)
                for _ in range(20):
                    loop.step(0.0)
                assert len(body.commands) == attempt + 1  # no pick inside the gap

                set_tick(loop, body, deadline)
                pump(
                    loop,
                    lambda count=attempt + 2: len(body.commands) >= count,
                    what=f"command {attempt + 2} after the gap",
                )

            assert loop._consecutive_failures == FAILURE_STREAK
            gaps = [entry["ticks"] for entry in evidence_entries(loop, "pick_gap")]
            assert gaps == [
                FAILURE_GAP_TICKS,
                FAILURE_GAP_TICKS,
                FAILURE_STREAK_GAP_TICKS,
            ]

            # A good goal ends the streak; the next failure gets the short gap.
            last = body.commands[FAILURE_STREAK]
            wait_accepted(body, last.id)
            body.settle(last.id, outcome="done", condition="true")
            pump(
                loop,
                lambda: loop.state == LoopState.IDLE
                and loop._consecutive_failures == 0,
                what="streak reset by done",
            )
        finally:
            loop.close()


def test_an_expired_goal_block_wakes_an_idle_loop() -> None:
    from praxion_brain.loop import Loop, LoopState

    class Memory:
        def __init__(self) -> None:
            self.now = {"count(cobblestone)>=8": object()}

        def blocked(self):
            return self.now

    class Evidence:
        def __init__(self) -> None:
            self.lines = []

        def write(self, kind, **fields):
            self.lines.append((kind, fields))

    loop = Loop.__new__(Loop)
    loop.memory, loop.evidence = Memory(), Evidence()
    loop.state = LoopState.IDLE
    loop._heartbeat_facts, loop._was_playing, loop._blocked_goals = None, False, set()
    woken = []
    loop._set_wake = woken.append
    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {}}
    loop._check_heartbeat(facts, is_playing=False, tick=1, heartbeat_pending=False)
    loop._check_heartbeat(facts, is_playing=False, tick=2, heartbeat_pending=False)
    assert woken == []
    loop.memory.now = {}
    loop._check_heartbeat(facts, is_playing=False, tick=3, heartbeat_pending=False)
    assert len(woken) == 1
    assert ("heartbeat_change", {"reasons": ["block_expired"], "tick": 3}) in loop.evidence.lines
