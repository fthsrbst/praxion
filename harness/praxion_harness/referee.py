"""Referee: reads server facts via RCON (and deaths via the log tail).

Praxion's own report never decides a verdict; only this module does.

Observed 26.3 output formats (captured in ``tests/fixtures/observed-26.3.txt``):
  ``time query day``      -> ``Timeline minecraft:day is at 1002 tick(s)``
  ``time query gametime`` -> ``The game time is 48000 tick(s)``
  ``time query time``     -> ``Clock minecraft:overworld is at 1006 tick(s)``
  ``time query daytime``  -> invalid in 26.3 (there is no ``minecraft:daytime`` timeline)
  ``execute if block``    -> ``Test passed`` / ``Test failed``
  ``scoreboard players get Steve praxion_deaths`` -> ``Steve has 5 [praxion_deaths]``
  ``data get entity`` inventory ->
  ``[{count: 5, Slot: 0b, id: "minecraft:oak_log"}]``: ``count`` before ``id``,
  and item components may nest braces, commas and quoted strings.

The inventory reply is SNBT, so :meth:`Referee.player_inventory` splits the list
into entries at top-level commas only, letting each entry carry its own ``id``
and ``count`` in any order.
"""

from __future__ import annotations

import re
from typing import Dict, Optional, Tuple

from .rcon import RconClient

# Time-of-day boundaries: the sky darkens at 12542 and brightens at 23460.
NIGHT_START = 12542
NIGHT_END = 23460


class Referee:
    def __init__(self, rcon: RconClient, logtail=None, player: Optional[str] = None,
                 death_objective: str = "praxion_deaths"):
        self.rcon = rcon
        self.logtail = logtail
        self.player = player
        self.death_objective = death_objective

        # First-night tracking.
        self._night_started = False
        self._deaths_this_night = 0
        self._first_night_survived: Optional[bool] = None

    # ------------------------------------------------------------------ #
    # helpers
    # ------------------------------------------------------------------ #
    def _cmd(self, command: str) -> str:
        return self.rcon.command(command)

    @staticmethod
    def _parse_number(text: str) -> Optional[float]:
        # Last number in the text.
        nums = re.findall(r"-?\d+(?:\.\d+)?", text)
        if not nums:
            return None
        return float(nums[-1])

    # ------------------------------------------------------------------ #
    # player facts
    # ------------------------------------------------------------------ #
    def player_health(self, player: Optional[str] = None) -> float:
        p = player or self.player
        if p is None:
            raise ValueError("player name required")
        out = self._cmd(f"data get entity {p} Health")
        val = self._parse_number(out)
        if val is None:
            raise ValueError(f"could not parse health from: {out!r}")
        return val

    def player_food(self, player: Optional[str] = None) -> int:
        p = player or self.player
        if p is None:
            raise ValueError("player name required")
        out = self._cmd(f"data get entity {p} foodLevel")
        val = self._parse_number(out)
        if val is None:
            raise ValueError(f"could not parse food from: {out!r}")
        return int(val)

    def player_pos(self, player: Optional[str] = None) -> Tuple[float, float, float]:
        p = player or self.player
        if p is None:
            raise ValueError("player name required")
        out = self._cmd(f"data get entity {p} Pos")
        m = re.search(r"\[\s*([-\d.eE+]+)d?,\s*([-\d.eE+]+)d?,\s*([-\d.eE+]+)d?\s*\]", out)
        if not m:
            # Fallback: grab three numbers after 'Pos'.
            nums = re.findall(r"-?\d+(?:\.\d+)?", out)
            if len(nums) >= 3:
                return float(nums[-3]), float(nums[-2]), float(nums[-1])
            raise ValueError(f"could not parse pos from: {out!r}")
        return float(m.group(1)), float(m.group(2)), float(m.group(3))

    # ------------------------------------------------------------------ #
    # SNBT helpers (inventory replies)
    # ------------------------------------------------------------------ #
    @staticmethod
    def _snbt_parts(text: str, separator: str = ",") -> list:
        """Split ``text`` at ``separator`` characters outside quotes and brackets.

        SNBT item components can contain their own braces, brackets, commas and
        quoted strings, so a plain ``text.split(",")`` would tear entries apart.
        """
        parts = []
        start = 0
        depth = 0
        quote = None
        escaped = False
        for index, ch in enumerate(text):
            if quote is not None:
                if escaped:
                    escaped = False
                elif ch == "\\":
                    escaped = True
                elif ch == quote:
                    quote = None
                continue
            if ch in "\"'":
                quote = ch
            elif ch in "[{":
                depth += 1
            elif ch in "]}":
                depth -= 1
            elif ch == separator and depth == 0:
                parts.append(text[start:index])
                start = index + 1
        parts.append(text[start:])
        return parts

    @staticmethod
    def _snbt_list_body(text: str) -> Optional[str]:
        """The body of the first SNBT list in ``text`` (``[]`` -> ``""``)."""
        start = text.find("[")
        if start < 0:
            return None
        depth = 0
        quote = None
        escaped = False
        for index in range(start, len(text)):
            ch = text[index]
            if quote is not None:
                if escaped:
                    escaped = False
                elif ch == "\\":
                    escaped = True
                elif ch == quote:
                    quote = None
                continue
            if ch in "\"'":
                quote = ch
            elif ch == "[":
                depth += 1
            elif ch == "]":
                depth -= 1
                if depth == 0:
                    return text[start + 1:index]
        # Unbalanced reply: no trustworthy entries.
        return None

    @staticmethod
    def _snbt_unquote(value: str) -> Optional[str]:
        """A string value without its SNBT quotes; ``None`` when empty."""
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
            out = []
            index = 0
            while index < len(value):
                if value[index] == "\\" and index + 1 < len(value):
                    out.append(value[index + 1])
                    index += 2
                else:
                    out.append(value[index])
                    index += 1
            value = "".join(out)
        elif not value:
            return None
        return value or None

    @staticmethod
    def _snbt_int(value: str) -> Optional[int]:
        """A numeric value, ignoring an SNBT suffix such as the ``b`` in ``0b``."""
        m = re.match(r"-?\d+", value.strip())
        return int(m.group(0)) if m else None

    @classmethod
    def _snbt_fields(cls, entry: str) -> Dict[str, str]:
        """The top-level ``key: value`` pairs of one ``{...}`` entry."""
        body = entry.strip()
        if body.startswith("{") and body.endswith("}"):
            body = body[1:-1]
        fields: Dict[str, str] = {}
        for part in cls._snbt_parts(body):
            key, sep, value = part.partition(":")
            if not sep:
                continue
            key = key.strip()
            if len(key) >= 2 and key[0] == key[-1] and key[0] in "\"'":
                key = key[1:-1]
            fields[key] = value.strip()
        return fields

    def player_inventory(self, player: Optional[str] = None) -> Dict[str, int]:
        p = player or self.player
        if p is None:
            raise ValueError("player name required")
        out = self._cmd(f"data get entity {p} Inventory")
        result: Dict[str, int] = {}
        body = self._snbt_list_body(out)
        if body is None:
            return result
        for entry in self._snbt_parts(body):
            fields = self._snbt_fields(entry)
            item = self._snbt_unquote(fields["id"]) if "id" in fields else None
            if item is None:
                continue
            # 26.3 always sends ``count``; a missing one means a single item.
            count = self._snbt_int(fields["count"]) if "count" in fields else 1
            if count is None:
                count = 1
            result[item] = result.get(item, 0) + count
        return result

    def player_dimension(self, player: Optional[str] = None) -> str:
        p = player or self.player
        if p is None:
            raise ValueError("player name required")
        out = self._cmd(f"data get entity {p} Dimension")
        m = re.search(r'"([^"]+)"', out)
        if m:
            return m.group(1)
        # Fallback: last token.
        return out.strip()

    # ------------------------------------------------------------------ #
    # world / time facts
    # ------------------------------------------------------------------ #
    def time_of_day(self) -> int:
        """Ticks within the current day (0..23999). 26.3: ``time query day``."""
        out = self._cmd("time query day")
        val = self._parse_number(out)
        if val is None:
            raise ValueError(f"could not parse time of day from: {out!r}")
        return int(val) % 24000

    def game_time(self) -> int:
        """Total world ticks since creation. 26.3: ``time query gametime``."""
        out = self._cmd("time query gametime")
        val = self._parse_number(out)
        if val is None:
            raise ValueError(f"could not parse game time from: {out!r}")
        return int(val)

    def day_count(self) -> int:
        """Day number, derived from game time (26.3 has no day-count query)."""
        return self.game_time() // 24000

    # Common block ids used as fallback candidates for identification.
    _COMMON_BLOCKS = [
        "minecraft:air", "minecraft:stone", "minecraft:dirt", "minecraft:grass_block",
        "minecraft:cobblestone", "minecraft:oak_planks", "minecraft:oak_log",
        "minecraft:sand", "minecraft:gravel", "minecraft:water", "minecraft:lava",
        "minecraft:bedrock", "minecraft:deepslate", "minecraft:glass",
        "minecraft:crafting_table", "minecraft:furnace", "minecraft:chest",
    ]

    def block_is(self, x, y, z, block: str) -> bool:
        """Exact check via ``execute if block`` (``Test passed``/``Test failed``)."""
        out = self._cmd(f"execute if block {x} {y} {z} {block}")
        return "Test passed" in out

    def block_at(self, x, y, z) -> str:
        """Best-effort block id at a position.

        Vanilla has no generic "read any block" command: block entities answer
        ``data get block``, everything else is identified by probing candidates
        with ``execute if block``. Use :meth:`block_is` when the id is known.
        Returns ``""`` when no candidate matches.
        """
        out = self._cmd(f"data get block {x} {y} {z}")
        if "not a block entity" not in out.lower():
            m = re.search(r"minecraft:[a-z0-9_]+", out)
            if m:
                return m.group(0)
        for cand in self._COMMON_BLOCKS:
            if self.block_is(x, y, z, cand):
                return cand
        return ""

    # ------------------------------------------------------------------ #
    # death count / scoreboard
    # ------------------------------------------------------------------ #
    def setup_death_objective(self, name: Optional[str] = None) -> str:
        """Create the ``deathCount`` objective (idempotent) and return the server reply."""
        obj = name or self.death_objective
        self.death_objective = obj
        out = self._cmd(f"scoreboard objectives add {obj} deathCount")
        lowered = out.lower()
        if "created new objective" not in lowered and "already exists" not in lowered:
            raise RuntimeError(f"could not create death objective: {out!r}")
        return out

    def death_count(self, player: Optional[str] = None) -> int:
        p = player or self.player
        if p is None:
            raise ValueError("player name required")
        out = self._cmd(f"scoreboard players get {p} {self.death_objective}")
        m = re.search(r"has (-?\d+) \[", out)
        if m is None:
            # "Can't get value ... none is set" means zero deaths.
            return 0
        return int(m.group(1))

    # ------------------------------------------------------------------ #
    # night / first-night tracking
    # ------------------------------------------------------------------ #
    @staticmethod
    def is_night_time(ticks: int) -> bool:
        """Minecraft night, by sky darkness (12542..23460)."""
        return NIGHT_START <= (ticks % 24000) <= NIGHT_END

    def is_night(self) -> bool:
        return self.is_night_time(self.time_of_day())

    def observe(self) -> None:
        """Update first-night state from the current time and new log deaths."""
        night = self.is_night_time(self.time_of_day())
        if night and not self._night_started:
            self._night_started = True
            self._deaths_this_night = 0
        # ``LogTail.poll`` returns only lines it has not returned before.
        if self.logtail is not None:
            try:
                events = self.logtail.poll()
            except Exception:
                events = []
            from .logtail import PlayerDied
            self._deaths_this_night += sum(1 for ev in events if isinstance(ev, PlayerDied))
        # Morning reached after night.
        if (not night) and self._night_started and self._first_night_survived is None:
            self._first_night_survived = (self._deaths_this_night == 0)

    def first_night_survived(self) -> Optional[bool]:
        self.observe()
        return self._first_night_survived


__all__ = ["Referee", "NIGHT_START", "NIGHT_END"]
