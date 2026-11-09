"""The body port v0: length-prefixed JSON over loopback TCP.

The body listens, the brain connects. A frame is a 4-byte big-endian length
followed by one UTF-8 JSON object with a ``t`` field; a frame larger than
1 MiB closes the connection. Every body message carries ``seq``: for an
``event`` it is the event-log sequence, for every other body message it is the
last event sequence at send time.

The client is blocking and thread-free on purpose: :class:`PortReader` owns the
one reader thread section 3.1 asks for, and the loop calls
:meth:`PortClient.poll` through it. Wire seqs skip numbers by design (the body
forwards only some event kinds); loss is announced by ``dropped(n)`` plus a
snapshot. An explicit :meth:`PortClient.resync` holds later events, delivers
them, then the snapshot.

Spec: ``docs/proposals/2026-11-08-demo2-loop.md`` section 2.
"""

from __future__ import annotations

import json
import queue
import select
import socket
import struct
import threading
import time
from collections import deque
from typing import (
    Annotated,
    Any,
    Deque,
    Dict,
    Iterator,
    Literal,
    Optional,
    Union,
)

from pydantic import BaseModel, ConfigDict, Field, TypeAdapter, ValidationError

#: Wire version of this port.
PORT_VERSION = 0
#: Largest accepted frame payload, in bytes. Larger closes the connection.
MAX_FRAME_BYTES = 1024 * 1024
#: Integer bounds of the Rust wire types (``u16``, ``u32``, ``u64``, ``i32``).
#: A value outside them fails Rust's JSON decoding and would close the
#: connection, so the client refuses to send it in the first place.
U16_MAX = 2**16 - 1
U32_MAX = 2**32 - 1
U64_MAX = 2**64 - 1
I32_MIN = -(2**31)
I32_MAX = 2**31 - 1
#: Default command time-to-live in ticks (spec: last seen tick + 100).
COMMAND_TTL_TICKS = 100
#: The loop pings at 1 Hz; any message counts as liveness.
KEEPALIVE_INTERVAL_S = 1.0
#: Silence after which the body logs ``brain_silent`` (A 5.4).
SILENCE_LIMIT_S = 3.0
#: How long one send may wait for the peer to accept bytes before it fails.
WRITE_TIMEOUT_S = 2.0

_HEADER = struct.Struct(">I")


# --------------------------------------------------------------------------- #
# Exceptions
# --------------------------------------------------------------------------- #
class PortError(RuntimeError):
    """Base error for the body port."""


class ProtocolError(PortError):
    """A frame is not a JSON object or a message has the wrong shape."""


class FrameTooLarge(ProtocolError):
    """A frame exceeds :data:`MAX_FRAME_BYTES`."""


class PortClosed(PortError):
    """The peer closed the connection or the client is not connected."""


class HandshakeError(PortError):
    """The first message was not a valid body ``hello``."""


class VersionMismatch(HandshakeError):
    """The body and the brain speak different port versions."""


# --------------------------------------------------------------------------- #
# Messages: body -> brain
# --------------------------------------------------------------------------- #
class _Message(BaseModel):
    model_config = ConfigDict(extra="ignore")


class Hello(_Message):
    """Body greeting, sent once on every connection."""

    t: Literal["hello"] = "hello"
    port_version: int = Field(ge=0, le=U16_MAX)
    caps: list[str] = Field(default_factory=list)
    body: Optional[str] = None
    run_id: Optional[str] = None


class StackEntry(_Message):
    """One entry of the body's intent stack."""

    intent_id: int = Field(ge=0, le=U64_MAX)
    tier: str
    skill: Optional[str] = None
    args: list[str] = Field(default_factory=list)
    state: str


class Facts(_Message):
    """The body facts the brain reads from a snapshot."""

    health: Optional[Union[int, float]] = None
    food: Optional[int] = Field(default=None, ge=0, le=U32_MAX)
    air: Optional[int] = Field(default=None, ge=0, le=U32_MAX)
    is_night: Optional[bool] = None
    day_time: Optional[int] = Field(default=None, ge=0, le=U64_MAX)
    dimension: Optional[str] = None
    pos: Optional[list[Annotated[int, Field(ge=I32_MIN, le=I32_MAX)]]] = None
    inventory: Dict[str, Annotated[int, Field(ge=0, le=U32_MAX)]] = Field(
        default_factory=dict
    )


class Snapshot(_Message):
    """State on demand: connect, resync and every wake."""

    t: Literal["snapshot"] = "snapshot"
    seq: int = Field(ge=0, le=U64_MAX)
    tick: int = Field(ge=0, le=U64_MAX)
    generation: int = Field(ge=0, le=U32_MAX)
    owner: Optional[str] = None
    stack: list[StackEntry] = Field(default_factory=list)
    facts: Facts = Field(default_factory=Facts)
    stale_ticks: int = Field(default=0, ge=0, le=U64_MAX)


class EventMessage(_Message):
    """A filtered view of the event log; ``event`` is the events.jsonl payload."""

    t: Literal["event"] = "event"
    seq: int = Field(ge=0, le=U64_MAX)
    tick: int = Field(ge=0, le=U64_MAX)
    event: Dict[str, Any]


class Dropped(_Message):
    """The body dropped ``n`` queued messages; a fresh snapshot follows."""

    t: Literal["dropped"] = "dropped"
    seq: int = Field(ge=0, le=U64_MAX)
    n: int = Field(ge=0, le=U64_MAX)


class Pong(_Message):
    t: Literal["pong"] = "pong"
    seq: int = Field(ge=0, le=U64_MAX)
    tick: int = Field(ge=0, le=U64_MAX)


class Bye(_Message):
    t: Literal["bye"] = "bye"
    reason: str


class CommandReceipt(_Message):
    """The payload of a ``command_receipt`` event (not a separate message)."""

    command_id: str
    state: Literal["pending", "accepted", "rejected", "expired", "settled"]
    intent_id: Optional[int] = Field(default=None, ge=0, le=U64_MAX)
    generation: Optional[int] = Field(default=None, ge=0, le=U32_MAX)
    outcome: Optional[str] = None
    failure_kind: Optional[str] = None
    reason: Optional[str] = None
    condition: Optional[str] = None


# --------------------------------------------------------------------------- #
# Messages: brain -> body
# --------------------------------------------------------------------------- #
class ClientHello(_Message):
    t: Literal["hello"] = "hello"
    port_version: int = Field(default=PORT_VERSION, ge=0, le=U16_MAX)


class Command(_Message):
    t: Literal["command"] = "command"
    id: str
    skill: str
    args: list[str] = Field(default_factory=list)
    tier: str = "command"
    expires_at_tick: int = Field(ge=0, le=U64_MAX)
    based_on_generation: int = Field(ge=0, le=U32_MAX)
    condition: Optional[str] = None
    budget_ticks: Optional[int] = Field(default=None, ge=0, le=U64_MAX)


class Pop(_Message):
    t: Literal["pop"] = "pop"
    id: str
    intent_id: int = Field(ge=0, le=U64_MAX)


class Say(_Message):
    """One line for the game chat (talk slice 1). Sent only when the body's
    ``hello.caps`` lists ``say``; the body answers with ``chat_sent`` or
    ``chat_dropped`` carrying ``id``."""

    t: Literal["say"] = "say"
    id: str
    text: str = Field(max_length=200)
    priority: Literal["urgent", "normal", "droppable"] = "normal"


class ResyncRequest(_Message):
    t: Literal["resync_request"] = "resync_request"


class Ping(_Message):
    t: Literal["ping"] = "ping"


BodyMessage = Union[Hello, EventMessage, Snapshot, Dropped, Pong, Bye]
ClientMessage = Union[ClientHello, Command, Pop, Say, ResyncRequest, Ping]

_BODY_ADAPTER: TypeAdapter[Any] = TypeAdapter(BodyMessage)
_CLIENT_ADAPTER: TypeAdapter[Any] = TypeAdapter(ClientMessage)


def parse_body_message(payload: Dict[str, Any]) -> Any:
    """Validate one decoded body frame; raises :class:`ProtocolError`."""
    try:
        return _BODY_ADAPTER.validate_python(payload)
    except ValidationError as error:
        raise ProtocolError(f"invalid body message: {error}") from error


def parse_client_message(payload: Dict[str, Any]) -> Any:
    """Validate one decoded brain frame; raises :class:`ProtocolError`."""
    try:
        return _CLIENT_ADAPTER.validate_python(payload)
    except ValidationError as error:
        raise ProtocolError(f"invalid client message: {error}") from error


# --------------------------------------------------------------------------- #
# Framing
# --------------------------------------------------------------------------- #
def encode_frame(message: Union[BaseModel, Dict[str, Any]]) -> bytes:
    """Frame one message: 4-byte big-endian length + UTF-8 JSON."""
    if isinstance(message, BaseModel):
        payload = message.model_dump(mode="json", exclude_none=True)
    else:
        payload = message
    body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if len(body) > MAX_FRAME_BYTES:
        raise FrameTooLarge(f"frame is {len(body)} bytes, limit is {MAX_FRAME_BYTES}")
    return _HEADER.pack(len(body)) + body


def decode_payload(payload: bytes) -> Dict[str, Any]:
    """Decode one frame payload; raises :class:`ProtocolError`."""
    try:
        message = json.loads(payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ProtocolError(f"frame is not UTF-8 JSON: {error}") from error
    if not isinstance(message, dict):
        raise ProtocolError(f"frame is not a JSON object: {message!r}")
    return message


class FrameDecoder:
    """Incremental frame parser for a buffered stream.

    ``feed`` accepts arbitrary chunks (a header split in half, a body split
    across reads); ``pop`` returns one complete message or ``None``.
    """

    def __init__(self) -> None:
        self._buffer = bytearray()

    def feed(self, data: bytes) -> None:
        self._buffer.extend(data)

    def pop(self) -> Optional[Dict[str, Any]]:
        if len(self._buffer) < _HEADER.size:
            return None
        (length,) = _HEADER.unpack_from(self._buffer)
        if length > MAX_FRAME_BYTES:
            raise FrameTooLarge(f"frame is {length} bytes, limit is {MAX_FRAME_BYTES}")
        end = _HEADER.size + length
        if len(self._buffer) < end:
            return None
        payload = bytes(self._buffer[_HEADER.size : end])
        del self._buffer[:end]
        return decode_payload(payload)

    @property
    def buffered(self) -> int:
        """Bytes held for an incomplete frame."""
        return len(self._buffer)


def _recv_exact(sock: socket.socket, count: int) -> Optional[bytes]:
    chunks = []
    remaining = count
    while remaining:
        chunk = sock.recv(remaining)
        if not chunk:
            if not chunks:
                return None
            raise PortClosed(f"connection closed with {remaining} bytes missing")
        chunks.append(chunk)
        remaining -= len(chunk)
    return b"".join(chunks)


def read_frame(sock: socket.socket) -> Optional[Dict[str, Any]]:
    """Read one frame from a blocking socket; ``None`` on a clean EOF."""
    header = _recv_exact(sock, _HEADER.size)
    if header is None:
        return None
    (length,) = _HEADER.unpack(header)
    if length > MAX_FRAME_BYTES:
        raise FrameTooLarge(f"frame is {length} bytes, limit is {MAX_FRAME_BYTES}")
    payload = _recv_exact(sock, length)
    if payload is None:
        raise PortClosed("connection closed mid-frame")
    return decode_payload(payload)


# --------------------------------------------------------------------------- #
# Client
# --------------------------------------------------------------------------- #
class PortClient:
    """A blocking, single-threaded client for one body.

    ``connect`` performs the handshake (body ``hello`` first, then the brain's
    ``hello``). ``poll`` returns one body message or ``None`` on timeout; a
    ``seq`` gap is normal (the body filters event kinds) and never resyncs.
    The client never starts a thread of its own: :class:`PortReader` runs
    ``poll`` and ``keepalive`` on its one reader thread and the loop writes
    from its own thread, serialized by an internal write lock.
    """

    def __init__(
        self,
        port: int,
        host: str = "127.0.0.1",
        *,
        port_version: int = PORT_VERSION,
        connect_timeout: float = 5.0,
        handshake_timeout: float = 5.0,
        command_ttl_ticks: int = COMMAND_TTL_TICKS,
        keepalive_interval: float = KEEPALIVE_INTERVAL_S,
        write_timeout: float = WRITE_TIMEOUT_S,
    ) -> None:
        self.host = host
        self.port = port
        self.port_version = port_version
        self.connect_timeout = connect_timeout
        self.handshake_timeout = handshake_timeout
        self.command_ttl_ticks = command_ttl_ticks
        self.keepalive_interval = keepalive_interval
        self.write_timeout = write_timeout

        self.hello: Optional[Hello] = None
        self.generation: Optional[int] = None
        self.tick: Optional[int] = None
        self.owner: Optional[str] = None
        self.stack: list[StackEntry] = []
        self.stale_ticks: Optional[int] = None
        self.last_seq: Optional[int] = None
        self.resync_count = 0
        self.bye_reason: Optional[str] = None

        self._sock: Optional[socket.socket] = None
        self._decoder = FrameDecoder()
        self._ready: Deque[Any] = deque()
        self._held: list[EventMessage] = []
        self._resyncing = False
        self._remote_closed = False
        self._last_received: Optional[float] = None
        self._last_sent: Optional[float] = None
        self._command_number = 0
        self._line_number = 0
        #: The loop sends commands while the reader thread pings, so one frame
        #: must never interleave with another on the wire.
        self._write_lock = threading.Lock()

    # -- connection ------------------------------------------------------ #
    @property
    def connected(self) -> bool:
        return self._sock is not None and not self._remote_closed

    def connect(self) -> Hello:
        """Open the socket and perform the handshake. Returns the body hello."""
        self.close()
        try:
            sock = socket.create_connection(
                (self.host, self.port), timeout=self.connect_timeout
            )
        except OSError as error:
            raise PortError(f"cannot connect to {self.host}:{self.port}: {error}") from error
        sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        sock.settimeout(None)
        self._sock = sock
        self._decoder = FrameDecoder()
        self._ready.clear()
        self._held.clear()
        self._resyncing = False
        self._remote_closed = False
        self.bye_reason = None
        self.last_seq = None
        self.generation = None
        self.tick = None
        self.owner = None
        self.stack = []
        self.stale_ticks = None
        self.hello = None

        try:
            raw = self._read_message(self.handshake_timeout)
        except ProtocolError:
            self.close()
            raise
        if raw is None:
            self.close()
            raise TimeoutError(f"no hello from the body within {self.handshake_timeout}s")
        try:
            message = parse_body_message(raw)
        except ProtocolError:
            self.close()
            raise
        if isinstance(message, Bye):
            self.close()
            if message.reason == "version":
                raise VersionMismatch("the body refused the port version")
            raise HandshakeError(f"the body said bye during the handshake: {message.reason}")
        if not isinstance(message, Hello):
            self.close()
            raise HandshakeError(f"expected hello, got {type(message).__name__}")
        if message.port_version != self.port_version:
            self.close()
            raise VersionMismatch(
                f"body port version {message.port_version}, brain speaks {self.port_version}"
            )

        self._send(ClientHello(port_version=self.port_version))
        self.hello = message
        self._last_received = time.monotonic()
        return message

    def close(self) -> None:
        sock, self._sock = self._sock, None
        if sock is not None:
            try:
                sock.close()
            except OSError:
                pass

    def __enter__(self) -> "PortClient":
        return self

    def __exit__(self, *_exc: Any) -> None:
        self.close()

    # -- sending --------------------------------------------------------- #
    def _send(self, message: BaseModel) -> None:
        with self._write_lock:
            self._send_locked(message)

    def _send_locked(self, message: BaseModel) -> None:
        sock = self._sock
        if sock is None:
            raise PortClosed("not connected")
        view = memoryview(encode_frame(message))
        deadline = time.monotonic() + self.write_timeout
        while view:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                self.close()
                raise PortClosed(f"send stalled for {self.write_timeout}s")
            try:
                _, writable, _ = select.select([], [sock], [], remaining)
            except (OSError, ValueError) as error:
                self.close()
                raise PortClosed(f"send failed: {error}") from error
            if not writable:
                self.close()
                raise PortClosed(f"send stalled for {self.write_timeout}s")
            # Writable means some room, not room for the whole frame. Bound the
            # send itself so a body that stops reading cannot block this thread.
            try:
                sock.settimeout(max(0.0, deadline - time.monotonic()))
                sent = sock.send(view)
            except TimeoutError as error:
                self.close()
                raise PortClosed(f"send stalled for {self.write_timeout}s") from error
            except OSError as error:
                self.close()
                raise PortClosed(f"send failed: {error}") from error
            finally:
                try:
                    sock.settimeout(None)
                except OSError:
                    pass
            if sent == 0:
                self.close()
                raise PortClosed("send failed: the body closed the connection")
            view = view[sent:]
        self._last_sent = time.monotonic()

    def new_command_id(self) -> str:
        """A brain-run-unique id: ``c-0001``, ``c-0002``, ..."""
        self._command_number += 1
        return f"c-{self._command_number:04d}"

    def send_command(
        self,
        skill: str,
        args: Optional[list[str]] = None,
        *,
        command_id: Optional[str] = None,
        tier: str = "command",
        expires_at_tick: Optional[int] = None,
        based_on_generation: Optional[int] = None,
        condition: Optional[str] = None,
        budget_ticks: Optional[int] = None,
    ) -> str:
        """Send one command and return its id.

        By default the generation and tick come from the newest snapshot (or
        an event carrying a generation), and the command expires 100 ticks
        after the last seen tick.
        """
        if based_on_generation is None:
            if self.generation is None:
                raise PortError("no snapshot yet: pass based_on_generation explicitly")
            based_on_generation = self.generation
        if expires_at_tick is None:
            if self.tick is None:
                raise PortError("no snapshot yet: pass expires_at_tick explicitly")
            expires_at_tick = self.tick + self.command_ttl_ticks
        command_id = command_id or self.new_command_id()
        self._send(
            Command(
                id=command_id,
                skill=skill,
                args=list(args or []),
                tier=tier,
                expires_at_tick=expires_at_tick,
                based_on_generation=based_on_generation,
                condition=condition,
                budget_ticks=budget_ticks,
            )
        )
        return command_id

    def pop(self, intent_id: int, *, command_id: Optional[str] = None) -> str:
        command_id = command_id or self.new_command_id()
        self._send(Pop(id=command_id, intent_id=intent_id))
        return command_id

    @property
    def supports_say(self) -> bool:
        """Whether the connected body advertised the ``say`` capability."""
        return self.hello is not None and "say" in self.hello.caps

    def new_line_id(self) -> str:
        """A brain-run-unique chat line id: ``s-0001``, ``s-0002``, ..."""
        self._line_number += 1
        return f"s-{self._line_number:04d}"

    def say(
        self,
        text: str,
        *,
        line_id: Optional[str] = None,
        priority: str = "normal",
    ) -> str:
        """Send one chat line and return its id. Raises :class:`PortError` when
        the body did not advertise ``say`` (an old body keeps working: the
        caller falls back to console-only narration)."""
        if not self.supports_say:
            raise PortError("the body does not advertise the say capability")
        line_id = line_id or self.new_line_id()
        self._send(Say(id=line_id, text=text, priority=priority))
        return line_id

    def resync(self) -> None:
        """Ask for a snapshot; later events are held until it arrives.

        The flag is raised *before* the request leaves: the reader thread may
        receive the snapshot reply between the send and the assignment, and
        clearing a flag that is set afterwards would strand every later event
        in ``_held`` (the loop would read nothing else on this connection).
        """
        if self._resyncing:
            return
        self._resyncing = True
        try:
            self._send(ResyncRequest())
        except PortError:
            self._resyncing = False
            raise
        self.resync_count += 1

    def ping(self) -> None:
        self._send(Ping())

    def keepalive(self, now: Optional[float] = None) -> bool:
        """Send a ping when 1 s passed since the last outgoing message.

        The loop calls this; there is no liveness thread. Returns whether a
        ping was sent.
        """
        now = time.monotonic() if now is None else now
        if self._last_sent is not None and now - self._last_sent < self.keepalive_interval:
            return False
        self.ping()
        return True

    def seconds_since_received(self, now: Optional[float] = None) -> Optional[float]:
        if self._last_received is None:
            return None
        now = time.monotonic() if now is None else now
        return now - self._last_received

    def is_silent(self, limit: float = SILENCE_LIMIT_S, now: Optional[float] = None) -> bool:
        silent_for = self.seconds_since_received(now)
        return silent_for is not None and silent_for >= limit

    # -- receiving ------------------------------------------------------- #
    def _read_message(self, timeout: Optional[float]) -> Optional[Dict[str, Any]]:
        sock = self._sock
        if sock is None:
            raise PortClosed("not connected")
        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            raw = self._decoder.pop()
            if raw is not None:
                return raw
            # A zero timeout still gets one non-blocking select, so pending
            # bytes are read instead of being mistaken for an idle port.
            remaining = (
                None if deadline is None else max(0.0, deadline - time.monotonic())
            )
            try:
                readable, _, _ = select.select([sock], [], [], remaining)
            except (OSError, ValueError) as error:
                self.close()
                raise PortClosed(f"select failed: {error}") from error
            if not readable:
                return None
            try:
                chunk = sock.recv(65536)
            except OSError as error:
                self.close()
                raise PortClosed(f"receive failed: {error}") from error
            if not chunk:
                self.close()
                raise PortClosed("the body closed the connection")
            self._decoder.feed(chunk)

    def poll(self, timeout: Optional[float] = 0.0) -> Optional[Any]:
        """Return the next body message, or ``None`` when ``timeout`` expires.

        ``timeout=None`` blocks until a message arrives. While an explicit
        :meth:`resync` is pending, later events are held and delivered before
        the snapshot.
        """
        if self._ready:
            return self._ready.popleft()
        if self._sock is None:
            raise PortClosed("not connected")
        if self._remote_closed:
            raise PortClosed(f"the body closed the connection: {self.bye_reason or 'bye'}")

        deadline = None if timeout is None else time.monotonic() + timeout
        while True:
            remaining = (
                None if deadline is None else max(0.0, deadline - time.monotonic())
            )
            try:
                raw = self._read_message(remaining)
            except ProtocolError:
                self.close()
                raise
            if raw is None:
                return None
            self._last_received = time.monotonic()
            try:
                message = parse_body_message(raw)
                self._ingest(message)
            except ProtocolError:
                self.close()
                raise
            if self._ready:
                return self._ready.popleft()
            if self._remote_closed:
                raise PortClosed(f"the body closed the connection: {self.bye_reason or 'bye'}")
            if deadline is not None and time.monotonic() >= deadline:
                return None

    def __iter__(self) -> Iterator[Any]:
        """Yield messages until ``bye`` or a closed connection.

        Idle waits call :meth:`keepalive`, so iterating also keeps the link
        alive without a background thread.
        """
        while True:
            try:
                message = self.poll(0.25)
            except PortClosed:
                return
            if message is None:
                self.keepalive()
                continue
            yield message
            if isinstance(message, Bye):
                return

    def _ingest(self, message: Any) -> None:
        if isinstance(message, Snapshot):
            self.generation = message.generation
            self.tick = message.tick
            self.owner = message.owner
            self.stack = list(message.stack)
            self.stale_ticks = message.stale_ticks
            held, self._held = self._held, []
            # The client already received these events, so never drop them: a
            # held ``command_receipt`` would otherwise vanish (and a settled
            # command would look open). Deliver them before the snapshot,
            # whose state supersedes what they describe.
            self.last_seq = max(
                [self.last_seq or 0, message.seq] + [event.seq for event in held]
            )
            self._ready.extend(held)
            self._ready.append(message)
            self._resyncing = False
            return

        if isinstance(message, EventMessage):
            self.tick = message.tick
            generation = message.event.get("generation")
            if isinstance(generation, int):
                self.generation = generation
            if self._resyncing:
                self._held.append(message)
                return
            if self.last_seq is None:
                self.last_seq = message.seq
                self._ready.append(message)
                return
            if message.seq <= self.last_seq:
                return
            # The wire seq is the event-log seq and the body forwards only the
            # kinds the brain needs, so seq gaps are normal. Loss is announced
            # explicitly by `dropped(n)` plus a snapshot. Treating gaps as loss
            # made every goal resync and discard its own receipts (live port
            # tests, 2026-11-08).
            self.last_seq = message.seq
            self._ready.append(message)
            return

        if isinstance(message, Pong):
            self.tick = message.tick
            self._ready.append(message)
            return

        if isinstance(message, Dropped):
            self._ready.append(message)
            return

        if isinstance(message, Bye):
            self.bye_reason = message.reason
            self._remote_closed = True
            self._ready.append(message)
            return

        if isinstance(message, Hello):
            raise ProtocolError("unexpected hello after the handshake")

        raise ProtocolError(f"unknown body message: {type(message).__name__}")


# --------------------------------------------------------------------------- #
# Reader thread (proposal section 3.1)
# --------------------------------------------------------------------------- #
class PortReader:
    """The one thread of section 3.1: it owns the socket reads and pings at 1 Hz.

    :class:`PortClient` stays blocking and thread-free; this wrapper owns the
    read side and puts every message in a queue. The loop thread writes
    (commands, pops, resync requests) and consumes the queue, so a slow
    proposal, Jev or admission call cannot stop the brain from answering: the
    reader keeps reading and keeps the link alive with
    :meth:`PortClient.keepalive`, and the body never sees 3 s of silence.

    A read error closes the connection in the client and arrives in the queue
    as the :class:`PortError` instance; the loop turns it into evidence and
    reconnects.
    """

    def __init__(self, client: PortClient, *, poll_seconds: float = 0.1) -> None:
        self.client = client
        self.poll_seconds = float(poll_seconds)
        self._messages: "queue.Queue[Any]" = queue.Queue()
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None

    def start(self) -> None:
        """(Re)start reading; anything left from an earlier connection is dropped."""
        self.stop()
        self._drain()
        self._stop.clear()
        self._thread = threading.Thread(
            target=self._run, name="port-reader", daemon=True
        )
        self._thread.start()

    def stop(self) -> None:
        """Ask the reader to finish and wait for it (a poll is at most one tick)."""
        self._stop.set()
        thread, self._thread = self._thread, None
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=2.0)

    @property
    def running(self) -> bool:
        thread = self._thread
        return thread is not None and thread.is_alive()

    def get(self, timeout: Optional[float]) -> Any:
        """The next message, a :class:`PortError`, or ``None`` on timeout."""
        try:
            return self._messages.get(timeout=timeout)
        except queue.Empty:
            return None

    def _drain(self) -> None:
        while True:
            try:
                self._messages.get_nowait()
            except queue.Empty:
                return

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                message = self.client.poll(self.poll_seconds)
                if message is not None:
                    self._messages.put(message)
                self.client.keepalive()
            except PortError as error:
                self._messages.put(error)
                return
            except Exception as error:  # noqa: BLE001 - the loop records what it gets
                self._messages.put(PortError(f"{type(error).__name__}: {error}"))
                return


__all__ = [
    "BodyMessage",
    "Bye",
    "ClientHello",
    "ClientMessage",
    "Command",
    "CommandReceipt",
    "COMMAND_TTL_TICKS",
    "Dropped",
    "EventMessage",
    "Facts",
    "FrameDecoder",
    "FrameTooLarge",
    "HandshakeError",
    "Hello",
    "KEEPALIVE_INTERVAL_S",
    "MAX_FRAME_BYTES",
    "Ping",
    "Pop",
    "PortClient",
    "PortClosed",
    "PortError",
    "PortReader",
    "PORT_VERSION",
    "Pong",
    "ProtocolError",
    "ResyncRequest",
    "SILENCE_LIMIT_S",
    "Snapshot",
    "StackEntry",
    "VersionMismatch",
    "WRITE_TIMEOUT_S",
    "decode_payload",
    "encode_frame",
    "parse_body_message",
    "parse_client_message",
    "read_frame",
]
