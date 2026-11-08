"""Live tests for the body port (Demo-2 step A2/A3, proposal §2.6).

The body runs in port mode (``start_session``: one process, many goals) and the
brain side is the real client from ``brain/praxion_brain/port.py``. The cases
implemented here are the proposal's live list:

1. ``get_wooden_pickaxe`` over the port gives the same referee inventory as the
   CLI smoke test, plus ``settled done`` and a true completion condition.
2. Two commands back to back in one process: the body is multi-goal.
3. A command while another runs: the first goes ``suspended``, the second is
   ``accepted`` with a higher generation, and the first resumes after it.
5. An expired command (ttl 0) never starts; a stale generation is rejected.
7. Tick work with the port on versus off (measurement 3).

Run:

```sh
cd harness
python -m pytest -q -m server tests/test_body_port.py
```
"""

from __future__ import annotations

import sys
import time
from typing import Any, Dict, Iterable, List, Optional

import pytest

from praxion_harness.body import BodySession, start_body, start_session
from praxion_harness.paths import repo_root

# The brain package is the other half of the wire; only this module needs it.
BRAIN_DIR = repo_root() / "brain"
if str(BRAIN_DIR) not in sys.path:
    sys.path.insert(0, str(BRAIN_DIR))

# The client is unit-tested in `brain/`; a harness checkout without its
# dependencies skips these live cases instead of failing collection.
pytest.importorskip(
    "pydantic", reason="the brain port client needs pydantic (brain/pyproject.toml)"
)
from praxion_brain.port import (  # noqa: E402  (the path is set just above)
    EventMessage,
    PortClient,
    PortClosed,
    Snapshot,
)

pytestmark = pytest.mark.server

BODY = "Praxion"
WOODEN_TIMEOUT = 300.0
SECOND_GOAL_TIMEOUT = 300.0
#: Generous enough that a slow accept can never expire the command.
TTL_TICKS = 6000


# --------------------------------------------------------------------------- #
# helpers
# --------------------------------------------------------------------------- #
def _count(inventory: Dict[str, int], item: str) -> int:
    return inventory.get(f"minecraft:{item}", inventory.get(item, 0))


def _setup_tools(fixture, referee, run) -> None:
    """Daytime, no mobs, and enough logs for two or three crafts."""
    fixture.advance_time(False)
    fixture.spawn_mobs(False)
    fixture.time_set(1000)
    fixture.give(BODY, "oak_log", 12)
    fixture.give(BODY, "cobblestone", 4)


def _context(session: BodySession, extra: Iterable[str] = ()) -> str:
    return "\n".join(
        [
            f"run dir: {session.run_dir}",
            f"summary: {session.summary()}",
            f"body output tail:\n{session.output()[-2000:]}",
            *extra,
        ]
    )


def _next_snapshot(client: PortClient, timeout: float) -> Snapshot:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        message = client.poll(0.1)
        if isinstance(message, Snapshot):
            return message
        client.keepalive()
    raise TimeoutError(f"no snapshot within {timeout:.0f}s")


def _request_snapshot(client: PortClient, timeout: float = 10.0) -> Snapshot:
    client.resync()
    return _next_snapshot(client, timeout)


def _wait_receipt(
    client: PortClient,
    command_id: str,
    state: str,
    timeout: float,
    *,
    seen: Optional[List[Dict[str, Any]]] = None,
) -> Dict[str, Any]:
    """Wait for one ``command_receipt`` event of ``command_id`` in ``state``.

    Events already in ``seen`` count: one tick often carries several events.
    """
    for event in seen or []:
        if (
            event.get("kind") == "command_receipt"
            and event.get("command_id") == command_id
            and event.get("state") == state
        ):
            return event
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            message = client.poll(0.1)
        except PortClosed as error:
            raise AssertionError(f"the body closed the port: {error}") from error
        if message is None:
            client.keepalive()
            continue
        if isinstance(message, EventMessage):
            if seen is not None:
                seen.append(message.event)
            event = message.event
            if (
                event.get("kind") == "command_receipt"
                and event.get("command_id") == command_id
                and event.get("state") == state
            ):
                return event
    raise TimeoutError(
        f"no {state!r} receipt for {command_id!r} within {timeout:.0f}s"
    )


def _wait_event(
    client: PortClient,
    predicate,
    timeout: float,
    *,
    seen: Optional[List[Dict[str, Any]]] = None,
) -> Dict[str, Any]:
    """Wait for any forwarded event matching ``predicate``.

    Events already in ``seen`` count: an `intent_state` arrives in the same
    tick as the receipt the caller waited for before.
    """
    for event in seen or []:
        if predicate(event):
            return event
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            message = client.poll(0.1)
        except PortClosed as error:
            raise AssertionError(f"the body closed the port: {error}") from error
        if message is None:
            client.keepalive()
            continue
        if isinstance(message, EventMessage):
            if seen is not None:
                seen.append(message.event)
            if predicate(message.event):
                return message.event
    raise TimeoutError(f"no matching event within {timeout:.0f}s")


def _stack_states(snapshot: Snapshot) -> List[Dict[str, Any]]:
    return [
        {"intent_id": entry.intent_id, "skill": entry.skill, "state": entry.state}
        for entry in snapshot.stack
    ]


def _wait_summary(session: BodySession, goals: int, timeout: float = 10.0) -> Dict[str, Any]:
    """Wait until summary.json lists ``goals`` settled goals."""
    deadline = time.monotonic() + timeout
    summary: Optional[Dict[str, Any]] = None
    while time.monotonic() < deadline:
        summary = session.summary()
        if summary is not None and len(summary.get("goals") or []) >= goals:
            return summary
        time.sleep(0.1)
    raise TimeoutError(
        f"summary.json did not list {goals} goals within {timeout:.0f}s: {summary}"
    )


def _open(port: int) -> PortClient:
    client = PortClient(port, handshake_timeout=15.0, connect_timeout=15.0)
    client.connect()
    return client


# --------------------------------------------------------------------------- #
# case 1: one goal over the port
# --------------------------------------------------------------------------- #
def test_one_goal_over_the_port_gives_the_same_referee_inventory() -> None:
    with start_session(101, setup=_setup_tools) as session:
        with _open(session.port) as client:
            snapshot = _next_snapshot(client, 15.0)
            command_id = client.send_command(
                "get_wooden_pickaxe",
                condition="has(wooden_pickaxe)",
                expires_at_tick=snapshot.tick + TTL_TICKS,
                based_on_generation=snapshot.generation,
                budget_ticks=12000,
            )
            settled = _wait_receipt(client, command_id, "settled", WOODEN_TIMEOUT)
            context = _context(session, [f"settled: {settled}"])
            # The event log is the session-side truth: wait for this command's
            # own receipt, never for a summary that may still be the last one.
            logged = session.wait_for_goal(command_id, 10.0)
            assert logged.get("outcome") == "done", context

            assert settled["outcome"] == "done", context
            assert settled.get("condition") == "true", (
                "the body must evaluate the goal's condition on its own facts\n" + context
            )
            inventory = session.referee.player_inventory(BODY)
            assert _count(inventory, "wooden_pickaxe") >= 1, (
                "the referee must see the pickaxe\n" + context
            )
            fresh = _request_snapshot(client)
            assert fresh.facts.inventory.get("wooden_pickaxe", 0) >= 1, (
                f"the snapshot inventory must agree with the referee: {fresh.facts.inventory}"
                "\n" + context
            )
            assert not any(entry.state == "running" for entry in fresh.stack), context


# --------------------------------------------------------------------------- #
# case 2: two commands back to back in one process
# --------------------------------------------------------------------------- #
def test_two_goals_run_back_to_back_in_one_process() -> None:
    with start_session(202, setup=_setup_tools) as session:
        with _open(session.port) as client:
            snapshot = _next_snapshot(client, 15.0)
            first = client.send_command(
                "get_wooden_pickaxe",
                expires_at_tick=snapshot.tick + TTL_TICKS,
                based_on_generation=snapshot.generation,
                budget_ticks=12000,
            )
            assert _wait_receipt(client, first, "settled", WOODEN_TIMEOUT)["outcome"] == "done"
            assert session.wait_for_goal(first, 10.0).get("outcome") == "done"

            # The generation moved; refresh it the way the loop does.
            snapshot = _request_snapshot(client)
            second = client.send_command(
                "get_crafting_table",
                expires_at_tick=snapshot.tick + TTL_TICKS,
                based_on_generation=snapshot.generation,
                budget_ticks=12000,
            )
            settled = _wait_receipt(client, second, "settled", SECOND_GOAL_TIMEOUT)
            context = _context(session, [f"second settled: {settled}"])
            assert session.wait_for_goal(second, 10.0).get("outcome") == "done", context
            assert settled["outcome"] == "done", context
            assert session.process.poll() is None, (
                "the body must stay alive after a goal\n" + context
            )

            summary = _wait_summary(session, 2)
            goals = summary.get("goals", [])
            assert len(goals) == 2, f"one process, two goals: {goals}\n" + context
            assert [goal["command_id"] for goal in goals] == [first, second], context
            assert summary["goal"]["skill"] == "get_crafting_table", context
            assert [goal["outcome"] for goal in goals] == ["done", "done"], context


# --------------------------------------------------------------------------- #
# case 3: a command while another runs
# --------------------------------------------------------------------------- #
def test_a_second_command_suspends_the_first_and_the_first_resumes() -> None:
    with start_session(303, setup=_setup_tools) as session:
        with _open(session.port) as client:
            seen: List[Dict[str, Any]] = []
            snapshot = _next_snapshot(client, 15.0)
            first = client.send_command(
                "get_wooden_pickaxe",
                expires_at_tick=snapshot.tick + TTL_TICKS,
                based_on_generation=snapshot.generation,
                budget_ticks=12000,
            )
            accepted_first = _wait_receipt(client, first, "accepted", 30.0, seen=seen)
            first_intent = accepted_first["intent_id"]

            snapshot = _request_snapshot(client)
            second = client.send_command(
                "get_crafting_table",
                expires_at_tick=snapshot.tick + TTL_TICKS,
                based_on_generation=snapshot.generation,
                budget_ticks=12000,
            )
            accepted_second = _wait_receipt(client, second, "accepted", 30.0, seen=seen)
            context = _context(
                session,
                [f"first: {accepted_first}", f"second: {accepted_second}"],
            )
            assert accepted_second["generation"] > accepted_first["generation"], context

            suspended = _wait_event(
                client,
                lambda event: (
                    event.get("kind") == "intent_state"
                    and event.get("intent_id") == first_intent
                    and event.get("state") == "suspended"
                ),
                30.0,
                seen=seen,
            )
            assert suspended["state"] == "suspended", context

            assert (
                _wait_receipt(client, second, "settled", SECOND_GOAL_TIMEOUT, seen=seen)[
                    "outcome"
                ]
                == "done"
            ), context
            resumed = _wait_event(
                client,
                lambda event: (
                    event.get("kind") == "intent_state"
                    and event.get("intent_id") == first_intent
                    and event.get("state") == "running"
                ),
                30.0,
                seen=seen,
            )
            assert resumed["intent_id"] == first_intent, context
            assert (
                _wait_receipt(client, first, "settled", SECOND_GOAL_TIMEOUT, seen=seen)[
                    "outcome"
                ]
                == "done"
            ), context
            assert session.wait_for_goal(first, 10.0).get("outcome") == "done", context


# --------------------------------------------------------------------------- #
# case 5: expiry and stale generation
# --------------------------------------------------------------------------- #
def test_expired_and_stale_commands_never_start() -> None:
    with start_session(101, setup=_setup_tools) as session:
        with _open(session.port) as client:
            snapshot = _next_snapshot(client, 15.0)

            expired_id = client.send_command(
                "get_wooden_pickaxe",
                expires_at_tick=0,
                based_on_generation=snapshot.generation,
            )
            expired = _wait_receipt(client, expired_id, "expired", 15.0)
            assert expired["command_id"] == expired_id

            stale_id = client.send_command(
                "get_wooden_pickaxe",
                expires_at_tick=snapshot.tick + TTL_TICKS,
                based_on_generation=snapshot.generation + 5,
            )
            rejected = _wait_receipt(client, stale_id, "rejected", 15.0)
            assert rejected["reason"] == "stale_generation"

            fresh = _request_snapshot(client)
            context = _context(session, [f"stack: {_stack_states(fresh)}"])
            assert _stack_states(fresh) == [], (
                "neither command may start\n" + context
            )
            events = session.events()
            assert not any(
                event.get("kind") == "intent_pushed"
                and event.get("skill") == "get_wooden_pickaxe"
                for event in events
            ), context


# --------------------------------------------------------------------------- #
# case 7: port overhead on the tick (measurement 3)
# --------------------------------------------------------------------------- #
def test_port_on_tick_work_is_close_to_port_off() -> None:
    """The same goal with the port off (CLI) and on; p99 work must stay sane."""
    with start_body(
        101,
        "get_wooden_pickaxe",
        setup=_setup_tools,
        max_ticks=6000,
        start_delay_ticks=80,
    ) as run:
        off = run.wait_for_outcome(WOODEN_TIMEOUT)
        off_times = off["tick_time"]

    with start_session(101, setup=_setup_tools) as session:
        with _open(session.port) as client:
            snapshot = _next_snapshot(client, 15.0)
            command_id = client.send_command(
                "get_wooden_pickaxe",
                expires_at_tick=snapshot.tick + TTL_TICKS,
                based_on_generation=snapshot.generation,
                budget_ticks=12000,
            )
            _wait_receipt(client, command_id, "settled", WOODEN_TIMEOUT)
            # The driver writes the summary on settle; give the file a moment.
            on = None
            deadline = time.monotonic() + 5.0
            while time.monotonic() < deadline:
                summary = session.summary()
                if summary and summary.get("goals"):
                    on = summary
                    break
                time.sleep(0.1)
            context = _context(session, [f"port off: {off_times}", f"port on: {on}"])

    assert on is not None, context
    on_times = on["tick_time"]
    assert on_times["samples"] > 0, context
    assert on_times["work_p99_ms"] <= 25.0, (
        "the port must not push a tick over the 50 ms budget\n" + context
    )
    # A measurement, not a tight bound: the body's tick work is dominated by
    # gameplay, and both runs are different worlds. Flag a real regression only.
    allowed = max(off_times["work_p99_ms"] * 3.0, off_times["work_p99_ms"] + 10.0)
    assert on_times["work_p99_ms"] <= allowed, (
        f"port p99 {on_times['work_p99_ms']:.3f} ms vs off "
        f"{off_times['work_p99_ms']:.3f} ms\n" + context
    )
