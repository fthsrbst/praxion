"""The plan queue in the loop (decision 2026-11-12 "Demo night").

One planner call returns a short plan; the loop plays it goal by goal, validating
each goal when its turn comes, and drops it when the world it was made for is
gone. Every model call here is a fake.
"""

from __future__ import annotations

import types

import pytest

from fake_body import FakeBody
from praxion_brain import commands, speech
from praxion_brain.console.hub import Console
from praxion_brain.curriculum import GoalProposal, Validation
from praxion_brain.evidence import Evidence
from praxion_brain.ledger import Ledger, estimate_cost
from praxion_brain.loop import FAILURE_GAP_TICKS, LoopState, skill_goal_text
from praxion_brain.memory import GoalRecord, Memory
from praxion_brain.planner import INPUT_ESTIMATE, OUTPUT_ESTIMATE, Plan, PlannedGoal
from praxion_brain.router import Route
from praxion_brain.skills import SkillCatalog, SkillInfo
from test_loop import (
    Clock,
    SpyGovernor,
    admission_options,
    evidence_entries,
    furnace_check_fn,
    furnace_write_fn,
    make_loop,
    pump,
    wait_accepted,
)

CAPS = ["command", "snapshot", "pop", "say"]
SKILLS = {"get_wood", "get_crafting_table", "get_wooden_pickaxe", "find_village", "get_food"}
#: The skills a plan step passes arguments to declare their parameter, so the
#: step's arguments can be typed like the body types them.
PARAMS = {"get_wood": ["n: int = 1"], "find_village": ["radius: int = 32"]}


def queue_catalog(*names: str) -> SkillCatalog:
    return SkillCatalog(
        skills=[
            SkillInfo(
                name=name,
                params=PARAMS.get(name, []),
                doc=f"doc for {name}",
                source=f"skill {name}()\n  rules:\n    true -> done\n",
                path=f"{name}.skill",
                origin="core",
            )
            for name in names
        ]
    )


CAT = queue_catalog(*sorted(SKILLS))

WOOD = PlannedGoal(
    text="get wood", condition="count(#logs) >= 4", skill="get_wood", args=["4"]
)
TABLE = PlannedGoal(text="get a crafting table", condition="has(crafting_table)")
PICKAXE = PlannedGoal(text="get a wooden pickaxe", condition="has(wooden_pickaxe)")
THREE = Plan(thought="Önce odun, sonra masa ve kazma.", goals=[WOOD, TABLE, PICKAXE])
LATER = Plan(thought="", goals=[PlannedGoal(text="get dirt", condition="count(dirt) >= 99")])


def says(body):
    return [message.text for message in body.says]


class Planner:
    """A fake planner: hands out ``plans`` in order, then ``LATER`` forever."""

    def __init__(self, *plans: Plan) -> None:
        self.plans = list(plans)
        self.calls: list[dict] = []

    def __call__(self, **kwargs) -> Plan:
        self.calls.append(kwargs)
        return self.plans.pop(0) if self.plans else LATER


def routing(goal, skills, **kwargs):
    """Play the planner's hint, else the skill named after the goal's item."""
    return Route(kind="play", skill=kwargs.get("hint") or goal.skill_name)


def plan_loop(body, tmp_path, planner, **kwargs):
    kwargs.setdefault("skills", CAT)
    kwargs.setdefault("route_fn", routing)
    kwargs.setdefault("map_fn", lambda text, **kw: None)  # never the network
    return make_loop(body, tmp_path, plan_fn=planner, **kwargs)


def settle_done(body, loop, index: int) -> None:
    pump(loop, lambda: len(body.commands) > index, what=f"command {index + 1}")
    command = body.commands[index]
    wait_accepted(body, command.id)
    body.settle(command.id, outcome="done", condition="true")


def test_a_three_goal_plan_is_played_with_one_planner_call(tmp_path) -> None:
    planner = Planner(THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            loop.start()
            settle_done(body, loop, 0)
            settle_done(body, loop, 1)
            pump(loop, lambda: len(body.commands) >= 3, what="the third command")

            assert len(planner.calls) == 1, "one model call for three goals"
            assert [c.skill for c in body.commands] == [
                "get_wood",  # the plan's hint won over the item-derived name
                "get_crafting_table",
                "get_wooden_pickaxe",
            ]
            # Explicit plan args are sent; the others keep the derived ones.
            assert body.commands[0].args == ["4"]
            assert body.commands[1].args == []
            assert [e["remaining"] for e in evidence_entries(loop, "plan_step")] == [2, 1, 0]

            plan = evidence_entries(loop, "plan")[0]
            assert plan["thought"] == THREE.thought and len(plan["goals"]) == 3
            narrated = [e for e in evidence_entries(loop, "narrate") if e["topic"] == "plan"]
            assert narrated and narrated[0]["text"] == THREE.thought
            assert THREE.thought in says(body)
        finally:
            loop.close()


def test_the_planner_gets_the_persona_and_a_briefing_with_it(tmp_path, monkeypatch) -> None:
    monkeypatch.setenv("PRAXION_PERSONA", "hunter")
    planner = Planner(THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            loop.start()
            pump(loop, lambda: planner.calls, what="the plan call")
            assert "## Persona  [this run]" in planner.calls[0]["briefing"]
            assert "hunter" in loop.persona.id
            persona = evidence_entries(loop, "persona")
            assert persona and persona[0]["id"] == "hunter"
        finally:
            loop.close()


def test_a_failed_goal_drops_the_queue_and_plans_again_at_once(tmp_path) -> None:
    second = Plan(thought="Yeniden deniyorum.", goals=[TABLE])
    planner = Planner(THREE, second)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the first command")
            first = body.commands[0]
            wait_accepted(body, first.id)
            body.settle(first.id, outcome="done", condition="false")  # skill_wrong

            pump(loop, lambda: len(body.commands) >= 2, what="the command of the new plan")
            dropped = evidence_entries(loop, "plan_dropped")
            assert [e["reason"] for e in dropped] == ["goal_failed"]
            assert dropped[0]["goals"] == [TABLE.text, PICKAXE.text]
            assert len(planner.calls) == 2
            assert body.commands[1].skill == "get_crafting_table"
            assert evidence_entries(loop, "pick_gap")[0]["ticks"] == 0
        finally:
            loop.close()


def test_a_second_failure_in_a_row_still_waits_the_gap(tmp_path) -> None:
    planner = Planner(THREE, THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            loop.start()
            for n in range(2):
                pump(loop, lambda n=n: len(body.commands) > n, what=f"command {n + 1}")
                command = body.commands[n]
                wait_accepted(body, command.id)
                body.settle(command.id, outcome="done", condition="false")
                pump(loop, lambda n=n: loop._consecutive_failures == n + 1, what="failure")
            gaps = [e["ticks"] for e in evidence_entries(loop, "pick_gap")]
            assert gaps == [0, FAILURE_GAP_TICKS]
            assert loop.state == LoopState.IDLE
        finally:
            loop.close()


def test_a_death_drops_the_plan_and_the_next_plan_escalates(tmp_path) -> None:
    planner = Planner(THREE, Plan(thought="", goals=[TABLE]), Plan(thought="", goals=[PICKAXE]))
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner, governor_class=SpyGovernor)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the first command")
            first = body.commands[0]
            wait_accepted(body, first.id)
            body.emit_event("wake_brain", cause="died", detail="Praxion was slain by Zombie")
            body.settle(first.id, outcome="failed", failure_kind="died")

            pump(loop, lambda: len(planner.calls) >= 2, what="the plan after the death")
            assert [e["reason"] for e in evidence_entries(loop, "plan_dropped")][0] == "died"
            assert [c["after_death"] for c in planner.calls[:2]] == [False, True]
            estimates = [e for call, e in loop.governor.calls if call == "propose"]
            assert estimates[0] == pytest.approx(
                estimate_cost("gpt-6.1-terra", INPUT_ESTIMATE, OUTPUT_ESTIMATE)
            )
            assert estimates[1] == pytest.approx(
                estimate_cost("gpt-6.1-sol", INPUT_ESTIMATE, OUTPUT_ESTIMATE)
            )

            # Only the first plan after the death escalates.
            pump(loop, lambda: len(body.commands) >= 2, what="the second plan's command")
            settle_done(body, loop, 1)
            pump(loop, lambda: len(planner.calls) >= 3, what="the third plan")
            assert planner.calls[2]["after_death"] is False
        finally:
            loop.close()


def test_an_already_met_step_is_skipped_without_a_model_call(tmp_path) -> None:
    planner = Planner(THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the first command")
            # The world already holds the second goal when its turn comes.
            body.facts["inventory"] = {"crafting_table": 1, "oak_log": 4}
            first = body.commands[0]
            wait_accepted(body, first.id)
            body.settle(first.id, outcome="done", condition="true")

            pump(loop, lambda: len(body.commands) >= 2, what="the third goal")
            assert body.commands[1].skill == "get_wooden_pickaxe"
            skipped = evidence_entries(loop, "plan_step_skipped")
            assert [e["reason"] for e in skipped] == ["already_met"]
            assert skipped[0]["step"]["text"] == TABLE.text
            assert len(planner.calls) == 1, "skipping costs no model call"
        finally:
            loop.close()


def test_same_condition_twice_in_a_plan_is_not_same_as_last(tmp_path) -> None:
    twice = Plan(
        thought="",
        goals=[
            PlannedGoal(text="logs", condition="count(#logs) >= 2", skill="get_wood"),
            PlannedGoal(text="logs again", condition="count(#logs) >= 2", skill="get_wood"),
        ],
    )
    planner = Planner(twice)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            loop.start()
            settle_done(body, loop, 0)
            pump(loop, lambda: len(body.commands) >= 2, what="the repeated condition")
            assert not evidence_entries(loop, "plan_step_skipped")
        finally:
            loop.close()


def test_a_blocked_goal_is_skipped_and_a_plan_of_blocked_goals_is_invalid(tmp_path) -> None:
    planner = Planner(
        Plan(thought="", goals=[TABLE]), Plan(thought="", goals=[TABLE]), Plan(thought="", goals=[TABLE])
    )
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        # No skill serves the first rung, so the code-only fallback has nothing.
        loop = plan_loop(
            body,
            tmp_path,
            planner,
            governor_class=SpyGovernor,
            skills=queue_catalog("get_crafting_table"),
        )
        try:
            for number in range(2):
                loop.memory.append_goal(
                    GoalRecord(
                        goal_id=f"fail-{number:04d}",
                        text="table",
                        condition="has(crafting_table)",
                        status="failed",
                        failure_kind="no_wood",
                        t_start=loop.memory.now(),
                        t_end=loop.memory.now(),
                    )
                )
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "nothing_to_do"), what="the end of the pick")
            assert [e["reason"] for e in evidence_entries(loop, "plan_step_skipped")] == ["blocked"] * 3
            invalid = evidence_entries(loop, "plan_invalid")
            assert [e["invalid_count"] for e in invalid] == [1, 2, 3]
            assert [c["invalid_count"] for c in planner.calls] == [0, 1, 2]
            assert evidence_entries(loop, "nothing_to_do")[0]["reason"] == "plans_invalid"
            # The third call is the escalated one.
            estimates = [e for call, e in loop.governor.calls if call == "propose"]
            assert estimates[2] > estimates[0]
            assert not body.commands
        finally:
            loop.close()


def test_an_empty_plan_is_an_invalid_plan(tmp_path) -> None:
    planner = Planner(Plan(thought="", goals=[]), Plan(thought="", goals=[TABLE]))
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the second plan's goal")
            invalid = evidence_entries(loop, "plan_invalid")
            assert [e["reason"] for e in invalid] == ["no_goals"]
            assert loop._invalid_proposals == 0, "a played goal resets the count"
        finally:
            loop.close()


def test_a_plan_keeps_at_most_five_goals(tmp_path) -> None:
    many = Plan(
        thought="", goals=[PlannedGoal(text=f"logs {n}", condition=f"count(#logs) >= {n + 1}") for n in range(7)]
    )
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, Planner(many))
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "plan"), what="the plan")
            assert evidence_entries(loop, "plan")[0]["kept"] == 5
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# Skill-verified goals: no inventory condition, the skill's own `done` settles
# --------------------------------------------------------------------------- #
VILLAGE = PlannedGoal(text="find a village", condition="", skill="find_village", args=["64"])


def test_a_skill_verified_goal_is_sent_without_a_condition_and_finishes_done(tmp_path) -> None:
    def never_routed(*args, **kwargs):
        raise AssertionError("a skill-verified goal needs no Jev")

    planner = Planner(Plan(thought="Köy arıyorum.", goals=[VILLAGE]))
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner, route_fn=never_routed)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the village command")
            command = body.commands[0]
            assert command.skill == "find_village"
            assert command.args == ["64"]
            assert command.condition is None
            wait_accepted(body, command.id)
            body.settle(command.id, outcome="done")  # no condition result

            pump(
                loop,
                lambda: any(r.status == "done" for r in loop.memory.goals()),
                what="the done record",
            )
            record = loop.memory.goals()[-1]
            assert record.status == "done" and record.unverifiable is True
            assert record.condition == skill_goal_text("find_village", ["64"]) == "find_village(64)"
            assert record.skill == "find_village"
        finally:
            loop.close()


def test_a_skill_verified_goal_names_its_failure_key_and_blocks_after_two_failures(tmp_path) -> None:
    planner = Planner(Plan(thought="", goals=[VILLAGE]))
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            for number in range(2):
                loop.memory.append_goal(
                    GoalRecord(
                        goal_id=f"fail-{number:04d}",
                        text="village",
                        condition="find_village(64)",
                        unverifiable=True,
                        status="failed",
                        failure_kind="not_found",
                        t_start=loop.memory.now(),
                        t_end=loop.memory.now(),
                    )
                )
            # A different argument is a different goal: not blocked.
            assert not loop.memory.is_blocked("find_village(32)")
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "plan_step_skipped"), what="the blocked step")
            assert evidence_entries(loop, "plan_step_skipped")[0]["reason"] == "blocked"
            assert not body.commands
        finally:
            loop.close()


def test_an_empty_condition_needs_a_known_public_skill(tmp_path) -> None:
    bad = Plan(
        thought="",
        goals=[
            PlannedGoal(text="no skill", condition=""),
            PlannedGoal(text="unknown skill", condition="", skill="fly"),
            PlannedGoal(text="a helper", condition="", skill="cut_log"),
            TABLE,
        ],
    )
    with FakeBody(skills=SKILLS | {"cut_log"}, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(
            body, tmp_path, Planner(bad), skills=queue_catalog(*sorted(SKILLS), "cut_log")
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the valid goal")
            reasons = [e["reason"] for e in evidence_entries(loop, "plan_step_skipped")]
            assert reasons == ["malformed_condition", "unknown_skill", "unknown_skill"]
            assert body.commands[0].skill == "get_crafting_table"
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# A step's arguments are typed like the body types them, and belong to the
# skill the planner named (review finding 2)
# --------------------------------------------------------------------------- #
def test_a_step_argument_the_named_skill_cannot_take_skips_the_step(tmp_path) -> None:
    impossible = Plan(
        thought="",
        goals=[
            PlannedGoal(
                text="wood, but not a count",
                condition="count(#logs) >= 4",
                skill="get_wood",
                args=["lots"],
            ),
            PlannedGoal(
                text="a table has no parameters",
                condition="has(crafting_table)",
                skill="get_crafting_table",
                args=["1"],
            ),
            TABLE,
        ],
    )
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, Planner(impossible))
        try:
            loop.start()
            # The bad steps are skipped in code, never sent to the body to fail
            # there under the goal's condition.
            pump(loop, lambda: len(body.commands) >= 1, what="the usable step")
            skipped = evidence_entries(loop, "plan_step_skipped")
            assert [e["reason"] for e in skipped] == ["bad_args", "bad_args"]
            assert "int" in skipped[0]["detail"]
            assert "arguments" in skipped[1]["detail"]
            assert body.commands[0].skill == "get_crafting_table"
            assert body.commands[0].args == []
        finally:
            loop.close()


def test_arguments_are_dropped_when_a_different_skill_plays(tmp_path) -> None:
    # The plan wrote "4" for get_wood; Jev routes the goal to another skill,
    # which must take its own shape, not get_wood's argument.
    step = PlannedGoal(
        text="planks", condition="count(#planks) >= 4", skill="get_wood", args=["4"]
    )
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(
            body,
            tmp_path,
            Planner(Plan(thought="", goals=[step])),
            route_fn=lambda *a, **k: Route(kind="play", skill="get_crafting_table"),
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the routed command")
            command = body.commands[0]
            assert command.skill == "get_crafting_table" and command.args == []
            dropped = evidence_entries(loop, "args_dropped")[0]
            assert dropped["written_for"] == "get_wood" and dropped["args"] == ["4"]
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# What drops the queue
# --------------------------------------------------------------------------- #
def test_an_idle_window_holds_no_pending_goal_for_a_stop(tmp_path) -> None:
    def broken_route(*args, **kwargs):
        raise RuntimeError("Jev is down")

    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, Planner(THREE), route_fn=broken_route)
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "pick_backoff"), what="the back-off")
            assert loop.state == LoopState.IDLE
            # The goal was abandoned when the route failed: a Stop in the idle
            # window must not report it as the cancelled goal (finding 3).
            assert loop._stop_goal() == ("no_goal", {})
        finally:
            loop.close()


def test_an_operator_task_drops_the_plan(tmp_path) -> None:
    planner = Planner(THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the first command")
            assert len(loop._plan) == 2
            body.emit_event("chat_received", sender="x", text="8 taş getir", control=None)
            pump(loop, lambda: evidence_entries(loop, "plan_dropped"), what="the drop")
            assert evidence_entries(loop, "plan_dropped")[0]["reason"] == "user_task"
            assert not loop._plan
        finally:
            loop.close()


def test_a_skill_write_drops_the_plan(tmp_path) -> None:
    planner = Planner(THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(
            body,
            tmp_path,
            planner,
            route_fn=lambda *a, **k: Route(kind="write"),
            admission_options=admission_options(
                tmp_path, write_fn=furnace_write_fn, check_fn=furnace_check_fn
            ),
        )
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "write_started"), what="the write")
            dropped = evidence_entries(loop, "plan_dropped")
            assert [e["reason"] for e in dropped][0] == "skill_write"
            assert dropped[0]["goals"] == [TABLE.text, PICKAXE.text]
        finally:
            loop.close()


def test_a_survival_goal_drops_the_plan(tmp_path) -> None:
    planner = Planner()
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"health": 20, "food": 20, "is_night": False, "inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner)
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            loop._plan.extend([TABLE, PICKAXE])
            loop._paused = False
            body.facts.update({"food": 4})
            body.emit_event("wake_brain", cause="hunger_no_food", detail="no food")
            pump(loop, lambda: len(body.commands) >= 1, what="the food command")
            assert body.commands[0].skill == "get_food"
            assert [e["reason"] for e in evidence_entries(loop, "plan_dropped")] == ["survival"]
            assert not planner.calls, "survival is decided in code, and the plan is gone"
        finally:
            loop.close()


def test_a_world_desync_drops_the_plan(tmp_path) -> None:
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, Planner())
        loop._plan.extend([TABLE])
        loop._on_world_desync("explode left bytes unread")
        assert [e["reason"] for e in evidence_entries(loop, "plan_dropped")] == ["world_desync"]
        assert not loop._plan


def test_a_done_goal_keeps_the_queue(tmp_path) -> None:
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, Planner(THREE))
        try:
            loop.start()
            settle_done(body, loop, 0)
            pump(loop, lambda: len(body.commands) >= 2, what="the second goal")
            assert not evidence_entries(loop, "plan_dropped")
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# The operator's tasks carry a skill and its arguments
# --------------------------------------------------------------------------- #
def _task_resolution(args):
    return types.SimpleNamespace(
        kind="goal",
        control=None,
        proposal=GoalProposal(text="find a village", condition="", rationale="user"),
        skill="find_village",
        validation=Validation(ok=True, unverifiable=True),
        reason=None,
        detail=None,
        via="model",
        args=args,
    )


def test_a_queued_skill_task_passes_its_args_and_has_no_condition(tmp_path, monkeypatch) -> None:
    monkeypatch.setattr(commands, "resolve", lambda *a, **k: _task_resolution(["128"]))
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(
            body,
            tmp_path,
            Planner(),
            route_fn=lambda *a, **k: (_ for _ in ()).throw(AssertionError("no Jev for a task")),
        )
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            body.emit_event("chat_received", sender="x", text="köy bul", control=None)
            pump(loop, lambda: loop._user_goals, what="the queued task")
            body.emit_event("chat_received", sender="x", text="devam", control="resume")
            pump(loop, lambda: len(body.commands) >= 1, what="the task command")
            command = body.commands[0]
            assert (command.skill, command.args, command.condition) == ("find_village", ["128"], None)
            wait_accepted(body, command.id)
            body.settle(command.id, outcome="done")
            pump(loop, lambda: any(r.status == "done" for r in loop.memory.goals()), what="done")
            assert loop.memory.goals()[-1].origin == "user"
        finally:
            loop.close()


def test_a_preempting_skill_task_passes_its_args(tmp_path, monkeypatch) -> None:
    monkeypatch.setattr(commands, "resolve", lambda *a, **k: _task_resolution(["32"]))
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, Planner(THREE))
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the plan's first command")
            body.emit_event("chat_received", sender="x", text="köy bul", control=None)
            pump(loop, lambda: len(body.commands) >= 2, what="the preempting command")
            command = body.commands[1]
            assert (command.skill, command.args, command.condition) == ("find_village", ["32"], None)
            assert [e["reason"] for e in evidence_entries(loop, "plan_dropped")] == ["user_task"]
        finally:
            loop.close()


def test_a_resolution_without_args_keeps_the_derived_count(tmp_path, monkeypatch) -> None:
    resolution = _task_resolution(None)
    del resolution.args  # a commands.py that predates `args`
    resolution.proposal = GoalProposal(text="get logs", condition="count(#logs) >= 5", rationale="u")
    resolution.skill = "get_wood"
    monkeypatch.setattr(commands, "resolve", lambda *a, **k: resolution)
    counted = SkillCatalog(
        skills=[
            SkillInfo(name="get_wood", params=["n: int = 3"], source="", path="", origin="core"),
            *[s for s in CAT.skills if s.name != "get_wood"],
        ]
    )
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, Planner(THREE), skills=counted)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the plan's first command")
            body.emit_event("chat_received", sender="x", text="5 odun", control=None)
            pump(loop, lambda: len(body.commands) >= 2, what="the preempting command")
            assert body.commands[1].args == ["5"]
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# The console state and the optional wiki
# --------------------------------------------------------------------------- #
def test_the_console_state_carries_persona_plan_and_thought(tmp_path) -> None:
    clock = Clock()
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        console = Console(
            evidence=Evidence(tmp_path / "evidence"),
            ledger=Ledger(tmp_path / "ledger.jsonl"),
            memory=Memory("w", root=tmp_path / "worlds", clock=clock),
            world_id="w",
            core_dir=str(tmp_path / "core"),
            active_dir=str(tmp_path / "active"),
        )
        loop = plan_loop(body, tmp_path, Planner(THREE), console=console)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the first command")
            loop._publish(force=True)
            state = console.state
            assert state["persona"]["id"] == loop.persona.id
            assert state["thought"] == THREE.thought
            assert state["plan"] == [
                {"text": TABLE.text, "condition": TABLE.condition, "skill": None},
                {"text": PICKAXE.text, "condition": PICKAXE.condition, "skill": None},
            ]
        finally:
            loop.close()


def test_wiki_snippets_reach_the_briefing_and_a_wiki_error_is_ignored(tmp_path, monkeypatch) -> None:
    import sys

    queries: list[tuple] = []

    def search(query, k=3):
        queries.append((query, k))
        return [types.SimpleNamespace(title="Wood", text="Logs\n come from trees. " + "z" * 400, url="u", score=1.0)]

    monkeypatch.setitem(sys.modules, "praxion_brain.wiki", types.SimpleNamespace(search=search))
    planner = Planner(THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner, wiki=True)
        try:
            loop.start()
            pump(loop, lambda: planner.calls, what="the plan call")
            assert queries == [("how to get wood in Minecraft survival", 2)]
            briefing = planner.calls[0]["briefing"]
            assert "## Knowledge  [wiki]\nWood: Logs come from trees. " in briefing
            line = [l for l in briefing.splitlines() if l.startswith("Wood: ")][0]
            assert len(line) == len("Wood: ") + 300
        finally:
            loop.close()

    def broken(query, k=3):
        raise RuntimeError("index is corrupt")

    monkeypatch.setitem(sys.modules, "praxion_brain.wiki", types.SimpleNamespace(search=broken))
    planner = Planner(THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path / "again", planner, wiki=True)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the plan still plays")
            assert evidence_entries(loop, "wiki_error")[0]["error"] == "RuntimeError: index is corrupt"
            assert "## Knowledge" not in planner.calls[0]["briefing"]
        finally:
            loop.close()


def test_without_the_wiki_package_the_briefing_has_no_knowledge(tmp_path, monkeypatch) -> None:
    import sys

    monkeypatch.setitem(sys.modules, "praxion_brain.wiki", None)  # `import` raises ImportError
    planner = Planner(THREE)
    with FakeBody(skills=SKILLS, caps=CAPS, facts={"inventory": {}}) as body:
        loop = plan_loop(body, tmp_path, planner, wiki=True)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the plan plays")
            assert "## Knowledge" not in planner.calls[0]["briefing"]
            assert not evidence_entries(loop, "wiki_error")
        finally:
            loop.close()


# --------------------------------------------------------------------------- #
# Night work: a pickaxe body plans instead of sheltering (decision 2026-11-13
# "night mining", run B)
# --------------------------------------------------------------------------- #
NIGHT_SKILLS = {"get_wood", "get_cobblestone", "get_coal", "night_watch", "take_shelter"}
NIGHT_CAT = queue_catalog(*sorted(NIGHT_SKILLS))
NIGHT_FACTS = {
    "health": 20,
    "food": 20,
    "is_night": True,
    "inventory": {"stone_pickaxe": 1},
}
COBBLE = PlannedGoal(
    text="get cobblestone", condition="count(cobblestone) >= 8", skill="get_cobblestone"
)
COAL = PlannedGoal(text="get coal", condition="count(coal) >= 4", skill="get_coal")
MINING = Plan(thought="Gece yerin altında çalışacağım.", goals=[COBBLE, COAL])


def test_a_surface_step_is_skipped_at_night_and_a_mining_step_plays(tmp_path) -> None:
    """The planner's named skill decides first: ``get_wood`` waits for morning,
    the mining step behind it plays (decision 2026-11-13)."""
    planner = Planner(THREE, MINING)
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=dict(NIGHT_FACTS)) as body:
        loop = plan_loop(body, tmp_path, planner, skills=NIGHT_CAT)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the night command")
            skipped = evidence_entries(loop, "plan_step_skipped")
            assert [e["reason"] for e in skipped] == ["night_unsafe"] * 3, skipped
            assert skipped[0]["step"]["skill"] == "get_wood"
            assert "get_wood" in skipped[0]["detail"]
            # The whole daytime plan is unsafe at night: the next plan is the
            # night one, and only its mining step plays.
            assert len(planner.calls) == 2
            assert body.commands[0].skill == "get_cobblestone"
            assert body.commands[0].condition == "count(cobblestone) >= 8"
        finally:
            loop.close()


def test_a_step_without_a_named_skill_is_never_written_in_the_dark(tmp_path) -> None:
    """An unnamed step would need a skill write, so it is skipped at night."""
    plan = Plan(
        thought="",
        goals=[PlannedGoal(text="get one log", condition="count(#logs) >= 1"), MINING.goals[0]],
    )
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=dict(NIGHT_FACTS)) as body:
        loop = plan_loop(body, tmp_path, Planner(plan), skills=NIGHT_CAT)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the mining command")
            skipped = evidence_entries(loop, "plan_step_skipped")
            assert [e["reason"] for e in skipped] == ["night_unsafe"]
            assert skipped[0]["step"]["skill"] is None
            assert body.commands[0].skill == "get_cobblestone"
        finally:
            loop.close()


def test_the_skill_jev_routes_to_is_checked_again_at_night(tmp_path) -> None:
    """The planner named a night skill; the router picks a surface one. The step
    is skipped after routing, and nothing is sent to the body."""
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=dict(NIGHT_FACTS)) as body:
        loop = plan_loop(
            body,
            tmp_path,
            Planner(Plan(thought="", goals=[COBBLE])),
            skills=NIGHT_CAT,
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
        )
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "pick_backoff"), what="the back-off")
            skipped = evidence_entries(loop, "plan_step_skipped")
            assert [e["reason"] for e in skipped] == ["night_unsafe"]
            assert skipped[0]["step"]["skill"] == "get_wood"
            assert not body.commands, "no surface command at night"
            assert evidence_entries(loop, "pick_backoff")[0]["reason"] == "night_unsafe"
        finally:
            loop.close()


def test_dusk_drops_the_daytime_plan_and_dawn_drops_the_night_plan(tmp_path) -> None:
    day = {"health": 20, "food": 20, "is_night": False, "inventory": {}}
    planner = Planner(THREE, MINING)
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=day) as body:
        loop = plan_loop(body, tmp_path, planner, skills=NIGHT_CAT)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the daytime command")
            assert len(loop._plan) == 2
            body.facts.update(NIGHT_FACTS)
            loop.session.refresh()
            pump(loop, lambda: evidence_entries(loop, "plan_dropped"), what="the dusk drop")
            dropped = evidence_entries(loop, "plan_dropped")
            assert [e["reason"] for e in dropped] == ["night"]
            assert dropped[0]["goals"] == [TABLE.text, PICKAXE.text]
            assert not loop._plan
        finally:
            loop.close()

    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=dict(NIGHT_FACTS)) as body:
        loop = plan_loop(body, tmp_path / "dawn", Planner(MINING), skills=NIGHT_CAT)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the night command")
            assert len(loop._plan) == 1
            body.facts.update({"is_night": False, "inventory": {"stone_pickaxe": 1}})
            loop.session.refresh()
            pump(loop, lambda: evidence_entries(loop, "plan_dropped"), what="the dawn drop")
            dropped = evidence_entries(loop, "plan_dropped")
            assert [e["reason"] for e in dropped] == ["morning"]
            assert dropped[0]["goals"] == [COAL.text]
            assert not loop._plan
        finally:
            loop.close()


def test_the_night_briefing_lists_only_the_night_skills(tmp_path) -> None:
    planner = Planner(MINING)
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=dict(NIGHT_FACTS)) as body:
        loop = plan_loop(body, tmp_path, planner, skills=NIGHT_CAT)
        try:
            loop.start()
            pump(loop, lambda: planner.calls, what="the plan call")
            lines = planner.calls[0]["briefing"].splitlines()
            header = lines.index("## Night  [body facts + catalog]")
            assert lines[header + 1] == "it is night; work only underground or at the crafting table"
            # Sorted, and only the night skills the catalog has: get_wood is a
            # catalog skill, not a night one.
            assert lines[header + 2] == "allowed skills: get_coal, get_cobblestone"
            assert lines[header + 3] == (
                "never plan surface gathering, hunting or exploring before morning"
            )
        finally:
            loop.close()


def test_a_hurt_body_at_night_gets_the_shelter_back(tmp_path) -> None:
    """Health at ``SHELTER_HEALTH`` ends night work: the ordinary survival path
    interrupts the mining goal and the shelter takes the night."""
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=dict(NIGHT_FACTS)) as body:
        loop = plan_loop(body, tmp_path, Planner(MINING), skills=NIGHT_CAT)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the mining command")
            mining = body.commands[0]
            assert mining.skill == "get_cobblestone"
            wait_accepted(body, mining.id)
            pump(
                loop,
                lambda: loop._open_goal is not None and loop._open_goal.intent_id is not None,
                what="accepted",
            )
            body.facts["health"] = 10
            loop.session.refresh()
            pump(loop, lambda: evidence_entries(loop, "survival_interrupt"), what="the interrupt")
            assert evidence_entries(loop, "survival_interrupt")[0]["survival"] == "night"
            pump(loop, lambda: body.pops, what="the pop")
            body.settle(mining.id, outcome="cancelled")
            pump(loop, lambda: len(body.commands) >= 2, what="the shelter command")
            assert body.commands[1].skill == "take_shelter"
            assert body.commands[1].condition == "not is_night()"
        finally:
            loop.close()


def test_the_night_work_line_is_said_once_a_night(tmp_path) -> None:
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=dict(NIGHT_FACTS)) as body:
        loop = plan_loop(body, tmp_path, Planner(MINING), skills=NIGHT_CAT)
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the night command")
            # Hurt and healed again in the same night: still one line.
            body.facts["health"] = 9
            loop.session.refresh()
            for _ in range(10):
                loop.step(0.02)
            body.facts["health"] = 20
            loop.session.refresh()
            for _ in range(10):
                loop.step(0.02)
            assert says(body).count(speech.night_work()) == 1
            narrated = [
                entry
                for entry in evidence_entries(loop, "narrate")
                if entry.get("topic") == "night_work"
            ]
            assert len(narrated) == 1
        finally:
            loop.close()


def test_the_code_only_fallback_never_gathers_on_the_surface_at_night(tmp_path) -> None:
    """Rate-limited or invalid plans fall back to the rung's skill; at night that
    skill may not be a surface one (decision 2026-11-13). A diamond pickaxe does
    not imply logs: the next unmet rung is still wood."""
    night = {**NIGHT_FACTS, "inventory": {"diamond_pickaxe": 1}}
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=night) as body:
        loop = plan_loop(
            body, tmp_path, Planner(Plan(thought="", goals=[])), skills=NIGHT_CAT
        )
        try:
            loop.start()
            pump(loop, lambda: evidence_entries(loop, "nothing_to_do"), what="the end of the pick")
            idle = evidence_entries(loop, "code_only_idle")
            assert idle and idle[-1]["reason"] == "night_unsafe", idle
            assert idle[-1]["detail"] == "get_wood is not a night skill"
            assert not body.commands, "no surface command at night"
        finally:
            loop.close()


def test_the_operators_task_still_plays_at_night(tmp_path) -> None:
    """The night restriction is the planner's, never the user's (architecture
    §1: the user's projects come before Praxion's own ideas)."""
    with FakeBody(skills=NIGHT_SKILLS, caps=CAPS, facts=dict(NIGHT_FACTS)) as body:
        loop = plan_loop(
            body,
            tmp_path,
            Planner(MINING),
            skills=NIGHT_CAT,
            route_fn=lambda *a, **k: Route(kind="play", skill="get_wood"),
        )
        loop._paused = True
        try:
            loop.start()
            pump(loop, lambda: loop.session.snapshot is not None, what="snapshot")
            loop._queue_user_goal({"text": "wood", "condition": "count(#logs) >= 3"})
            loop._paused = False
            pump(loop, lambda: len(body.commands) >= 1, what="the user command")
            assert body.commands[0].skill == "get_wood"
            assert not evidence_entries(loop, "plan_step_skipped")
        finally:
            loop.close()
