"""Logged fixture channel.

Every fixture command is executed through RCON and recorded as one JSON line
``{t, run_id, command, response}`` in ``log_file`` before the method returns.
The agent's own API is never used for setup.

Gamerule names below are the ones the 26.3 server itself reported (``help gamerule``
and ``gamerule <name>``); see ``tests/fixtures/observed-26.3.txt``. 26.3 renamed
them to snake_case: the daylight cycle is ``advance_time`` (not ``doDaylightCycle``)
and general mob spawning is ``spawn_mobs`` (not ``doMobSpawning``).
"""

from __future__ import annotations

import json
import time
from pathlib import Path
from typing import Any, Optional

from .rcon import RconClient

#: Name of the fixture log inside a run directory.
FIXTURE_LOG_NAME = "fixtures.jsonl"


class GameRules:
    """The 26.3 gamerule names the harness needs, observed on the real server."""

    #: Daylight cycle: false freezes the time of day.
    ADVANCE_TIME = "advance_time"
    #: General mob spawning.
    SPAWN_MOBS = "spawn_mobs"
    #: Keep the inventory on death.
    KEEP_INVENTORY = "keep_inventory"


def fixture_log_path(run_dir) -> Path:
    """The conventional fixture log path: ``<run_dir>/fixtures.jsonl``."""
    return Path(run_dir) / FIXTURE_LOG_NAME


class FixtureChannel:
    def __init__(self, rcon: RconClient, log_file, run_id: Optional[str] = None):
        self.rcon = rcon
        self.log_file = Path(log_file)
        self.run_id = run_id
        if self.run_id is None:
            # Best-effort inference from a runs/<id>/... path.
            parts = self.log_file.resolve().parts
            if "runs" in parts:
                idx = parts.index("runs")
                if idx + 1 < len(parts):
                    self.run_id = parts[idx + 1]
            if self.run_id is None:
                self.run_id = "unknown"

    def _record(self, command: str, response: str) -> str:
        entry = {
            "t": time.time(),
            "run_id": self.run_id,
            "command": command,
            "response": response,
        }
        self.log_file.parent.mkdir(parents=True, exist_ok=True)
        with self.log_file.open("a", encoding="utf-8") as fh:
            fh.write(json.dumps(entry, ensure_ascii=False) + "\n")
        return response

    def _run(self, command: str) -> str:
        response = self.rcon.command(command)
        return self._record(command, response)

    def raw(self, command: str) -> str:
        """Send one arbitrary fixture command and log it."""
        return self._run(command)

    @staticmethod
    def _value(value: Any) -> str:
        # Minecraft booleans are lowercase; Python would print True/False.
        if isinstance(value, bool):
            return "true" if value else "false"
        return str(value)

    # ------------------------------------------------------------------ #
    def tp(self, target: str, x, y, z) -> str:
        return self._run(f"tp {target} {x} {y} {z}")

    def give(self, target: str, item: str, count: int = 1) -> str:
        return self._run(f"give {target} {item} {count}")

    def summon(self, entity: str, x, y, z, nbt: Optional[str] = None) -> str:
        cmd = f"summon {entity} {x} {y} {z}"
        if nbt:
            cmd += " " + nbt
        return self._run(cmd)

    def setblock(self, x, y, z, block: str) -> str:
        return self._run(f"setblock {x} {y} {z} {block}")

    def fill(self, x1, y1, z1, x2, y2, z2, block: str) -> str:
        return self._run(f"fill {x1} {y1} {z1} {x2} {y2} {z2} {block}")

    def time_set(self, value) -> str:
        return self._run(f"time set {value}")

    def gamerule(self, name: str, value: Any) -> str:
        return self._run(f"gamerule {name} {self._value(value)}")

    def gamerule_value(self, name: str) -> str:
        """Read a gamerule, for example ``doDaylightCycle``/``advance_time``."""
        return self._run(f"gamerule {name}")

    def advance_time(self, enabled: bool) -> str:
        """Freeze (False) or resume (True) the daylight cycle."""
        return self.gamerule(GameRules.ADVANCE_TIME, enabled)

    def spawn_mobs(self, enabled: bool) -> str:
        return self.gamerule(GameRules.SPAWN_MOBS, enabled)

    def keep_inventory(self, enabled: bool) -> str:
        return self.gamerule(GameRules.KEEP_INVENTORY, enabled)

    def difficulty(self, value: str) -> str:
        return self._run(f"difficulty {value}")

    def effect(self, target: str, effect: str, duration: int = 30,
               amplifier: int = 0, clear: bool = False) -> str:
        if clear:
            return self._run(f"effect clear {target} {effect}")
        return self._run(f"effect give {target} {effect} {duration} {amplifier}")

    def kill(self, target: str) -> str:
        return self._run(f"kill {target}")


__all__ = [
    "FixtureChannel",
    "GameRules",
    "FIXTURE_LOG_NAME",
    "fixture_log_path",
]
