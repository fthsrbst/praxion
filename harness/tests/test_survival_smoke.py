"""Live smoke tests for survival slice 3. Written, not yet run.

1. A port session where no brain ever connects: at night the body pushes
   ``night_watch`` itself, at the survival tier (architecture §5.4), and ends
   it at morning.
2. A hungry body with no food and a cow nearby: the brain starts ``get_food``
   from the snapshot facts (no model call), the body hunts the cow, picks up
   the beef and eats it.

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

from praxion_harness.body import start_session
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
