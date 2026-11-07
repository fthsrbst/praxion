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


def _arena_zombie(fixture, referee, distance: int = 12) -> None:
    """A flat stone strip from the player to a zombie ``distance`` blocks east.

    The earlier ring probe could put the zombie a block below the player in a
    one-wide trench it never left (2026-11-03: the goal finished in 124 ticks
    with no reflex). A flat, open strip at the player's level makes the
    zombie's walk, and so the moment it arrives, repeatable.
    """
    x, y, z = referee.player_pos(BODY)
    px, py, pz = int(x // 1), int(y // 1), int(z // 1)
    fixture.fill(px - 2, py, pz - 3, px + distance + 2, py + 3, pz + 3, "air")
    fixture.fill(px - 2, py - 1, pz - 3, px + distance + 2, py - 1, pz + 3, "stone")
    fixture.summon(
        "zombie", px + distance + 0.5, py, pz + 0.5, "{Health:1.0f,PersistenceRequired:1b}"
    )


def test_melee_reflex_preempts_and_the_skill_resumes() -> None:
    def setup(fixture, referee, run: BodyRun) -> None:
        fixture.advance_time(False)
        # Only the summoned zombie: random night spawns made the timing vary.
        fixture.spawn_mobs(False)
        fixture.time_set(13000)
        # No sticks and no table: the skill crafts planks, sticks and a table
        # first, so it is still running when the zombie arrives.
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
        # The arena is flat and the items are known, so the goal must finish:
        # a failure here (e.g. `no_result` from a craft the reflex cut short)
        # is a regression, not bad luck.
        assert summary["goal"]["outcome"] == "done", context
