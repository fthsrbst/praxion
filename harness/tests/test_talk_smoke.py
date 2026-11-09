"""Live smoke test for the talking bot (talk slice 1). Written, not yet run.

A fake operator (the spike's headless client in ``--operator`` mode, logged in as
the player the body obeys) sends ``durum`` and ``p: 16 taş getir`` in game chat.
The body answers ``durum`` itself, the brain maps the task (its prefix stripped)
to ``count(cobblestone) >= 16`` and the bot plays ``get_cobblestone``. Sixteen, not
eight: the brain starts autonomous at once and its code-only fallback is
``get_cobblestone(8)``, so an eight-stone task arrives already met and is
rightly refused (live run 2026-11-09). Checked from
the outside: the body's event log (`chat_received`, `chat_sent`), the brain's
evidence and the referee's inventory.

Decision 2026-11-09 "Chat: control words bare, tasks with a prefix": the control
word is bare, the task needs ``p:``.

Prerequisites, once:

```sh
cd body && cargo build --release
cd ../spikes/chat && cargo build          # builds the fake operator
```

Run:

```sh
cd harness
python3 -m pytest -q -m server tests/test_talk_smoke.py
```

Decision 2026-11-09 "Five next areas" (the talking bot); the chat spike in
``docs/sessions/09-commands.md`` measured the packets this test relies on.
"""

from __future__ import annotations

import subprocess
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
OPERATOR = "Fatih"
SPIKE = repo_root() / "spikes" / "chat" / "target" / "debug" / "chat-spike"
TIMEOUT = 420.0


def _setup(fixture, referee, session) -> None:
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "wooden_pickaxe", 1)


def _chat_sent(session) -> list[str]:
    return [
        event["text"]
        for event in session.events()
        if event.get("kind") == "chat_sent"
    ]


def _wait(predicate, what: str, timeout: float = TIMEOUT, pause: float = 1.0) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(pause)
    raise AssertionError(f"timed out waiting for {what}")


def test_the_bot_answers_durum_and_plays_a_chat_task(tmp_path) -> None:
    if not SPIKE.is_file():
        pytest.skip(f"build the fake operator first: cd spikes/chat && cargo build ({SPIKE})")

    from praxion_brain.evidence import Evidence
    from praxion_brain.governor import Governor
    from praxion_brain.ledger import Ledger
    from praxion_brain.loop import Loop
    from praxion_brain.memory import Memory
    from praxion_brain.port import PortClient
    from praxion_brain.skills import load_catalog

    def no_model(*args, **kwargs):
        # The fast path maps "16 taş getir" without a model. Anything else here
        # (a free sentence, the autonomous pick after the task) would reach the
        # real router; with no model configured that path falls back to code
        # only, so nothing is paid.
        raise RuntimeError("no model in the smoke test")

    with start_session(
        606,
        setup=_setup,
        max_ticks=10**9,
        extra_args=["--operator", OPERATOR],
    ) as session:
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
        operator = subprocess.Popen(
            [
                str(SPIKE),
                "--addr", f"127.0.0.1:{session.server.port}",
                "--operator", OPERATOR,
                "--say", "15:durum",
                "--say", "30:p: 16 taş getir",
                "--seconds", "400",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            loop.start()
            stop_at = time.monotonic() + TIMEOUT

            def stepping(predicate):
                def check():
                    # Drain the port for about a second: `step` handles one
                    # message, and the body streams several a second, so one
                    # step per check let the brain fall minutes behind.
                    until = time.monotonic() + 1.0
                    while time.monotonic() < until:
                        loop.step(0.05)
                    return predicate()

                return check

            # 1. The body answers `durum` with its own facts, brain or not.
            _wait(stepping(lambda: any(t.startswith("durum:") for t in _chat_sent(session))), "the durum answer", pause=0.0)

            # 2. The brain maps the sentence, acknowledges and plays it.
            _wait(stepping(lambda: any(t.startswith("tamam:") for t in _chat_sent(session))), "the acknowledgement", pause=0.0)
            _wait(
                stepping(
                    lambda: session.referee.player_inventory(BODY).get("minecraft:cobblestone", 0) >= 16
                ),
                "sixteen cobblestone",
                timeout=max(10.0, stop_at - time.monotonic()),
                pause=0.0,
            )
            _wait(stepping(lambda: any(t.startswith("bitti:") for t in _chat_sent(session))), "the done line", pause=0.0)

            # 3. Every claim in chat has a closed evidence line.
            sent = [
                line for line in evidence.path.read_text(encoding="utf-8").splitlines()
                if '"narrate_sent"' in line
            ]
            assert sent, "no narration line was confirmed by the body"
        finally:
            operator.terminate()
            loop.close()
