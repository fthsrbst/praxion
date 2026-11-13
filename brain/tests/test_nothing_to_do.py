"""The ``nothing_to_do`` back-off of the plan queue, and what replaced the needs gate.

Run 6 (2026-11-09) played eighteen stock goals while the furnace skill was
written; the needs gate stopped them. Decision 2026-11-12 "Demo night" retired
that gate for the plan queue: the planner is told what is covered, a plan whose
every goal is the one being written ends the pick as one visible
``nothing_to_do`` (``plans_excluded``) without escalating the model, and the
back-off doubles until an admitted skill or a changed world lifts it.
"""

from __future__ import annotations

import threading

from fake_body import FakeBody
from gamedata_fixture import gamedata
from praxion_brain.loop import (
    NOTHING_TO_DO_GAP_TICKS,
    NOTHING_TO_DO_MAX_GAP_TICKS,
)
from praxion_brain.memory import GoalRecord, Memory
from praxion_brain.planner import Plan, PlannedGoal
from praxion_brain.records import GoalRecords
from praxion_brain.router import Route
from test_loop import (
    admission_options,
    catalog,
    evidence_entries,
    evidence_kinds,
    furnace_check_fn,
    furnace_write_fn,
    gated_practice,
    make_loop,
    pump,
    set_tick,
)

#: The inventory at the start of run 6's stock phase (06:57:45): the stone
#: sword is made, the furnace is the next rung and 64 cobblestone are held.
RUN6_INVENTORY = {
    "cobblestone": 64,
    "birch_planks": 13,
    "stick": 11,
    "dirt": 24,
    "birch_log": 2,
    "wooden_pickaxe": 1,
    "stone_pickaxe": 1,
    "stone_sword": 1,
}

def _loop(body, tmp_path, started, release, calls):
    def plan_fn(**kwargs):
        calls.append(kwargs.get("invalid_count", 0))
        return Plan(
            thought="",
            goals=[PlannedGoal(text="get a furnace", condition="has(furnace)")],
        )

    def route_fn(goal, *args, **kwargs):
        assert goal.item == "furnace", f"another goal reached the router: {goal}"
        return Route(kind="write")

    return make_loop(
        body,
        tmp_path,
        plan_fn=plan_fn,
        route_fn=route_fn,
        skills=catalog("get_wooden_pickaxe"),
        admission_options=admission_options(
            tmp_path,
            write_fn=furnace_write_fn,
            check_fn=furnace_check_fn,
            practice_fn=gated_practice(started, release),
        ),
        catalog_loader=lambda: catalog("get_wooden_pickaxe", "get_furnace"),
        gamedata=gamedata(),
    )


def test_plans_naming_only_the_goal_being_written_back_off_with_a_doubling_gap(
    tmp_path,
) -> None:
    started, release = threading.Event(), threading.Event()
    calls: list[int] = []
    with FakeBody(
        skills={"get_wooden_pickaxe", "get_furnace"},
        facts={"inventory": dict(RUN6_INVENTORY)},
    ) as body:
        loop = _loop(body, tmp_path, started, release, calls)
        try:
            loop.start()
            pump(loop, lambda: loop._job is not None, what="admission job")
            assert started.wait(4.0), "the furnace skill was never written"

            # One pick: max_proposals planner calls, each naming the goal being
            # written, end as one visible nothing_to_do.
            pump(
                loop,
                lambda: len(evidence_entries(loop, "nothing_to_do")) >= 1,
                what="the first nothing_to_do",
            )
            assert calls == [0, 0, 0, 0], "the write, then one pick of three plans"
            first = evidence_entries(loop, "nothing_to_do")
            assert len(first) == 1
            assert first[0]["reason"] == "plans_excluded"
            assert first[0]["writing"] == "get_furnace"
            assert first[0]["gap_ticks"] == NOTHING_TO_DO_GAP_TICKS
            skipped = evidence_entries(loop, "plan_step_skipped")
            assert [entry["reason"] for entry in skipped] == ["writing"] * 3

            # Quiet while waiting: no more calls inside the back-off.
            for _ in range(40):
                loop.step(0.01)
            assert len(calls) == 4
            assert len(evidence_entries(loop, "nothing_to_do")) == 1

            # Each later pick ends as one more nothing_to_do, with a doubling
            # gap, until the cap.
            tick = int(loop.session.tick or 0)
            gaps = [NOTHING_TO_DO_GAP_TICKS]
            for expected in range(2, 6):
                gap = gaps[-1]
                tick = max(tick, int(loop._next_pick_tick)) + 1
                set_tick(loop, body, tick)
                pump(
                    loop,
                    lambda: len(evidence_entries(loop, "nothing_to_do")) >= expected,
                    what=f"nothing_to_do #{expected}",
                )
                gaps.append(min(gap * 2, NOTHING_TO_DO_MAX_GAP_TICKS))
            reports = evidence_entries(loop, "nothing_to_do")
            assert [r["gap_ticks"] for r in reports] == gaps[: len(reports)]
            assert max(r["gap_ticks"] for r in reports) == NOTHING_TO_DO_MAX_GAP_TICKS

            # An excluded plan is not an invalid one: no escalation, no counter.
            assert not evidence_entries(loop, "plan_invalid")
            assert loop._invalid_proposals == 0
            assert set(calls) == {0}
            assert not body.commands, "no goal was played while the skill was written"
            assert "command_sent" not in evidence_kinds(loop)

            # An admitted skill lifts the back-off at once and the furnace is played.
            release.set()
            pump(loop, lambda: len(body.commands) >= 1, what="the written furnace goal")
            assert body.commands[0].skill == "get_furnace"
        finally:
            release.set()
            loop.close()


def test_a_stock_goal_is_played_and_the_needs_line_is_only_a_fact(tmp_path) -> None:
    """The needs gate is gone: a stock goal plays, the briefing still shows the needs."""
    briefings: list[str] = []

    def plan_fn(briefing, **kwargs):
        briefings.append(briefing)
        return Plan(
            thought="",
            goals=[PlannedGoal(text="stock up", condition="count(cobblestone) >= 96")],
        )

    with FakeBody(skills={"get_cobblestone"}, facts={"inventory": dict(RUN6_INVENTORY)}) as body:
        loop = make_loop(
            body,
            tmp_path,
            plan_fn=plan_fn,
            route_fn=lambda *a, **k: Route(kind="play", skill="get_cobblestone"),
            skills=catalog("get_cobblestone"),
            gamedata=gamedata(),
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="stock goal played")
            assert not evidence_entries(loop, "nothing_to_do")
            assert "## Needs" in briefings[0]
        finally:
            loop.close()


def test_the_drift_callback_fires_after_five_flat_goals(tmp_path) -> None:
    from praxion_brain.evidence import Evidence

    evidence = Evidence(tmp_path / "evidence")
    memory = Memory("w", root=tmp_path / "worlds", evidence=evidence)
    fired: list[tuple[str, int]] = []
    records = GoalRecords(
        memory,
        evidence,
        inventory=lambda: dict(RUN6_INVENTORY),
        on_drift=lambda record, met: fired.append((record.goal_id, met)),
    )
    for number in range(1, 8):
        record = GoalRecord(
            goal_id=f"g-{number:04d}",
            text="stock",
            condition="count(dirt) >= 30",
            status="open",
        )
        records.finish(record, status="failed", failure_kind="x", wake=False)
    # The first goal sets the baseline, then five flat goals make the drift.
    assert len(fired) == 1
    assert fired[0][1] == 6, "the ladder stood at 6 of 12 rungs"
