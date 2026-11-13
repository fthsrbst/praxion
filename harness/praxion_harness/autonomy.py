"""Run the brain loop against a live body for a while: the autonomy smoke run.

    python -m praxion_harness.autonomy --seed 303 --minutes 25

Starts a fresh-seed body in port mode (nothing given), prints the address a
real client can join to watch, runs ``python -m praxion_brain run --port N``
until the time is up, then prints the referee's inventory, deaths, time of day
and first-night verdict, and where the evidence is.

By default this is a survival run (decision 2026-11-09 "Five next areas"): the
day cycle runs, mobs spawn, and the clock starts in the morning, so a long run
meets dusk and a night. ``--peaceful`` restores the old frozen, mob-free
daytime for debugging; the smoke tests keep their own frozen fixtures.

The body obeys the chat of the player named by ``--operator`` (default
``Fatih``, the watcher): ``dur``, ``iptal``, ``durum`` and ``devam`` act bare; a
task needs the prefix ``p:`` (``p: 8 taş getir``, ``--chat-prefix`` changes it,
an empty prefix takes every line as a task); any other chat stays chat. The
brain maps a task to a goal and the bot answers in Turkish. Join under that
name or nothing you type is read (decision 2026-11-09 "Chat: control words
bare, tasks with a prefix").
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

from .body import start_session
from .paths import repo_root
from .rcon import RconError


#: Morning: tick 0 of the day (sunrise). 24000 ticks are 20 minutes, so a
#: 45 minute run sees dusk after about ten.
DEFAULT_START_TIME = 0


def peaceful_setup(fixture, referee, session) -> None:
    """The old run: the clock frozen at 1000, no mobs."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)


def survival_setup(start_time: int):
    """The survival run: the day cycle on, mobs on, the clock at ``start_time``."""

    def setup(fixture, referee, session) -> None:
        fixture.advance_time(True)
        fixture.spawn_mobs(True)
        fixture.time_set(start_time)

    return setup


#: Old names kept for callers that imported them before the console reused the
#: same two setups (the argument list and the fixture calls are unchanged).
_peaceful = peaceful_setup
_survival = survival_setup


def survival_report(referee, player: str = "Praxion") -> dict:
    """What the referee, not the body, says about the run (deaths, clock, night).

    Every value comes from the server: the body's own report is never the verdict.
    """
    ticks = referee.time_of_day()
    return {
        "deaths": referee.death_count(player),
        "time_of_day": ticks,
        "is_night": referee.is_night_time(ticks),
        "first_night_survived": referee.first_night_survived(),
    }


class DesyncWatch:
    """Reads the body's event log as it grows and reports a ``world_desync``.

    Decision 2026-11-10 "A world desync stops the run loudly": the body logs one
    ``world_desync`` event when its world mirror can no longer be trusted, and
    the run ends at once with that reason. Only new bytes are read on each
    :meth:`poll` (the offset is kept), and only complete lines: a line the body
    is still writing is read on the next poll.
    """

    def __init__(self, path: Path) -> None:
        self.path = Path(path)
        self._offset = 0

    def poll(self) -> Optional[str]:
        """The desync reason, if a new complete line reports one; else ``None``."""
        try:
            with self.path.open("rb") as handle:
                handle.seek(self._offset)
                chunk = handle.read()
        except FileNotFoundError:
            return None
        end = chunk.rfind(b"\n")
        if end < 0:
            return None
        self._offset += end + 1
        for line in chunk[: end + 1].splitlines():
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if isinstance(event, dict) and event.get("kind") == "world_desync":
                return str(event.get("reason") or "unknown")
        return None


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--seed", type=int, default=303)
    parser.add_argument("--minutes", type=float, default=25.0)
    parser.add_argument(
        "--peaceful",
        action="store_true",
        help="the old run: the clock frozen at 1000 and no mobs (for debugging)",
    )
    parser.add_argument(
        "--start-time",
        type=int,
        default=DEFAULT_START_TIME,
        help="the world clock at the start of a survival run, in ticks (default: morning)",
    )
    parser.add_argument(
        "--operator",
        default="Fatih",
        help="the player name whose chat the bot obeys (the one who watches)",
    )
    args = parser.parse_args(argv)

    root = repo_root()
    setup = peaceful_setup if args.peaceful else survival_setup(args.start_time)
    with start_session(
        args.seed,
        setup=setup,
        max_ticks=10**9,
        keep=True,
        extra_args=["--operator", args.operator],
    ) as session:
        print(f"Watch: Minecraft 26.3 > Direct Connection > 127.0.0.1:{session.server.port}", flush=True)
        print(f"Join as {args.operator!r}: the bot reads only this player's chat", flush=True)
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
        desync_watch = DesyncWatch(session.run_dir / "body" / "events.jsonl")
        desync: Optional[str] = None
        try:
            while time.monotonic() < deadline and brain.poll() is None:
                time.sleep(5)
                desync = desync_watch.poll()
                if desync is not None:
                    # The body is blind: every fact it serves from now on is
                    # stale, so the run ends here, loudly (decision 2026-11-10);
                    # the reason is printed once, after the cleanup.
                    break
                # The first-night verdict needs the clock watched through the
                # night, not read once at the end.
                if not args.peaceful:
                    try:
                        session.referee.observe()
                    except (ValueError, OSError, RconError) as error:
                        # One empty or dropped RCON reply must not end a long
                        # run (run 8, 2026-11-10, died on `time query day` -> '').
                        print(f"referee: observation skipped ({error})", flush=True)
        finally:
            if brain.poll() is None:
                brain.terminate()
                try:
                    brain.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    brain.kill()
        print(f"brain exit {brain.returncode}", flush=True)
        print(f"inventory {session.referee.player_inventory('Praxion')}", flush=True)
        if not args.peaceful:
            report = survival_report(session.referee)
            print(
                f"deaths {report['deaths']} time_of_day {report['time_of_day']} "
                f"is_night {report['is_night']} "
                f"first_night_survived {report['first_night_survived']}",
                flush=True,
            )
    if desync is not None:
        print(f"run stopped: world desync: {desync}", flush=True)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
