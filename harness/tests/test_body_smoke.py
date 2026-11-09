"""Level-2 smoke tests for the body binary (Demo-0 step 10a).

A real server, the real forwarder and ``praxion-body`` run one hand-written
goal. The referee, not the body's report, decides whether the inventory holds
the item; the summary's counters are read for ticks, receipts, tick times and
reflex activations.

Run:

```sh
cd harness
python -m pytest -q -m server tests/test_body_smoke.py
```
"""

from __future__ import annotations

import json
import time
from typing import Any, Callable, Dict, Iterable, List, Tuple

import pytest

from praxion_harness.body import BodyRun, start_body, start_session

pytestmark = pytest.mark.server

SEEDS = [101, 202, 303]
BODY = "Praxion"
#: A wooden pickaxe from three logs; a stone pickaxe adds the given cobblestone
#: instead of a mining trip, which is step 9c's job.
WOODEN_TIMEOUT = 240.0
STONE_TIMEOUT = 300.0
REFLEX_TIMEOUT = 360.0
#: One furnace cook is 200 ticks; the rest is the walk to the furnace, the menu
#: round trips and the take.
SMELT_TIMEOUT = 300.0
#: Craft planks, craft a table, place it: seconds, not minutes.
TABLE_TIMEOUT = 240.0


def _count(inventory: Dict[str, int], item: str) -> int:
    return inventory.get(f"minecraft:{item}", inventory.get(item, 0))


def _feet_cell(referee) -> Tuple[int, int, int]:
    """The body's feet cell. ``x // 1`` floors, so negative cells stay right."""
    x, y, z = referee.player_pos(BODY)
    return int(x // 1), int(y // 1), int(z // 1)


def _intent_starts(events: Iterable[dict], primitive: str) -> List[dict]:
    """Every ``intent_started`` for ``primitive``, in log order."""
    return [
        event
        for event in events
        if event.get("kind") == "intent_started" and event.get("primitive") == primitive
    ]


def _first_arg(event: dict) -> str:
    args = event.get("args") or []
    return str(args[0]) if args else ""


def _setup_peaceful(fixture, referee, run: BodyRun) -> None:
    """Daytime, no mobs, and the items the goal would otherwise gather."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "oak_log", 3)
    fixture.give(BODY, "cobblestone", 3)


def _context(run: BodyRun, summary: Dict[str, Any], extra: Iterable[str] = ()) -> str:
    events = run.events()
    tail = events[-25:]
    lines = [
        f"run dir: {run.run_dir}",
        f"summary: {json.dumps(summary, indent=2)}",
        f"body output tail:\n{run.output()[-2000:]}",
        f"events tail: {json.dumps(tail, indent=2)}",
        *extra,
    ]
    return "\n".join(lines)


def _assert_tick_times(summary: Dict[str, Any]) -> None:
    times = summary["tick_time"]
    assert times["samples"] > 0, "no tick was measured"
    assert times["work_p50_ms"] >= 0.0
    assert times["work_p99_ms"] >= times["work_p50_ms"]
    assert times["period_p99_ms"] >= times["period_p50_ms"]


@pytest.mark.parametrize("seed", SEEDS)
def test_wooden_pickaxe_from_given_logs(seed: int) -> None:
    with start_body(
        seed,
        "get_wooden_pickaxe",
        setup=_setup_peaceful,
        max_ticks=6000,
        start_delay_ticks=80,
    ) as run:
        summary = run.wait_for_outcome(WOODEN_TIMEOUT)
        inventory = run.referee.player_inventory(BODY)
        context = _context(run, summary, [f"inventory: {inventory}"])

        assert summary["goal"]["skill"] == "get_wooden_pickaxe", context
        assert summary["goal"]["outcome"] == "done", context
        assert _count(inventory, "wooden_pickaxe") >= 1, "the referee must see the pickaxe\n" + context
        assert summary["ticks"] > 0, context
        assert sum(summary["receipts"].values()) > 0, "no receipt settled\n" + context
        assert "reflex_activations" in summary, context
        _assert_tick_times(summary)
        assert run.referee.player_health(BODY) > 0.0, context


@pytest.mark.parametrize("seed", SEEDS)
def test_stone_pickaxe_from_given_cobblestone(seed: int) -> None:
    with start_body(
        seed,
        "get_stone_pickaxe",
        setup=_setup_peaceful,
        max_ticks=6000,
        start_delay_ticks=80,
    ) as run:
        summary = run.wait_for_outcome(STONE_TIMEOUT)
        inventory = run.referee.player_inventory(BODY)
        context = _context(run, summary, [f"inventory: {inventory}"])

        assert summary["goal"]["skill"] == "get_stone_pickaxe", context
        assert summary["goal"]["outcome"] == "done", context
        assert _count(inventory, "stone_pickaxe") >= 1, "the referee must see the pickaxe\n" + context
        assert summary["ticks"] > 0, context
        assert sum(summary["receipts"].values()) > 0, context
        _assert_tick_times(summary)
        assert run.referee.player_health(BODY) > 0.0, context


def _furnace_setup(fuel: str, count: int) -> Callable:
    """Peaceful daytime, one raw iron, the fuel, a furnace two cells east."""

    def setup(fixture, referee, run: BodyRun) -> None:
        fixture.advance_time(False)
        fixture.spawn_mobs(False)
        fixture.time_set(1000)
        fixture.give(BODY, "raw_iron", 1)
        fixture.give(BODY, fuel, count)
        px, py, pz = _feet_cell(referee)
        # `near(furnace, 4)` spends no ray, but the walk and the Use click need
        # the furnace's face in sight of the eye: clear the cells between.
        fixture.fill(px + 1, py, pz, px + 2, py + 1, pz, "air")
        fixture.setblock(px + 2, py, pz, "furnace")

    return setup


@pytest.mark.parametrize(
    "fuel, count",
    [pytest.param("coal", 1, id="coal"), pytest.param("oak_planks", 2, id="planks")],
)
def test_iron_ingot_from_given_raw_iron(fuel: str, count: int) -> None:
    with start_body(
        101,
        "get_iron_ingot",
        setup=_furnace_setup(fuel, count),
        max_ticks=8000,
        start_delay_ticks=120,
    ) as run:
        summary = run.wait_for_outcome(SMELT_TIMEOUT)
        inventory = run.referee.player_inventory(BODY)
        events = run.events()
        context = _context(run, summary, [f"inventory: {inventory}", f"fuel: {fuel} x{count}"])

        assert summary["goal"]["skill"] == "get_iron_ingot", context
        assert summary["goal"]["outcome"] == "done", context
        assert _count(inventory, "iron_ingot") >= 1, "the referee must see the ingot\n" + context
        assert _count(inventory, "raw_iron") == 0, "the raw iron was not consumed\n" + context
        smelts = _intent_starts(events, "smelt")
        assert len(smelts) == 1, (
            f"exactly one smelt call, no retry loop, got {len(smelts)}\n" + context
        )
        assert run.referee.player_health(BODY) > 0.0, context


def _setup_sword_with_a_table(fixture, referee, run: BodyRun) -> None:
    """Peaceful daytime, the sword's items given, a table near but far to click."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "cobblestone", 2)
    fixture.give(BODY, "stick", 1)
    px, py, pz = _feet_cell(referee)
    # `near(crafting_table, 4)` is a sphere of integer cells around the eye
    # cell, and the eye sits one cell above the feet: a table four cells away
    # diagonally is 3^2 + 3^2 + 1^2 = 19, over the 16 the fact allows, so the
    # skill would fall through to exploring for logs. Three east and two south
    # is 14, well inside; its centre is 3.8 blocks from the eye, so from the
    # far corner of the cell the station has to walk before the pick reaches it.
    fixture.fill(px + 1, py, pz, px + 3, py + 1, pz + 2, "air")
    fixture.setblock(px + 3, py, pz + 2, "crafting_table")


def test_craft_walks_to_a_table_out_of_reach() -> None:
    with start_body(
        101,
        "get_stone_sword",
        setup=_setup_sword_with_a_table,
        max_ticks=6000,
        start_delay_ticks=120,
    ) as run:
        summary = run.wait_for_outcome(STONE_TIMEOUT)
        inventory = run.referee.player_inventory(BODY)
        events = run.events()
        context = _context(run, summary, [f"inventory: {inventory}"])

        assert summary["goal"]["outcome"] == "done", context
        assert _count(inventory, "stone_sword") >= 1, (
            "the referee must see the sword\n" + context
        )
        needs_table = [
            event
            for event in events
            if event.get("kind") == "primitive_finished"
            and event.get("primitive") == "craft"
            and event.get("outcome") == "failed"
            and event.get("failure_kind") == "needs_table"
        ]
        assert not needs_table, "craft failed needs_table although the table is near\n" + context


def _setup_logs_only(fixture, referee, run: BodyRun) -> None:
    """Peaceful daytime and three logs: planks and a table are all it needs."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "oak_log", 3)


def test_a_placed_table_is_known_at_once() -> None:
    with start_body(
        101,
        "get_crafting_table_near",
        setup=_setup_logs_only,
        max_ticks=6000,
        start_delay_ticks=120,
    ) as run:
        summary = run.wait_for_outcome(TABLE_TIMEOUT)
        events = run.events()
        context = _context(run, summary)

        assert summary["goal"]["outcome"] == "done", context
        places = [
            event
            for event in _intent_starts(events, "place")
            if _first_arg(event) == "crafting_table"
        ]
        assert places, "the skill never placed a table\n" + context
        # The place receipt writes the cell into the known map, so the fact
        # must answer true at once; before that fix the skill explored for
        # logs and crafted a second table (backlog, 2026-11-03).
        placed = places[-1]
        after = [
            event
            for event in _intent_starts(events, "explore") + _intent_starts(events, "mine")
            if event["seq"] > placed["seq"]
        ]
        assert not after, (
            "after the place the body explored or mined: `near` did not know the "
            "table it had just placed\n" + context
        )
        table_crafts = [
            event
            for event in _intent_starts(events, "craft")
            if _first_arg(event) == "crafting_table"
        ]
        assert len(table_crafts) == 1, (
            f"exactly one table craft, got {len(table_crafts)}\n" + context
        )


def _arena_zombie(fixture, referee, distance: int = 12) -> None:
    """A flat stone strip from the player to where a zombie will be ``distance`` blocks east.

    The earlier ring probe could put the zombie a block below the player in a
    one-wide trench it never left (2026-11-03: the goal finished in 124 ticks
    with no reflex). A flat, open strip at the player's level makes the
    zombie's walk, and so the moment it arrives, repeatable. The zombie itself
    is summoned later, once a craft is in flight (`_summon_zombie`).
    """
    px, py, pz = _feet_cell(referee)
    fixture.fill(px - 2, py, pz - 3, px + distance + 2, py + 3, pz + 3, "air")
    fixture.fill(px - 2, py - 1, pz - 3, px + distance + 2, py - 1, pz + 3, "stone")


def _summon_zombie(fixture, referee, distance: int = 3) -> None:
    """One one-hit zombie ``distance`` blocks east of the body, on its level."""
    px, py, pz = _feet_cell(referee)
    fixture.summon(
        "zombie", px + distance + 0.5, py, pz + 0.5, "{Health:1.0f,PersistenceRequired:1b}"
    )


def _wait_for_first_craft(run: BodyRun, timeout: float = 60.0) -> None:
    """Block until a ``craft`` is in flight, so the summon lands mid-primitive.

    A reflex that fires between two primitives preempts nothing and logs no
    ``preempted`` outcome, so the test waits for the primitive to start rather
    than for a fixed number of ticks.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if _intent_starts(run.events(), "craft"):
            return
        if run.process.poll() is not None:
            break
        time.sleep(0.05)
    raise AssertionError(
        f"no craft started within {timeout:.0f}s (or the body exited); the reflex "
        f"would have nothing to preempt (run dir: {run.run_dir}):\n{run.output()[-2000:]}"
    )


def test_melee_reflex_preempts_and_the_skill_resumes() -> None:
    def setup(fixture, referee, run: BodyRun) -> None:
        fixture.advance_time(False)
        # Only the summoned zombie: random night spawns made the timing vary.
        fixture.spawn_mobs(False)
        fixture.time_set(13000)
        # No sticks and no table: the skill crafts planks, sticks and a table
        # first, so a craft is in flight when the zombie is summoned.
        fixture.give(BODY, "oak_log", 3)
        fixture.give(BODY, "cobblestone", 3)
        _arena_zombie(fixture, referee)

    with start_body(
        101,
        "get_stone_sword",
        setup=setup,
        max_ticks=8000,
        start_delay_ticks=80,
        linger_ticks=100,
    ) as run:
        # The setup runs before the skill starts, so the summon goes here: the
        # zombie arrives three blocks out while `craft(#planks)` is running.
        _wait_for_first_craft(run)
        _summon_zombie(run.fixture, run.referee)
        summary = run.wait_for_outcome(REFLEX_TIMEOUT)
        events = run.events()
        context = _context(run, summary)

        assert summary["deaths"] == 0, "the body died\n" + context
        assert run.referee.death_count(BODY) == 0, "the referee saw a death\n" + context
        assert run.referee.player_health(BODY) > 0.0, context
        assert sum(summary["reflex_activations"].values()) >= 1, (
            "no reflex acquired the body\n" + context
        )

        preempted = [
            event
            for event in events
            if event.get("kind") == "primitive_finished" and event.get("outcome") == "preempted"
        ]
        assert preempted, "the reflex preempted no primitive\n" + context

        reflex_seq = next(
            (
                event["seq"]
                for event in events
                if event.get("kind") == "owner_changed"
                and str(event.get("owner", "")).startswith("reflex:")
            ),
            None,
        )
        assert reflex_seq is not None, "no owner_changed reflex event\n" + context
        resumed = any(
            event.get("kind") == "owner_changed"
            and event["seq"] > reflex_seq
            and str(event.get("owner", "")).startswith("skill:")
            for event in events
        )
        assert resumed or summary["goal"]["outcome"] == "done", (
            "the skill never resumed after the reflex\n" + context
        )
        took_back = any(
            event.get("kind") == "owner_changed"
            and event["seq"] > reflex_seq
            and not str(event.get("owner", "")).startswith("reflex:")
            for event in events
        )
        assert took_back, "no non-reflex owner took the body back after the reflex\n" + context
        # The arena is flat and the items are known, so the goal must finish:
        # a failure here (e.g. `no_result` from a craft the reflex cut short)
        # is a regression, not bad luck.
        assert summary["goal"]["outcome"] == "done", context


#: A hunger effect this strong takes the bar from full to near empty in about
#: two seconds (exhaustion 0.025 x (amplifier + 1) per tick), long enough for
#: the arbiter's hunger need to start and short enough that the meal is not
#: fighting it for long.
HUNGER_EFFECT_SECONDS = 4
HUNGER_EFFECT_AMPLIFIER = 120
EAT_TIMEOUT = 120.0


def _setup_hungry_with_bread(fixture, referee, run: BodyRun) -> None:
    """Peaceful daytime, eight bread, and a hunger effect that empties the bar."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "bread", 8)
    fixture.effect(
        BODY, "minecraft:hunger", HUNGER_EFFECT_SECONDS, HUNGER_EFFECT_AMPLIFIER
    )


def test_the_body_eats_when_hungry() -> None:
    """Decision 2026-11-09 "Five next areas": hunger runs the ``eat_food`` skill.

    The arbiter used to send one use click that could not finish a 32-tick
    meal. Now it pushes ``eat_food`` with the food's item name, the tested
    ``eat`` primitive holds the use button, and the bar rises. The referee, not
    the body's report, decides: the food level goes up and the bread count goes
    down. Written for the orchestrator's live run.
    """
    with start_session(
        101, setup=_setup_hungry_with_bread, max_ticks=6000, start_delay_ticks=0
    ) as session:
        deadline = time.monotonic() + EAT_TIMEOUT
        food = bread = None
        while time.monotonic() < deadline:
            food = session.referee.player_food(BODY)
            bread = _count(session.referee.player_inventory(BODY), "bread")
            if bread < 8 and food > 16:
                break
            time.sleep(0.5)
        events = session.events()
        pushes = [
            event
            for event in events
            if event.get("kind") == "intent_pushed" and event.get("skill") == "eat_food"
        ]
        context = "\n".join(
            [
                f"run dir: {session.run_dir}",
                f"food {food}, bread {bread}",
                f"eat_food pushes: {pushes}",
                f"body output tail:\n{session.output()[-2000:]}",
            ]
        )
        assert pushes, "hunger never pushed the eat_food skill\n" + context
        assert pushes[0].get("args") == ["bread"], (
            "the argument is the item name, never a slot number\n" + context
        )
        assert bread is not None and bread < 8, "no bread was eaten\n" + context
        assert food is not None and food > 16, "the food bar did not rise\n" + context
        assert session.referee.player_health(BODY) > 0.0, context


HUNT_TIMEOUT = 240.0


def _setup_cow_arena(fixture, referee, run: BodyRun) -> None:
    """Peaceful daytime, a sword, one cow six blocks east.

    ``/give`` may put the sword in the main inventory rather than the hotbar;
    ``attack`` brings the best carried weapon into the main hand either way
    (review finding 8), so this setup does not depend on the slot it lands in.
    """
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "stone_sword", 1)
    px, py, pz = _feet_cell(referee)
    fixture.summon("cow", px + 6.5, py, pz + 0.5, "{PersistenceRequired:1b}")


def test_get_food_hunts_a_summoned_cow_and_collects_beef() -> None:
    """Decision 2026-11-09 "Five next areas": ``get_food`` -> ``hunt``.

    The body walks to the cow, hits it with the sword in hand, picks up the
    beef and the goal ends ``done``; the referee, not the body, decides that
    the inventory holds beef. Written for the orchestrator's live run.
    """
    with start_body(
        101,
        "get_food",
        ["1"],
        setup=_setup_cow_arena,
        max_ticks=8000,
        start_delay_ticks=80,
    ) as run:
        summary = run.wait_for_outcome(HUNT_TIMEOUT)
        inventory = run.referee.player_inventory(BODY)
        events = run.events()
        context = _context(run, summary, [f"inventory: {inventory}"])

        assert summary["goal"]["skill"] == "get_food", context
        assert summary["goal"]["outcome"] == "done", context
        assert _count(inventory, "beef") >= 1, "the referee must see beef\n" + context
        assert _intent_starts(events, "attack"), "the body never attacked\n" + context
        assert _intent_starts(events, "pickup"), "the drop was not picked up by `pickup`\n" + context
        assert run.referee.player_health(BODY) > 0.0, context


def _setup_night_with_a_sword(fixture, referee, run: BodyRun) -> None:
    """Frozen night, no natural spawns, a sword, one weak zombie six blocks east.

    The sword may land in the main inventory; ``attack`` equips the best
    carried weapon itself (review finding 8).
    """
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(14000)
    fixture.give(BODY, "stone_sword", 1)
    fixture.give(BODY, "bread", 4)
    px, py, pz = _feet_cell(referee)
    # Health 2: a stone sword kills it in one hit, so a failure is the body's.
    fixture.summon(
        "zombie", px + 6.5, py, pz + 0.5, "{Health:2.0f,PersistenceRequired:1b}"
    )


def _zombie_alive(run: BodyRun) -> bool:
    reply = run.fixture.raw("execute if entity @e[type=zombie]")
    return "passed" in reply.lower()


def test_night_watch_kills_a_zombie_with_the_sword_and_survives() -> None:
    """Armed and healthy at night, the body hunts the hostile in sight.

    The clock is frozen at night, so ``night_watch`` itself never ends: the
    test waits for the zombie to be gone, then checks the body lived.
    """
    with start_body(
        101,
        "night_watch",
        setup=_setup_night_with_a_sword,
        max_ticks=8000,
        start_delay_ticks=80,
    ) as run:
        deadline = time.monotonic() + HUNT_TIMEOUT
        while time.monotonic() < deadline and _zombie_alive(run):
            if run.process.poll() is not None:
                break
            time.sleep(1.0)
        summary = run.summary() or {}
        context = _context(run, summary)

        assert not _zombie_alive(run), "the zombie is still alive\n" + context
        assert run.referee.death_count(BODY) == 0, "the referee saw a death\n" + context
        assert run.referee.player_health(BODY) > 0.0, context


REACH_TIMEOUT = 240.0
#: A log this many blocks above the feet cell. The eye is 1.62 above the feet
#: and the reach is 4.5 (4.25 for the body's own margin), so six blocks up is
#: 4.88 above the eye: out of reach from the ground, and 3.88 above it after one
#: placed block.
HIGH_LOG_HEIGHT = 6


def _setup_high_log(fixture, referee, run: BodyRun) -> None:
    """Peaceful daytime, two logs to climb on, one lone log six blocks up.

    Two, not three: the goal below asks for three, so the done rule cannot fire
    on the first tick and the climb gets its turn (review finding 2 — with three
    logs given, `count(#logs) >= 3` was already true when the goal started, the
    skill ended at once and `pillar` never ran). Two logs also cover the one
    placement the six-block log needs, with one left over after the climb.
    """
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "oak_log", 2)
    px, py, pz = _feet_cell(referee)
    fixture.setblock(px, py + HIGH_LOG_HEIGHT, pz, "oak_log")


def test_the_body_pillars_up_to_a_log_it_cannot_reach() -> None:
    """Decision 2026-11-09 "Five next areas": climb a log at a time.

    The only log in sight is six blocks overhead. ``mine`` fails
    ``out_of_reach``, ``get_wood`` places a held log under the feet (``pillar``)
    and mines it from the top of that block; the referee, not the body, sees the
    log cell become air. The goal asks for three logs while two are given, so it
    cannot be done before the climb; the climb spends one of them again and the
    cut log pays one back, still short of three, so the goal does not settle and
    the test watches the world instead. Written for the orchestrator's live run.
    """
    with start_body(
        101,
        "get_wood",
        ["3"],
        setup=_setup_high_log,
        max_ticks=8000,
        start_delay_ticks=80,
    ) as run:
        px, py, pz = _feet_cell(run.referee)
        cell = (px, py + HIGH_LOG_HEIGHT, pz)
        deadline = time.monotonic() + REACH_TIMEOUT
        gone = False
        while time.monotonic() < deadline:
            if run.referee.block_is(*cell, "air"):
                gone = True
                break
            if run.process.poll() is not None:
                break
            time.sleep(1.0)
        events = run.events()
        context = "\n".join(
            [
                f"run dir: {run.run_dir}",
                f"log cell {cell}, feet {_feet_cell(run.referee)}",
                f"pillar starts: {_intent_starts(events, 'pillar')}",
                f"body output tail:\n{run.output()[-2000:]}",
            ]
        )
        assert _intent_starts(events, "pillar"), "the body never climbed\n" + context
        assert _intent_starts(events, "mine"), "nothing was mined\n" + context
        assert gone, "the log six blocks up was never broken\n" + context
        assert run.referee.player_health(BODY) > 0.0, context


def _setup_axe_from_planks(fixture, referee, run: BodyRun) -> None:
    """Peaceful daytime; eight planks, two sticks, a table two blocks east and
    a three-log trunk four blocks east."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "oak_planks", 8)
    fixture.give(BODY, "stick", 2)
    px, py, pz = _feet_cell(referee)
    fixture.setblock(px + 2, py, pz, "crafting_table")
    for dy in range(3):
        fixture.setblock(px + 4, py + dy, pz + 1, "oak_log")


def test_get_wood_crafts_an_axe_from_surplus_planks_and_chops_with_it() -> None:
    """Eight planks and two sticks beside a table: the axe comes before the chop."""
    with start_body(
        101,
        "get_wood",
        ["1"],
        setup=_setup_axe_from_planks,
        max_ticks=8000,
        start_delay_ticks=80,
    ) as run:
        summary = run.wait_for_outcome(REACH_TIMEOUT)
        inventory = run.referee.player_inventory(BODY)
        events = run.events()
        context = _context(run, summary, [f"inventory: {inventory}"])

        assert summary["goal"]["outcome"] == "done", context
        assert _count(inventory, "wooden_axe") >= 1, "no axe was crafted\n" + context
        assert _count(inventory, "oak_log") >= 1, "no log was cut\n" + context
        crafts = [
            event
            for event in _intent_starts(events, "craft")
            if _first_arg(event) == "wooden_axe"
        ]
        mines = _intent_starts(events, "mine")
        assert crafts, "the axe was not crafted by `craft`\n" + context
        assert mines, "nothing was mined\n" + context
        assert crafts[0]["seq"] < mines[0]["seq"], "the axe came after the first chop\n" + context
