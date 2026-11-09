"""One connection to one body: reader thread, keepalive, reconnect, snapshots.

It owns the socket's read side, keepalive, bye and dropped handling, reconnect
timing, the snapshot cache, refreshes and sends; no goal, record, route or model
call. The owner hears about events, snapshots and errors through the listener.
Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` sections 2 and 3.1.
"""

from __future__ import annotations

import time
from typing import Any, Callable, Mapping, Optional, Protocol

from .memory import EvidenceSink
from .conditions import is_verifiable
from .port import (
    Bye,
    Dropped,
    EventMessage,
    PortClient,
    PortError,
    PortReader,
    ProtocolError,
    Snapshot,
)

#: Per-goal body limit in ticks.
COMMAND_BUDGET_TICKS = 6000
#: Reconnect delay after a lost connection, and between failed attempts.
RECONNECT_DELAY = 0.5
RECONNECT_RETRY = 1.0
#: Bye reasons that end this brain; ``replaced`` means another brain took over.
TERMINAL_BYES = frozenset({"shutdown", "version", "replaced"})
#: The recovery cause of the first connection and of a reconnect.
RECOVERY_START = "start"
RECOVERY_RECONNECT = "reconnect"


class SessionListener(Protocol):
    """What the session must tell its owner; everything else stays inside."""

    def on_event(self, event: Mapping[str, Any]) -> None:
        """One body event, in arrival order, while the owner was blocked."""

    def on_snapshot(self, snapshot: Snapshot, *, heartbeat_pending: bool) -> None:
        """A snapshot arrived and is already cached; ``heartbeat_pending`` says
        whether a heartbeat asked for it."""

    def on_error(self, error: PortError) -> None:
        """A protocol error the owner must count as a step error (A6)."""


class BodySession:
    """The wire: one reader thread, one queue, one snapshot cache, one connection."""

    def __init__(
        self,
        port: PortClient,
        *,
        evidence: EvidenceSink,
        listener: SessionListener,
        clock: Callable[[], float] = time.monotonic,
        state: Callable[[], str],
        log: Callable[[str], None] = print,
        command_budget_ticks: int = COMMAND_BUDGET_TICKS,
        reconnect_delay: float = RECONNECT_DELAY,
        reconnect_retry: float = RECONNECT_RETRY,
        terminal_byes: frozenset[str] = TERMINAL_BYES,
    ) -> None:
        self.port = port
        self.evidence = evidence
        self.command_budget_ticks = int(command_budget_ticks)
        self.reconnect_delay = float(reconnect_delay)
        self.reconnect_retry = float(reconnect_retry)
        self.terminal_byes = terminal_byes

        self._listener = listener
        self._clock = clock
        self._state = state
        self._log = log
        self._reader = PortReader(port)
        self._snapshot: Optional[dict[str, Any]] = None
        #: ``start``/``reconnect`` until the next snapshot is taken as a recovery.
        self._recovering: Optional[str] = None
        #: A heartbeat asked for a snapshot that has not arrived yet.
        self._awaiting_snapshot = False
        self._next_reconnect = 0.0
        self._closed = False
        self._terminal = False

    # -- what the owner reads ------------------------------------------- #
    @property
    def connected(self) -> bool:
        return self.port.connected

    @property
    def reading(self) -> bool:
        """Whether the reader thread is still alive and filling the queue."""
        return self._reader.running

    @property
    def recovering(self) -> Optional[str]:
        """The cause the next snapshot must recover from, or ``None``."""
        return self._recovering

    @property
    def awaiting_snapshot(self) -> bool:
        """A heartbeat asked for a snapshot that has not arrived yet."""
        return self._awaiting_snapshot

    @property
    def terminated(self) -> bool:
        """The body said a terminal bye; the owner should stop."""
        return self._terminal

    @property
    def snapshot(self) -> Optional[dict[str, Any]]:
        """The last snapshot as a plain dict, or ``None`` before the first one."""
        return self._snapshot

    @property
    def tick(self) -> Optional[int]:
        return self.port.tick

    @property
    def generation(self) -> Optional[int]:
        return self.port.generation

    @property
    def command_ttl_ticks(self) -> int:
        return self.port.command_ttl_ticks

    @property
    def port_number(self) -> Optional[int]:
        """The body's TCP port, for evidence only."""
        return getattr(self.port, "port", None)

    # -- lifecycle ------------------------------------------------------ #
    def start(self) -> None:
        """Connect and start the reader; the first snapshot is the start recovery."""
        self._closed = False
        self._terminal = False
        self.port.connect()
        self._reader.start()
        self._recovering = RECOVERY_START

    def close(self) -> None:
        """Stop reading and close the socket; no reconnect follows."""
        self._closed = True
        self._reader.stop()
        self.port.close()

    def disconnect(self, error: str) -> None:
        """The connection is gone: stop reading, close it, plan a reconnect."""
        self._reader.stop()
        self.port.close()
        self._next_reconnect = self._clock() + self.reconnect_delay
        self._awaiting_snapshot = False
        self.evidence.write("loop_disconnected", error=error, state=self._state())

    def try_reconnect(self) -> bool:
        """Dial again once the delay passed; the next snapshot is the recovery."""
        now = self._clock()
        if now < self._next_reconnect:
            return False
        self._next_reconnect = now + self.reconnect_retry
        try:
            self.port.connect()
        except (PortError, OSError) as error:
            self.evidence.write("loop_reconnect_failed", error=str(error))
            return False
        self._reader.start()
        self._recovering = RECOVERY_RECONNECT
        self._awaiting_snapshot = False
        self.evidence.write("loop_reconnect", port=self.port_number)
        return True

    def take_recovery(self) -> Optional[str]:
        """The cause of the pending recovery (``start``/``reconnect``), once."""
        cause, self._recovering = self._recovering, None
        return cause

    # -- receiving ------------------------------------------------------ #
    def poll(self, timeout: Optional[float] = 0.0) -> Optional[EventMessage]:
        """Handle one queued message and return it only when it is a body event.

        A snapshot, bye, ``dropped`` and ``pong`` are the session's own business;
        a :class:`PortError` is handled or announced here too. Only the event
        comes back, for the owner to dispatch.
        """
        message = self._reader.get(timeout)
        if isinstance(message, PortError):
            self._on_port_error(message)
            return None
        if message is None or isinstance(message, EventMessage):
            return message
        self._take(message)
        return None

    def refresh(self, timeout: float = 1.0) -> bool:
        """Ask for one snapshot and consume the queue until it arrives.

        Every event that arrives first goes to the owner in arrival order, as
        if the owner had polled it; the awaited snapshot comes last.
        """
        if not self.connected:
            return False
        try:
            self.port.resync()
        except PortError as error:
            self.disconnect(str(error))
            return False
        deadline = time.monotonic() + timeout
        while not self._closed:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            message = self._reader.get(remaining)
            if isinstance(message, PortError):
                self._on_port_error(message)
                return False
            if message is None:
                continue
            if isinstance(message, EventMessage):
                self._listener.on_event(message.event)
            elif self._take(message):
                return True
        return False

    def request_snapshot(self) -> bool:
        """Ask for a snapshot for the heartbeat; ``False`` when it could not leave."""
        if not self.connected:
            return False
        self._awaiting_snapshot = True
        try:
            self.port.resync()
        except PortError as error:
            self.disconnect(str(error))
            return False
        return True

    # -- sending -------------------------------------------------------- #
    def send_goal(
        self,
        skill: str,
        condition: str,
        args: list[str] | None = None,
        *,
        budget_ticks: int | None = None,
    ) -> str:
        """Send one command against the current snapshot and return its id.

        Only a condition the brain can verify (inventory facts) goes to the body:
        the body rejected others as `bad_condition` and the goal never ran. An
        unverifiable goal still runs and is recorded unverifiable (session 10).
        ``budget_ticks`` overrides the session default (a night goal outlasts the
        night; every other goal keeps the default).
        """
        if self.port.tick is None or self.port.generation is None:
            raise PortError("no snapshot yet")
        return self.port.send_command(
            skill,
            args=list(args or []),
            condition=condition if is_verifiable(condition) else None,
            budget_ticks=(
                self.command_budget_ticks if budget_ticks is None else int(budget_ticks)
            ),
            expires_at_tick=int(self.port.tick) + self.port.command_ttl_ticks,
            based_on_generation=int(self.port.generation),
        )

    # -- inside the queue ----------------------------------------------- #
    def _take(self, message: Any) -> bool:
        """Handle a non-event message; ``True`` when it was a snapshot.

        A ``pong`` is liveness only: the reader thread already counted it.
        """
        if isinstance(message, Snapshot):
            self._on_snapshot(message)
            return True
        if isinstance(message, Bye):
            self._on_bye(message.reason)
        elif isinstance(message, Dropped):
            self.evidence.write("loop_dropped", n=message.n, tick=message.tick)
        return False

    def _on_snapshot(self, snapshot: Snapshot) -> None:
        self._snapshot = snapshot.model_dump(mode="python")
        pending, self._awaiting_snapshot = self._awaiting_snapshot, False
        self._listener.on_snapshot(snapshot, heartbeat_pending=pending)

    def _on_port_error(self, error: PortError) -> None:
        """A broken frame is a step error (A6); a closed socket is a disconnect."""
        if isinstance(error, ProtocolError):
            self._listener.on_error(error)
        else:
            self.disconnect(str(error))

    def _on_bye(self, reason: str) -> None:
        self.evidence.write("loop_bye", reason=reason, state=self._state())
        if reason in self.terminal_byes:
            self._closed = True
            self._terminal = True
            if reason == "replaced":
                self._log(
                    "loop: the body said bye(replaced): another brain took the "
                    "connection, stopping so the two do not ping-pong"
                )
            else:
                self._log(f"loop: the body said bye({reason}), stopping")
            return
        self._next_reconnect = self._clock() + self.reconnect_delay


__all__ = [
    "BodySession",
    "COMMAND_BUDGET_TICKS",
    "RECONNECT_DELAY",
    "RECONNECT_RETRY",
    "RECOVERY_RECONNECT",
    "RECOVERY_START",
    "SessionListener",
    "TERMINAL_BYES",
]
