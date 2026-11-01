"""Follow a Minecraft server log and turn lines into typed events.

Death-message templates are derived from the 26.3 language file inside the server
jar (``assets/minecraft/lang/en_us.json``, keys starting with ``death.``) instead
of a hand-written list. The jar is read with :mod:`zipfile`; both the flat and
the Mojang-bundled (nested) jar layouts are supported.
"""

from __future__ import annotations

import io
import json
import re
import time
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Iterator, Optional


@dataclass
class ServerDone:
    line: str


@dataclass
class PlayerJoined:
    name: str


@dataclass
class PlayerLeft:
    name: str


@dataclass
class PlayerDied:
    name: str
    message: str


@dataclass
class ChatMessage:
    sender: str
    message: str


Event = ServerDone | PlayerJoined | PlayerLeft | PlayerDied | ChatMessage


# --------------------------------------------------------------------------- #
# Jar / language handling
# --------------------------------------------------------------------------- #

def _open_lang_json(jar_path: Path) -> dict:
    """Return the parsed en_us.json dict from a (possibly bundled) server jar."""
    jar_path = Path(jar_path)
    # Try flat layout first.
    try:
        with zipfile.ZipFile(jar_path) as z:
            if "assets/minecraft/lang/en_us.json" in z.namelist():
                return json.loads(z.read("assets/minecraft/lang/en_us.json"))
            # Bundled (Mojang launcher) layout: nested jar.
            nested = [n for n in z.namelist()
                      if n.endswith(".jar") and "server" in n.lower()]
            for entry in nested:
                try:
                    inner_bytes = z.read(entry)
                    with zipfile.ZipFile(io.BytesIO(inner_bytes)) as iz:
                        if "assets/minecraft/lang/en_us.json" in iz.namelist():
                            return json.loads(iz.read("assets/minecraft/lang/en_us.json"))
                except (zipfile.BadZipFile, KeyError, json.JSONDecodeError):
                    continue
    except (zipfile.BadZipFile, OSError):
        pass
    # Last resort: search any nested jar.
    try:
        with zipfile.ZipFile(jar_path) as z:
            for entry in z.namelist():
                if not entry.endswith(".jar"):
                    continue
                try:
                    inner_bytes = z.read(entry)
                    with zipfile.ZipFile(io.BytesIO(inner_bytes)) as iz:
                        if "assets/minecraft/lang/en_us.json" in iz.namelist():
                            return json.loads(iz.read("assets/minecraft/lang/en_us.json"))
                except (zipfile.BadZipFile, KeyError, json.JSONDecodeError):
                    continue
    except (zipfile.BadZipFile, OSError):
        pass
    raise FileNotFoundError(f"en_us.json not found in jar: {jar_path}")


def load_death_templates(jar_path) -> list[str]:
    """Return the raw ``death.*`` templates from the jar's language file."""
    data = _open_lang_json(Path(jar_path))
    return [v for k, v in data.items() if k.startswith("death.") and isinstance(v, str)]


def _template_to_regex(template: str) -> re.Pattern:
    """Convert a Minecraft lang template (with %s/%1$s/...) to a regex."""
    parts = re.split(r"(%\d*\$?[sdif])", template)
    out = ""
    for part in parts:
        if re.fullmatch(r"%\d*\$?[sdif]", part):
            if "d" in part:
                out += r"(-?\d+)"
            else:
                out += r"(.+?)"
        else:
            out += re.escape(part)
    return re.compile(out)


def death_patterns(jar_path=None) -> list[re.Pattern]:
    """Compiled regexes for death messages, derived from the jar's lang file.

    If ``jar_path`` is None, resolve via :mod:`praxion_harness.paths`.
    """
    if jar_path is None:
        from .paths import resolve_server_jar

        jar_path = resolve_server_jar()
    templates = load_death_templates(jar_path)
    return [_template_to_regex(t) for t in templates]


# Cache compiled patterns (resolved lazily).
_PATTERN_CACHE: dict[str, list[re.Pattern]] = {}


def get_death_patterns(jar_path=None) -> list[re.Pattern]:
    key = str(jar_path) if jar_path is not None else "__default__"
    if key not in _PATTERN_CACHE:
        _PATTERN_CACHE[key] = death_patterns(jar_path)
    return _PATTERN_CACHE[key]


def parse_death_message(text: str, jar_path=None) -> Optional[tuple[str, str]]:
    """If ``text`` is a death message, return (victim_name, full_message)."""
    patterns = get_death_patterns(jar_path)
    for pat in patterns:
        m = pat.fullmatch(text.strip())
        if m:
            groups = m.groups()
            name = groups[0] if groups else text.strip().split()[0]
            return name, text.strip()
    return None


# --------------------------------------------------------------------------- #
# Log prefix handling
# --------------------------------------------------------------------------- #

# Example lines:
# [13:05:59] [Server thread/INFO]: Done (0.666s)! For help, type "help"
# [13:05:59] [Server thread/INFO]: Steve joined the game
# [..] [Server thread/INFO]: <Steve> hello
_LOG_PREFIX = re.compile(r"^\[[^\]]*\]\s*\[[^\]]*\]:\s*(.*)$")


def _strip_prefix(line: str) -> str:
    m = _LOG_PREFIX.match(line.strip())
    if m:
        return m.group(1)
    return line.strip()


def parse_line(line: str, jar_path=None) -> Optional[Event]:
    body = _strip_prefix(line)
    if not body:
        return None
    if "Done (" in body and "For help" in body:
        return ServerDone(line=body)
    m = re.match(r"^(.+?) joined the game$", body)
    if m:
        return PlayerJoined(name=m.group(1))
    m = re.match(r"^(.+?) left the game$", body)
    if m:
        return PlayerLeft(name=m.group(1))
    m = re.match(r"^<([^>]+)>\s*(.*)$", body)
    if m:
        return ChatMessage(sender=m.group(1), message=m.group(2))
    died = parse_death_message(body, jar_path=jar_path)
    if died is not None:
        name, message = died
        return PlayerDied(name=name, message=message)
    return None


# --------------------------------------------------------------------------- #
# Follower
# --------------------------------------------------------------------------- #

class LogTail:
    def __init__(self, log_path, jar_path=None):
        self.log_path = Path(log_path)
        self.jar_path = jar_path
        self._offset = 0

    def poll(self) -> list[Event]:
        if not self.log_path.exists():
            return []
        events: list[Event] = []
        with self.log_path.open("r", encoding="utf-8", errors="replace") as fh:
            fh.seek(self._offset)
            data = fh.read()
            self._offset = fh.tell()
        for line in data.splitlines():
            ev = parse_line(line, jar_path=self.jar_path)
            if ev is not None:
                events.append(ev)
        return events

    def follow(self, timeout: Optional[float] = None) -> Iterator[Event]:
        deadline = (time.monotonic() + timeout) if timeout is not None else None
        while True:
            if deadline is not None and time.monotonic() > deadline:
                return
            for ev in self.poll():
                yield ev
            time.sleep(0.25)


__all__ = [
    "ServerDone", "PlayerJoined", "PlayerLeft", "PlayerDied", "ChatMessage",
    "Event", "LogTail", "parse_line", "parse_death_message",
    "load_death_templates", "death_patterns", "get_death_patterns",
]