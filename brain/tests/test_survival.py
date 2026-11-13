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
from praxion_brain import survival, speech
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
    set_tick,
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


def test_hungry_at_night_unarmed_or_sheltered_hunts_only_when_critical() -> None:
    """Decision 2026-11-10 "Hungry at night: hunt only when it is critical":
    unarmed or sheltered at night, ``get_food`` (which hunts) waits until the
    food bar is at ``CRITICAL_FOOD_AT`` (3) or below."""
    assert survival.CRITICAL_FOOD_AT == 3
    # Unarmed, food 5: no hunt; the night goal (shelter) comes instead.
    due = survival.choose({**NIGHT, "food": 5}, {}, CAT)
    assert due is not None and due.skill == "take_shelter"
    # Unarmed, food 3: critical, hunt in the dark.
    due = survival.choose({**NIGHT, "food": 3}, {}, CAT)
    assert due is not None and due.skill == "get_food"
    # Armed but sheltered (the fact): no hunt above critical.
    due = survival.choose({**NIGHT, "food": 5, "sheltered": True}, SWORD, CAT)
    assert due is not None and due.skill == "night_watch"
    # Armed, the snapshot has no fact, a live take_shelter counts as sheltered.
    shelter = [{"intent_id": 4, "tier": "survival", "skill": "take_shelter", "args": [], "state": "running"}]
    due = survival.choose({**NIGHT, "food": 5}, SWORD, CAT, stack=shelter)
    assert due is None or due.skill != "get_food"
    assert survival.choose({**NIGHT, "food": 3}, SWORD, CAT, stack=shelter).skill == "get_food"
    # Armed and out in the open: hunger still comes first.
    assert survival.choose({**NIGHT, "food": 5}, SWORD, CAT).skill == "get_food"
    # An explicit unsheltered fact beats a stale stack entry.
    assert survival.choose({**NIGHT, "food": 5, "sheltered": False}, SWORD, CAT, stack=shelter).skill == "get_food"
    # Day: unchanged, a hungry unarmed body gets food.
    due = survival.choose({**NIGHT, "is_night": False, "food": 5}, {}, CAT)
    assert due is not None and due.skill == "get_food"


def test_two_failed_survival_goals_never_block_the_night(tmp_path) -> None:
    """Decision 2026-11-10: run 9's two failed shelters blocked the night goal
    for two hours and the bot went gathering in the dark."""
    from praxion_brain.memory import GoalRecord

    clock = Clock()
    memory = Memory("w", root=tmp_path / "worlds", clock=clock)
    for n, origin in ((1, "survival"), (2, "survival")):
        memory.append_goal(
            GoalRecord(
                goal_id=f"g-{n}",
                text="take shelter",
                condition=survival.NIGHT_CONDITION,
                origin=origin,
                skill="take_shelter",
                status="failed",
                failure_kind="budget",
                t_end=clock(),
            )
        )
    assert memory.is_blocked(survival.NIGHT_CONDITION) is False
    assert memory.failures() == {}
    # A ladder goal with two failures is still blocked: only survival is exempt.
    for n in (3, 4):
        memory.append_goal(
            GoalRecord(
                goal_id=f"g-{n}",
                text="wood",
                condition="count(#logs) >= 5",
                skill="get_wood",
                status="failed",
                failure_kind="budget",
                t_end=clock(),
            )
        )
    assert memory.is_blocked("count(#logs) >= 5") is True


def test_a_failed_survival_skill_backs_off_for_600_ticks() -> None:
    backoff = {"take_shelter": 1_000 + survival.SURVIVAL_BACKOFF_TICKS}
    assert survival.SURVIVAL_BACKOFF_TICKS == 600
    assert survival.choose(NIGHT, {}, CAT, backoff=backoff, tick=1_000) is None
    assert survival.choose(NIGHT, {}, CAT, backoff=backoff, tick=1_599) is None
    due = survival.choose(NIGHT, {}, CAT, backoff=backoff, tick=1_600)
    assert due is not None and due.skill == "take_shelter"
    # The back-off is per skill: an armed body still watches the night.
    assert survival.choose(NIGHT, SWORD, CAT, backoff=backoff, tick=1_000).skill == "night_watch"


def test_the_night_holds_ladder_work_while_unarmed_backing_off_or_watched() -> None:
    assert survival.night_hold(NIGHT, {}) == "unarmed at night"
    assert survival.night_hold(NIGHT, SWORD) is None, "armed: choose names the goal"
    backoff = {"night_watch": 700}
    assert "backing off" in survival.night_hold(NIGHT, SWORD, backoff=backoff, tick=100)
    assert survival.night_hold(NIGHT, SWORD, backoff=backoff, tick=700) is None
    live = [{"intent_id": 3, "tier": "survival", "skill": "night_watch", "args": [], "state": "running"}]
    assert "running" in survival.night_hold(NIGHT, SWORD, stack=live)
    # Day, or a desynchronised world: no hold.
    assert survival.night_hold({**NIGHT, "is_night": False}, {}) is None
    assert survival.night_hold({**NIGHT, "world_valid": False}, {}) is None


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


def test_a_reconnect_clears_the_survival_back_off(tmp_path) -> None:
    """The back-off deadlines are body ticks; a reconnected body may count its
    ticks afresh, so an old deadline must not outlive the connection."""
    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {}}
    clock = Clock()
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body, clock=clock)
        try:
            loop.start()
            loop._survival_backoff["take_shelter"] = 10_000_000
            body.disconnect()
            deadline = time.monotonic() + 4.0
            while time.monotonic() < deadline:
                if body.connections >= 2 and not loop._survival_backoff:
                    break
                loop.step(0.02)
                clock.now += 0.3
            assert body.connections >= 2, "the loop reconnected"
            assert loop._survival_backoff == {}, "the old body's deadlines are gone"
        finally:
            loop.close()


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


def _fail_the_shelter(loop, body) -> None:
    pump(loop, lambda: len(body.commands) >= 1, what="the shelter command")
    shelter = body.commands[0]
    assert shelter.skill == "take_shelter"
    wait_accepted(body, shelter.id)
    body.settle(shelter.id, outcome="failed", failure_kind="unsafe_drop")
    pump(loop, lambda: evidence_entries(loop, "survival_backoff"), what="the back-off")


def test_a_failed_shelter_backs_off_and_no_ladder_goal_starts_at_night(tmp_path) -> None:
    """Run 9: after the shelter failed the bot proposed ladder work at night.

    The failed survival goal is not blocked; its skill waits 600 ticks and the
    pick waits with it (``night_wait``), never calling the proposer.
    """
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {"dirt": 4}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            loop.start()
            _fail_the_shelter(loop, body)
            backoff = evidence_entries(loop, "survival_backoff")[0]
            assert backoff["skill"] == "take_shelter"
            failed_at = int(loop.session.tick or 0)
            assert backoff["until_tick"] - failed_at <= survival.SURVIVAL_BACKOFF_TICKS
            assert loop.memory.is_blocked(survival.NIGHT_CONDITION) is False

            loop._next_pick_tick = 0  # the failure gap is not what is tested
            pump(loop, lambda: evidence_entries(loop, "night_wait"), what="the night wait")
            assert len(body.commands) == 1, "no ladder goal at night"

            # The back-off runs out: the shelter is picked again.
            set_tick(loop, body, backoff["until_tick"] + 1)
            loop._next_pick_tick = 0
            loop._nothing_gap = True
            pump(loop, lambda: len(body.commands) >= 2, what="the second shelter")
            assert body.commands[1].skill == "take_shelter"
        finally:
            loop.close()


def test_a_user_goal_still_goes_first_during_the_night_back_off(tmp_path) -> None:
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {"dirt": 4}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            loop.start()
            _fail_the_shelter(loop, body)
            loop._queue_user_goal({"text": "wood", "condition": "count(#logs) >= 3"})
            loop._next_pick_tick = 0
            pump(loop, lambda: len(body.commands) >= 2, what="the user's goal")
            assert body.commands[1].skill == "get_wood"
        finally:
            loop.close()


# -- critical food interrupts a night goal (decision 2026-11-10 "Hungry at
# night: hunt only when it is critical") ------------------------------------ #


def _open_night_goal(loop, body, skill: str):
    """Start the loop and let its night goal be accepted; return the command."""
    loop.start()
    pump(loop, lambda: len(body.commands) >= 1, what="the night command")
    night = body.commands[0]
    assert night.skill == skill
    wait_accepted(body, night.id)
    pump(
        loop,
        lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None,
        what="accepted",
    )
    return night


@pytest.mark.parametrize(
    ("inventory", "skill"),
    ((({"dirt": 2}), "take_shelter"), (({"stone_sword": 1}), "night_watch")),
)
def test_critical_food_interrupts_an_open_night_goal_once(tmp_path, inventory, skill) -> None:
    """Food at ``CRITICAL_FOOD_AT`` with nothing to eat: ``get_food`` stops the
    open night goal through the survival interrupt path (one pop, the night
    goal closes ``interrupted``, never ``failed``) and then plays."""
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": inventory}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            night = _open_night_goal(loop, body, skill)
            body.facts["food"] = survival.CRITICAL_FOOD_AT
            loop.session.refresh()
            pump(loop, lambda: evidence_entries(loop, "survival_interrupt"), what="the interrupt")
            pump(loop, lambda: body.pops, what="the pop")
            for _ in range(10):
                loop.step(0.02)
            assert len(body.pops) == 1, "one interrupt is one pop"
            assert len(evidence_entries(loop, "survival_interrupt")) == 1
            interrupt = evidence_entries(loop, "survival_interrupt")[0]
            assert interrupt["survival"] == "food" and interrupt["goal_id"]
            body.settle(night.id, outcome="cancelled")
            pump(loop, lambda: len(body.commands) >= 2, what="the food command")
            assert body.commands[1].skill == "get_food"
            assert loop._open_goal.survival is True
            closed = next(
                entry
                for entry in evidence_entries(loop, "goal_finished")
                if entry.get("command_id") == night.id
            )
            assert closed["status"] == "interrupted", "the night goal is not a failure"
            assert closed["failure_kind"] == "preempted_by_survival"
            assert not evidence_entries(loop, "survival_backoff")
        finally:
            loop.close()


@pytest.mark.parametrize(
    ("inventory", "skill"),
    ((({"dirt": 2}), "take_shelter"), (({"stone_sword": 1}), "night_watch")),
)
def test_food_above_critical_never_interrupts_a_night_goal(tmp_path, inventory, skill) -> None:
    """Food 5 is hungry but not critical: the night goal keeps the body, even
    armed and out in the open, where ``choose`` alone would pick ``get_food``."""
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": inventory}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            _open_night_goal(loop, body, skill)
            body.facts["food"] = survival.CRITICAL_FOOD_AT + 2
            loop.session.refresh()
            for _ in range(30):
                loop.step(0.02)
            assert not body.pops
            assert not evidence_entries(loop, "survival_interrupt")
            assert len(body.commands) == 1
        finally:
            loop.close()


def test_critical_food_never_interrupts_a_running_food_goal(tmp_path) -> None:
    """Only a night goal yields to critical food; ``get_food`` itself is never
    interrupted by another survival goal."""
    facts = {"health": 20, "food": 5, "is_night": False, "inventory": {}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            _open_night_goal(loop, body, "get_food")
            body.facts.update({"food": 2, "is_night": True})
            loop.session.refresh()
            for _ in range(30):
                loop.step(0.02)
            assert not body.pops
            assert not evidence_entries(loop, "survival_interrupt")
        finally:
            loop.close()


def test_a_running_night_watch_on_its_shelter_rule_counts_as_sheltered() -> None:
    """The snapshot stack shows each intent's root skill only (the body's
    ``publish_state``), so a ``take_shelter`` that ``night_watch`` runs as its
    child is invisible. ``night_watch`` runs it exactly when its shelter rule
    holds (health 10 or less, or no sword): then the body counts as sheltered."""
    watch = [{"intent_id": 5, "tier": "survival", "skill": "night_watch", "args": [], "state": "running"}]
    hurt = {**NIGHT, "food": 5, "health": 8}
    # Armed but hurt, night_watch live: sheltered, so food 5 does not hunt.
    due = survival.choose(hurt, SWORD, CAT, stack=watch)
    assert due is None or due.skill != "get_food"
    assert "sheltered" not in hurt
    assert survival.choose({**hurt, "food": 3}, SWORD, CAT, stack=watch).skill == "get_food"
    # Armed and healthy, night_watch live: it hunts, not sheltered.
    assert survival.choose({**NIGHT, "food": 5}, SWORD, CAT, stack=watch).skill == "get_food"
    # The fact, when present, still wins.
    assert survival.choose({**hurt, "sheltered": False}, SWORD, CAT, stack=watch).skill == "get_food"


# -- a sealed body is never left sealed by day (third review of run 9) ------- #

DAY_SEALED = {"health": 20, "food": 20, "is_night": False, "sheltered": True, "inventory": {}}


def test_the_port_reads_sheltered_and_conditions_decide_it() -> None:
    from praxion_brain.port import Facts

    assert Facts().sheltered is False, "the body sends it only when true"
    assert Facts.model_validate({"sheltered": True}).sheltered is True
    assert is_verifiable(survival.LEAVE_CONDITION)
    assert evaluate(survival.LEAVE_CONDITION, {}, {"sheltered": True}) is False
    assert evaluate(survival.LEAVE_CONDITION, {}, {"sheltered": False}) is True
    assert evaluate(survival.LEAVE_CONDITION, {}, {}) is None


def test_a_body_sealed_in_by_day_gets_take_shelter_to_leave() -> None:
    due = survival.choose(DAY_SEALED, {}, CAT)
    assert due is not None and due.skill == "take_shelter" and due.kind == "leave"
    assert due.proposal.condition == "not sheltered()"
    # Armed too: night_watch never opens a shelter, take_shelter does.
    assert survival.choose(DAY_SEALED, SWORD, CAT).skill == "take_shelter"
    # Not sealed, or night (the night goal), or an unknown clock: no leave goal.
    assert survival.choose({**DAY_SEALED, "sheltered": False}, {}, CAT) is None
    assert survival.choose({**DAY_SEALED, "is_night": True}, {}, CAT).kind == "night"
    assert survival.choose({**DAY_SEALED, "is_night": None}, {}, CAT) is None
    assert survival.choose({**DAY_SEALED, "world_valid": False}, {}, CAT) is None
    # Hunger comes first: get_food opens the shelter itself (hunt_food).
    assert survival.choose({**DAY_SEALED, "food": 5}, {}, CAT).skill == "get_food"
    # Running already, or backing off after a failed leave: nothing new.
    live = [{"intent_id": 4, "tier": "command", "skill": "take_shelter", "args": [], "state": "running"}]
    assert survival.choose(DAY_SEALED, {}, CAT, stack=live) is None
    backoff = {"take_shelter": 900}
    assert survival.choose(DAY_SEALED, {}, CAT, backoff=backoff, tick=300) is None
    assert survival.choose(DAY_SEALED, {}, CAT, backoff=backoff, tick=900).kind == "leave"


def test_a_body_sealed_in_by_day_holds_ladder_work() -> None:
    assert survival.night_hold(DAY_SEALED, SWORD) == "sealed in by day"
    backoff = {"take_shelter": 900}
    assert "backing off" in survival.night_hold(DAY_SEALED, SWORD, backoff=backoff, tick=300)
    live = [{"intent_id": 4, "tier": "command", "skill": "take_shelter", "args": [], "state": "running"}]
    assert "running" in survival.night_hold(DAY_SEALED, SWORD, stack=live)
    # Open by day: no hold, the ladder plays.
    assert survival.night_hold({**DAY_SEALED, "sheltered": False}, SWORD) is None


def test_the_loop_opens_a_shelter_left_sealed_by_day(tmp_path) -> None:
    """A body that wakes sealed by day (any night goal that ended early) gets
    ``take_shelter`` on ``not sheltered()``, never a ladder goal (the proposer
    of ``_loop`` raises)."""
    with FakeBody(skills=SKILLS, facts=dict(DAY_SEALED)) as body:
        loop = _loop(tmp_path, body)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the leave command")
            leave = body.commands[0]
            assert leave.skill == "take_shelter"
            assert leave.condition == "not sheltered()"
            entry = evidence_entries(loop, "survival_goal")[0]
            assert entry["survival"] == "leave" and entry["reason"] == "sealed in by day"
        finally:
            loop.close()


def test_a_failed_leave_backs_off_and_holds_ladder_work_by_day(tmp_path) -> None:
    """``leave_shelter()`` failing (``no_exit``) fails the goal; the skill backs
    off 600 ticks, the pick waits instead of sending ladder work into a closed
    cell, and then tries to leave again."""
    with FakeBody(skills=SKILLS, facts=dict(DAY_SEALED)) as body:
        loop = _loop(tmp_path, body)
        try:
            loop.start()
            _fail_the_shelter(loop, body)
            backoff = evidence_entries(loop, "survival_backoff")[0]
            loop._next_pick_tick = 0
            pump(loop, lambda: evidence_entries(loop, "night_wait"), what="the day wait")
            assert evidence_entries(loop, "night_wait")[0]["reason"].startswith("sealed in by day")
            assert len(body.commands) == 1, "no ladder goal while sealed in"
            set_tick(loop, body, backoff["until_tick"] + 1)
            loop._next_pick_tick = 0
            loop._nothing_gap = True
            pump(loop, lambda: len(body.commands) >= 2, what="the second leave")
            assert body.commands[1].skill == "take_shelter"
            assert body.commands[1].condition == "not sheltered()"
        finally:
            loop.close()


def test_after_a_user_goal_a_body_sealed_by_day_goes_back_to_take_shelter(tmp_path) -> None:
    """The user's goal stops the night shelter and plays first; it ends after
    dawn with the body still sealed in, so ``take_shelter`` opens the shelter
    before any ladder work."""
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {"dirt": 4}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            night = _open_night_goal(loop, body, "take_shelter")
            body.facts["sheltered"] = True
            loop._queue_user_goal({"text": "wood", "condition": "count(#logs) >= 3"})
            pump(loop, lambda: body.pops, what="the pop")
            body.settle(night.id, outcome="cancelled")
            pump(loop, lambda: len(body.commands) >= 2, what="the user command")
            user = body.commands[1]
            assert user.skill == "get_wood"
            wait_accepted(body, user.id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="accepted")
            # Dawn while the user's goal runs: it is never interrupted.
            body.facts["is_night"] = False
            loop.session.refresh()
            for _ in range(10):
                loop.step(0.02)
            assert len(body.pops) == 1
            body.settle(user.id, outcome="done", condition="true")
            pump(loop, lambda: len(body.commands) >= 3, what="the leave command")
            assert body.commands[2].skill == "take_shelter"
            assert body.commands[2].condition == "not sheltered()"
        finally:
            loop.close()


def test_a_ladder_goal_is_interrupted_when_the_body_is_sealed_by_day(tmp_path) -> None:
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
            body.facts["sheltered"] = True
            loop.session.refresh()
            pump(loop, lambda: evidence_entries(loop, "survival_interrupt"), what="the interrupt")
            assert evidence_entries(loop, "survival_interrupt")[0]["survival"] == "leave"
            pump(loop, lambda: body.pops, what="the pop")
            body.settle(ladder.id, outcome="cancelled")
            pump(loop, lambda: len(body.commands) >= 2, what="the leave command")
            assert body.commands[1].skill == "take_shelter"
        finally:
            loop.close()


def test_a_failed_critical_food_goal_does_not_ping_pong_with_the_night(tmp_path) -> None:
    """Critical food stops the sealed shelter and ``get_food`` plays; it fails.
    Its 600-tick back-off makes the night goal the pick again, and that goal is
    not interrupted again until the back-off ends."""
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {"dirt": 4}}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)
        try:
            night = _open_night_goal(loop, body, "take_shelter")
            body.facts.update({"sheltered": True, "food": survival.CRITICAL_FOOD_AT})
            loop.session.refresh()
            pump(loop, lambda: body.pops, what="the first pop")
            body.settle(night.id, outcome="cancelled")
            pump(loop, lambda: len(body.commands) >= 2, what="the food command")
            food = body.commands[1]
            assert food.skill == "get_food"
            wait_accepted(body, food.id)
            body.settle(food.id, outcome="failed", failure_kind="not_found")
            pump(loop, lambda: evidence_entries(loop, "survival_backoff"), what="the back-off")
            backoff = evidence_entries(loop, "survival_backoff")[0]
            assert backoff["skill"] == "get_food"
            loop._next_pick_tick = 0
            pump(loop, lambda: len(body.commands) >= 3, what="the night again")
            again = body.commands[2]
            assert again.skill == "take_shelter"
            wait_accepted(body, again.id)
            pump(
                loop,
                lambda: loop._open_goal is not None and loop._open_goal.intent_id == body._command_intents.get(again.id),
                what="accepted",
            )
            for _ in range(30):
                loop.step(0.02)
            assert len(body.pops) == 1, "no second interrupt during the back-off"
            assert len(evidence_entries(loop, "survival_interrupt")) == 1
            # The back-off ends: critical food may interrupt again.
            set_tick(loop, body, backoff["until_tick"] + 1)
            loop.session.refresh()
            pump(loop, lambda: len(body.pops) >= 2, what="the second pop")
        finally:
            loop.close()


def test_a_body_that_cannot_get_out_stops_holding_ladder_work() -> None:
    """Fourth review, finding 1: a cover the body cannot mine (a broken pickaxe)
    made every leave fail and held the ladder all day."""
    stuck = {"take_shelter": survival.LEAVE_GIVE_UP}
    assert survival.night_hold(DAY_SEALED, SWORD, failures=stuck) is None
    fewer = {"take_shelter": survival.LEAVE_GIVE_UP - 1}
    assert survival.night_hold(DAY_SEALED, SWORD, failures=fewer) == "sealed in by day"


def test_a_leave_goal_gets_the_night_budget() -> None:
    """Fourth review, finding 3: sealed at dusk, the leave goal waits for sunrise."""
    goal = survival.choose(DAY_SEALED, SWORD, CAT)
    assert goal is not None and goal.kind == "leave"
    assert goal.budget_ticks == survival.NIGHT_BUDGET_TICKS


def test_the_survival_back_off_doubles_and_a_success_resets_it(tmp_path) -> None:
    """Fourth review, finding 2: a failed night hunt must not reopen the shelter
    every 600 ticks all night."""
    from praxion_brain.memory import GoalRecord

    with FakeBody(skills=SKILLS, facts=dict(DAY_SEALED)) as body:
        loop = _loop(tmp_path, body)
        try:
            def finish(status):
                record = GoalRecord(
                    goal_id="g", text="t", condition="false", status=status,
                    skill="get_food", origin="survival", ticks=1,
                )
                loop._back_off_survival(record)
                return loop._survival_backoff.get("get_food")

            now = int(loop.session.tick or 0)
            assert finish("failed") == now + survival.SURVIVAL_BACKOFF_TICKS
            assert finish("failed") == now + 2 * survival.SURVIVAL_BACKOFF_TICKS
            for _ in range(6):
                last = finish("failed")
            assert last == now + survival.SURVIVAL_BACKOFF_MAX_TICKS
            finish("done")
            assert "get_food" not in loop._survival_failures
        finally:
            loop.close()


# -- night work: a pickaxe body mines instead of sheltering (decision
# 2026-11-13 "night mining", run B) ----------------------------------------- #
PICK = {"stone_pickaxe": 1}
#: A pickaxe and a sword: the body is armed *and* able to work, which is the
#: case that used to start ``night_watch`` over a running shelter.
PICK_AND_SWORD = {"stone_pickaxe": 1, "stone_sword": 1}


def test_night_work_is_a_pickaxe_healthy_body_at_night() -> None:
    assert survival.night_work(NIGHT, PICK) is True
    assert survival.night_work(NIGHT, {"wooden_pickaxe": 1}) is True
    assert survival.night_work(NIGHT, {"minecraft:iron_pickaxe": 1}) is True
    # No pickaxe, or none left.
    assert survival.night_work(NIGHT, {}) is False
    assert survival.night_work(NIGHT, SWORD) is False
    assert survival.night_work(NIGHT, {"stone_pickaxe": 0}) is False
    # Day, or a body without a clock.
    assert survival.night_work({**NIGHT, "is_night": False}, PICK) is False
    assert survival.night_work({**NIGHT, "is_night": None}, PICK) is False
    # Hurt at or below the shelter threshold, or no health fact at all.
    assert survival.night_work({**NIGHT, "health": survival.SHELTER_HEALTH}, PICK) is False
    assert survival.night_work({**NIGHT, "health": survival.SHELTER_HEALTH - 1}, PICK) is False
    assert survival.night_work({**NIGHT, "health": survival.SHELTER_HEALTH + 1}, PICK) is True
    assert survival.night_work({**NIGHT, "health": None}, PICK) is False
    # A desynchronised world has stale facts: no decision.
    assert survival.night_work({**NIGHT, "world_valid": False}, PICK) is False
    assert survival.night_work(None, PICK) is False
    # The set is the work that needs no surface light.
    assert {"get_cobblestone", "get_coal", "find_ore", "make_camp", "cook_food"} <= (
        survival.NIGHT_WORK_SKILLS
    )
    assert "get_wood" not in survival.NIGHT_WORK_SKILLS
    assert "find_village" not in survival.NIGHT_WORK_SKILLS
    assert survival.SHELTER_SKILL not in survival.NIGHT_WORK_SKILLS


def test_a_pickaxe_body_works_at_night_instead_of_sheltering() -> None:
    assert survival.choose(NIGHT, PICK, CAT) is None, "night work: the planner keeps planning"
    assert survival.night_hold(NIGHT, PICK) is None, "and nothing holds the pick"
    # Without a pickaxe today's night is unchanged.
    due = survival.choose(NIGHT, SWORD, CAT)
    assert due is not None and due.skill == "night_watch"
    assert survival.night_hold(NIGHT, {}) == "unarmed at night"
    # Hurt: the shelter, exactly as before (the world check comes first).
    hurt = {**NIGHT, "health": survival.SHELTER_HEALTH}
    due = survival.choose(hurt, PICK, CAT)
    assert due is not None and due.skill == "take_shelter"
    # Hunger is unchanged: critical food still starts get_food.
    due = survival.choose({**NIGHT, "food": survival.CRITICAL_FOOD_AT}, PICK, CAT)
    assert due is not None and due.kind == "food"


def test_a_live_shelter_goal_is_not_interrupted_by_night_work() -> None:
    """The body healed (or the pickaxe arrived) mid-night: it is already sealed
    in and its running shelter goal keeps the night."""
    live = [
        {"intent_id": 4, "tier": "survival", "skill": "take_shelter", "args": [], "state": "running"}
    ]
    facts = {**NIGHT, "sheltered": True}
    assert survival.choose(facts, PICK_AND_SWORD, CAT, stack=live) is None
    # Without night work the same snapshot names the armed night watch, which
    # only the pickaxe rule keeps away from the running shelter.
    due = survival.choose(facts, SWORD, CAT, stack=live)
    assert due is not None and due.skill == "night_watch"


def test_the_loop_works_at_night_and_says_it_once(tmp_path) -> None:
    """The pickaxe body plans instead of sheltering; the night line is said once."""
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": dict(PICK)}
    with FakeBody(skills=SKILLS, facts=facts) as body:
        loop = _loop(tmp_path, body)  # its proposer raises: night work must plan
        loop._paused = True
        try:
            loop.start()

            def lines():
                return [
                    entry
                    for entry in evidence_entries(loop, "narrate")
                    if entry.get("topic") == "night_work"
                ]

            pump(loop, lines, what="the night line")
            for _ in range(20):
                loop.step(0.02)
            assert len(lines()) == 1, "once per night"
            assert lines()[0]["text"] == speech.night_work()
            assert not evidence_entries(loop, "survival_goal"), "no shelter goal at night"
        finally:
            loop.close()
