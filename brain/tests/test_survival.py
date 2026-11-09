"""Survival slice 3 in the brain: food and night goals decided in code.

Decision 2026-11-09 "Five next areas": find food when hungry; at night, armed
and healthy hunt hostiles, else shelter. The brain starts these goals from the
snapshot facts with no model call, ahead of every ladder goal, and their
conditions are verifiable (`count_food()`, `is_night()`).
"""

from __future__ import annotations

import time

import pytest

from fake_body import FakeBody
from praxion_brain import survival
from praxion_brain.conditions import evaluate, is_verifiable
from praxion_brain.console.hub import Console
from praxion_brain.evidence import Evidence
from praxion_brain.ledger import Ledger
from praxion_brain.loop import LoopState
from praxion_brain.memory import Memory
from praxion_brain.router import Route
from test_loop import (
    Clock,
    catalog,
    evidence_entries,
    make_loop,
    proposal,
    pump,
    wait_accepted,
)

SKILLS = {"get_wood", "get_food", "night_watch", "take_shelter"}
CAT = catalog("get_wood", "get_food", "night_watch", "take_shelter")
NIGHT = {"health": 20, "food": 20, "is_night": True, "inventory": {}}
SWORD = {"stone_sword": 1}


def test_survival_facts_make_conditions_verifiable() -> None:
    facts = {"health": 6.5, "food": 4, "is_night": True}
    inventory = {"cooked_beef": 1, "dirt": 5}
    for text in ("count_food() >= 2", "not is_night()", "health() <= 10", "food() < 8"):
        assert is_verifiable(text), text
    assert evaluate("count_food() >= 1", inventory, facts) is True
    assert evaluate("count_food() >= 2", inventory, facts) is False
    assert evaluate("not is_night()", inventory, facts) is False
    assert evaluate("health() <= 10", inventory, facts) is True
    assert evaluate("food() < 8 and has(dirt)", inventory, facts) is True
    # Without the snapshot facts they are unknown, never guessed.
    assert evaluate("is_night()", inventory) is None
    assert evaluate("count_food() >= 1", inventory) is None


def test_hunger_with_no_food_wants_get_food() -> None:
    due = survival.choose({"food": 6, "health": 20, "is_night": False}, {}, CAT)
    assert due.kind == "food" and due.skill == "get_food"
    assert due.proposal.condition == "count_food() >= 2"
    # Food held: the body's own meal handles it. Not hungry: nothing due.
    assert survival.choose({"food": 6}, {"bread": 1}, CAT) is None
    assert survival.choose({"food": 7}, {}, CAT) is None
    # A catalog without the skill: skipped, never an error.
    assert survival.choose({"food": 1}, {}, catalog("get_wood")) is None


def test_night_is_watch_when_armed_and_healthy_and_shelter_otherwise() -> None:
    armed = survival.choose(NIGHT, SWORD, CAT)
    assert armed.skill == "night_watch" and armed.proposal.condition == "not is_night()"
    assert survival.choose(NIGHT, {}, CAT).skill == "take_shelter", "unarmed"
    assert survival.choose({**NIGHT, "health": 10}, SWORD, CAT).skill == "take_shelter", "hurt"
    assert survival.choose({**NIGHT, "is_night": False}, SWORD, CAT) is None, "morning"
    assert survival.choose({}, SWORD, CAT) is None, "no facts, no decision"


def test_food_comes_before_the_night() -> None:
    due = survival.choose({**NIGHT, "food": 2}, SWORD, CAT)
    assert due.kind == "food"


class Blocked:
    def is_blocked(self, condition):
        return condition == "not is_night()"


def test_a_blocked_night_goal_is_skipped_so_the_body_fallback_covers_it() -> None:
    assert survival.choose(NIGHT, SWORD, CAT, memory=Blocked()) is None


def _loop(tmp_path, body, **kwargs):
    def propose_fn(*args, **kw):
        raise AssertionError("a survival decision never calls the proposer")

    return make_loop(
        body,
        tmp_path,
        propose_fn=propose_fn,
        route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
        skills=CAT,
        **kwargs,
    )


def test_a_hungry_body_with_no_food_gets_get_food_without_a_model_call(tmp_path) -> None:
    facts = {"health": 20, "food": 5, "is_night": False, "inventory": {"cobblestone": 3}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the food command")
            command = body.commands[0]
            assert command.skill == "get_food"
            assert command.condition == "count_food() >= 2", "verifiable, so it is sent"
            goal = evidence_entries(loop, "survival_goal")[0]
            assert goal["survival"] == "food" and "nothing to eat" in goal["reason"]
            wait_accepted(body, command.id)
            body.settle(command.id, outcome="done", condition="true")
            pump(loop, lambda: any(e["status"] == "done" for e in evidence_entries(loop, "goal_finished")), what="done")
        finally:
            loop.close()


def test_the_hunger_wake_starts_the_food_goal_at_once(tmp_path) -> None:
    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        loop._paused = True  # nothing else may start the loop
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            loop._paused = False
            body.facts.update({"food": 4})
            body.emit_event("wake_brain", cause="hunger_no_food", detail="no food")
            pump(loop, lambda: len(body.commands) >= 1, what="the food command")
            assert body.commands[0].skill == "get_food"
        finally:
            loop.close()


def test_night_starts_the_right_skill_for_the_inventory(tmp_path) -> None:
    for inventory, skill in (({"iron_sword": 1}, "night_watch"), ({"dirt": 2}, "take_shelter")):
        facts = {"health": 20, "food": 20, "is_night": True, "inventory": inventory}
        with FakeBody(skills=SKILLS, facts=facts) as body:
            loop = _loop(tmp_path / skill, body)
            try:
                loop.start()
                pump(loop, lambda: len(body.commands) >= 1, what="the night command")
                assert body.commands[0].skill == skill
                assert body.commands[0].condition == "not is_night()"
            finally:
                loop.close()


def test_a_ladder_goal_is_interrupted_for_a_due_survival_goal(tmp_path) -> None:
    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {"stone_sword": 1}}
    queue = [proposal("count(#logs) >= 5", "get wood", "wood")]

    def propose_fn(*a, **k):
        return queue.pop(0)

    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
            skills=CAT,
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the ladder goal")
            ladder = body.commands[0]
            wait_accepted(body, ladder.id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="accepted")

            # Dusk: the body says it is night; the next snapshot carries it.
            body.facts["is_night"] = True
            loop.session.refresh()
            pump(loop, lambda: evidence_entries(loop, "survival_interrupt"), what="the interrupt")
            pump(loop, lambda: body.pops, what="the pop")
            body.settle(ladder.id, outcome="cancelled")
            pump(loop, lambda: len(body.commands) >= 2, what="the night command")
            assert body.commands[1].skill == "night_watch"
            assert loop._open_goal.survival is True
        finally:
            loop.close()


def test_a_survival_interrupt_is_not_recorded_as_the_users_stop(tmp_path) -> None:
    """A food or night goal stops the ladder goal; nobody pressed Stop.

    The ledger must say why the record closed. ``stopped_by_user``/``console``
    for an interrupt the loop made by itself would put a user action in the
    record that never happened (review of slice 3, finding 1).
    """
    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {"stone_sword": 1}}
    queue = [proposal("count(#logs) >= 5", "get wood", "wood")]

    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: queue.pop(0),
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
            skills=CAT,
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the ladder goal")
            ladder = body.commands[0]
            wait_accepted(body, ladder.id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="accepted")
            body.facts["is_night"] = True
            loop.session.refresh()
            pump(loop, lambda: body.pops, what="the pop")
            body.settle(ladder.id, outcome="cancelled")
            pump(
                loop,
                lambda: any(
                    entry.get("status") == "interrupted"
                    for entry in evidence_entries(loop, "goal_finished")
                ),
                what="the closed record",
            )
            closed = next(
                entry
                for entry in evidence_entries(loop, "goal_finished")
                if entry.get("status") == "interrupted"
            )
            assert closed["failure_kind"] == "preempted_by_survival", closed
            assert closed["source"] == "survival", "not a user stop"
        finally:
            loop.close()


def test_one_interrupt_is_one_pop_however_long_the_receipt_takes(tmp_path) -> None:
    """The interrupt must not repeat every step while the pop is unconfirmed.

    ``_stop_goal`` sends a pop and the record closes on the receipt; until then
    the goal is still open and due, so a second check would pop again, step
    after step (review of slice 3, finding 2).
    """
    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {"stone_sword": 1}}
    queue = [proposal("count(#logs) >= 5", "get wood", "wood")]

    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: queue.pop(0),
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
            skills=CAT,
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the ladder goal")
            wait_accepted(body, body.commands[0].id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="accepted")
            body.facts["is_night"] = True
            loop.session.refresh()
            pump(loop, lambda: body.pops, what="the pop")
            for _ in range(40):
                loop.step(0.02)
            assert len(body.pops) == 1, f"one pop, not {body.pops}"
            assert len(evidence_entries(loop, "survival_interrupt")) == 1
        finally:
            loop.close()


def test_a_console_stop_after_a_survival_interrupt_sends_no_second_pop(tmp_path) -> None:
    """One goal gets one pop, whichever stop path arrives first.

    The survival interrupt pops the ladder goal; the user pressing Stop before
    the body confirms it must not pop that same goal again, or the body sees two
    stops for one intent (review of slice 3, finding 2). The goal's record still
    says survival asked, not the console.
    """
    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {"stone_sword": 1}}
    queue = [proposal("count(#logs) >= 5", "get wood", "wood")]
    clock = Clock()
    with FakeBody(skills=SKILLS, facts=facts) as body:
        console = Console(
            evidence=Evidence(tmp_path / "evidence"),
            ledger=Ledger(tmp_path / "ledger.jsonl"),
            memory=Memory("w", root=tmp_path / "worlds", clock=clock),
            world_id="w",
            core_dir=str(tmp_path / "core"),
            active_dir=str(tmp_path / "active"),
        )
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: queue.pop(0),
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
            skills=CAT,
            console=console,
            clock=clock,
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the ladder goal")
            wait_accepted(body, body.commands[0].id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="accepted")
            body.facts["is_night"] = True
            loop.session.refresh()
            pump(loop, lambda: body.pops, what="the survival pop")

            # The user presses Stop while the interrupt's pop is unconfirmed.
            console.control.put({"action": "stop_goal", "text": "stop"})
            loop.step(0.02)
            assert len(body.pops) == 1, f"one pop, not {body.pops}"
            applied = evidence_entries(loop, "console_control")
            assert applied[-1]["action"] == "stop_goal"
            assert applied[-1]["result"] == "pop_sent", applied[-1]

            # The receipt closes the record with the cause that stopped it.
            body.settle(body.commands[0].id, outcome="cancelled")
            pump(
                loop,
                lambda: any(
                    entry.get("status") == "interrupted"
                    for entry in evidence_entries(loop, "goal_finished")
                ),
                what="the closed record",
            )
            closed = next(
                entry
                for entry in evidence_entries(loop, "goal_finished")
                if entry.get("status") == "interrupted"
            )
            assert closed["failure_kind"] == "preempted_by_survival", closed
            assert closed["source"] == "survival", closed
        finally:
            loop.close()


def test_a_live_night_goal_in_the_stack_is_never_started_twice() -> None:
    """The body's own fallback (or our own goal) is already the night watch.

    A snapshot taken before or while the body pushed its fallback still shows
    night; without the stack check the brain sends a second ``night_watch`` that
    then plays in front of the body's own one (review of slice 3, finding 3).
    """
    live = [
        {"intent_id": 7, "tier": "survival", "skill": "night_watch", "args": [], "state": "running"}
    ]
    assert survival.choose(NIGHT, SWORD, CAT, stack=live) is None
    assert survival.choose(NIGHT, SWORD, CAT, stack=[{**live[0], "state": "suspended"}]) is None
    # A finished entry is history, not a claim on the night.
    assert survival.choose(NIGHT, SWORD, CAT, stack=[{**live[0], "state": "done"}]) is not None
    # A different live skill does not stand in for the night goal.
    other = [{"intent_id": 8, "tier": "command", "skill": "get_wood", "args": [], "state": "running"}]
    assert survival.choose(NIGHT, SWORD, CAT, stack=other) is not None
    # The same rule keeps a second food goal off a running one.
    hungry = {"food": 3, "health": 20, "is_night": False}
    food_up = [{"intent_id": 9, "tier": "command", "skill": "get_food", "args": [], "state": "running"}]
    assert survival.choose(hungry, {}, CAT, stack=food_up) is None


def test_the_loop_reads_the_snapshot_stack_before_the_night_goal(tmp_path) -> None:
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {"iron_sword": 1}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            loop.session.snapshot["stack"].append(
                {
                    "intent_id": 4,
                    "tier": "survival",
                    "skill": "night_watch",
                    "args": [],
                    "state": "running",
                }
            )
            assert loop._due_survival() is None, "the body already watches the night"
            loop.session.snapshot["stack"].clear()
            due = loop._due_survival()
            assert due is not None and due.skill == "night_watch"
        finally:
            loop.close()


def test_a_night_goal_gets_a_budget_that_outlasts_the_night(tmp_path) -> None:
    """Night is 10 000 ticks; the default goal budget is 6 000.

    Sent with the default, the body settles a working night watch
    ``failed(budget)`` in the middle of the night and the loop picks it again
    before dawn: two failed goals, a failure streak and no ladder work in
    between (review of slice 3, finding 6). Food keeps the default: a food goal
    that never ends is a bug, a night goal that ends at dawn is the design.
    """
    assert survival.NIGHT_BUDGET_TICKS > 10_000, "a whole night fits"
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {"iron_sword": 1}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the night command")
            assert body.commands[0].budget_ticks == survival.NIGHT_BUDGET_TICKS
        finally:
            loop.close()

    hungry = {"health": 20, "food": 4, "is_night": False, "inventory": {}}
    with FakeBody(skills=SKILLS, facts=hungry) as body:
        loop = _loop(tmp_path / "food", body)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the food command")
            assert body.commands[0].budget_ticks == loop.session.command_budget_ticks
        finally:
            loop.close()


def test_a_queued_user_goal_is_picked_before_the_night_goal(tmp_path) -> None:
    """The user's projects come before Praxion's own ideas (architecture §1).

    Survival is exempt from the need gate, not from the user: a queued console
    goal must not wait behind a night goal that only ends at dawn (review of
    slice 3, finding 5).
    """
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            loop._queue_user_goal(
                {"text": "wood", "condition": "count(#logs) >= 3"}
            )
            loop._paused = False
            pump(loop, lambda: len(body.commands) >= 1, what="a command")
            assert body.commands[0].skill == "get_wood", "the user's goal, not the night"
        finally:
            loop.close()


def test_a_queued_user_goal_interrupts_the_loops_own_night_goal(tmp_path) -> None:
    """A running survival goal yields to the user, as the body's fallback does.

    The record says the user preempted it, never that the brain failed at night
    (review of slice 3, finding 5).
    """
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the night command")
            night = body.commands[0]
            assert night.skill == "take_shelter"
            wait_accepted(body, night.id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="accepted")
            loop._queue_user_goal({"text": "wood", "condition": "count(#logs) >= 3"})
            pump(loop, lambda: evidence_entries(loop, "user_interrupt"), what="the user interrupt")
            pump(loop, lambda: body.pops, what="the pop")
            body.settle(night.id, outcome="cancelled")
            pump(loop, lambda: len(body.commands) >= 2, what="the user command")
            assert body.commands[1].skill == "get_wood"
            closed = next(
                entry
                for entry in evidence_entries(loop, "goal_finished")
                if entry.get("command_id") == night.id
            )
            assert closed["failure_kind"] == "preempted_by_user", closed
            assert closed["source"] == "console"
        finally:
            loop.close()


def test_a_recovered_night_goal_keeps_its_survival_flag(tmp_path) -> None:
    """A reconnect adopts the record; a survival goal stays a survival goal.

    Without the flag the next ``_survival_check`` pops the night goal it just
    adopted and sends the very same one again, all night long (review of slice
    3, finding 7).
    """
    clock = Clock()
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body, clock=clock)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the night command")
            wait_accepted(body, body.commands[0].id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="accepted")
            body.disconnect()
            deadline = time.monotonic() + 4.0
            while time.monotonic() < deadline:
                if body.connections >= 2 and loop.state == LoopState.PLAYING:
                    break
                loop.step(0.02)
                clock.now += 0.3
            else:
                raise AssertionError(f"no reconnect recovery; state={loop.state}")
            assert loop._open_goal is not None
            assert loop._open_goal.survival is True, "the adopted night goal is still survival"
            for _ in range(20):
                loop.step(0.02)
            assert not body.pops, "an adopted survival goal is not interrupted again"
            assert not evidence_entries(loop, "survival_interrupt")
        finally:
            loop.close()


def test_a_user_task_is_not_interrupted_by_survival(tmp_path) -> None:
    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            loop._paused = False
            from praxion_brain.curriculum import GoalProposal

            loop._user_goals.append(GoalProposal(text="wood", condition="count(#logs) >= 3", rationale="user"))
            loop._set_wake(__import__("praxion_brain.loop", fromlist=["WakeCause"]).WakeCause.USER)
            pump(loop, lambda: len(body.commands) >= 1, what="the user goal")
            wait_accepted(body, body.commands[0].id)
            body.facts["is_night"] = True
            loop.session.refresh()
            for _ in range(30):
                loop.step(0.02)
            assert not evidence_entries(loop, "survival_interrupt")
            assert not body.pops
        finally:
            loop.close()
