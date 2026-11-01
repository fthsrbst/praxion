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
from typing import Any, Dict, Iterable

import pytest

from praxion_harness.body import BodyRun, start_body

pytestmark = pytest.mark.server

SEEDS = [101, 202, 303]
BODY = "Praxion"
#: A wooden pickaxe from three logs; a stone pickaxe adds the given cobblestone
#: instead of a mining trip, which is step 9c's job.
WOODEN_TIMEOUT = 240.0
STONE_TIMEOUT = 300.0
REFLEX_TIMEOUT = 360.0


def _count(inventory: Dict[str, int], item: str) -> int:
    return inventory.get(f"minecraft:{item}", inventory.get(item, 0))


def _setup_peaceful(fixture, referee, run: BodyRun) -> None:
    """Daytime, no mobs, and the items the goal would otherwise gather."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "oak_log", 3)
    fixture.give(BODY, "cobblestone", 3)


def _place_table(fixture, referee) -> None:
    """Put a crafting table within reach so the reflex test is not a table hunt."""
    x, y, z = referee.player_pos(BODY)
    for dx, dz in ((1, 0), (0, 1), (-1, 0), (0, -1), (2, 0), (0, 2)):
        cx, cy, cz = int(x) + dx, int(y), int(z) + dz
        if referee.block_is(cx, cy, cz, "minecraft:air") and not referee.block_is(
            cx, cy - 1, cz, "minecraft:air"
        ):
            fixture.setblock(cx, cy, cz, "crafting_table")
            return


def _summon_ground_zombie(fixture, referee) -> None:
    """Summon a zombie six blocks away on solid ground at the player's level.

    A plain ``summon`` at ``x + 6`` drops the zombie off the first cliff (seed
    101's spawn is on a slope): it fell to y=10 and never reached the player.
    Probe the six-block ring for two air blocks over a solid block and put the
    zombie there, still exactly six blocks from the player.

    The zombie has one health point: the reflex still has to acquire the body
    and fight, but the fight ends in a hit or two, so the skill resumes and the
    goal can settle. A full-health zombie keeps re-acquiring the body for the
    whole run and the goal never finishes.
    """
    x, y, z = referee.player_pos(BODY)
    ring = [(6, 0), (0, 6), (-6, 0), (0, -6), (4, 4), (4, -4), (-4, 4), (-4, -4)]
    nbt = "{Health:1.0f,PersistenceRequired:1b}"

    def air(cx: int, cy: int, cz: int) -> bool:
        return referee.block_is(cx, cy, cz, "minecraft:air")

    for dx, dz in ring:
        for dy in (0, -1, 1):
            cy = int(y) + dy
            cx, cz = int(x) + dx, int(z) + dz
            if not air(cx, cy - 1, cz) and air(cx, cy, cz) and air(cx, cy + 1, cz):
                # Clear a one-wide, three-high corridor with a stone floor so
                # the zombie can path to the player instead of falling off the
                # slope or standing behind a wall.
                px, pz = int(x), int(z)
                fixture.fill(
                    min(px, cx), cy, min(pz, cz), max(px, cx), cy + 2, max(pz, cz), "air"
                )
                fixture.fill(
                    min(px, cx), cy - 1, min(pz, cz), max(px, cx), cy - 1, max(pz, cz),
                    "stone",
                )
                fixture.summon("zombie", cx + 0.5, cy, cz + 0.5, nbt)
                return
    fixture.summon("zombie", x + 6, y, z, nbt)


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


def test_melee_reflex_preempts_and_the_skill_resumes() -> None:
    def setup(fixture, referee, run: BodyRun) -> None:
        fixture.advance_time(False)
        fixture.spawn_mobs(True)
        fixture.time_set(13000)
        fixture.give(BODY, "oak_log", 3)
        fixture.give(BODY, "cobblestone", 3)
        fixture.give(BODY, "stick", 1)
        _summon_ground_zombie(fixture, referee)
        _place_table(fixture, referee)

    with start_body(
        101,
        "get_stone_sword",
        setup=setup,
        max_ticks=8000,
        start_delay_ticks=80,
        linger_ticks=100,
    ) as run:
        summary = run.wait_for_outcome(REFLEX_TIMEOUT)
        events = run.events()
        context = _context(run, summary)

        assert summary["deaths"] == 0, "the body died\n" + context
        assert run.referee.death_count(BODY) == 0, "the referee saw a death\n" + context
        assert run.referee.player_health(BODY) > 0.0, context
        assert sum(summary["reflex_activations"].values()) >= 1, (
            "no reflex acquired the body\n" + context
        )

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
        # The goal may legitimately fail after the interruption (no_rule on a
        # moved world); what this test proves is preemption, resume and no death.
        assert summary["goal"]["outcome"] in ("done",) or summary["goal"]["outcome"].startswith(
            "failed("
        ), context
