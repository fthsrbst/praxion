"""Build the briefing: the only remembered text the curriculum model sees.

The shape is fixed by proposal section 3.4 and the briefing contract in S12:

```
## Now  [live, tick 4521]
health 20/20, food 18/20, day_time 4521 (day), dimension overworld, pos 12,64,-30
inventory: oak_log 3, cobblestone 8
## Ladder  [computed from inventory now]
done: wood, crafting_table, wooden_pickaxe, stone_pickaxe, furnace   next: coal   (rung 6/12)
## Recent goals  [goals.jsonl]
- have a furnace: done, 41 s ago, skill get_furnace (written, 2 repairs)
- have coal: failed(not_found) x2 -> blocked for 2 h
## Skills  [catalog]
get_furnace() - get a furnace
## Budget  [ledger] $0.31 last hour, $1.90 of $4.50
```

Every section header names its source and every remembered goal line carries an
age or a remaining block. Input is plain data: a snapshot dict shaped like
``port.Snapshot``, the :class:`~praxion_brain.ladder.LadderProgress`, recent goal
records (dicts may carry derived ``attempts``, ``blocked_until`` and
``repairs``; newest first, as :meth:`~praxion_brain.memory.Memory.recent`
returns them), the skill catalog and ledger totals.

The token cap is ``len(text) / 4`` and it is a hard bound. When the text is
over it, the oldest recent goal lines are removed first; then skill lines, then
the budget values, and a final tail truncation cuts the last non-``Now``
section when a single line alone is too big. The ``Now`` section is never
trimmed; if it alone exceeds the cap, it is returned alone.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` section 3.4.
"""

from __future__ import annotations

import time
from typing import Any, Mapping, Optional, Sequence, Union

from pydantic import BaseModel

from .ladder import LadderProgress
from .memory import GoalRecord
from .skills import SkillCatalog, SkillInfo

#: Hard briefing cap in estimated tokens (proposal section 3.4).
BRIEFING_MAX_TOKENS = 1500
#: The contract's token estimate: characters divided by four.
CHARS_PER_TOKEN = 4

Record = Union[GoalRecord, Mapping[str, Any]]


def estimate_tokens(text: str) -> int:
    """The briefing contract's estimate: ``ceil(len/4)``."""
    return (len(text) + CHARS_PER_TOKEN - 1) // CHARS_PER_TOKEN


def _mapping(value: Any) -> Mapping[str, Any]:
    if isinstance(value, BaseModel):
        return value.model_dump()
    if isinstance(value, Mapping):
        return value
    raise TypeError(f"expected a model or mapping, got {type(value).__name__}")


def _get(value: Any, field: str, default: Any = None) -> Any:
    if isinstance(value, BaseModel):
        return getattr(value, field, default)
    if isinstance(value, Mapping):
        return value.get(field, default)
    return default


def _number(value: Any) -> str:
    if value is None:
        return "?"
    if isinstance(value, float) and value.is_integer():
        return str(int(value))
    return str(value)


def _money(value: Any) -> str:
    try:
        return f"${float(value):.2f}"
    except (TypeError, ValueError):
        return "$?"


def _duration(seconds: float) -> str:
    if seconds >= 3600:
        return f"{round(seconds / 3600)} h"
    if seconds >= 60:
        return f"{round(seconds / 60)} min"
    return f"{round(seconds)} s"


def _age(timestamp: Any, now: float) -> str:
    if timestamp is None:
        return "age unknown"
    try:
        moment = float(timestamp)
    except (TypeError, ValueError):
        return "age unknown"
    if moment >= now:
        return "just now"
    return f"{_duration(now - moment)} ago"


def _time(record: Any, first: str, second: str) -> Any:
    value = _get(record, first)
    return value if value is not None else _get(record, second)


# --------------------------------------------------------------------------- #
# Sections
# --------------------------------------------------------------------------- #
def _now_section(snapshot: Mapping[str, Any]) -> list[str]:
    facts = snapshot.get("facts") or snapshot
    tick = snapshot.get("tick")
    header = "## Now  [live, tick {}]".format(tick) if tick is not None else "## Now  [live]"

    day_time = facts.get("day_time")
    if facts.get("is_night") is True:
        part = "night"
    elif facts.get("is_night") is False:
        part = "day"
    elif isinstance(day_time, (int, float)):
        part = "day" if day_time < 13000 else "night"
    else:
        part = "?"

    position = facts.get("pos")
    if isinstance(position, (list, tuple)) and len(position) == 3:
        pos = ",".join(_number(value) for value in position)
    else:
        pos = "?"

    facts_line = (
        f"health {_number(facts.get('health'))}/20, "
        f"food {_number(facts.get('food'))}/20, "
        f"day_time {_number(day_time)} ({part}), "
        f"dimension {facts.get('dimension') or '?'}, pos {pos}"
    )

    inventory = facts.get("inventory") or {}
    items = []
    if isinstance(inventory, Mapping):
        for name in sorted(inventory):
            try:
                amount = int(inventory[name])
            except (TypeError, ValueError):
                continue
            if amount > 0:
                items.append(f"{name} {amount}")
    inventory_line = "inventory: " + (", ".join(items) if items else "(empty)")
    return [header, facts_line, inventory_line]


def _ladder_section(ladder: Union[LadderProgress, Mapping[str, Any]]) -> list[str]:
    done = list(_get(ladder, "done", []) or [])
    following = _get(ladder, "next")
    total = _get(ladder, "total", 12) or 12
    met = _get(ladder, "met", len(done))
    next_index = _get(ladder, "next_index")
    if next_index is None:
        next_index = (met + 1) if following is not None else total
    done_text = ", ".join(done) if done else "(none)"
    next_text = following if following is not None else "(complete)"
    line = (
        f"done: {done_text}   next: {next_text}   "
        f"(rung {next_index}/{total})"
    )
    return ["## Ladder  [computed from inventory now]", line]


def _goal_line(record: Record, now: float) -> str:
    text = _get(record, "text") or "(unnamed goal)"
    status = _get(record, "status") or "open"
    if status == "done":
        age = _age(_time(record, "t_end", "t_start"), now)
        skill = _get(record, "skill")
        suffix = ""
        if skill:
            meta = []
            origin = _get(record, "skill_origin")
            if origin:
                meta.append(str(origin))
            repairs = _get(record, "repairs")
            if repairs is not None:
                meta.append(f"{repairs} repairs")
            suffix = f", skill {skill}" + (f" ({', '.join(meta)})" if meta else "")
        return f"- {text}: done, {age}{suffix}"

    if status in ("failed", "blocked"):
        kind = _get(record, "failure_kind")
        label = f"failed({kind})" if kind else "failed"
        attempts = _get(record, "attempts") or 1
        blocked_until = _get(record, "blocked_until")
        if blocked_until is not None:
            try:
                remaining = float(blocked_until) - now
            except (TypeError, ValueError):
                remaining = 0.0
            if remaining > 0:
                return (
                    f"- {text}: {label} x{attempts} -> "
                    f"blocked for {_duration(remaining)}"
                )
        age = _age(_time(record, "t_end", "t_start"), now)
        return f"- {text}: {label} x{attempts}, {age}"

    age = _age(_time(record, "t_start", "t_end"), now)
    return f"- {text}: {status}, {age}"


def _skill_items(catalog: Any) -> list[str]:
    if isinstance(catalog, SkillCatalog):
        entries = list(catalog.skills)
    else:
        entries = list(catalog or [])
    lines = []
    for entry in entries:
        name = _get(entry, "name") or "?"
        params = _get(entry, "params", []) or []
        doc = _get(entry, "doc", "") or ""
        line = f"{name}({', '.join(str(param) for param in params)})"
        if doc:
            line += f" - {doc}"
        lines.append(line)
    return lines


def _truncate_tail(text: str, limit: int) -> str:
    """Cut ``text`` to at most ``limit`` characters, keeping its head."""
    return text[: max(limit, 0)].rstrip()


def _budget_line(ledger: Mapping[str, Any], *, values: bool = True) -> str:
    if not values:
        return "## Budget  [ledger]"
    last_hour = ledger.get("last_hour", 0.0)
    spent = ledger.get("spent", ledger.get("total", 0.0))
    budget = ledger.get("budget", 0.0)
    return (
        f"## Budget  [ledger] {_money(last_hour)} last hour, "
        f"{_money(spent)} of {_money(budget)}"
    )


# --------------------------------------------------------------------------- #
# Build
# --------------------------------------------------------------------------- #
def build(
    snapshot: Mapping[str, Any],
    ladder: Union[LadderProgress, Mapping[str, Any]],
    memory: Sequence[Record],
    catalog: Union[SkillCatalog, Sequence[Union[SkillInfo, Mapping[str, Any]]]],
    ledger: Mapping[str, Any],
    *,
    now: Optional[float] = None,
    max_tokens: int = BRIEFING_MAX_TOKENS,
) -> str:
    """Render the briefing text, trimming it to ``max_tokens`` when possible."""
    moment = time.time() if now is None else float(now)
    now_lines = _now_section(snapshot)
    ladder_lines = _ladder_section(ladder)
    records = list(memory)
    recent_items = [_goal_line(record, moment) for record in records]
    recent_header = "## Recent goals  [goals.jsonl]"
    skills_header = "## Skills  [catalog]"
    skills = _skill_items(catalog)
    if not skills:
        skills = ["(none)"]
    budget_values = True

    def render() -> str:
        lines = list(now_lines)
        lines.extend(ladder_lines)
        lines.append(recent_header)
        # An empty list means every goal line was trimmed, not that the world
        # has no history yet; both cases say so explicitly.
        lines.extend(recent_items or ["(none)"])
        lines.append(skills_header)
        lines.extend(skills)
        lines.append(_budget_line(ledger, values=budget_values))
        return "\n".join(lines)

    text = render()
    # Lowest priority first: the oldest recent goals, then skill lines, then the
    # budget values, then the tail of whatever section is last. The Now section
    # is never touched.
    has_goals = bool(records)
    while estimate_tokens(text) > max_tokens and has_goals and recent_items:
        recent_items.pop()
        text = render()
    while estimate_tokens(text) > max_tokens and len(skills) > 1:
        skills.pop()
        text = render()
    if estimate_tokens(text) > max_tokens and budget_values:
        budget_values = False
        text = render()

    # Final hard bound. Every priority trim above can leave one enormous line
    # (a 6 kB skill doc, say), so cut the tail of the last non-Now section.
    # The Now section is at the head and survives; when it alone is over the
    # cap, no rendering can satisfy the bound, so it is returned whole.
    limit = max_tokens * CHARS_PER_TOKEN
    now_text = "\n".join(now_lines)
    if estimate_tokens(text) > max_tokens:
        text = now_text if len(now_text) >= limit else _truncate_tail(text, limit)
    if len(now_text) < limit:
        assert estimate_tokens(text) <= max_tokens, "briefing exceeded its cap"
    return text


__all__ = [
    "BRIEFING_MAX_TOKENS",
    "CHARS_PER_TOKEN",
    "build",
    "estimate_tokens",
]
