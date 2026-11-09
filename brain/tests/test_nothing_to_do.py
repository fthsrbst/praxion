"""Smarter play slice 1: the run-6 replay and the loop control it needs.

Run 6 (2026-11-09) played eighteen stock goals (96 cobblestone) while the
furnace skill was being written. With the recipe export loaded the same
proposals must be stopped by code: no stock goal is played, no paid re-ask
follows a code rejection, and each pick ends as one visible ``nothing_to_do``.
Decision 2026-11-09 "Five next areas"; critique findings 1, 6, 7 and 12 of
``logs/mac1109/design/smarter.crit.out.md``.
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
    proposal,
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

#: The stock proposals run 6 made after `has(furnace)`, in order (brain.jsonl).
RUN6_STOCK = [
    "count(cobblestone) >= 96",
    "count(#logs) >= 4",
    "count(cobblestone) >= 128",
    "count(#logs) >= 8",
    "count(#planks) >= 16",
    "count(stick) >= 12",
]


def _loop(body, tmp_path, proposals, started, release, calls):
    def propose_fn(*args, **kwargs):
        calls.append(kwargs.get("invalid_count", 0))
        return proposals.pop(0)

    def route_fn(goal, *args, **kwargs):
        assert goal.item == "furnace", f"a stock goal reached the router: {goal}"
        return Route(kind="write")

    return make_loop(
        body,
        tmp_path,
        propose_fn=propose_fn,
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


def test_run_6_replays_to_zero_stock_goals_and_visible_nothing_to_do(tmp_path) -> None:
    started, release = threading.Event(), threading.Event()
    proposals = [proposal("has(furnace)", "get a furnace", "furnace")] + [
        proposal(condition, "stock up") for condition in RUN6_STOCK
    ]
    calls: list[int] = []
    with FakeBody(skills={"get_wooden_pickaxe", "get_furnace"},
                  facts={"inventory": dict(RUN6_INVENTORY)}) as body:
        loop = _loop(body, tmp_path, proposals, started, release, calls)
        try:
            loop.start()
            pump(loop, lambda: loop._job is not None, what="admission job")
            assert started.wait(4.0), "the furnace skill was never written"

            # One pick, one paid call: the first stock proposal ended it.
            pump(
                loop,
                lambda: len(evidence_entries(loop, "nothing_to_do")) >= 1,
                what="the first nothing_to_do",
            )
            assert len(calls) == 2, "furnace, then exactly one stock proposal"
            assert calls == [0, 0], "a code rejection never escalates the model"
            first = evidence_entries(loop, "nothing_to_do")
            assert len(first) == 1
            assert first[0]["reason"] == "not_needed"
            assert first[0]["writing"] == "get_furnace"
            assert first[0]["gap_ticks"] == NOTHING_TO_DO_GAP_TICKS
            gate = evidence_entries(loop, "needs_gate")[-1]
            assert gate["rung"] == "furnace" and gate["mode"] == "recipe"
            assert gate["covered"] is True and gate["waiting"] is True

            # Quiet while waiting: no more calls inside the back-off.
            for _ in range(40):
                loop.step(0.01)
            assert len(calls) == 2
            assert len(evidence_entries(loop, "nothing_to_do")) == 1

            # Each later pick spends one call and ends as one more nothing_to_do,
            # with a doubling gap, until the cap.
            tick = int(loop.session.tick or 0)
            gaps = [NOTHING_TO_DO_GAP_TICKS]
            for expected in range(2, 2 + len(RUN6_STOCK) - 1):
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

            # The whole stock sequence was refused in code.
            assert not body.commands, "no goal was played while the skill was written"
            assert not evidence_entries(loop, "proposal_invalid")
            assert loop._invalid_proposals == 0
            assert all(r["reason"] == "not_needed" for r in reports)
            assert "command_sent" not in evidence_kinds(loop)

            # An admitted skill lifts the back-off at once and the furnace is played.
            release.set()
            pump(loop, lambda: len(body.commands) >= 1, what="the written furnace goal")
            assert body.commands[0].skill == "get_furnace"
        finally:
            release.set()
            loop.close()


def test_without_the_export_the_loop_behaves_as_before(tmp_path) -> None:
    """No gamedata: a stock proposal is accepted and played (checks 6/7 are off)."""
    proposals = [proposal("count(cobblestone) >= 96", "stock up")]

    def propose_fn(*args, **kwargs):
        return proposals.pop(0)

    with FakeBody(skills={"get_cobblestone"}, facts={"inventory": dict(RUN6_INVENTORY)}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *a, **k: Route(kind="play", skill="get_cobblestone"),
            skills=catalog("get_cobblestone"),
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="stock goal played")
            assert not evidence_entries(loop, "nothing_to_do")
            assert not evidence_entries(loop, "needs_gate")
        finally:
            loop.close()


def test_a_repeated_item_is_refused_only_while_the_rung_is_covered(tmp_path) -> None:
    """Finding 6: a second harvest after the stock was spent is legitimate."""
    calls: list[int] = []

    def propose_fn(*args, **kwargs):
        calls.append(1)
        return proposal("count(cobblestone) >= 20", "more stone")

    # Furnace rung, only 5 cobblestone held: the cobblestone is missing, so
    # asking for it twice in a row is the right move, not a repeat.
    inventory = dict(RUN6_INVENTORY, cobblestone=5)
    with FakeBody(skills={"get_cobblestone"}, facts={"inventory": inventory}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *a, **k: Route(kind="play", skill="get_cobblestone"),
            skills=catalog("get_cobblestone"),
            gamedata=gamedata(),
        )
        try:
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="cobblestone goal played")
            gate = evidence_entries(loop, "needs_gate")[-1]
            assert gate["covered"] is False
            assert not evidence_entries(loop, "nothing_to_do")
        finally:
            loop.close()


def test_the_drift_brake_measures_a_fresh_ladder_and_lifts_on_a_new_rung(
    tmp_path,
) -> None:
    """Findings 1 and 4: the brake bites, its baseline is not a stale read, it lifts.

    The settle-time inventory can be two rungs behind (design §7). Taking that
    reading as the baseline would release the brake on the very next pick, so it
    is measured from the first fresh snapshot instead.
    """
    proposals = [proposal("count(dirt) >= 30", "stock up")] * 2
    calls: list[int] = []

    def propose_fn(*args, **kwargs):
        calls.append(kwargs.get("invalid_count", 0))
        return proposals.pop(0)

    inventory = dict(RUN6_INVENTORY)  # six rungs met, the furnace is next
    with FakeBody(skills={"get_dirt"}, facts={"inventory": inventory}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *a, **k: Route(kind="play", skill="get_dirt"),
            skills=catalog("get_dirt"),
            gamedata=gamedata(),
        )
        try:
            # Six flat goals close while the snapshot is still empty: the
            # settle-time ladder reads 0 although the world stands at 6.
            for number in range(1, 7):
                loop.records.finish(
                    GoalRecord(
                        goal_id=f"flat-{number:04d}",
                        text="stock",
                        condition="count(andesite) >= 5",
                        status="open",
                    ),
                    status="failed",
                    failure_kind="x",
                    wake=False,
                )
            assert loop._brake_armed
            assert evidence_entries(loop, "drift_brake")[0]["rungs_met"] == 0
            loop.start()

            pump(
                loop,
                lambda: len(evidence_entries(loop, "nothing_to_do")) >= 1,
                what="the braked pick",
            )
            assert len(calls) == 1, "the flat goal was refused in code"
            refusal = evidence_entries(loop, "nothing_to_do")[0]
            assert refusal["reason"] == "not_needed"
            assert "drift brake" in (refusal["detail"] or "")
            arm = evidence_entries(loop, "drift_brake_armed")[0]
            assert arm["rungs_met"] == 6, "the baseline is the fresh read, not 0"
            assert not evidence_entries(loop, "drift_brake_released")
            assert not body.commands

            # A rung gained: the next pick measures 7, logs the release and the
            # flat goal is played again.
            tick = max(int(loop.session.tick or 0), int(loop._next_pick_tick)) + 1
            body.facts["inventory"] = dict(inventory, furnace=1)
            set_tick(loop, body, tick)
            pump(
                loop,
                lambda: len(evidence_entries(loop, "drift_brake_released")) >= 1,
                what="the brake release",
            )
            release = evidence_entries(loop, "drift_brake_released")[0]
            assert release["rungs_met"] == 7 and release["was"] == 6
            pump(loop, lambda: len(body.commands) >= 1, what="the goal after the brake")
            assert body.commands[0].skill == "get_dirt"
        finally:
            loop.close()


def test_a_blocked_rung_goal_ends_the_pick_with_one_visible_nothing_to_do(
    tmp_path,
) -> None:
    """Finding 2: the next rung's own goal blocked, needs covered -> no re-ask.

    Design A6 step 3: the goal is unplayable and its needs are held, so every
    other proposal is stock. The pick ends as one ``nothing_to_do`` with the
    normal back-off instead of paying for a second proposal and idling at
    ``code_only_blocked``.
    """
    calls: list[int] = []

    def propose_fn(*args, **kwargs):
        calls.append(kwargs.get("invalid_count", 0))
        return proposal("has(furnace)", "get a furnace", "furnace")

    with FakeBody(facts={"inventory": dict(RUN6_INVENTORY)}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *a, **k: Route(kind="write"),
            skills=catalog("get_wooden_pickaxe"),
            gamedata=gamedata(),
        )
        try:
            for number in range(2):
                loop.memory.append_goal(
                    GoalRecord(
                        goal_id=f"fail-{number:04d}",
                        text="furnace",
                        condition="has(furnace)",
                        status="failed",
                        failure_kind="inconclusive_body",
                        t_start=loop.memory.now(),
                        t_end=loop.memory.now(),
                    )
                )
            loop.start()
            pump(
                loop,
                lambda: len(evidence_entries(loop, "nothing_to_do")) >= 1,
                what="the blocked pick",
            )
            assert calls == [0], "one paid proposal, no re-ask"
            report = evidence_entries(loop, "nothing_to_do")[0]
            assert report["reason"] == "blocked"
            assert report["gap_ticks"] == NOTHING_TO_DO_GAP_TICKS
            assert loop._invalid_proposals == 0
            assert not evidence_entries(loop, "proposal_invalid")
            assert not body.commands
        finally:
            loop.close()


def test_a_blocked_rung_goal_with_missing_needs_still_asks_for_the_input(
    tmp_path,
) -> None:
    """Finding 2's other half: while the needs are short the model has work to do.

    The rung goal is blocked, but only 3 cobblestone are held: the pick must
    keep going and accept the missing input instead of ending in nothing_to_do.
    """
    calls: list[int] = []
    proposals = [
        proposal("has(furnace)", "get a furnace", "furnace"),
        proposal("count(cobblestone) >= 8", "more stone"),
    ]

    def propose_fn(*args, **kwargs):
        calls.append(kwargs.get("invalid_count", 0))
        return proposals.pop(0)

    inventory = dict(RUN6_INVENTORY, cobblestone=3)
    with FakeBody(skills={"get_cobblestone"}, facts={"inventory": inventory}) as body:
        loop = make_loop(
            body,
            tmp_path,
            propose_fn=propose_fn,
            route_fn=lambda *a, **k: Route(kind="play", skill="get_cobblestone"),
            skills=catalog("get_cobblestone"),
            gamedata=gamedata(),
        )
        try:
            for number in range(2):
                loop.memory.append_goal(
                    GoalRecord(
                        goal_id=f"fail-{number:04d}",
                        text="furnace",
                        condition="has(furnace)",
                        status="failed",
                        failure_kind="inconclusive_body",
                        t_start=loop.memory.now(),
                        t_end=loop.memory.now(),
                    )
                )
            loop.start()
            pump(loop, lambda: len(body.commands) >= 1, what="the missing input")
            assert calls == [0, 1], "the blocked rung goal is counted, then the input"
            assert body.commands[0].skill == "get_cobblestone"
            assert not evidence_entries(loop, "nothing_to_do")
        finally:
            loop.close()


def test_a_compound_stock_goal_is_refused_while_the_skill_is_written(
    tmp_path,
) -> None:
    """Finding 3: one ``and`` must not carry a stock goal past the waiting rule."""
    started, release = threading.Event(), threading.Event()
    proposals = [
        proposal("has(furnace)", "get a furnace", "furnace"),
        proposal("count(#logs) >= 4 and count(stick) >= 12", "stock up"),
    ]
    calls: list[int] = []
    with FakeBody(
        skills={"get_wooden_pickaxe", "get_furnace"},
        facts={"inventory": dict(RUN6_INVENTORY)},
    ) as body:
        loop = _loop(body, tmp_path, proposals, started, release, calls)
        try:
            loop.start()
            pump(loop, lambda: loop._job is not None, what="admission job")
            assert started.wait(4.0), "the furnace skill was never written"
            pump(
                loop,
                lambda: len(evidence_entries(loop, "nothing_to_do")) >= 1,
                what="the refused compound",
            )
            assert calls == [0, 0], "furnace, then exactly one compound proposal"
            report = evidence_entries(loop, "nothing_to_do")[0]
            assert report["reason"] == "not_needed"
            assert "serves a later rung" in (report["detail"] or "")
            assert not body.commands
        finally:
            release.set()
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
