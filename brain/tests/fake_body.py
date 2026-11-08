"""A scriptable in-process body for brain port tests.

``FakeBody`` listens on a loopback ephemeral port and speaks the protocol of
``docs/proposals/2026-11-08-demo2-loop.md`` section 2. Like the Rust body in
``body/crates/port`` it sends its ``hello`` first and then waits at most
:data:`HANDSHAKE_TIMEOUT_S` (2 s) for the brain's ``hello``; a first frame that
is not ``hello`` gets ``bye {reason: "protocol"}``. A new connection replaces
the old one, which gets ``bye {reason: "replaced"}``. On a successful connect it
sends a snapshot unless ``send_snapshot_on_connect`` is false.

Tests script it from the main thread: ``emit_event`` appends an event,
``advance`` moves the tick, ``drop_next_events`` suppresses frames to force a
seq gap, ``dropped`` sends an overflow notice followed by a snapshot,
``disconnect`` drops the current connection (the server keeps listening),
``settle`` finishes a command, ``set_intent_state`` moves an intent to
``done``/``suspended`` and ``stop_reading`` pauses the reader so a brain send
can stall.
"""

from __future__ import annotations

import select
import socket
import threading
import time
from typing import Any, Optional

from praxion_brain.port import (
    PORT_VERSION,
    Bye,
    ClientHello,
    Command,
    Dropped,
    EventMessage,
    Facts,
    Hello,
    Ping,
    Pong,
    Pop,
    PortClosed,
    ProtocolError,
    ResyncRequest,
    Snapshot,
    StackEntry,
    encode_frame,
    parse_client_message,
    read_frame,
)

#: How long the brain has to answer the body's ``hello``, mirroring the Rust
#: server's constant.
HANDSHAKE_TIMEOUT_S = 2.0

#: Facts a fresh fake body reports until a test changes them.
DEFAULT_FACTS: dict[str, Any] = {
    "health": 20,
    "food": 18,
    "air": 300,
    "is_night": False,
    "day_time": 0,
    "dimension": "overworld",
    "pos": [0, 64, 0],
    "inventory": {},
}


class FakeBody:
    """One fake body server, usable as a context manager."""

    def __init__(
        self,
        *,
        host: str = "127.0.0.1",
        port_version: int = PORT_VERSION,
        accept_version: Optional[int] = None,
        body: str = "fake-body 0.0.0",
        run_id: str = "fake-run",
        skills: Optional[set[str]] = None,
        max_stack: int = 8,
        generation: int = 0,
        tick: int = 0,
        seq: int = 0,
        facts: Optional[dict[str, Any]] = None,
        stale_ticks: int = 0,
        send_snapshot_on_connect: bool = True,
        handshake_timeout: float = HANDSHAKE_TIMEOUT_S,
    ) -> None:
        self.host = host
        self.port_version = port_version
        self.accept_version = port_version if accept_version is None else accept_version
        self.body = body
        self.run_id = run_id
        self.skills = skills
        self.max_stack = max_stack
        self.send_snapshot_on_connect = send_snapshot_on_connect
        self.handshake_timeout = handshake_timeout

        self.generation = generation
        self.tick = tick
        self.seq = seq
        self.owner: Optional[str] = None
        self.stack: list[StackEntry] = []
        self.facts: dict[str, Any] = dict(DEFAULT_FACTS if facts is None else facts)
        self.stale_ticks = stale_ticks

        self.commands: list[Command] = []
        self.receipts: list[dict[str, Any]] = []
        self.settled: list[dict[str, Any]] = []
        self.pops: list[int] = []
        self.errors: list[str] = []
        self.connections = 0
        self.port = 0

        self._next_intent = 0
        self._command_intents: dict[str, int] = {}
        self._drop_events = 0
        self._read_paused = False
        self._state = threading.RLock()
        self._send_lock = threading.Lock()
        self._stop = threading.Event()
        self._listener: Optional[socket.socket] = None
        self._conn: Optional[socket.socket] = None
        self._thread: Optional[threading.Thread] = None
        self._handlers: list[threading.Thread] = []

    # -- lifecycle ------------------------------------------------------- #
    def start(self) -> "FakeBody":
        if self._listener is not None:
            raise RuntimeError("FakeBody already started")
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((self.host, 0))
        listener.listen(2)
        listener.settimeout(0.2)
        self._listener = listener
        self.port = listener.getsockname()[1]
        self._thread = threading.Thread(
            target=self._accept_loop, name="fake-body-accept", daemon=True
        )
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stop.set()
        self.disconnect()
        listener, self._listener = self._listener, None
        if listener is not None:
            try:
                listener.close()
            except OSError:
                pass
        if self._thread is not None:
            self._thread.join(timeout=2.0)
        with self._state:
            handlers = list(self._handlers)
            self._handlers.clear()
        for handler in handlers:
            handler.join(timeout=2.0)

    def __enter__(self) -> "FakeBody":
        return self.start()

    def __exit__(self, *_exc: Any) -> None:
        self.stop()

    # -- scripting ------------------------------------------------------- #
    def advance(self, ticks: int = 1) -> int:
        """Move the body tick without sending anything."""
        with self._state:
            self.tick += int(ticks)
            return self.tick

    def bump_generation(self, count: int = 1) -> int:
        """Simulate an arbiter generation bump without telling the brain."""
        with self._state:
            self.generation += int(count)
            return self.generation

    def set_owner(self, owner: Optional[str]) -> None:
        with self._state:
            self.owner = owner

    def set_intent_state(self, intent_id: int, state: str) -> None:
        """Set one stack entry's state (``running``, ``suspended``, ``done``)."""
        with self._state:
            self.stack = [
                entry.model_copy(update={"state": state})
                if entry.intent_id == intent_id
                else entry
                for entry in self.stack
            ]

    def stop_reading(self) -> None:
        """Stop draining the connection so the brain's send buffer can fill."""
        with self._state:
            self._read_paused = True

    def drop_next_events(self, count: int = 1) -> None:
        """Suppress the next ``count`` emitted messages while seq advances."""
        with self._state:
            self._drop_events += int(count)

    def dropped(self, n: int = 1) -> None:
        """Send ``dropped(n)`` and then a fresh snapshot (proposal §2.5)."""
        with self._state:
            self._send(Dropped(seq=self.seq, n=int(n)))
            self._send(self._snapshot_locked())

    def emit_event(self, kind: str, **fields: Any) -> dict[str, Any]:
        """Append and send one event; returns its events.jsonl payload."""
        with self._state:
            return self._emit_locked({"kind": kind, **fields})

    def emit(self, event: dict[str, Any], *, advance_tick: bool = True) -> dict[str, Any]:
        with self._state:
            return self._emit_locked(event, advance_tick=advance_tick)

    def send(self, message: Any) -> None:
        """Send an arbitrary body-to-brain message."""
        with self._state:
            self._send(message)

    def disconnect(self) -> None:
        """Close the current connection; the listener keeps accepting."""
        with self._state:
            conn, self._conn = self._conn, None
        self._close(conn)

    def settle(
        self,
        command_id: str,
        *,
        outcome: str = "done",
        condition: Optional[str] = None,
        failure_kind: Optional[str] = None,
    ) -> dict[str, Any]:
        """Finish one accepted command and emit its ``settled`` receipt."""
        with self._state:
            intent_id = self._command_intents.pop(command_id, None)
            if intent_id is None:
                raise KeyError(f"no accepted command {command_id!r}")
            self.stack = [entry for entry in self.stack if entry.intent_id != intent_id]
            record = {
                "command_id": command_id,
                "outcome": outcome,
                "condition": condition,
                "failure_kind": failure_kind,
            }
            self.settled.append(record)
            self._receipt(
                command_id=command_id,
                state="settled",
                intent_id=intent_id,
                generation=self.generation,
                outcome=outcome,
                condition=condition,
                failure_kind=failure_kind,
            )
            return record

    # -- internals ------------------------------------------------------- #
    def _emit_locked(
        self, event: dict[str, Any], *, advance_tick: bool = True
    ) -> dict[str, Any]:
        self.seq += 1
        if advance_tick:
            self.tick += 1
        if self._drop_events > 0:
            self._drop_events -= 1
            return event
        self._send(EventMessage(seq=self.seq, tick=self.tick, event=event))
        return event

    def _receipt(self, **fields: Any) -> None:
        clean = {key: value for key, value in fields.items() if value is not None}
        self.receipts.append(clean)
        self._emit_locked({"kind": "command_receipt", **clean})

    def _snapshot_locked(self) -> Snapshot:
        return Snapshot(
            seq=self.seq,
            tick=self.tick,
            generation=self.generation,
            owner=self.owner,
            stack=[entry.model_copy() for entry in self.stack],
            facts=Facts(**self.facts),
            stale_ticks=self.stale_ticks,
        )

    def _send_snapshot(self) -> None:
        with self._state:
            self._send(self._snapshot_locked())

    def _send(self, message: Any) -> None:
        with self._state:
            conn = self._conn
            if conn is None:
                raise RuntimeError("no brain connected")
            self._send_to(conn, message)

    def _send_to(self, conn: socket.socket, message: Any) -> None:
        data = encode_frame(message)
        with self._send_lock:
            conn.sendall(data)

    @staticmethod
    def _close(conn: Optional[socket.socket]) -> None:
        if conn is None:
            return
        try:
            conn.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            conn.close()
        except OSError:
            pass

    def _accept_loop(self) -> None:
        listener = self._listener
        if listener is None:
            return
        while not self._stop.is_set():
            try:
                conn, _ = listener.accept()
            except socket.timeout:
                continue
            except OSError:
                break
            conn.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
            conn.settimeout(None)
            with self._state:
                old = self._conn
                self._conn = conn
                self.connections += 1
            if old is not None and old is not conn:
                try:
                    with self._send_lock:
                        old.sendall(encode_frame(Bye(reason="replaced")))
                except OSError:
                    pass
                self._close(old)
            handler = threading.Thread(
                target=self._serve, args=(conn,), name="fake-body-conn", daemon=True
            )
            with self._state:
                self._handlers.append(handler)
            handler.start()

    def _serve(self, conn: socket.socket) -> None:
        try:
            self._send_to(
                conn,
                Hello(
                    port_version=self.port_version,
                    caps=["command", "snapshot", "pop"],
                    body=self.body,
                    run_id=self.run_id,
                ),
            )
            conn.settimeout(self.handshake_timeout)
            try:
                raw = read_frame(conn)
            except TimeoutError:
                self._send_to(conn, Bye(reason="protocol"))
                return
            except ProtocolError:
                self._send_to(conn, Bye(reason="protocol"))
                return
            if raw is None:
                return
            try:
                message = parse_client_message(raw)
            except ProtocolError:
                self._send_to(conn, Bye(reason="protocol"))
                return
            if not isinstance(message, ClientHello):
                self._send_to(conn, Bye(reason="protocol"))
                return
            if message.port_version != self.accept_version:
                self._send_to(conn, Bye(reason="version"))
                return
            conn.settimeout(None)

            if self.send_snapshot_on_connect:
                with self._state:
                    self._send_to(conn, self._snapshot_locked())

            while not self._stop.is_set():
                with self._state:
                    paused = self._read_paused
                if paused:
                    # Leave the bytes in the kernel so a brain send can stall.
                    time.sleep(0.02)
                    continue
                try:
                    readable, _, _ = select.select([conn], [], [], 0.2)
                except (OSError, ValueError):
                    break
                if not readable:
                    continue
                with self._state:
                    if self._read_paused:
                        continue
                raw = read_frame(conn)
                if raw is None:
                    break
                self._dispatch(parse_client_message(raw))
        except (PortClosed, ConnectionError, OSError):
            pass
        except ProtocolError as error:
            self.errors.append(f"{type(error).__name__}: {error}")
        except Exception as error:  # noqa: BLE001 - recorded so tests can fail
            self.errors.append(f"{type(error).__name__}: {error}")
        finally:
            with self._state:
                if self._conn is conn:
                    self._conn = None
            self._close(conn)

    def _dispatch(self, message: Any) -> None:
        if isinstance(message, Command):
            with self._state:
                self._handle_command(message)
            return
        if isinstance(message, Ping):
            with self._state:
                self._send(Pong(seq=self.seq, tick=self.tick))
            return
        if isinstance(message, ResyncRequest):
            self._send_snapshot()
            return
        if isinstance(message, Pop):
            with self._state:
                self.pops.append(message.intent_id)
                # Spec: the brain may pop only a done or suspended intent.
                entry = next(
                    (
                        item
                        for item in self.stack
                        if item.intent_id == message.intent_id
                    ),
                    None,
                )
                if entry is not None and entry.state in ("done", "suspended"):
                    self.stack = [
                        item
                        for item in self.stack
                        if item.intent_id != message.intent_id
                    ]
                    self._command_intents = {
                        command_id: intent_id
                        for command_id, intent_id in self._command_intents.items()
                        if intent_id != message.intent_id
                    }
            return
        if isinstance(message, ClientHello):
            raise ProtocolError("unexpected hello after the handshake")
        raise ProtocolError(f"unknown client message: {type(message).__name__}")

    def _handle_command(self, command: Command) -> None:
        self.commands.append(command)
        # The driver checks expiry when it drains the inbox, before generation.
        expired = self.tick > command.expires_at_tick
        self._receipt(command_id=command.id, state="pending")
        if expired:
            self._receipt(command_id=command.id, state="expired")
            return
        if command.based_on_generation != self.generation:
            self._receipt(
                command_id=command.id,
                state="rejected",
                reason="stale_generation",
            )
            return
        if self.skills is not None and command.skill not in self.skills:
            self._receipt(command_id=command.id, state="rejected", reason="unknown_skill")
            return
        if len(self.stack) >= self.max_stack:
            self._receipt(command_id=command.id, state="rejected", reason="stack_full")
            return

        self.generation += 1
        self._next_intent += 1
        entry = StackEntry(
            intent_id=self._next_intent,
            tier=command.tier,
            skill=command.skill,
            args=list(command.args),
            state="running",
        )
        self.stack.append(entry)
        self._command_intents[command.id] = entry.intent_id
        self._receipt(
            command_id=command.id,
            state="accepted",
            intent_id=entry.intent_id,
            generation=self.generation,
        )


__all__ = ["DEFAULT_FACTS", "FakeBody", "HANDSHAKE_TIMEOUT_S"]
