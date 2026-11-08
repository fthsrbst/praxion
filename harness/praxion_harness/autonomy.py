"""Run the brain loop against a live body for a while: the autonomy smoke run.

    python -m praxion_harness.autonomy --seed 303 --minutes 25

Starts a fresh-seed body in port mode (peaceful, daytime, nothing given),
prints the address a real client can join to watch, runs
``python -m praxion_brain run --port N`` until the time is up, then prints the
referee inventory and where the evidence is.
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import time

from .body import start_session
from .paths import repo_root


def _peaceful(fixture, referee, session) -> None:
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--seed", type=int, default=303)
    parser.add_argument("--minutes", type=float, default=25.0)
    args = parser.parse_args(argv)

    root = repo_root()
    with start_session(args.seed, setup=_peaceful, max_ticks=10**9, keep=True) as session:
        print(f"Watch: Minecraft 26.3 > Direct Connection > 127.0.0.1:{session.server.port}", flush=True)
        print(f"body port {session.port}, run_dir {session.run_dir}", flush=True)
        print("Console: http://127.0.0.1:8765/", flush=True)
        # The body watches this directory: a skill admitted mid-run is live on
        # the next command, with no restart.
        print(f"hot skills {session.skills_dir}", flush=True)
        brain = subprocess.Popen(
            [sys.executable, "-m", "praxion_brain", "run", "--port", str(session.port),
             # Each run is a fresh server world: its memory must be too, or
             # failure blocks from an earlier run leak into this one.
             "--world", session.run_dir.name],
            cwd=root / "brain",
        )
        deadline = time.monotonic() + args.minutes * 60
        try:
            while time.monotonic() < deadline and brain.poll() is None:
                time.sleep(5)
        finally:
            if brain.poll() is None:
                brain.terminate()
                try:
                    brain.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    brain.kill()
        print(f"brain exit {brain.returncode}", flush=True)
        print(f"inventory {session.referee.player_inventory('Praxion')}", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
