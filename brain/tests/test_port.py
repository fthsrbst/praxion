from __future__ import annotations

import socket
import struct
import time
from pathlib import Path
from typing import Any, Callable

import pytest

from fake_body import FakeBody
from praxion_brain.port import (
    MAX_FRAME_BYTES,
    Bye,
    ClientHello,
    Command,
    CommandReceipt,
    Dropped,
    EventMessage,
    Facts,
    FrameDecoder,
    FrameTooLarge,
    Hello,
    Ping,
    Pop,
    PortClient,
    PortClosed,
    Pong,
    ProtocolError,
    ResyncRequest,
    Snapshot,
    StackEntry,
    VersionMismatch,
    decode_payload,
    encode_frame,
    parse_body_message,
    parse_client_message,
    read_frame,
)

GOLDEN_DIR = (
    Path(__file__).resolve().parents[2]
    / "body"
    / "crates"
    / "port"
    / "tests"
    / "golden"
)


def golden_bytes(name: str) -> bytes:
    return (GOLDEN_DIR / name).read_bytes()


def parse_golden(name: str) -> Any:
    data = golden_bytes(name)
    (length,) = struct.unpack(">I", data[:4])
    assert length == len(data) - 4, f"{name} has a bad length prefix"
    return parse_body_message(decode_payload(data[4:]))


@pytest.fixture
def body() -> Any:
    server = FakeBody().start()
    try:
        yield server
    finally:
        server.stop()
    assert server.errors == []


@pytest.fixture
def client(body: FakeBody) -> Any:
    port = PortClient(body.port, handshake_timeout=2.0)
    port.connect()
    try:
        yield port
    finally:
        port.close()


def wait_for(
    port: PortClient,
    predicate: Callable[[Any], bool],
    timeout: float = 2.0,
    what: str = "message",
) -> Any:
    deadline = time.monotonic() + timeout
    seen = []
    while time.monotonic() < deadline:
        message = port.poll(0.1)
        if message is None:
            continue
        seen.append(message)
        if predicate(message):
            return message
    raise AssertionError(f"no {what} within {timeout}s; saw {seen!r}")


def wait_snapshot(port: PortClient, timeout: float = 2.0) -> Snapshot:
    return wait_for(port, lambda message: isinstance(message, Snapshot), timeout, "snapshot")


def wait_event(port: PortClient, kind: str, timeout: float = 2.0) -> EventMessage:
    return wait_for(
        port,
        lambda message: isinstance(message, EventMessage) and message.event.get("kind") == kind,
        timeout,
        f"{kind} event",
    )


def receipts_until(
    port: PortClient, command_id: str, state: str, timeout: float = 2.0
) -> list[dict[str, Any]]:
    """Collect command receipts for ``command_id`` up to and including ``state``."""
    found: list[dict[str, Any]] = []

    def matches(message: Any) -> bool:
        if not isinstance(message, EventMessage):
            return False
        event = message.event
        if event.get("kind") != "command_receipt":
            return False
        if event.get("command_id") != command_id:
            return False
        found.append(event)
        return event.get("state") == state

    wait_for(port, matches, timeout, f"receipt {state} for {command_id}")
    return found


def test_handshake_ok(body: FakeBody) -> None:
    port = PortClient(body.port, handshake_timeout=2.0)
    hello = port.connect()

    assert hello.port_version == 0
    assert hello.run_id == "fake-run"
    assert hello.body == "fake-body 0.0.0"
    assert hello.caps == ["command", "snapshot", "pop"]
    assert port.hello is hello

    snapshot = wait_snapshot(port)
    assert (snapshot.seq, snapshot.tick, snapshot.generation) == (0, 0, 0)
    assert snapshot.facts.health == 20
    assert snapshot.facts.inventory == {}
    port.close()


def test_handshake_refuses_version_mismatch() -> None:
    with FakeBody(port_version=1) as body:
        port = PortClient(body.port, handshake_timeout=2.0)
        with pytest.raises(VersionMismatch):
            port.connect()
        assert not port.connected


def test_body_refuses_client_version() -> None:
    with FakeBody(port_version=0, accept_version=1) as body:
        port = PortClient(body.port, handshake_timeout=2.0)
        port.connect()
        bye = wait_for(port, lambda message: message.t == "bye", what="bye")
        assert bye.reason == "version"
        with pytest.raises(PortClosed):
            port.poll(0.5)
        port.close()


def test_command_accepted_with_generation(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    assert client.generation == 0

    first = client.send_command("get_furnace", args=["minecraft:furnace"])
    receipts = receipts_until(client, first, "accepted")

    assert [receipt["state"] for receipt in receipts] == ["pending", "accepted"]
    accepted = receipts[-1]
    assert accepted["intent_id"] == 1
    assert accepted["generation"] == 1
    assert client.generation == 1
    assert body.stack[0].skill == "get_furnace"
    assert body.stack[0].state == "running"

    second = client.send_command("get_furnace")
    second_receipts = receipts_until(client, second, "accepted")
    assert second_receipts[-1]["intent_id"] == 2
    assert second_receipts[-1]["generation"] == 2
    assert client.generation == 2


def test_stale_generation_rejected(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    body.bump_generation(3)

    command_id = client.send_command("get_furnace", based_on_generation=0)
    receipts = receipts_until(client, command_id, "rejected")

    assert [receipt["state"] for receipt in receipts] == ["pending", "rejected"]
    assert receipts[-1]["reason"] == "stale_generation"
    assert body.stack == []
    assert client.generation == 0


def test_expired_command_never_starts(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    body.advance(5)

    command_id = client.send_command(
        "get_furnace", based_on_generation=0, expires_at_tick=4
    )
    receipts = receipts_until(client, command_id, "expired")

    assert [receipt["state"] for receipt in receipts] == ["pending", "expired"]
    assert body.stack == []
    assert body.generation == 0


def test_a_seq_gap_is_normal_and_never_resyncs(client: PortClient, body: FakeBody) -> None:
    # The body forwards only some event-log kinds, so wire seqs skip numbers.
    # Loss is announced by `dropped(n)`, never inferred from a gap.
    wait_snapshot(client)
    body.emit_event("note", text="one")
    assert wait_event(client, "note").seq == 1

    body.drop_next_events(1)
    body.emit_event("note", text="filtered")  # seq 2 is never sent
    body.emit_event("note", text="after")  # seq 3 arrives after the gap
    after = wait_event(client, "note")
    assert (after.seq, after.event["text"]) == (3, "after")
    assert client.last_seq == 3
    assert client.resync_count == 0


def test_reconnect_after_drop(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    body.emit_event("note", text="before")
    assert wait_event(client, "note").seq == 1

    body.disconnect()
    with pytest.raises(PortClosed):
        client.poll(1.0)

    client.connect()
    snapshot = wait_snapshot(client)
    assert snapshot.seq == 1
    assert body.connections == 2

    body.emit_event("note", text="after")
    after = wait_event(client, "note")
    assert after.seq == 2


def test_large_frame_round_trip(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    text = "x" * 900_000

    body.emit_event("note", text=text)
    message = wait_event(client, "note", timeout=5.0)

    assert len(message.event["text"]) == len(text)


def test_split_frame_is_read_in_pieces() -> None:
    data = encode_frame(
        EventMessage(seq=7, tick=3, event={"kind": "note", "text": "hello"})
    )

    left, right = socket.socketpair()
    try:
        for chunk in (data[:2], data[2:5], data[5:11], data[11:]):
            left.sendall(chunk)
        message = read_frame(right)
    finally:
        left.close()
        right.close()

    assert message == {"t": "event", "seq": 7, "tick": 3, "event": {"kind": "note", "text": "hello"}}


def test_frame_decoder_holds_incomplete_frames() -> None:
    data = encode_frame({"t": "pong", "seq": 3, "tick": 9})
    decoder = FrameDecoder()

    decoder.feed(data[:3])
    assert decoder.pop() is None
    decoder.feed(data[3:-1])
    assert decoder.pop() is None
    decoder.feed(data[-1:])
    assert decoder.pop() == {"t": "pong", "seq": 3, "tick": 9}
    assert decoder.buffered == 0

    decoder.feed(struct.pack(">I", MAX_FRAME_BYTES + 1))
    with pytest.raises(FrameTooLarge):
        decoder.pop()


def test_oversized_outgoing_frame_is_refused() -> None:
    with pytest.raises(FrameTooLarge):
        encode_frame({"t": "event", "seq": 1, "tick": 1, "event": {"text": "x" * MAX_FRAME_BYTES}})


def test_ping_and_keepalive(client: PortClient) -> None:
    wait_snapshot(client)

    client.ping()
    pong = wait_for(client, lambda message: isinstance(message, Pong), what="pong")
    assert pong.seq == 0

    assert client.keepalive() is False  # under one second
    assert client.keepalive(now=time.monotonic() + 2.0) is True
    wait_for(client, lambda message: isinstance(message, Pong), what="second pong")


def test_command_receipt_model() -> None:
    receipt = CommandReceipt(
        command_id="c-0001",
        state="settled",
        outcome="done",
        condition="true",
    )
    assert receipt.intent_id is None
    assert receipt.model_dump(exclude_none=True) == {
        "command_id": "c-0001",
        "state": "settled",
        "outcome": "done",
        "condition": "true",
    }
    assert not isinstance(receipt, Dropped)


def test_dropped_is_followed_by_a_snapshot(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    body.advance(2)
    body.dropped(5)

    dropped = client.poll(1.0)
    assert isinstance(dropped, Dropped)
    assert (dropped.seq, dropped.n) == (0, 5)

    snapshot = client.poll(1.0)
    assert isinstance(snapshot, Snapshot)
    assert snapshot.seq == 0
    assert snapshot.tick == 2


def test_poll_default_returns_a_frame_already_sent(
    client: PortClient, body: FakeBody
) -> None:
    wait_snapshot(client)
    body.emit_event("note", text="ready")
    # The write is synchronous, but macOS loopback can keep the bytes in
    # transit for a moment, and a zero-timeout select is then honestly empty.
    # Retry the zero-timeout poll until they land rather than sleeping a fixed
    # time: a sleep would hide a pump that stopped pumping.
    deadline = time.monotonic() + 2.0
    message = None
    while message is None:
        assert time.monotonic() < deadline, "the note never reached the client"
        message = client.poll()  # the default timeout is zero, but it must pump

    assert isinstance(message, EventMessage)
    assert message.event["text"] == "ready"


def test_an_explicit_resync_ends_with_a_snapshot_and_loses_no_event(
    client: PortClient, body: FakeBody
) -> None:
    wait_snapshot(client)
    body.emit_event("note", text="held")
    client.resync()

    messages = []
    deadline = time.monotonic() + 2.0
    while time.monotonic() < deadline:
        message = client.poll(0.1)
        if message is not None:
            messages.append(message)
        texts = [m.event["text"] for m in messages if isinstance(m, EventMessage)]
        if "held" in texts and any(isinstance(m, Snapshot) for m in messages):
            break
    assert any(isinstance(m, Snapshot) for m in messages)
    assert [m.event["text"] for m in messages if isinstance(m, EventMessage)] == ["held"]


def test_send_times_out_when_the_body_stops_reading(
    client: PortClient, body: FakeBody
) -> None:
    wait_snapshot(client)
    body.stop_reading()
    time.sleep(0.3)  # let the reader loop observe the pause
    client.write_timeout = 0.2

    started = time.monotonic()
    with pytest.raises(PortClosed):
        # Loopback buffers can be large, so fill them until the write blocks.
        for _ in range(10):
            client.send_command("get_furnace", args=["x" * 900_000])

    assert time.monotonic() - started < 5.0
    assert not client.connected


def test_pong_updates_the_tick(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    body.advance(7)

    client.ping()
    pong = wait_for(client, lambda message: isinstance(message, Pong), what="pong")

    assert pong.tick == 7
    assert client.tick == 7


def test_pop_removes_a_done_intent(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    command_id = client.send_command("get_furnace")
    receipts_until(client, command_id, "accepted")
    intent_id = body.stack[0].intent_id
    body.set_intent_state(intent_id, "done")

    client.pop(intent_id)
    client.ping()
    wait_for(client, lambda message: isinstance(message, Pong), what="pong after pop")

    assert body.pops == [intent_id]
    assert body.stack == []


def test_pop_of_a_running_intent_cancels_it(client: PortClient, body: FakeBody) -> None:
    wait_snapshot(client)
    command_id = client.send_command("get_furnace")
    receipts_until(client, command_id, "accepted")
    intent_id = body.stack[0].intent_id

    client.pop(intent_id)
    client.ping()
    wait_for(client, lambda message: isinstance(message, Pong), what="pong after pop")

    # Stop and cancel always work: the running intent leaves the stack, and the
    # body confirms the cancel with a `settled outcome=cancelled` receipt.
    assert body.pops == [intent_id]
    assert body.stack == []
    body.settle(command_id, outcome="cancelled")
    settled = receipts_until(client, command_id, "settled")[-1]
    assert (settled["intent_id"], settled["outcome"]) == (intent_id, "cancelled")


def test_snapshot_passes_owner_stack_and_stale_ticks(
    client: PortClient, body: FakeBody
) -> None:
    wait_snapshot(client)
    command_id = client.send_command("get_furnace")
    receipts_until(client, command_id, "accepted")
    intent_id = body.stack[0].intent_id
    body.set_owner("skill:1")
    body.stale_ticks = 4

    client.resync()
    snapshot = wait_snapshot(client)

    assert snapshot.owner == "skill:1"
    assert snapshot.stale_ticks == 4
    assert client.owner == "skill:1"
    assert client.stale_ticks == 4
    assert [(entry.intent_id, entry.state) for entry in client.stack] == [
        (intent_id, "running")
    ]


def test_silence_helpers(client: PortClient) -> None:
    wait_snapshot(client)
    later = time.monotonic() + 10.0

    assert client.seconds_since_received(now=later) >= 3.0
    assert client.is_silent(now=later)
    assert not client.is_silent(limit=1000.0, now=later)


def test_connect_without_a_snapshot_then_resync() -> None:
    with FakeBody(send_snapshot_on_connect=False) as body:
        port = PortClient(body.port, handshake_timeout=2.0)
        port.connect()
        try:
            assert port.poll(0.2) is None
            port.resync()
            snapshot = wait_snapshot(port)
            assert (snapshot.seq, snapshot.tick) == (0, 0)
        finally:
            port.close()


def test_first_frame_not_hello_gets_a_protocol_bye() -> None:
    with FakeBody() as body:
        sock = socket.create_connection((body.host, body.port), timeout=2.0)
        sock.settimeout(2.0)
        try:
            hello = read_frame(sock)
            assert hello is not None and hello["t"] == "hello"
            sock.sendall(encode_frame(Ping()))
            assert read_frame(sock) == {"t": "bye", "reason": "protocol"}
            try:
                assert read_frame(sock) is None
            except (PortClosed, OSError):
                pass
        finally:
            sock.close()


def test_handshake_timeout_gets_a_protocol_bye() -> None:
    with FakeBody(handshake_timeout=0.2) as body:
        sock = socket.create_connection((body.host, body.port), timeout=2.0)
        sock.settimeout(2.0)
        try:
            hello = read_frame(sock)
            assert hello is not None and hello["t"] == "hello"
            assert read_frame(sock) == {"t": "bye", "reason": "protocol"}
        finally:
            sock.close()


def test_command_args_must_be_strings() -> None:
    with pytest.raises(ProtocolError):
        parse_client_message({
            "t": "command",
            "id": "c-1",
            "skill": "get_furnace",
            "args": [3],
            "tier": "command",
            "expires_at_tick": 1,
            "based_on_generation": 0,
        })


def test_wire_integers_are_bounded() -> None:
    command = {
        "t": "command",
        "id": "c-1",
        "skill": "get_furnace",
        "args": [],
        "tier": "command",
        "expires_at_tick": 1,
        "based_on_generation": 0,
    }
    with pytest.raises(ProtocolError):
        parse_client_message({**command, "expires_at_tick": 2**64})
    with pytest.raises(ProtocolError):
        parse_client_message({**command, "based_on_generation": 2**32})
    with pytest.raises(ProtocolError):
        parse_client_message({"t": "pop", "id": "c-2", "intent_id": -1})
    with pytest.raises(ProtocolError):
        parse_body_message({
            "t": "snapshot",
            "seq": 0,
            "tick": 0,
            "generation": 2**32,
            "stack": [],
            "facts": {},
            "stale_ticks": 0,
        })


def test_python_brain_frames_are_pinned() -> None:
    """The Rust decoder replay test reads these exact files."""
    frames = {
        "brain_hello.bin": ClientHello(),
        "brain_command.bin": Command(
            id="c-0007",
            skill="get_furnace",
            args=["minecraft:furnace"],
            tier="command",
            expires_at_tick=4621,
            based_on_generation=7,
            condition="count(furnace) >= 1",
            budget_ticks=6000,
        ),
        "brain_pop.bin": Pop(id="c-0008", intent_id=3),
        "brain_resync_request.bin": ResyncRequest(),
        "brain_ping.bin": Ping(),
    }
    for name, message in frames.items():
        assert encode_frame(message) == golden_bytes(name), name


def test_rust_body_frames_parse() -> None:
    """Frames written by the Rust server must parse with the Python models."""
    assert parse_golden("rust_hello.bin") == Hello(
        port_version=0,
        caps=["command", "snapshot", "pop"],
        body="praxion 0.x (abc123)",
        run_id="run-1",
    )
    assert parse_golden("rust_event.bin") == EventMessage(
        seq=812,
        tick=4521,
        event={"kind": "owner_changed", "generation": 7, "owner": "skill:3"},
    )
    assert parse_golden("rust_snapshot.bin") == Snapshot(
        seq=3,
        tick=4,
        generation=1,
        owner="skill:1",
        stack=[
            StackEntry(
                intent_id=1,
                tier="command",
                skill="get_wood",
                args=[],
                state="running",
            )
        ],
        facts=Facts(
            health=20.0,
            food=18,
            air=300,
            is_night=False,
            day_time=4,
            dimension="overworld",
            pos=[12, 64, -30],
            inventory={"oak_log": 3},
        ),
        stale_ticks=0,
    )
    assert parse_golden("rust_dropped.bin") == Dropped(seq=812, n=37)
    assert parse_golden("rust_pong.bin") == Pong(seq=812, tick=4521)
    assert parse_golden("rust_bye_replaced.bin") == Bye(reason="replaced")
    assert parse_golden("rust_bye_protocol.bin") == Bye(reason="protocol")

def test_resync_is_marked_before_the_request_is_sent(client: PortClient) -> None:
    # The reader thread may see the snapshot reply before resync() returns;
    # the flag must already be up or it would clear a flag set afterwards.
    wait_snapshot(client)
    seen: list[bool] = []
    original = client._send

    def spy(message) -> None:
        seen.append(client._resyncing)
        original(message)

    client._send = spy
    client.resync()
    assert seen == [True]
    assert client.resync_count == 1
    # A second request while one is in flight is a no-op.
    client.resync()
    assert seen == [True]
    assert client.resync_count == 1
    wait_snapshot(client)
