"""The console's read side: everything an endpoint returns, from files and the hub.

No function here writes anything or touches the loop; each one reads the
evidence index, the ledger, the world's goal store, the skill directories or the
state the loop published.
"""

from __future__ import annotations

import json
import statistics
import time
from datetime import datetime
from pathlib import Path
from typing import Any, Optional

from ..ladder import RUNGS
from ..ledger import DEFAULT_BUDGETS, Ledger, cost, entry_epoch
from ..memory import normalize_condition
from ..skills import load_catalog
from .hub import Console
from .log import EvidenceLog

#: A string longer than this is cut in list views; the exchange view has it all.
SLIM_CHARS = 400
#: Request kind -> its response kind.
PAIRS = {
    "proposal_request": "proposal_response",
    "router_request": "router_response",
    "writer_request": "writer_response",
    "plan_request": "plan_response",
    "command_map_request": "command_map_response",
}
#: Response kind -> the request kind it answers (the reverse of :data:`PAIRS`).
REQUESTS = {response: request for request, response in PAIRS.items()}
#: Lines that belong to an exchange but are not part of it -> the response they follow.
ANCHORS = {
    "router_decision": "router_response",
    "loop_route": "router_response",
    "proposal_result": "proposal_response",
    "admission_attempt": "writer_response",
    "plan": "plan_response",
}
#: How far from a line its request, response or decision is searched.
PAIR_WINDOW = 60
MAX_SOURCE_CHARS = 8000
MAX_DIGEST_CHARS = 1500
#: Evidence kinds of a narration line: ``narrate`` opens it (and carries the
#: topic), ``narrate_sent`` closes it as really sent to chat.
NARRATION_KINDS = ("narrate", "narrate_sent")
#: At most this many lines go to one voice poll; the narrator speaks far slower.
NARRATION_MAX = 20
#: How many narration evidence lines one poll looks back through. A page that
#: fell further behind gets silence: stale speech is worse than no speech.
NARRATION_WINDOW = 64


def slim(value: Any) -> Any:
    """``value`` with every long string shortened, for list views."""
    if isinstance(value, str):
        if len(value) <= SLIM_CHARS:
            return value
        return value[:SLIM_CHARS] + f"... [{len(value)} chars]"
    if isinstance(value, dict):
        return {key: slim(item) for key, item in value.items()}
    if isinstance(value, list):
        return [slim(item) for item in value]
    return value


# --------------------------------------------------------------------------- #
def state(
    console: Optional[Console],
    *,
    session: Optional[dict[str, Any]] = None,
    ledger: Optional[Ledger] = None,
) -> dict[str, Any]:
    """The loop's published state plus the money, read fresh.

    A managed console has no ``Console`` while no session is live: the page
    still gets the phase from ``session`` and the budget from the shared ledger,
    with no goal, snapshot or rung state left over from the previous run.
    """
    if console is not None:
        result = dict(console.state)
        ledger = ledger if ledger is not None else console.ledger
        memory = console.memory
    else:
        result = {
            "world_id": None,
            "state": "idle",
            "paused": False,
            "connected": False,
        }
        ledger = ledger if ledger is not None else Ledger()
        memory = None
    totals = ledger.totals()
    budget: dict[str, Any] = {}
    for provider in sorted(set(totals) | set(DEFAULT_BUDGETS)):
        try:
            limit = ledger.budget(provider)
        except ValueError:
            continue
        budget[provider] = {"spent": totals.get(provider, 0.0), "limit": limit}
    result["budget"] = budget
    # One failure-memory read for every rung, not one per rung.
    blocked = memory.blocked() if memory is not None else {}
    result["rungs"] = [
        {
            "id": rung.id,
            "condition": rung.condition,
            "blocked": normalize_condition(rung.condition) in blocked,
        }
        for rung in RUNGS
    ]
    result["spent_last_hour"] = ledger.spent_since(time.time() - 3600)
    # Additive keys of the planning loop (2026-11-12). A loop without them, or
    # no loop at all, still answers with the same shape, so the page never has
    # to guess whether a key exists.
    result.setdefault("persona", None)
    result.setdefault("plan", [])
    result.setdefault("thought", None)
    result["nearby"] = _nearby(result.get("facts"))
    if session is not None:
        result["session"] = session
    return result


def _nearby(facts: Any) -> Optional[dict[str, Any]]:
    """The body's ``nearby`` fact, cleaned, or ``None`` when absent or empty.

    Older bodies send none; a newer one omits it when nothing is near. The page
    hides its surroundings card on ``None``, so an empty shell is never shown.
    """
    raw = facts.get("nearby") if isinstance(facts, dict) else None
    if not isinstance(raw, dict):
        return None

    def counts(value: Any) -> dict[str, int]:
        if not isinstance(value, dict):
            return {}
        return {
            str(name): int(count)
            for name, count in value.items()
            if isinstance(count, (int, float)) and count > 0
        }

    sounds = raw.get("sounds")
    cleaned = {
        "entities": counts(raw.get("entities")),
        "blocks": counts(raw.get("blocks")),
        "sounds": [str(item) for item in sounds if item][:8] if isinstance(sounds, list) else [],
    }
    return cleaned if any(cleaned.values()) else None


def goals(console: Optional[Console]) -> list[dict[str, Any]]:
    if console is None:
        return []
    return [record.model_dump() for record in console.memory.goals()]


# --------------------------------------------------------------------------- #
def _run_start(log: EvidenceLog) -> int:
    """The line of the current run's ``loop_started``, or 0 when none is logged."""
    starts = log.numbers(kinds=["loop_started"], limit=1)
    return starts[-1] if starts else 0


def narration_lines(
    log: EvidenceLog,
    after: Optional[str] = None,
    limit: int = NARRATION_MAX,
) -> dict[str, Any]:
    """The narration lines the voice panel has not seen yet.

    Only lines the body confirmed as sent (``narrate_sent``) count, and only
    inside the current run: a bot restart starts a new run, and a page left
    open across it must not read the old run's lines aloud. ``topic`` comes
    from the ``narrate`` line that opened the same ``line_id``.

    ``after`` is the last ``line_id`` the page read. With no ``after`` the
    answer carries no lines at all, only the newest id, so a fresh page never
    replays old speech. An empty ``after`` means "initialised, seen nothing
    yet": the page opened before the run spoke, so the newest lines are the
    ones to say. An unknown ``after`` (a page that fell out of the window, or
    a truncated log) also gets silence and the newest id to resynchronise on.
    """
    entries = log.entries(log.numbers(kinds=NARRATION_KINDS, limit=NARRATION_WINDOW))
    start = _run_start(log)
    topics: dict[str, str] = {}
    lines: list[dict[str, Any]] = []
    for entry in entries:
        if entry["n"] <= start:
            continue
        line_id = str(entry.get("line_id") or "")
        if entry["kind"] == "narrate":
            if line_id:
                topics[line_id] = str(entry.get("topic") or "")
        elif line_id:
            lines.append(
                {
                    "line_id": line_id,
                    "text": str(entry.get("text") or ""),
                    "topic": topics.get(line_id, ""),
                }
            )
    newest = lines[-1]["line_id"] if lines else None
    if after is None:
        return {"lines": [], "last": newest}
    if after == "":
        return {"lines": lines[-limit:], "last": newest}
    index = next((i for i, line in enumerate(lines) if line["line_id"] == after), None)
    if index is None:
        return {"lines": [], "last": newest}
    return {"lines": lines[index + 1 :][:limit], "last": newest}


# --------------------------------------------------------------------------- #
def _seconds(first: Optional[str], second: Optional[str]) -> Optional[float]:
    try:
        return (
            datetime.fromisoformat(str(second)) - datetime.fromisoformat(str(first))
        ).total_seconds()
    except (TypeError, ValueError):
        return None


def _find(log: EvidenceLog, start: int, step: int, kind: str) -> Optional[int]:
    """The nearest line of ``kind`` from ``start`` in direction ``step``."""
    for offset in range(1, PAIR_WINDOW + 1):
        number = start + step * offset
        found = log.kind(number)
        if found is None:
            return None
        if found == kind:
            return number
    return None


def exchange(log: EvidenceLog, number: int) -> Optional[dict[str, Any]]:
    """One model request with its response, resolved from any line of the pair."""
    log.refresh()
    kind = log.kind(number)
    if kind is None:
        return None
    if kind in ANCHORS:
        anchored = _find(log, number, -1, ANCHORS[kind])
        if anchored is None:
            return None
        number, kind = anchored, ANCHORS[kind]
    if kind in PAIRS:
        request_line, response_line = number, _find(log, number, 1, PAIRS[kind])
    elif kind in REQUESTS:
        request_line, response_line = _find(log, number, -1, REQUESTS[kind]), number
    else:
        return None
    request = log.entry(request_line) if request_line else None
    response = log.entry(response_line) if response_line else None
    if request is None and response is None:
        return None
    decision = None
    if response is not None and response["kind"] == "router_response":
        decision_line = _find(log, response["n"], 1, "router_decision")
        decision = log.entry(decision_line) if decision_line else None
    input_tokens = (response or {}).get("input_tokens")
    output_tokens = (response or {}).get("output_tokens")
    price = None
    if input_tokens is not None:
        # The response names the served version (jev-1.14.0); the price list
        # is keyed by the requested model (jev-latest).
        for model in ((request or {}).get("model"), (response or {}).get("model")):
            try:
                price = cost(model, int(input_tokens), int(output_tokens or 0))
                break
            except ValueError:
                continue
    name = (request or response)["kind"]
    return {
        "kind": name.replace("_request", "").replace("_response", ""),
        "request": request,
        "response": response,
        "decision": decision,
        "input_tokens": input_tokens,
        "output_tokens": output_tokens,
        "cost": price,
        "latency_s": _seconds(
            (request or {}).get("timestamp"), (response or {}).get("timestamp")
        ),
    }


# --------------------------------------------------------------------------- #
def _read_json(path: Path) -> Optional[dict[str, Any]]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    return value if isinstance(value, dict) else None


def _attempt(raw: dict[str, Any], root: Path) -> dict[str, Any]:
    """One attempt with its candidate source, read only from inside ``root``.

    ``candidate_path`` comes out of a report file; a hand-edited report could
    point anywhere, so the path is resolved and must lie under the evidence
    directory before it is read.
    """
    source = None
    path = raw.get("candidate_path")
    if path:
        try:
            target = Path(path).resolve()
            if root in target.parents and target.is_file():
                source = target.read_text(encoding="utf-8")[:MAX_SOURCE_CHARS]
        except OSError:
            source = None
    return {
        "attempt": raw.get("attempt"),
        "model": raw.get("model"),
        "static_ok": raw.get("static_ok"),
        "failure_kind": raw.get("failure_kind"),
        "source": source,
        "practices": [
            {
                "seed": practice.get("seed"),
                "passed": practice.get("passed"),
                "outcome": practice.get("outcome"),
                "ticks": practice.get("ticks"),
                "failure_kind": practice.get("failure_kind"),
                "digest": str(practice.get("digest") or "")[:MAX_DIGEST_CHARS],
            }
            for practice in raw.get("practices") or []
        ],
    }


def admission_reports(directory: Path) -> list[dict[str, Any]]:
    """Admission reports, newest first (their names carry a UTC stamp)."""
    directory = Path(directory)
    root = directory.resolve()
    reports = []
    for path in sorted(directory.glob("admission-*.json"), reverse=True):
        raw = _read_json(path)
        if raw is None:
            continue
        reports.append(
            {
                "file": path.name,
                "skill": raw.get("skill_name"),
                "status": raw.get("status"),
                "goal": raw.get("goal"),
                "reason": raw.get("reason"),
                "repairs_used": raw.get("repairs_used"),
                "attempts": [_attempt(item, root) for item in raw.get("attempts") or []],
            }
        )
    return reports


def skills(console: Optional[Console]) -> dict[str, Any]:
    """Core, learned (active) and candidate skills, each with what is known."""
    if console is None:
        # A managed console with no session shows nothing rather than the
        # previous session's catalogue.
        return {"core": [], "learned": [], "candidates": []}
    catalog = load_catalog(
        Path(console.core_dir) if console.core_dir else None,
        Path(console.active_dir) if console.active_dir else None,
    )
    reports = admission_reports(console.evidence.directory)
    core, learned = [], []
    for info in catalog.skills:
        entry = {
            "name": info.name,
            "params": info.params,
            "doc": info.doc,
            "rules": info.rules,
            "source": info.source,
        }
        if info.origin == "core":
            core.append(entry)
        else:
            entry["report"] = next(
                (r for r in reports if r["skill"] == info.name and r["status"] == "admitted"),
                None,
            )
            learned.append(entry)
    known = {info.name for info in catalog.skills}
    candidates, seen = [], set()
    for report in reports:
        if report["skill"] in seen or report["skill"] in known:
            continue
        seen.add(report["skill"])
        candidates.append(report)
    return {"core": core, "learned": learned, "candidates": candidates}


# --------------------------------------------------------------------------- #
def _useful_goal_ticks(log: EvidenceLog, records: dict[str, Any]) -> dict[str, Any]:
    """Two numbers to judge a run by (smarter play slice 1, design D4).

    A goal is *useful* when it raised the ladder past every earlier goal's
    ``rungs_met``; its ticks are the useful ticks. A *stock goal* is one that
    finished ``done``, raised nothing and gathered a count (``count(X) >= n``):
    run 6 played eighteen of them for 80 % wasted ticks. Both come from the
    ``goal_finished`` evidence alone, so they need no plan view.
    """
    best: Optional[int] = None
    useful = total = stock = 0
    for entry in log.tail(kinds=["goal_finished"], limit=10_000):
        met = entry.get("rungs_met")
        ticks = int(entry.get("ticks") or 0)
        total += ticks
        raised = met is not None and (best is None or met > best)
        if met is not None:
            best = met if best is None else max(best, met)
        if raised and entry.get("status") != "failed":
            useful += ticks
        elif entry.get("status") == "done":
            record = records.get(entry.get("goal_id"))
            if record is not None and record.condition.lstrip().startswith("count("):
                stock += 1
    return {
        "useful_goal_ticks": {
            "useful": useful,
            "total": total,
            "ratio": round(useful / total, 3) if total else None,
        },
        "stock_goals": stock,
    }


def metrics(
    console: Optional[Console],
    log: EvidenceLog,
    *,
    ledger: Optional[Ledger] = None,
) -> dict[str, Any]:
    ledger = ledger if ledger is not None else (console.ledger if console else Ledger())
    hours: dict[int, dict[str, float]] = {}
    for item in ledger.entries():
        moment = entry_epoch(item)
        if moment is None:
            continue
        bucket = hours.setdefault(int(moment // 3600), {"cost": 0.0, "calls": 0})
        bucket["cost"] += float(item.get("cost") or 0.0)
        bucket["calls"] += 1
    cost_per_hour = []
    if hours:
        last = max(hours)
        for hour in range(max(min(hours), last - 47), last + 1):
            bucket = hours.get(hour, {"cost": 0.0, "calls": 0})
            cost_per_hour.append({"t": hour * 3600, **bucket})

    records = console.memory.goals() if console is not None else []
    by_skill: dict[str, dict[str, int]] = {}
    for record in records:
        counts = by_skill.setdefault(record.skill or "(none)", {})
        counts[record.status] = counts.get(record.status, 0) + 1
    finished = [r for r in records if r.ticks is not None and r.status != "open"]
    done_ticks = [r.ticks for r in finished if r.status == "done"]
    basis = done_ticks or [r.ticks for r in finished]

    rungs = []
    for entry in log.tail(kinds=["goal_finished"], limit=10_000):
        if entry.get("rungs_met") is not None:
            rungs.append(
                {
                    "n": entry["n"],
                    "t": entry_epoch(entry),
                    "goal_id": entry.get("goal_id"),
                    "rungs": entry["rungs_met"],
                }
            )
    useful = _useful_goal_ticks(log, {r.goal_id: r for r in records})
    return {
        "cost_per_hour": cost_per_hour,
        "goals_by_skill": by_skill,
        "useful_goal_ticks": useful["useful_goal_ticks"],
        "stock_goals": useful["stock_goals"],
        "ticks": {
            "median": statistics.median(basis) if basis else None,
            "recent": [
                {"goal_id": r.goal_id, "ticks": r.ticks, "status": r.status}
                for r in finished[-40:]
            ],
        },
        "rungs": rungs,
        "totals": {
            "goals": len(records),
            "done": sum(1 for r in records if r.status == "done"),
            "failed": sum(1 for r in records if r.status == "failed"),
        },
    }


# --------------------------------------------------------------------------- #
#: Response evidence kind -> the exchange family the Mind page filters by.
EXCHANGE_FAMILIES = {
    "plan_response": "plan",
    "router_response": "router",
    "command_map_response": "command_map",
    "writer_response": "writer",
    "proposal_response": "proposal",
}
EXCHANGES_MAX = 200
#: Characters of a briefing the list view keeps; the inspector has all of it.
BRIEFING_EXCERPT = 600


def _percentile(values: list[float], fraction: float) -> Optional[float]:
    if not values:
        return None
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(round(fraction * (len(ordered) - 1))))]


def _exchange_row(number: int, found: dict[str, Any]) -> dict[str, Any]:
    """One exchange cut down to what a card shows; ``n`` opens the inspector."""
    request = found.get("request") or {}
    response = found.get("response") or {}
    decision = found.get("decision") or {}
    parsed = response.get("parsed")
    row: dict[str, Any] = {
        "n": number,
        "request_n": request.get("n"),
        "kind": found["kind"],
        "timestamp": response.get("timestamp") or request.get("timestamp"),
        "provider": response.get("provider") or request.get("provider"),
        "model": response.get("model") or request.get("model"),
        "latency_s": found.get("latency_s"),
        "input_tokens": found.get("input_tokens"),
        "output_tokens": found.get("output_tokens"),
        "cost": found.get("cost"),
        "error": response.get("error"),
        "invalid_count": response.get("invalid_count"),
        "after_death": response.get("after_death"),
    }
    if found["kind"] == "router":
        row["scores"] = response.get("scores") or {}
        row["decision"] = decision.get("decision")
        row["skill"] = decision.get("skill")
        row["hint"] = decision.get("hint")
        row["source"] = decision.get("source")
        state = request.get("state") or {}
        row["goal"] = state.get("goal") if isinstance(state, dict) else None
    elif found["kind"] == "plan":
        row["parsed"] = parsed
        briefing = request.get("briefing")
        row["briefing"] = (
            briefing[:BRIEFING_EXCERPT] + ("..." if len(briefing) > BRIEFING_EXCERPT else "")
            if isinstance(briefing, str)
            else None
        )
    elif found["kind"] == "command_map":
        row["parsed"] = parsed
        row["text"] = request.get("text")
    else:
        row["parsed"] = parsed
    return slim(row)


def _exchange_summary(rows: list[dict[str, Any]]) -> dict[str, Any]:
    """Count, cost, tokens and latency, overall and per family."""

    def block(items: list[dict[str, Any]]) -> dict[str, Any]:
        latencies = [r["latency_s"] for r in items if isinstance(r.get("latency_s"), (int, float))]
        return {
            "count": len(items),
            "cost": sum(float(r.get("cost") or 0.0) for r in items),
            "input_tokens": sum(int(r.get("input_tokens") or 0) for r in items),
            "output_tokens": sum(int(r.get("output_tokens") or 0) for r in items),
            "latency_p50": _percentile(latencies, 0.5),
            "latency_p95": _percentile(latencies, 0.95),
            "latency_max": max(latencies) if latencies else None,
        }

    families: dict[str, list[dict[str, Any]]] = {}
    for row in rows:
        families.setdefault(row["kind"], []).append(row)
    return {"all": block(rows), "by_kind": {name: block(items) for name, items in families.items()}}


def exchanges(
    log: EvidenceLog,
    *,
    limit: int = 60,
    kinds: Optional[list[str]] = None,
) -> dict[str, Any]:
    """The newest model exchanges, newest first, each ready to draw as a card.

    ``kinds`` are exchange families (``plan``, ``router``, ``command_map``,
    ``writer``, ``proposal``). The summary covers exactly the rows returned.
    """
    limit = max(1, min(int(limit), EXCHANGES_MAX))
    wanted = [
        response
        for response, family in EXCHANGE_FAMILIES.items()
        if not kinds or family in kinds
    ]
    rows = []
    for number in reversed(log.numbers(kinds=wanted, limit=limit)):
        found = exchange(log, number)
        if found is not None:
            rows.append(_exchange_row(number, found))
    return {"items": rows, "summary": _exchange_summary(rows)}


# --------------------------------------------------------------------------- #
#: Idle gaps shorter than this are noise (one tick of bookkeeping between goals).
MIN_GAP_SECONDS = 1.0


def _merge(intervals: list[tuple[float, float]]) -> list[tuple[float, float]]:
    merged: list[tuple[float, float]] = []
    for start, end in sorted(intervals):
        if merged and start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))
    return merged


def goals_timeline(log: EvidenceLog, run: Optional[int] = None) -> dict[str, Any]:
    """The goals of one run as bars on a time axis, from the evidence alone.

    A run is the stretch from one ``loop_started`` to the next (lines before the
    first one form run 0). A goal begins at its ``command_sent`` and ends at its
    ``goal_finished``; one with no finish is ``running`` in the newest run and
    ``unfinished`` in an older one (the process ended under it). The idle gaps
    are the stretches of the run in which no goal was active, so waiting is a
    number instead of an impression. ``run`` defaults to the newest run that
    sent at least one command.
    """
    total = log.refresh()
    starts = log.numbers(kinds=["loop_started"], limit=10_000)
    bounds = [0] + starts if not starts or starts[0] != 1 else list(starts)
    boundaries = [(start, (bounds[i + 1] - 1) if i + 1 < len(bounds) else total) for i, start in enumerate(bounds)]
    entries = log.tail(kinds=["command_sent", "goal_finished"], limit=20_000)

    def index_of(number: int) -> int:
        for position in range(len(boundaries) - 1, -1, -1):
            if number > boundaries[position][0]:
                return position
        return 0

    per_run: dict[int, list[dict[str, Any]]] = {}
    open_by_command: dict[tuple[int, Any], dict[str, Any]] = {}
    for entry in entries:
        position = index_of(entry["n"])
        epoch = entry_epoch(entry)
        if epoch is None:
            continue
        if entry["kind"] == "command_sent":
            goal = {
                "goal_id": entry.get("goal_id"),
                "command_id": entry.get("command_id"),
                "skill": entry.get("skill"),
                "condition": entry.get("condition"),
                "n": entry["n"],
                "t0": epoch,
                "t1": None,
                "status": None,
                "failure_kind": None,
                "ticks": None,
                "rungs_met": None,
            }
            per_run.setdefault(position, []).append(goal)
            open_by_command[(position, entry.get("command_id"))] = goal
        else:
            goal = open_by_command.pop((position, entry.get("command_id")), None)
            if goal is None:
                continue
            goal.update(
                t1=epoch,
                status=entry.get("status"),
                failure_kind=entry.get("failure_kind"),
                ticks=entry.get("ticks"),
                rungs_met=entry.get("rungs_met"),
            )

    runs = []
    for position, (first, last) in enumerate(boundaries):
        head = log.entry(first) if first else None
        runs.append(
            {
                "index": position,
                "n": first,
                "started_at": entry_epoch(head) if head else None,
                "goals": len(per_run.get(position, [])),
                "last_n": last,
            }
        )
    with_goals = [r["index"] for r in runs if r["goals"]]
    chosen = run if run is not None and 0 <= run < len(runs) else (with_goals[-1] if with_goals else len(runs) - 1)
    selected = runs[chosen]
    newest = chosen == len(runs) - 1
    goals_here = per_run.get(chosen, [])
    for goal in goals_here:
        if goal["status"] is None:
            goal["status"] = "running" if newest else "unfinished"

    tail_entry = log.entry(selected["last_n"]) if selected["last_n"] else None
    begin = selected["started_at"]
    end = entry_epoch(tail_entry) if tail_entry else None
    finishes = [g["t1"] for g in goals_here if g["t1"] is not None]
    if goals_here:
        begin = begin if begin is not None else goals_here[0]["t0"]
        end = max([e for e in [end, *finishes] if e is not None], default=goals_here[-1]["t0"])
        if newest and any(g["status"] == "running" for g in goals_here):
            end = max(end, time.time())
    gaps: list[dict[str, float]] = []
    busy = 0.0
    if goals_here and begin is not None and end is not None:
        spans = [(g["t0"], g["t1"] if g["t1"] is not None else end) for g in goals_here]
        merged = _merge(spans)
        busy = sum(b - a for a, b in merged)
        cursor = begin
        for a, b in merged:
            if a - cursor >= MIN_GAP_SECONDS:
                gaps.append({"t0": cursor, "t1": a})
            cursor = max(cursor, b)
        if end - cursor >= MIN_GAP_SECONDS:
            gaps.append({"t0": cursor, "t1": end})
    span = (end - begin) if begin is not None and end is not None else 0.0
    return {
        "runs": runs,
        "run": chosen,
        "begin": begin,
        "end": end,
        "goals": goals_here,
        "gaps": gaps,
        "idle": {
            "busy_s": round(busy, 1),
            "idle_s": round(max(0.0, span - busy), 1),
            "longest_gap_s": round(max((g["t1"] - g["t0"] for g in gaps), default=0.0), 1),
        },
    }


# --------------------------------------------------------------------------- #
#: Search results per query: a card list, not a document.
WIKI_RESULTS = 6
WIKI_QUERY_CHARS = 200


def wiki_search(query: str, k: int = WIKI_RESULTS) -> dict[str, Any]:
    """Wiki passages for ``query``; ``available`` says whether an index answers.

    The wiki package needs numpy and a locally built index, so it is imported
    here, guarded: a machine without either gets ``available: false`` and the
    page explains how to build one, instead of a server error.
    """
    try:
        from .. import wiki  # noqa: PLC0415 - optional dependency, guarded
    except Exception:  # noqa: BLE001 - any import failure means "not available"
        return {"available": False, "results": []}
    try:
        available = wiki._state() is not None  # noqa: SLF001 - the module's own loaded-index probe
    except Exception:  # noqa: BLE001
        available = False
    text = " ".join(str(query or "").split())[:WIKI_QUERY_CHARS]
    if not available or not text:
        return {"available": available, "results": []}
    snippets = wiki.search(text, k)
    return {
        "available": True,
        "results": [
            {
                "title": snippet.title,
                "text": snippet.text,
                "url": snippet.url,
                "score": round(float(snippet.score), 4),
            }
            for snippet in snippets
        ],
    }


__all__ = [
    "admission_reports",
    "exchange",
    "exchanges",
    "goals",
    "goals_timeline",
    "metrics",
    "narration_lines",
    "skills",
    "slim",
    "state",
    "wiki_search",
]
