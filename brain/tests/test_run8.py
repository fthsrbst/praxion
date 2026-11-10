"""Run 8 (2026-11-10, seed 606): the loop must never idle deaf, blind or dead.

Decisions 2026-11-10: a pick that ends with nothing to do backs off instead of a
dead IDLE; survival goals start during a skill admission; a deferred write is
not the "last" goal; death is an event; a world desync stops picking.
"""

from __future__ import annotations

import threading

import pytest

from fake_body import FakeBody
from praxion_brain import speech, survival
from praxion_brain.governor import CallTimeout
from praxion_brain.loop import NOTHING_TO_DO_GAP_TICKS, LoopState, WakeCause
from praxion_brain.memory import GoalRecord, Memory, SkillUse
from praxion_brain.port import Facts, PortError
from praxion_brain.records import settled_status
from praxion_brain.router import Route
from test_loop import (
    admission_options,
    catalog,
    evidence_entries,
    furnace_check_fn,
    furnace_write_fn,
    gated_practice,
    make_loop,
    proposal,
    pump,
    set_tick,
    wait_accepted,
)


#: The hello capabilities that let the brain talk (``FakeBody(caps=...)``).
CAPS = ["command", "snapshot", "pop", "say"]


def says(body) -> list[str]:
    return [message.text for message in body.says]


def _failing_loop(body, tmp_path, calls: list[int]):
    """A loop whose every proposal call fails and that knows no code-only skill."""

    def propose_fn(*args, **kwargs):
        calls.append(1)
        raise RuntimeError("provider down")

    return make_loop(
        body,
        tmp_path,
        propose_fn=propose_fn,
        route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
        skills=catalog(),
    )


# --------------------------------------------------------------------------- #
# 1. No code-only goal: back off, never a dead IDLE
# --------------------------------------------------------------------------- #
def test_a_pick_without_a_code_only_goal_backs_off_and_picks_again(tmp_path) -> None:
    calls: list[int] = []
    with FakeBody(skills=set(), facts={"inventory": {}}) as body:
        loop = _failing_loop(body, tmp_path, calls)
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "nothing_to_do"), what="the back-off")
            # The old behaviour (the run 8 bug): code_only_idle and IDLE with no
            # wake and no gap, so nothing ever picked again.
            assert evidence_entries(loop, "code_only_idle")[0]["reason"] == "proposal_error"
            first = evidence_entries(loop, "nothing_to_do")[0]
            assert first["reason"] == "proposal_error"
            assert first["gap_ticks"] == NOTHING_TO_DO_GAP_TICKS
            assert loop.state == LoopState.IDLE and loop._nothing_gap
            assert len(calls) == 1

            set_tick(loop, body, loop._next_pick_tick)
            pump(loop, lambda: len(evidence_entries(loop, "nothing_to_do")) >= 2, what="the second pick")
            assert len(calls) == 2, "one proposal per pick, no recursion"
            second = evidence_entries(loop, "nothing_to_do")[1]
            assert second["gap_ticks"] == 2 * NOTHING_TO_DO_GAP_TICKS, "the gap doubles"
            assert len(evidence_entries(loop, "code_only_idle")) == 2
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# 2. A deferred write does not become the last proposal
# --------------------------------------------------------------------------- #
def test_a_deferred_write_can_be_proposed_again(tmp_path) -> None:
    """Run 8 at 05:02 and 05:25 UTC: the deferred goal was refused same_as_last."""
    started, release = threading.Event(), threading.Event()
    furnace = proposal("has(furnace)", "get a furnace", "furnace")
    coal = proposal("count(coal) >= 4", "get coal", "coal")
    queue = [furnace, coal]

    def propose_fn(*args, **kwargs):
        return queue.pop(0) if queue else proposal("count(coal) >= 4", "get coal", "coal")

    with FakeBody(skills=set(), facts={"inventory": {}}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *a, **k: Route(kind="write"),
            skills=catalog(),
            admission_options=admission_options(
                tmp_path,
                write_fn=furnace_write_fn,
                check_fn=furnace_check_fn,
                practice_fn=gated_practice(started, release),
            ),
            catalog_loader=lambda: catalog(),
        )
        try:
            loop.start()
            pump(loop, lambda: loop._job is not None, what="admission job")
            pump(loop, lambda: evidence_entries(loop, "write_deferred"), what="the first deferral")
            assert loop._last_proposal is furnace, "the deferred coal goal is forgotten"
            assert tuple(loop._recent_items) == ("furnace",)

            set_tick(loop, body, loop._next_pick_tick)
            pump(
                loop,
                lambda: len(evidence_entries(loop, "write_deferred")) >= 2,
                what="the second deferral",
            )
            refused = [
                entry
                for entry in evidence_entries(loop, "proposal_result")
                if entry["validation"]["reason"] == "same_as_last"
            ]
            assert not refused, refused
            assert [e["condition"] for e in evidence_entries(loop, "write_deferred")][:2] == [
                "count(coal) >= 4",
                "count(coal) >= 4",
            ]
        finally:
            release.set()
            loop.close()


def test_forget_undoes_only_the_matching_remember(tmp_path) -> None:
    with FakeBody(skills=set()) as body:
        loop = _failing_loop(body, tmp_path, [])
        a = proposal("count(oak_log) >= 2", "logs", "log")
        b = proposal("count(cobblestone) >= 3", "stone", "stone")
        loop._remember_proposal(a)
        loop._remember_proposal(b)
        loop._forget_proposal(a)  # not the newest: nothing changes
        assert loop._last_proposal is b
        assert tuple(loop._recent_items) == ("cobblestone", "oak_log")
        loop._forget_proposal(b)
        assert loop._last_proposal is a
        assert tuple(loop._recent_items) == ("oak_log",)
        loop._forget_proposal(b)  # twice is harmless
        assert loop._last_proposal is a


# --------------------------------------------------------------------------- #
# 3. Survival during a skill admission
# --------------------------------------------------------------------------- #
def test_night_starts_while_a_skill_is_written_and_the_written_goal_waits(tmp_path) -> None:
    started, release = threading.Event(), threading.Event()
    skills = {"get_food", "night_watch", "take_shelter", "get_furnace"}
    with FakeBody(skills=skills, facts={"health": 20, "food": 20, "is_night": False, "inventory": {}}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: proposal("has(furnace)", "get a furnace", "furnace"),
            route_fn=lambda *a, **k: Route(kind="write"),
            # No code-only skill for the first rung: the excluded re-proposals
            # end the pick as nothing_to_do.
            skills=catalog("get_food", "night_watch", "take_shelter"),
            admission_options=admission_options(
                tmp_path,
                write_fn=furnace_write_fn,
                check_fn=furnace_check_fn,
                practice_fn=gated_practice(started, release),
            ),
            catalog_loader=lambda: catalog("get_food", "night_watch", "take_shelter", "get_furnace"),
        )
        try:
            loop.start()
            pump(loop, lambda: loop._job is not None, what="admission job")
            assert started.wait(4.0), "the admission job never started"
            pump(loop, lambda: evidence_entries(loop, "nothing_to_do"), what="the waiting pick")
            assert loop.state == LoopState.IDLE and loop._wake is None
            assert loop._next_pick_tick > int(loop.session.tick or 0)

            # Night falls in the snapshot the loop holds, without a heartbeat
            # diff to wake it: only the survival check can (the run 8 guard
            # `self._job is None` kept it asleep).
            body.facts["is_night"] = True
            loop.session.snapshot["facts"]["is_night"] = True
            pump(loop, lambda: len(body.commands) >= 1, what="the night command")
            command = body.commands[0]
            assert command.skill == "take_shelter", "unarmed at night"
            assert loop._job is not None and not loop._job.finished, "the job keeps running"

            release.set()
            pump(loop, lambda: loop._written is not None, what="the admitted skill")
            assert len(body.commands) == 1, "the written goal waits for the survival goal"
            assert loop._open_goal is not None and loop._open_goal.survival

            wait_accepted(body, command.id)
            body.facts["is_night"] = False
            body.settle(command.id, outcome="done", condition="true")
            pump(loop, lambda: len(body.commands) >= 2, what="the written goal")
            assert body.commands[1].skill == "get_furnace"
            assert loop._written is None
        finally:
            release.set()
            loop.close()


# --------------------------------------------------------------------------- #
# 4. Death and world desync from the body
# --------------------------------------------------------------------------- #
def test_death_is_recorded_said_and_wakes_a_waiting_loop(tmp_path) -> None:
    calls: list[int] = []
    with FakeBody(skills=set(), caps=CAPS, facts={"inventory": {}}) as body:
        loop = _failing_loop(body, tmp_path, calls)
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "nothing_to_do"), what="the back-off")
            assert len(calls) == 1
            body.emit_event("wake_brain", cause="died", detail="Praxion was blown up by Creeper")
            pump(loop, lambda: len(calls) >= 2, what="a pick with fresh facts")
            died = evidence_entries(loop, "died")
            assert len(died) == 1 and died[0]["detail"] == "Praxion was blown up by Creeper"
            assert any(e["cause"] == WakeCause.DIED.value for e in evidence_entries(loop, "loop_wake"))
            pump(loop, lambda: "öldüm: Praxion was blown up by Creeper" in says(body), what="the death line")
        finally:
            loop.close()


def test_a_command_failed_by_death_is_said_in_turkish() -> None:
    assert speech.goal_failed("get coal", "died", 40) == "olmadı: get coal (öldüm), 2 sn"
    assert speech.died(None) == "öldüm: bilinmiyor"
    assert speech.died("  ") == "öldüm: bilinmiyor"


def test_a_world_desync_stops_picking_and_is_said_once(tmp_path) -> None:
    calls: list[int] = []
    with FakeBody(skills=set(), caps=CAPS, facts={"inventory": {}}) as body:
        loop = _failing_loop(body, tmp_path, calls)
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "nothing_to_do"), what="the back-off")
            reason = "explode left 1 of 83 bytes unread"
            body.emit_event("wake_brain", cause="world_desync", detail=reason)
            body.emit_event("wake_brain", cause="world_desync", detail=reason)
            pump(loop, lambda: speech.world_desync() in says(body), what="the desync line")
            desync = evidence_entries(loop, "world_desync")
            assert len(desync) == 1 and desync[0]["reason"] == reason

            # The back-off ends, and so would a heartbeat: no new pick.
            set_tick(loop, body, loop._next_pick_tick + 1)
            loop._set_wake(WakeCause.HEARTBEAT)
            for _ in range(20):
                loop.step(0.01)
            assert len(calls) == 1, "no proposal on stale facts"
            assert loop.state == LoopState.IDLE
            assert says(body).count(speech.world_desync()) == 1
        finally:
            loop.close()


def test_a_snapshot_with_an_invalid_world_picks_nothing(tmp_path) -> None:
    calls: list[int] = []
    facts = {"inventory": {}, "is_night": True, "health": 20, "food": 2, "world_valid": False}
    with FakeBody(skills={"get_food", "take_shelter"}, facts=facts) as body:
        loop = _failing_loop(body, tmp_path, calls)
        loop.catalog = catalog("get_food", "take_shelter")
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "world_desync"), what="the desync")
            for _ in range(20):
                loop.step(0.01)
            assert calls == [] and body.commands == [], "neither a proposal nor a survival goal"
            assert evidence_entries(loop, "world_desync")[0]["reason"] == "unknown"
        finally:
            loop.close()


def test_survival_decides_nothing_on_an_invalid_world() -> None:
    cat = catalog("get_food", "night_watch", "take_shelter")
    night = {"health": 20, "food": 2, "is_night": True}
    assert survival.choose(night, {}, cat) is not None
    assert survival.choose({**night, "world_valid": True}, {}, cat) is not None
    assert survival.choose({**night, "world_valid": False}, {}, cat) is None


def test_facts_world_valid_defaults_true_and_reads_false() -> None:
    assert Facts().world_valid is True
    assert Facts.model_validate({"world_valid": False}).world_valid is False


# --------------------------------------------------------------------------- #
# 5. Review of the run 8 fixes: a death is not the goal's failure
# --------------------------------------------------------------------------- #
def test_settled_status_records_death_and_desync_as_interrupted() -> None:
    assert settled_status("failed", None, False, "died") == ("interrupted", "died")
    assert settled_status("failed", None, False, "world_desync") == ("interrupted", "world_desync")
    assert settled_status("failed", None, False, "not_found") == ("failed", "not_found")


def test_failure_memory_skips_old_died_records(tmp_path) -> None:
    """A history written before the rule holds ``failed(died)`` records."""
    memory = Memory("w", root=tmp_path, clock=lambda: 300.0)
    for goal_id, at, kind in (("g1", 100, "died"), ("g2", 200, "died"), ("g3", 250, "world_desync")):
        memory.append_goal(
            GoalRecord(
                goal_id=goal_id,
                text="watch the night",
                condition=survival.NIGHT_CONDITION,
                status="failed",
                failure_kind=kind,
                skill="night_watch",
                t_start=at - 10,
                t_end=at,
            )
        )
        memory.append_skill_use(
            SkillUse(skill="night_watch", goal_id=goal_id, outcome="failed", failure_kind=kind)
        )
    assert memory.failures() == {}
    assert memory.is_blocked(survival.NIGHT_CONDITION) is False
    assert memory.failure_counts("night_watch") == {}


def test_two_deaths_in_night_watch_block_nothing_and_say_it_once(tmp_path) -> None:
    calls: list[int] = []
    facts = {"health": 20, "food": 20, "is_night": True, "inventory": {"stone_sword": 1}}
    skills = {"get_food", "night_watch", "take_shelter"}
    with FakeBody(skills=skills, caps=CAPS, facts=facts) as body:
        loop = _failing_loop(body, tmp_path, calls)
        loop.catalog = catalog("get_food", "night_watch", "take_shelter")
        loop._consecutive_failures = 1
        try:
            loop.start()
            for n in (1, 2):
                pump(loop, lambda n=n: len(body.commands) >= n, what=f"night command {n}")
                command = body.commands[n - 1]
                assert command.skill == "night_watch", "the night is never skipped"
                wait_accepted(body, command.id)
                body.emit_event("wake_brain", cause="died", detail="Praxion was slain by Zombie")
                body.settle(command.id, outcome="failed", failure_kind="died")
            # The third night command is the proof: the condition is not blocked.
            pump(loop, lambda: len(body.commands) >= 3, what="night command 3")
            assert body.commands[2].skill == "night_watch"

            finished = [e for e in evidence_entries(loop, "goal_finished") if e["failure_kind"] == "died"]
            assert [e["status"] for e in finished] == ["interrupted", "interrupted"]
            assert loop.memory.is_blocked(survival.NIGHT_CONDITION) is False
            chosen = survival.choose(facts, {"stone_sword": 1}, loop.catalog)
            assert chosen is not None and chosen.skill == "night_watch"
            assert loop._consecutive_failures == 1, "the failure streak is not advanced"
            assert not evidence_entries(loop, "pick_gap"), "no failure gap"
            assert loop.memory.failure_counts("night_watch") == {}

            pump(loop, lambda: "öldüm: Praxion was slain by Zombie" in says(body), what="the death line")
            assert not [line for line in says(body) if line.startswith("olmadı")], says(body)
        finally:
            loop.close()


@pytest.mark.parametrize("kind", ["died", "world_desync"])
def test_a_goal_ended_by_death_or_desync_is_not_narrated_as_failed(tmp_path, kind) -> None:
    """Even a ``failed`` record of these kinds (an old history): one line, not two."""
    with FakeBody(skills=set()) as body:
        loop = _failing_loop(body, tmp_path, [])
        said: list[tuple[str, str]] = []
        loop._say = lambda topic, text, **kwargs: said.append((topic, text))
        for status in ("failed", "interrupted"):
            loop._narrate_finished(
                GoalRecord(goal_id="g", text="get coal", condition="count(coal) >= 4", status=status, failure_kind=kind)
            )
        assert said == []
        loop._narrate_finished(
            GoalRecord(goal_id="g", text="get coal", condition="count(coal) >= 4", status="failed", failure_kind="not_found")
        )
        assert [topic for topic, _ in said] == ["failed"]


# --------------------------------------------------------------------------- #
# 6. Review of the run 8 fixes: route and play failures back off
# --------------------------------------------------------------------------- #
ROUTE_CASES = ["route_timeout", "route_error", "route_no_skill", "no_snapshot", "send_failed"]


@pytest.mark.parametrize("case", ROUTE_CASES)
def test_a_route_or_play_failure_backs_off_and_picks_again(tmp_path, monkeypatch, case) -> None:
    calls: list[int] = []
    holder: dict = {}

    def propose_fn(*args, **kwargs):
        calls.append(1)
        return proposal("count(oak_log) >= 2", "get logs", "log")

    def route_fn(*args, **kwargs):
        loop = holder["loop"]
        if case == "route_timeout":
            raise CallTimeout("route", 1.0)
        if case == "route_error":
            raise RuntimeError("router down")
        if case == "route_no_skill":
            loop._pending.validation = loop._pending.validation.model_copy(update={"skill_name": None})
            return Route(kind="play", skill=None)
        if case == "no_snapshot":
            monkeypatch.setattr(type(loop.session), "generation", property(lambda self: None))
        if case == "send_failed":
            def refuse(*a, **k):
                raise PortError("socket closed")

            monkeypatch.setattr(loop, "_send_goal", refuse)
        return Route(kind="play", skill="get_wood")

    with FakeBody(skills={"get_wood"}, facts={"inventory": {}}) as body:
        loop = make_loop(body, tmp_path, propose_fn=propose_fn, route_fn=route_fn, skills=catalog("get_wood"))
        holder["loop"] = loop
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "pick_backoff"), what="the back-off")
            monkeypatch.undo()
            own = {
                "route_timeout": "route_timeout",
                "route_error": "route_error",
                "route_no_skill": "route_no_skill",
                "no_snapshot": "play_no_snapshot",
                "send_failed": "command_send_failed",
            }[case]
            assert evidence_entries(loop, own), f"the {own} line is kept"
            backoff = evidence_entries(loop, "pick_backoff")[0]
            assert backoff["reason"] == case
            assert backoff["gap_ticks"] == NOTHING_TO_DO_GAP_TICKS
            assert loop.state == LoopState.IDLE and loop._nothing_gap
            assert loop._next_pick_tick > int(loop.session.tick or 0)
            assert body.commands == [] and len(calls) == 1
            # Nothing was played, so the proposal is not the "last" goal.
            assert loop._last_proposal is None, "an unplayed proposal is forgotten"

            set_tick(loop, body, loop._next_pick_tick)
            pump(loop, lambda: len(calls) >= 2, what="the next pick")
            refused = [
                entry for entry in evidence_entries(loop, "proposal_invalid")
                if entry.get("reason") == "same_as_last"
            ]
            assert refused == [], "the re-proposal is not refused same_as_last"
        finally:
            loop.close()


@pytest.mark.parametrize("case", ["play_without_goal", "no_skill"])
def test_a_play_without_a_goal_or_skill_backs_off(tmp_path, case) -> None:
    holder: dict = {}

    def route_fn(*args, **kwargs):
        holder.setdefault("pending", holder["loop"]._pending)
        return Route(kind="play", skill="get_wood")

    with FakeBody(skills={"get_wood"}, facts={"inventory": {}}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: proposal("count(oak_log) >= 2", "get logs", "log"),
            route_fn=route_fn,
            skills=catalog("get_wood"),
        )
        holder["loop"] = loop
        try:
            loop.start()
            pump(loop, lambda: "pending" in holder, what="a routed goal")
            pending = holder["pending"]
            if case == "play_without_goal":
                loop._pending = None
            else:
                pending.forced_skill = None
                pending.validation = pending.validation.model_copy(update={"skill_name": None})
                loop._pending = pending
            loop._play()
            backoff = evidence_entries(loop, "pick_backoff")[-1]
            assert backoff["reason"] == case
            assert loop.state == LoopState.IDLE and loop._nothing_gap
            assert loop._next_pick_tick > int(loop.session.tick or 0)
            if case == "no_skill":
                assert evidence_entries(loop, "play_no_skill")
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# 7. Review of the run 8 fixes: a deferred write backs off, survival does not
# --------------------------------------------------------------------------- #
def test_a_deferred_write_backs_off_doubling_and_night_still_starts(tmp_path) -> None:
    started, release = threading.Event(), threading.Event()
    furnace = proposal("has(furnace)", "get a furnace", "furnace")
    queue = [furnace]
    calls: list[int] = []

    def propose_fn(*args, **kwargs):
        calls.append(1)
        return queue.pop(0) if queue else proposal("count(coal) >= 4", "get coal", "coal")

    facts = {"health": 20, "food": 20, "is_night": False, "inventory": {}}
    with FakeBody(skills={"night_watch", "take_shelter"}, facts=facts) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *a, **k: Route(kind="write"),
            skills=catalog("night_watch", "take_shelter"),
            admission_options=admission_options(
                tmp_path,
                write_fn=furnace_write_fn,
                check_fn=furnace_check_fn,
                practice_fn=gated_practice(started, release),
            ),
            catalog_loader=lambda: catalog("night_watch", "take_shelter", "get_furnace"),
        )
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "write_deferred"), what="the first deferral")
            first = evidence_entries(loop, "pick_backoff")[0]
            assert first["reason"] == "write_deferred"
            assert first["gap_ticks"] == NOTHING_TO_DO_GAP_TICKS
            assert loop._nothing_gap and loop._wake is None
            proposals = len(calls)

            # The gap holds: no paid re-proposal before it runs out.
            for _ in range(20):
                loop.step(0.01)
            assert len(calls) == proposals

            set_tick(loop, body, loop._next_pick_tick)
            pump(loop, lambda: len(evidence_entries(loop, "write_deferred")) >= 2, what="the second deferral")
            second = evidence_entries(loop, "pick_backoff")[1]
            assert second["gap_ticks"] == 2 * NOTHING_TO_DO_GAP_TICKS, "the gap doubles"
            assert loop._next_pick_tick > int(loop.session.tick or 0)

            # Night inside the gap: survival does not wait for it.
            loop.session.snapshot["facts"]["is_night"] = True
            body.facts["is_night"] = True
            pump(loop, lambda: len(body.commands) >= 1, what="the night command")
            assert body.commands[0].skill == "take_shelter"
        finally:
            release.set()
            loop.close()


def test_a_dropped_notice_is_logged_and_never_fails_the_goal(tmp_path) -> None:
    """Run 9: the body's queue overflowed while a goal played; the session read a
    `tick` the `dropped` message does not have, the loop raised, and a goal the
    body had finished was recorded failed(loop_error)."""
    with FakeBody(skills={"get_wood"}, facts={"inventory": {}}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: proposal("count(oak_log) >= 2", "get logs", "log"),
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
            skills=catalog("get_wood"),
        )
        try:
            loop.start()
            pump(loop, lambda: body.commands, what="the command")
            body.dropped(3)
            pump(loop, lambda: evidence_entries(loop, "loop_dropped"), what="the dropped line")
            assert evidence_entries(loop, "loop_dropped")[0]["n"] == 3
            assert not evidence_entries(loop, "loop_error")
        finally:
            loop.close()


def _played_goal(loop, body):
    loop.start()
    pump(loop, lambda: body.commands, what="the command")
    command = body.commands[0]
    wait_accepted(body, command.id)
    pump(
        loop,
        lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None,
        what="accepted",
    )
    return command


def test_a_settled_receipt_lost_to_a_real_overflow_does_not_hang_the_goal(tmp_path) -> None:
    """Run 9: a `dropped` notice swallowed a goal's settled receipt and the goal
    stayed open. The snapshot after `dropped(n)` closes it from the stack."""
    with FakeBody(skills={"get_wood"}, facts={"inventory": {}}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: proposal("count(oak_log) >= 2", "get logs", "log"),
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
            skills=catalog("get_wood"),
        )
        try:
            command = _played_goal(loop, body)
            goal_id = loop._open_goal.record.goal_id
            body.facts["inventory"] = {"oak_log": 2}
            body.drop_next_events(1)
            body.settle(command.id, outcome="done", condition="true")
            body.dropped(1)
            pump(
                loop,
                lambda: any(e["goal_id"] == goal_id for e in evidence_entries(loop, "goal_finished")),
                what="the goal closed",
            )
            finished = [e for e in evidence_entries(loop, "goal_finished") if e["goal_id"] == goal_id]
            assert finished[0]["status"] == "done", finished
            lost = evidence_entries(loop, "receipt_lost")
            assert lost and lost[0]["goal_id"] == goal_id
        finally:
            loop.close()


def test_a_dropped_notice_leaves_a_live_goal_open(tmp_path) -> None:
    with FakeBody(skills={"get_wood"}, facts={"inventory": {}}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=lambda *a, **k: proposal("count(oak_log) >= 2", "get logs", "log"),
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
            skills=catalog("get_wood"),
        )
        try:
            command = _played_goal(loop, body)
            opened = loop._open_goal
            body.dropped(2)
            pump(loop, lambda: evidence_entries(loop, "loop_dropped"), what="the dropped line")
            for _ in range(10):
                loop.step(0.02)
            assert loop._open_goal is opened, "the intent is still running"
            assert not evidence_entries(loop, "receipt_lost")
            body.settle(command.id, outcome="done", condition="true")
        finally:
            loop.close()
