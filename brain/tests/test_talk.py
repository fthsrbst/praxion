"""The talking bot in the loop: chat in, narration out, preempt and resume.

Decision 2026-11-09 "Five next areas" (talk slice 1). The body filters chat by
operator UUID and parses control verbs; the brain gets `chat_received` events
and speaks through the `say` capability.
"""

from __future__ import annotations

import pytest

from fake_body import FakeBody
from praxion_brain.commands import CommandPlan
from praxion_brain.loop import LoopState
from praxion_brain.router import Route
from test_loop import (
    Clock,
    catalog,
    evidence_entries,
    evidence_kinds,
    make_loop,
    proposal,
    pump,
    wait_accepted,
)

CAPS = ["command", "snapshot", "pop", "say"]
SKILLS = {"get_wood", "get_cobblestone"}


def run_loop(tmp_path, body, *, goals, **kwargs):
    """A loop whose autonomous proposer hands out ``goals`` once, in order."""
    queue = list(goals)

    def propose_fn(*args, **kw):
        return queue.pop(0) if queue else proposal("count(dirt) >= 99", "idle dirt")

    kwargs.setdefault("map_fn", lambda text, **kw: None)  # never the network
    return make_loop(
        body,
        tmp_path,
        propose_fn=propose_fn,
        route_fn=lambda goal, *a, **k: Route(kind="play", skill="get_wood"),
        skills=catalog("get_wood", "get_cobblestone"),
        **kwargs,
    )


def chat(body, text, control=None):
    body.emit_event("chat_received", sender="x", text=text, control=control)


def says(body):
    return [message.text for message in body.says]


def test_a_task_from_chat_preempts_the_goal_and_the_old_goal_resumes(tmp_path) -> None:
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = run_loop(
            tmp_path, body, goals=[proposal("count(#logs) >= 3", "get wood", "wood")]
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the autonomous goal")
            first = body.commands[0]
            wait_accepted(body, first.id)
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None, what="accepted")
            assert loop.state == LoopState.PLAYING

            chat(body, "8 taş getir")
            pump(loop, lambda: len(body.commands) >= 2, what="the preempting command")
            second = body.commands[1]
            assert second.skill == "get_cobblestone"
            assert second.condition == "count(cobblestone) >= 8"
            assert evidence_entries(loop, "goal_preempted")
            assert [g.command_id for g in loop._suspended] == [first.id]
            assert any(entry.state == "suspended" for entry in body.stack), "the body suspended it"
            pump(loop, lambda: any(text.startswith("tamam:") for text in says(body)), what="an acknowledgement")

            # The task settles; the body resumes the old goal and so does the loop.
            wait_accepted(body, second.id)
            body.settle(second.id, outcome="done", condition="true")
            pump(loop, lambda: loop._open_goal is not None and loop._open_goal.command_id == first.id, what="resume")
            assert evidence_entries(loop, "goal_resumed")[0]["restart"] is True
            assert not loop._suspended
            assert loop.state == LoopState.PLAYING, "the loop keeps playing for the resumed goal"

            body.settle(first.id, outcome="done", condition="true")
            pump(loop, lambda: loop._open_goal is None and loop.state != LoopState.PLAYING, what="both done")
            statuses = {
                entry["goal_id"]: entry["status"] for entry in evidence_entries(loop, "goal_finished")
            }
            assert sorted(statuses.values()) == ["done", "done"], statuses
        finally:
            loop.close()


def test_a_stop_word_pauses_picking_and_clears_the_queue(tmp_path) -> None:
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = run_loop(tmp_path, body, goals=[])
        try:
            loop.start()
            loop._enqueue_user(proposal("count(cobblestone) >= 8", "queued"), repeatable=False)
            chat(body, "dur", control="stop")
            pump(loop, lambda: loop._paused, what="paused")
            assert not loop._user_goals
            pump(loop, lambda: "durdum; devam demeni bekliyorum" in says(body), what="the pause line")
            chat(body, "devam", control="resume")
            pump(loop, lambda: not loop._paused, what="resumed")
        finally:
            loop.close()


def test_a_stop_never_resumes_or_narrates_a_suspended_goal(tmp_path) -> None:
    """`dur` pops the preempted goals too, so there is nothing to resume.

    Before the fix the loop saw the running goal's `cancelled` receipt, found
    its suspended list non-empty and said "devam ediyorum" for a goal the body
    had already dropped, then went back to PLAYING for it (review finding 2).
    """
    clock = Clock()
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = run_loop(
            tmp_path, body, goals=[proposal("count(#logs) >= 3", "get wood", "wood")], clock=clock
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the autonomous goal")
            first = body.commands[0]
            wait_accepted(body, first.id)
            pump(
                loop,
                lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None,
                what="the autonomous goal accepted",
            )

            chat(body, "8 taş getir")
            pump(loop, lambda: len(body.commands) >= 2, what="the preempting command")
            second = body.commands[1]
            wait_accepted(body, second.id)
            pump(loop, lambda: [g.command_id for g in loop._suspended] == [first.id], what="suspended")

            chat(body, "dur", control="stop")
            pump(loop, lambda: loop._paused, what="paused")
            # The body's own stop: the running command is cancelled and the
            # suspended one is popped, in that order (driver.rs act_on_control).
            body.settle(second.id, outcome="cancelled")
            pump(loop, lambda: loop._open_goal is None, what="the running goal closed")
            body.settle(first.id, outcome="popped")
            pump(
                loop,
                lambda: len(evidence_entries(loop, "goal_finished")) >= 2,
                what="both goals closed",
            )
            # Let every queued line have its slot.
            for _ in range(20):
                clock.now += 10
                loop.step(0.01)

            assert not evidence_entries(loop, "goal_resumed"), "a popped goal is not resumed"
            assert not any("devam ediyorum" in text for text in says(body)), says(body)
            assert loop.state == LoopState.IDLE
            assert not loop._suspended
            statuses = {
                entry["goal_id"]: (entry["status"], entry["failure_kind"])
                for entry in evidence_entries(loop, "goal_finished")
            }
            assert set(statuses.values()) == {("interrupted", "stopped_by_user")}, statuses
        finally:
            loop.close()


def test_a_repeated_task_is_played_again_not_refused_as_same_as_last(tmp_path) -> None:
    """The operator may repeat a request; the raw `same_as_last` key never leaks."""
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = run_loop(tmp_path, body, goals=[])
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            chat(body, "8 taş getir")
            pump(loop, lambda: loop._user_goals, what="the first task")
            chat(body, "8 taş getir")
            pump(loop, lambda: len(loop._user_goals) == 2, what="the repeated task")

            chat(body, "devam", control="resume")
            pump(loop, lambda: len(body.commands) >= 1, what="the first command")
            first = body.commands[0]
            wait_accepted(body, first.id)
            body.settle(first.id, outcome="done", condition="true")
            pump(loop, lambda: len(body.commands) >= 2, what="the repeated command")
            second = body.commands[1]
            assert second.condition == "count(cobblestone) >= 8"
            assert not any("same_as_last" in text for text in says(body)), says(body)
            assert not any("bunu yapmıyorum" in text for text in says(body)), says(body)
            assert loop._last_proposal is not None
            assert loop._last_proposal.condition == "count(cobblestone) >= 8"
        finally:
            loop.close()


def test_a_slow_map_call_is_bounded_by_its_deadline(tmp_path) -> None:
    """A model that hangs cannot freeze the loop past the class deadline."""
    import time as real_time

    def slow_map(text, **kwargs):
        real_time.sleep(1.0)
        return None

    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = run_loop(
            tmp_path,
            body,
            goals=[],
            map_fn=slow_map,
            governor_kwargs={"call_timeouts": {"map": 0.05}},
        )
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            started = real_time.monotonic()
            assert loop._handle_user_text("ev yap", source="console") == "unsupported"
            elapsed = real_time.monotonic() - started
            assert elapsed < 0.9, f"the loop waited {elapsed:.2f}s on the map call"
            assert evidence_entries(loop, "command_map_error"), "the timeout is recorded"
        finally:
            loop.close()


def test_free_text_goes_to_the_model_once_and_is_validated(tmp_path) -> None:
    calls = []

    def map_fn(text, **kwargs):
        calls.append(text)
        return CommandPlan(text="iki fırın", condition="count(furnace) >= 2", skill="get_wood")

    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = run_loop(tmp_path, body, goals=[], map_fn=map_fn)
        loop._paused = True
        try:
            loop.start()
            chat(body, "bana iki tane fırın lazım")
            pump(loop, lambda: calls, what="the mapper")
            pump(loop, lambda: loop._user_goals, what="the queued task")
            assert calls == ["bana iki tane fırın lazım"]
            assert loop._user_goals[0].condition == "count(furnace) >= 2"
            resolved = evidence_entries(loop, "command_resolved")[0]
            assert resolved["via"] == "model" and resolved["resolution"] == "goal"
        finally:
            loop.close()


def test_what_cannot_be_done_is_answered_in_turkish_and_not_forwarded(tmp_path) -> None:
    clock = Clock()
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {"cobblestone": 9}}) as body:
        loop = run_loop(tmp_path, body, goals=[], clock=clock)
        loop._paused = True
        try:
            loop.start()
            chat(body, "8 taş getir")
            pump(loop, lambda: any(t.startswith("bunu yapmıyorum: zaten var") for t in says(body)), what="refusal")
            clock.now += 3  # past the urgent quota's 2 s gap
            chat(body, "üç elmas bul")
            pump(loop, lambda: any("riskli" in t for t in says(body)), what="risk refusal")
            clock.now += 3
            chat(body, "ev yap")
            pump(loop, lambda: any(t.startswith("anlayamadım") for t in says(body)), what="cannot understand")
            assert not loop._user_goals and not body.commands
        finally:
            loop.close()


def test_the_console_task_takes_the_same_path_as_chat(tmp_path) -> None:
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = run_loop(tmp_path, body, goals=[])
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            assert loop._handle_user_text("get cobblestone 8", source="console") == "queued"
            assert loop._user_goals[0].condition == "count(cobblestone) >= 8"
            assert loop._last_reply["text"].startswith("tamam:")
            assert loop._handle_user_text("durum", source="console") == "status"
            assert loop._last_reply["text"].startswith("durum:")
            received = evidence_entries(loop, "command_received")
            assert received[0]["source"] == "console"
        finally:
            loop.close()


def test_a_line_is_closed_by_the_bodys_receipt(tmp_path) -> None:
    for reply, closing in (("sent", "narrate_sent"), ("dropped", "narrate_dropped")):
        with FakeBody(skills=SKILLS, caps=CAPS, chat_reply=reply, facts={"inventory": {}}) as body:
            loop = run_loop(tmp_path / reply, body, goals=[])
            loop._paused = True
            try:
                loop.start()
                pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
                loop._reply("merhaba")
                pump(loop, lambda: closing in evidence_kinds(loop), what=closing)
                assert evidence_entries(loop, "narrate")[0]["status"] == "send_pending"
                assert not loop.narrator.open_lines
            finally:
                loop.close()


def test_a_body_without_the_say_capability_keeps_the_console_working(tmp_path) -> None:
    with FakeBody(skills=SKILLS, facts={"inventory": {}}) as body:  # old caps
        loop = run_loop(tmp_path, body, goals=[])
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            loop._reply("merhaba")
            assert body.says == []
            assert "narration_unavailable" in evidence_kinds(loop)
            assert loop._last_reply["text"] == "merhaba"
        finally:
            loop.close()
