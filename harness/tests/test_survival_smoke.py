"""Live smoke tests for survival slice 3 and the two search skills. Written,
not yet run.

1. A port session where no brain ever connects: at night the body pushes
   ``night_watch`` itself, at the survival tier (architecture §5.4), and ends
   it at morning.
2. A hungry body with no food and a cow nearby: the brain starts ``get_food``
   from the snapshot facts (no model call), the body hunts the cow, picks up
   the beef and eats it.
3. ``find_block``: the public search wrapper walks until the named block is in
   mining reach (a goal with no inventory condition, decision 2026-11-12
   "Wider tasks" and "Camp").
4. ``find_village``: the bounded search for a village marker, here a bell.

Prerequisite: ``cd body && cargo build --release``.

Run:

```sh
cd harness
python3 -m pytest -q -m server tests/test_survival_smoke.py
```

Decision 2026-11-09 "Five next areas" (survival); the design is
``logs/mac1109/design/survival.out.md``.
"""

from __future__ import annotations

import sys
import time

import pytest

from praxion_harness.body import start_body, start_session
from praxion_harness.paths import repo_root

BRAIN_DIR = repo_root() / "brain"
if str(BRAIN_DIR) not in sys.path:
    sys.path.insert(0, str(BRAIN_DIR))

pytest.importorskip("pydantic", reason="the brain is the other half of the wire")

pytestmark = pytest.mark.server

BODY = "Praxion"
TIMEOUT = 240.0


def _wait(predicate, what: str, timeout: float = TIMEOUT, step=lambda: None) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        step()
        if predicate():
            return
        time.sleep(0.5)
    raise AssertionError(f"timed out waiting for {what}")


def _events(session, kind: str) -> list[dict]:
    return [event for event in session.events() if event.get("kind") == kind]


def test_a_silent_brain_at_night_the_body_runs_night_watch_itself() -> None:
    def setup(fixture, referee, session) -> None:
        fixture.spawn_mobs(False)
        fixture.advance_time(False)
        fixture.time_set(14000)  # dusk is past: is_night

    with start_session(707, setup=setup, max_ticks=10**9) as session:
        # No brain connects at all: the port is "not connected", so the short
        # wait (about five seconds of idle ticks) applies.
        _wait(
            lambda: any(
                event.get("tier") == "survival" and event.get("skill") == "night_watch"
                for event in _events(session, "intent_pushed")
            ),
            "the body's own night_watch at the survival tier",
            timeout=90.0,
        )
        # Morning ends it by its own rule, and the body settles the goal.
        session.fixture.time_set(1000)
        _wait(
            lambda: any(
                event.get("command_id", "").startswith("body-night-") and event.get("state") == "settled"
                for event in _events(session, "command_receipt")
            ),
            "the night watch settled at morning",
            timeout=90.0,
        )


def test_a_hungry_body_with_no_food_hunts_a_cow_and_eats_the_beef(tmp_path) -> None:
    from praxion_brain.evidence import Evidence
    from praxion_brain.governor import Governor
    from praxion_brain.ledger import Ledger
    from praxion_brain.loop import Loop
    from praxion_brain.memory import Memory
    from praxion_brain.port import PortClient
    from praxion_brain.skills import load_catalog

    def no_model(*args, **kwargs):
        raise RuntimeError("survival decisions make no model call")

    def setup(fixture, referee, session) -> None:
        fixture.spawn_mobs(False)
        fixture.advance_time(False)
        fixture.time_set(1000)
        x, y, z = referee.player_pos(BODY)
        # A cow a few blocks away, in sight on open ground.
        fixture.summon("cow", x + 6, y, z + 0.5, "{PersistenceRequired:1b}")
        # A short, maximal hunger effect drains the food bar to the threshold.
        fixture.effect(BODY, "hunger", 8, 255)

    with start_session(707, setup=setup, max_ticks=10**9) as session:
        evidence = Evidence(tmp_path / "evidence")
        ledger = Ledger(tmp_path / "ledger.jsonl")
        loop = Loop(
            PortClient(session.port),
            world_id=session.run_dir.name,
            memory=Memory(session.run_dir.name, root=tmp_path / "worlds", evidence=evidence),
            catalog=load_catalog(),
            ledger=ledger,
            evidence=evidence,
            governor=Governor(ledger, evidence=evidence),
            propose_fn=no_model,
            map_fn=no_model,
            logger=lambda _message: None,
        )
        try:
            loop.start()

            def kinds(name):
                path = evidence.path
                if not path.is_file():
                    return []
                return [line for line in path.read_text(encoding="utf-8").splitlines() if f'"{name}"' in line]

            step = lambda: loop.step(0.2)  # noqa: E731
            _wait(lambda: kinds("survival_goal"), "the get_food goal", step=step)
            _wait(
                lambda: session.referee.player_inventory(BODY).get("minecraft:beef", 0) > 0
                or any(
                    event.get("primitive") == "eat" and event.get("outcome") == "confirmed"
                    for event in _events(session, "primitive_finished")
                ),
                "beef in the inventory or eaten",
                step=step,
            )
            _wait(
                lambda: any(
                    event.get("primitive") == "eat" and event.get("outcome") == "confirmed"
                    for event in _events(session, "primitive_finished")
                ),
                "the body eating the beef",
                step=step,
            )
        finally:
            loop.close()


#: One `explore` call walks for up to 1200 ticks; the search needs one walk to
#: a block already in plain sight.
SEARCH_TIMEOUT = 240.0


def _feet_cell(referee) -> tuple[int, int, int]:
    """The body's feet cell. ``x // 1`` floors, so negative cells stay right."""
    x, y, z = referee.player_pos(BODY)
    return int(x // 1), int(y // 1), int(z // 1)


def _flat_ground(fixture, referee, distance: int) -> None:
    """A flat, open stone strip from the body to ``distance`` blocks east.

    Open air keeps the target in the view scan and the strip keeps the walk
    straight, so a search that fails is the skill's and not the terrain's.
    """
    px, py, pz = _feet_cell(referee)
    fixture.fill(px - 6, py, pz - 6, px + distance + 2, py + 3, pz + 6, "air")
    fixture.fill(px - 6, py - 1, pz - 6, px + distance + 2, py - 1, pz + 6, "stone")


def test_find_block_walks_to_a_named_block() -> None:
    """``find_block(oak_log)`` walks until the log is in mining reach.

    The only log in the world stands eight blocks east: outside the five-block
    reach of the skill's own ``done`` check, and inside the sight of the known
    map, so ``search_surface`` has to walk to it. The goal is verified by the
    skill's ``done``, which is what a goal with no inventory condition rests on
    (decision 2026-11-12 "Wider tasks"). Written for the orchestrator's live
    run.
    """
    log_at = {}

    def setup(fixture, referee, run) -> None:
        fixture.spawn_mobs(False)
        fixture.advance_time(False)
        fixture.time_set(1000)
        _flat_ground(fixture, referee, 10)
        px, py, pz = _feet_cell(referee)
        fixture.setblock(px + 8, py, pz, "oak_log")
        log_at["cell"] = (px + 8, py, pz)

    with start_body(
        707,
        "find_block",
        ["oak_log"],
        setup=setup,
        max_ticks=8000,
        start_delay_ticks=80,
    ) as run:
        summary = run.wait_for_outcome(SEARCH_TIMEOUT)
        events = run.events()
        cell = log_at["cell"]
        x, _y, _z = run.referee.player_pos(BODY)
        context = "\n".join(
            [
                f"run dir: {run.run_dir}",
                f"summary: {summary}",
                f"log cell {cell}, body x {x:.2f}",
                f"explore starts: {_events(run, 'intent_started')}",
                f"body output tail:\n{run.output()[-2000:]}",
            ]
        )
        assert summary["goal"]["skill"] == "find_block", context
        assert summary["goal"]["outcome"] == "done", context
        assert any(
            event.get("kind") == "intent_started" and event.get("primitive") == "explore"
            for event in events
        ), "the body never explored\n" + context
        assert abs(x - cell[0]) <= 6.0, "the body never walked to the log\n" + context
        assert run.referee.death_count(BODY) == 0, "the referee saw a death\n" + context


def test_find_village_walks_to_a_bell() -> None:
    """``find_village`` ends on a village marker: a bell within eight blocks.

    A lone bell twenty blocks east is the marker; ``explore(bell)`` walks to it
    and the skill's ``done`` ends the goal. Villages are rare in a real world,
    so the search is bounded: this test only proves the marker path, not that
    every seed has a village within reach. Written for the orchestrator's live
    run.
    """
    bell_at = {}

    def setup(fixture, referee, run) -> None:
        fixture.spawn_mobs(False)
        fixture.advance_time(False)
        fixture.time_set(1000)
        _flat_ground(fixture, referee, 24)
        px, py, pz = _feet_cell(referee)
        fixture.setblock(px + 20, py, pz, "bell")
        bell_at["cell"] = (px + 20, py, pz)

    with start_body(
        707,
        "find_village",
        setup=setup,
        max_ticks=12000,
        start_delay_ticks=80,
    ) as run:
        summary = run.wait_for_outcome(SEARCH_TIMEOUT)
        events = run.events()
        cell = bell_at["cell"]
        x, _y, _z = run.referee.player_pos(BODY)
        context = "\n".join(
            [
                f"run dir: {run.run_dir}",
                f"summary: {summary}",
                f"bell cell {cell}, body x {x:.2f}",
                f"explore starts: {[e for e in events if e.get('kind') == 'intent_started']}",
                f"body output tail:\n{run.output()[-2000:]}",
            ]
        )
        assert summary["goal"]["skill"] == "find_village", context
        assert summary["goal"]["outcome"] == "done", context
        assert any(
            event.get("kind") == "intent_started" and event.get("primitive") == "explore"
            for event in events
        ), "the body never explored\n" + context
        assert abs(x - cell[0]) <= 8.0, "the body never walked to the bell\n" + context
        assert run.referee.death_count(BODY) == 0, "the referee saw a death\n" + context
