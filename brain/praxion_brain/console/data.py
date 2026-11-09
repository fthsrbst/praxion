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
from ..ledger import DEFAULT_BUDGETS, cost, entry_epoch
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
}
#: Response kind -> the request kind it answers (the reverse of :data:`PAIRS`).
REQUESTS = {response: request for request, response in PAIRS.items()}
#: Lines that belong to an exchange but are not part of it -> the response they follow.
ANCHORS = {
    "router_decision": "router_response",
    "loop_route": "router_response",
    "proposal_result": "proposal_response",
    "admission_attempt": "writer_response",
}
#: How far from a line its request, response or decision is searched.
PAIR_WINDOW = 60
MAX_SOURCE_CHARS = 8000
MAX_DIGEST_CHARS = 1500


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
def state(console: Console) -> dict[str, Any]:
    """The loop's published state plus the money, read fresh."""
    result = dict(console.state)
    ledger = console.ledger
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
    blocked = console.memory.blocked()
    result["rungs"] = [
        {
            "id": rung.id,
            "condition": rung.condition,
            "blocked": normalize_condition(rung.condition) in blocked,
        }
        for rung in RUNGS
    ]
    result["spent_last_hour"] = ledger.spent_since(time.time() - 3600)
    return result


def goals(console: Console) -> list[dict[str, Any]]:
    return [record.model_dump() for record in console.memory.goals()]


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


def skills(console: Console) -> dict[str, Any]:
    """Core, learned (active) and candidate skills, each with what is known."""
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


def metrics(console: Console, log: EvidenceLog) -> dict[str, Any]:
    hours: dict[int, dict[str, float]] = {}
    for item in console.ledger.entries():
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

    records = console.memory.goals()
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


__all__ = [
    "admission_reports",
    "exchange",
    "goals",
    "metrics",
    "skills",
    "slim",
    "state",
]
