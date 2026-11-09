//! Wire-level and loopback tests (proposal §2.6, unit list).

use std::fs;
use std::io::Cursor;
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use praxion_eventlog::{Event, EventKind};

use crate::forwards;
use crate::frame::{FrameError, FrameReader, MAX_FRAME_SIZE, write_frame, write_payload};
use crate::server::{ConnectionState, PortConfig, PortServer, PublishOutcome};
use crate::wire::{
    BodyMessage, BrainMessage, ByeReason, Capability, Command, Facts, PORT_VERSION, SlotFact,
    Snapshot, StackEntry,
};

fn config() -> PortConfig {
    PortConfig {
        body: "praxion test".to_owned(),
        run_id: "run-1".to_owned(),
    }
}

fn server() -> PortServer {
    PortServer::bind("127.0.0.1:0".parse().unwrap(), config()).unwrap()
}

fn event(seq: u64, tick: u64) -> Event {
    Event {
        seq,
        tick,
        ts_ns: seq * 1_000,
        event: EventKind::Note {
            text: format!("event {seq}"),
        },
    }
}

fn snapshot(seq: u64) -> Snapshot {
    Snapshot {
        seq,
        tick: seq + 1,
        generation: 1,
        owner: "skill:1".to_owned(),
        stack: vec![StackEntry {
            intent_id: 1,
            tier: "command".to_owned(),
            skill: "get_wood".to_owned(),
            args: vec![],
            state: "running".to_owned(),
        }],
        facts: Facts {
            health: 20.0,
            food: 18,
            air: 300,
            is_night: false,
            day_time: seq + 1,
            dimension: "overworld".to_owned(),
            pos: [12, 64, -30],
            inventory: [("oak_log".to_owned(), 3)].into_iter().collect(),
            selected_hotbar_slot: None,
            slots: Vec::new(),
        },
        stale_ticks: 0,
    }
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }
    panic!("condition was not met within 5 s");
}

/// A fake brain over the real wire.
struct Brain {
    writer: TcpStream,
    reader: FrameReader<TcpStream>,
}

impl Brain {
    fn connect(server: &PortServer) -> Self {
        let stream = TcpStream::connect(server.local_addr()).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reader = FrameReader::new(stream.try_clone().unwrap());
        let mut brain = Self {
            writer: stream,
            reader,
        };
        // The body speaks first (proposal §2.1).
        match brain.receive() {
            BodyMessage::Hello {
                port_version, caps, ..
            } => {
                assert_eq!(port_version, PORT_VERSION);
                assert_eq!(
                    caps,
                    vec![Capability::Command, Capability::Snapshot, Capability::Pop]
                );
            }
            other => panic!("expected hello, got {other:?}"),
        }
        brain.send(&BrainMessage::Hello {
            port_version: PORT_VERSION,
        });
        brain
    }

    fn send(&mut self, message: &BrainMessage) {
        write_frame(&mut self.writer, message).unwrap();
    }

    fn receive(&mut self) -> BodyMessage {
        self.reader
            .read_json::<BodyMessage>()
            .expect("frame read failed")
            .expect("body closed the connection")
    }

    fn receive_until(&mut self, mut matches: impl FnMut(&BodyMessage) -> bool) -> BodyMessage {
        loop {
            let message = self.receive();
            if matches(&message) {
                return message;
            }
        }
    }
}

fn wait_incoming(server: &PortServer) -> Vec<BrainMessage> {
    let mut messages = Vec::new();
    wait_for(|| {
        messages.extend(server.poll_incoming());
        !messages.is_empty()
    });
    messages
}

#[test]
fn frames_round_trip() {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &BodyMessage::Pong { seq: 7, tick: 9 }).unwrap();
    let len = u32::try_from(bytes.len() - 4).unwrap();
    assert_eq!(&bytes[..4], &len.to_be_bytes());

    let mut reader = FrameReader::new(Cursor::new(bytes));
    let message = reader.read_json::<BodyMessage>().unwrap().unwrap();
    assert_eq!(message, BodyMessage::Pong { seq: 7, tick: 9 });
    assert_eq!(reader.read_json::<BodyMessage>().unwrap(), None);
}

#[test]
fn a_partial_frame_is_reported_as_truncated() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&5_u32.to_be_bytes());
    bytes.extend_from_slice(b"{\"");
    let mut reader = FrameReader::new(Cursor::new(bytes));
    match reader.read_json::<BodyMessage>() {
        Err(FrameError::Truncated { expected, got }) => {
            assert_eq!(expected, 9);
            assert_eq!(got, 6);
        }
        other => panic!("expected truncated, got {other:?}"),
    }
}

#[test]
fn a_truncated_header_is_reported_as_truncated() {
    let mut reader = FrameReader::new(Cursor::new(vec![0, 0]));
    match reader.read_json::<BodyMessage>() {
        Err(FrameError::Truncated { expected, got }) => {
            assert_eq!(expected, 4);
            assert_eq!(got, 2);
        }
        other => panic!("expected truncated, got {other:?}"),
    }
}

#[test]
fn an_oversized_frame_is_rejected() {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&((MAX_FRAME_SIZE + 1) as u32).to_be_bytes());
    let mut reader = FrameReader::new(Cursor::new(bytes));
    match reader.read_json::<BodyMessage>() {
        Err(FrameError::TooLarge { len }) => assert_eq!(len, MAX_FRAME_SIZE + 1),
        other => panic!("expected too large, got {other:?}"),
    }
}

#[test]
fn writing_an_oversized_payload_fails() {
    let payload = vec![b'x'; MAX_FRAME_SIZE + 1];
    let mut bytes = Vec::new();
    match write_payload(&mut bytes, &payload) {
        Err(FrameError::TooLarge { len }) => assert_eq!(len, MAX_FRAME_SIZE + 1),
        other => panic!("expected too large, got {other:?}"),
    }
}

#[test]
fn the_wire_shapes_match_the_proposal() {
    let hello = BodyMessage::Hello {
        port_version: PORT_VERSION,
        caps: vec![Capability::Command, Capability::Snapshot, Capability::Pop],
        body: "praxion 0.x (abc123)".to_owned(),
        run_id: "run-1".to_owned(),
    };
    assert_eq!(
        serde_json::to_string(&hello).unwrap(),
        r#"{"t":"hello","port_version":0,"caps":["command","snapshot","pop"],"body":"praxion 0.x (abc123)","run_id":"run-1"}"#
    );

    let event = BodyMessage::Event {
        seq: 812,
        tick: 4521,
        event: EventKind::OwnerChanged {
            generation: 7,
            owner: "skill:3".to_owned(),
        },
    };
    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        r#"{"t":"event","seq":812,"tick":4521,"event":{"kind":"owner_changed","generation":7,"owner":"skill:3"}}"#
    );

    let snapshot = BodyMessage::Snapshot(snapshot(3));
    assert_eq!(
        serde_json::to_string(&snapshot).unwrap(),
        r#"{"t":"snapshot","seq":3,"tick":4,"generation":1,"owner":"skill:1","stack":[{"intent_id":1,"tier":"command","skill":"get_wood","args":[],"state":"running"}],"facts":{"health":20.0,"food":18,"air":300,"is_night":false,"day_time":4,"dimension":"overworld","pos":[12,64,-30],"inventory":{"oak_log":3}},"stale_ticks":0}"#
    );

    let command = BrainMessage::Command(Command {
        id: "c-0007".to_owned(),
        skill: "get_furnace".to_owned(),
        args: vec![],
        tier: "command".to_owned(),
        expires_at_tick: 4621,
        based_on_generation: 7,
        condition: Some("count(furnace) >= 1".to_owned()),
        budget_ticks: Some(6000),
    });
    assert_eq!(
        serde_json::to_string(&command).unwrap(),
        r#"{"t":"command","id":"c-0007","skill":"get_furnace","args":[],"tier":"command","expires_at_tick":4621,"based_on_generation":7,"condition":"count(furnace) >= 1","budget_ticks":6000}"#
    );

    assert_eq!(
        serde_json::to_string(&BrainMessage::ResyncRequest).unwrap(),
        r#"{"t":"resync_request"}"#
    );
    assert_eq!(
        serde_json::to_string(&BrainMessage::Ping).unwrap(),
        r#"{"t":"ping"}"#
    );
}

#[test]
fn slots_are_optional_so_the_golden_frame_stays_put() {
    // Additive fields without a PORT_VERSION bump: an old brain ignores them
    // and an old body sends neither (decision 2026-11-09 "Console").
    let mut facts = Facts::default();
    assert_eq!(
        serde_json::to_string(&facts).unwrap(),
        r#"{"health":0.0,"food":0,"air":0,"is_night":false,"day_time":0,"dimension":"","pos":[0,0,0],"inventory":{}}"#
    );

    facts.selected_hotbar_slot = Some(2);
    facts.slots = vec![SlotFact {
        slot: 9,
        item: "oak_log".to_owned(),
        count: 12,
    }];
    let json = serde_json::to_string(&facts).unwrap();
    assert!(json.contains(r#""selected_hotbar_slot":2"#));
    assert!(json.contains(r#""slots":[{"slot":9,"item":"oak_log","count":12}]"#));
    assert_eq!(serde_json::from_str::<Facts>(&json).unwrap(), facts);
}

#[test]
fn every_message_round_trips() {
    let body = [
        BodyMessage::Hello {
            port_version: PORT_VERSION,
            caps: vec![Capability::Command],
            body: "b".to_owned(),
            run_id: "r".to_owned(),
        },
        BodyMessage::Event {
            seq: 1,
            tick: 2,
            event: EventKind::WakeBrain {
                cause: "reflex".to_owned(),
                detail: "creeper".to_owned(),
            },
        },
        BodyMessage::Snapshot(snapshot(5)),
        BodyMessage::Dropped { seq: 5, n: 3 },
        BodyMessage::Pong { seq: 5, tick: 6 },
        BodyMessage::Bye {
            reason: ByeReason::Shutdown,
        },
    ];
    for message in body {
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(serde_json::from_str::<BodyMessage>(&json).unwrap(), message);
    }

    let brain = [
        BrainMessage::Hello {
            port_version: PORT_VERSION,
        },
        BrainMessage::Command(Command {
            id: "c-1".to_owned(),
            skill: "s".to_owned(),
            args: vec!["a".to_owned()],
            tier: "command".to_owned(),
            expires_at_tick: 100,
            based_on_generation: 2,
            condition: None,
            budget_ticks: None,
        }),
        BrainMessage::Pop {
            id: "c-2".to_owned(),
            intent_id: 3,
        },
        BrainMessage::ResyncRequest,
        BrainMessage::Ping,
    ];
    for message in brain {
        let json = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<BrainMessage>(&json).unwrap(),
            message
        );
    }
}

#[test]
fn the_handshake_succeeds() {
    let server = server();
    let brain = Brain::connect(&server);
    wait_for(|| server.connection_state() == ConnectionState::Connected);
    drop(brain);
    wait_for(|| server.connection_state() == ConnectionState::Disconnected);
}

#[test]
fn a_version_mismatch_is_refused() {
    let server = server();
    let stream = TcpStream::connect(server.local_addr()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = FrameReader::new(stream.try_clone().unwrap());
    let mut writer = stream;

    // The body greets first, then judges the brain's hello.
    assert!(matches!(
        reader.read_json::<BodyMessage>().unwrap(),
        Some(BodyMessage::Hello { .. })
    ));
    write_frame(
        &mut writer,
        &BrainMessage::Hello {
            port_version: PORT_VERSION + 1,
        },
    )
    .unwrap();
    assert_eq!(
        reader.read_json::<BodyMessage>().unwrap(),
        Some(BodyMessage::Bye {
            reason: ByeReason::Version
        })
    );
    assert_eq!(reader.read_json::<BodyMessage>().unwrap(), None);
}

#[test]
fn a_first_frame_that_is_not_hello_gets_a_protocol_bye() {
    let server = server();
    let stream = TcpStream::connect(server.local_addr()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = FrameReader::new(stream.try_clone().unwrap());
    let mut writer = stream;

    assert!(matches!(
        reader.read_json::<BodyMessage>().unwrap(),
        Some(BodyMessage::Hello { .. })
    ));
    write_frame(&mut writer, &BrainMessage::Ping).unwrap();
    assert_eq!(
        reader.read_json::<BodyMessage>().unwrap(),
        Some(BodyMessage::Bye {
            reason: ByeReason::Protocol
        })
    );
    assert_eq!(reader.read_json::<BodyMessage>().unwrap(), None);
}

#[test]
fn a_second_brain_replaces_the_first() {
    let server = server();
    let mut first = Brain::connect(&server);
    wait_for(|| server.connection_state() == ConnectionState::Connected);

    let mut second = Brain::connect(&server);
    assert_eq!(
        first.receive(),
        BodyMessage::Bye {
            reason: ByeReason::Replaced
        }
    );
    assert_eq!(first.reader.read_json::<BodyMessage>().unwrap(), None);
    wait_for(|| server.connection_state() == ConnectionState::Connected);

    server.publish(event(4, 9));
    let message = second.receive();
    assert!(
        matches!(message, BodyMessage::Event { seq: 4, .. }),
        "{message:?}"
    );
    assert_eq!(server.connection_state(), ConnectionState::Connected);
}

#[test]
fn events_arrive_in_seq_order_and_commands_reach_the_driver() {
    let server = server();
    let mut brain = Brain::connect(&server);

    for seq in 0..10 {
        assert_eq!(
            server.publish(event(seq, 100 + seq)),
            PublishOutcome::Queued
        );
    }
    for seq in 0..10 {
        match brain.receive() {
            BodyMessage::Event { seq: got, tick, .. } => {
                assert_eq!(got, seq);
                assert_eq!(tick, 100 + seq);
            }
            other => panic!("expected event {seq}, got {other:?}"),
        }
    }

    let command = Command {
        id: "c-0001".to_owned(),
        skill: "get_wood".to_owned(),
        args: vec!["oak".to_owned()],
        tier: "command".to_owned(),
        expires_at_tick: 500,
        based_on_generation: 3,
        condition: Some("has(oak_log)".to_owned()),
        budget_ticks: Some(6000),
    };
    brain.send(&BrainMessage::Command(command.clone()));

    let messages = wait_incoming(&server);
    assert!(messages.contains(&BrainMessage::Command(command)));
}

#[test]
fn overflow_sends_dropped_and_a_fresh_snapshot() {
    let server =
        PortServer::bind_with_capacity("127.0.0.1:0".parse().unwrap(), config(), 0).unwrap();
    let mut brain = Brain::connect(&server);
    wait_for(|| server.connection_state() == ConnectionState::Connected);

    assert_eq!(server.publish(event(7, 70)), PublishOutcome::Dropped);
    assert!(server.needs_snapshot());
    assert_eq!(server.stats().dropped, 1);

    // An old state may not stand in for the dropped events: the port holds it
    // back until a snapshot at or after seq 7 arrives.
    server.publish_snapshot(snapshot(3));
    server.publish_snapshot(snapshot(7));
    assert_eq!(brain.receive(), BodyMessage::Dropped { seq: 7, n: 1 });
    assert_eq!(brain.receive(), BodyMessage::Snapshot(snapshot(7)));
    wait_for(|| !server.needs_snapshot());
}

#[test]
fn a_ping_is_answered_with_the_last_event_seq() {
    let server = server();
    let mut brain = Brain::connect(&server);

    server.publish(event(3, 42));
    assert!(matches!(brain.receive(), BodyMessage::Event { seq: 3, .. }));

    brain.send(&BrainMessage::Ping);
    assert_eq!(
        brain.receive_until(|message| matches!(message, BodyMessage::Pong { .. })),
        BodyMessage::Pong { seq: 3, tick: 42 }
    );

    // An idle body publishes nothing; the pong still carries the live tick.
    server.set_tick(900);
    brain.send(&BrainMessage::Ping);
    assert_eq!(
        brain.receive_until(|message| matches!(message, BodyMessage::Pong { .. })),
        BodyMessage::Pong { seq: 3, tick: 900 }
    );
}

#[test]
fn a_resync_request_is_answered_with_a_snapshot_then_deltas() {
    let server = server();
    let mut brain = Brain::connect(&server);

    server.publish(event(1, 10));
    server.publish(event(2, 20));
    brain.receive();
    brain.receive();

    brain.send(&BrainMessage::ResyncRequest);
    // Wait until the connection thread has seen the request. Otherwise the
    // published snapshot can go out as a wake snapshot, cut at the last sent
    // event, before the resync cut is known.
    wait_for(|| server.needs_snapshot());
    // The stored snapshot (seq 5) is the cut; later events must follow it.
    server.publish_snapshot(snapshot(5));
    assert_eq!(
        brain.receive_until(|message| matches!(message, BodyMessage::Snapshot(_))),
        BodyMessage::Snapshot(snapshot(5))
    );

    server.publish(event(6, 60));
    let message = brain.receive_until(|message| matches!(message, BodyMessage::Event { .. }));
    assert!(
        matches!(
            message,
            BodyMessage::Event {
                seq: 6,
                tick: 60,
                ..
            }
        ),
        "expected event 6, got {message:?}"
    );
}

#[test]
fn a_brain_can_reconnect_after_a_drop() {
    let server = server();
    let mut first = Brain::connect(&server);
    wait_for(|| server.connection_state() == ConnectionState::Connected);

    server.publish(event(0, 1));
    assert!(matches!(first.receive(), BodyMessage::Event { seq: 0, .. }));
    drop(first);
    wait_for(|| server.connection_state() == ConnectionState::Disconnected);

    server.publish(event(1, 2));
    server.publish_snapshot(snapshot(1));

    let mut second = Brain::connect(&server);
    assert_eq!(
        second.receive_until(|message| matches!(message, BodyMessage::Snapshot(_))),
        BodyMessage::Snapshot(snapshot(1))
    );

    server.publish(event(2, 3));
    assert!(matches!(
        second.receive(),
        BodyMessage::Event {
            seq: 2,
            tick: 3,
            ..
        }
    ));
    assert_eq!(server.connection_state(), ConnectionState::Connected);
}

#[test]
fn only_the_forwarded_kinds_cross_the_port() {
    assert!(forwards(&EventKind::CommandReceipt {
        command_id: "c-1".to_owned(),
        state: "accepted".to_owned(),
        intent_id: Some(1),
        generation: Some(2),
        outcome: None,
        failure_kind: None,
        reason: None,
        condition: None,
    }));
    assert!(forwards(&EventKind::OwnerChanged {
        generation: 2,
        owner: "skill:1".to_owned(),
    }));
    assert!(forwards(&EventKind::SkillFrame {
        skill: "root".to_owned(),
        depth: 1,
        outcome: "pushed".to_owned(),
        failure_kind: None,
    }));
    assert!(!forwards(&EventKind::SkillFrame {
        skill: "child".to_owned(),
        depth: 2,
        outcome: "pushed".to_owned(),
        failure_kind: None,
    }));
    assert!(!forwards(&EventKind::RuleSelected {
        skill: "root".to_owned(),
        rule: 0,
    }));
    assert!(!forwards(&EventKind::PrimitiveFinished {
        primitive: "mine".to_owned(),
        args: vec![],
        outcome: "confirmed".to_owned(),
        failure_kind: None,
        ticks: 4,
    }));
    assert!(!forwards(&EventKind::ReceiptSettled {
        receipt_id: 1,
        intent_id: 1,
        outcome: praxion_eventlog::Outcome::Confirmed,
        evidence: vec![],
        note: None,
    }));
    assert!(!forwards(&EventKind::Ledger {
        query: "q".to_owned(),
        fact: "f".to_owned(),
        count: 1,
        first_tick: 1,
        last_tick: 2,
    }));
    // Debug notes flooded the queue and lost a receipt.
    assert!(!forwards(&EventKind::Note {
        text: "look yaw=1.2".to_owned(),
    }));
}

#[test]
fn any_brain_message_moves_the_liveness_clock() {
    let server = server();
    assert!(server.last_brain_message().is_none());
    let mut brain = Brain::connect(&server);
    wait_for(|| server.last_brain_message().is_some());
    let first = server.last_brain_message().expect("the hello counts");
    brain.send(&BrainMessage::Ping);
    wait_for(|| server.last_brain_message() != Some(first));
}

#[test]
fn a_wake_snapshot_does_not_swallow_a_queued_event() {
    let server = server();
    let mut brain = Brain::connect(&server);
    server.publish(event(1, 10));
    assert!(matches!(brain.receive(), BodyMessage::Event { seq: 1, .. }));

    // The driver logs event 2 and publishes a state that already covers it in
    // the same tick. The brain has only seen seq 1, so the wake snapshot must
    // be cut there and event 2 must follow.
    server.publish_snapshot(snapshot(2));
    server.publish(event(2, 20));

    let mut wake_seq = None;
    let mut saw_event_two = false;
    for _ in 0..3 {
        match brain.receive() {
            BodyMessage::Snapshot(state) => wake_seq = Some(state.seq),
            BodyMessage::Event { seq: 2, .. } => {
                saw_event_two = true;
                break;
            }
            BodyMessage::Event { seq: 1, .. } => {}
            other => panic!("unexpected message {other:?}"),
        }
    }
    assert!(saw_event_two, "the queued event must follow the wake snapshot");
    if let Some(seq) = wake_seq {
        assert!(seq <= 1, "the wake snapshot is cut at the last sent event, got {seq}");
    }
}

#[test]
fn a_full_queue_with_no_brain_never_asks_for_a_snapshot() {
    // Capacity 1 and no connection: the second event cannot be queued and is
    // dropped without waiting for the network. Nobody is connected to resync,
    // so the drop must not leave `needs_snapshot` set forever.
    let server =
        PortServer::bind_with_capacity("127.0.0.1:0".parse().unwrap(), config(), 1).unwrap();
    assert_eq!(server.publish(event(0, 1)), PublishOutcome::Queued);
    assert_eq!(server.publish(event(1, 2)), PublishOutcome::Dropped);
    assert!(!server.needs_snapshot());
    assert_eq!(server.stats().dropped, 0);
}

// --------------------------------------------------------------------------- //
// Golden frames: the byte-level contract with `brain/praxion_brain/port.py`.
//
// `body/crates/port/tests/golden/` holds raw frames (4-byte header + JSON).
// The `rust_*` files are written by this test and parsed by `test_port.py`;
// the `brain_*` files are written by a Python run of `encode_frame` and are
// replayed here. Both suites read the same files, so a change on either side
// that breaks the other fails locally, without a live socket.
//
// Regenerate the Rust side with `PRAXION_UPDATE_GOLDEN=1 cargo test -p
// praxion-port`. The Python side is regenerated by the script noted in
// `tests/golden/README.md`.
// --------------------------------------------------------------------------- //

const GOLDEN_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden");

fn golden_path(name: &str) -> PathBuf {
    Path::new(GOLDEN_DIR).join(name)
}

/// Frames the Rust server writes, in a fixed deterministic shape.
fn body_golden_frames() -> Vec<(&'static str, BodyMessage)> {
    vec![
        (
            "rust_hello.bin",
            BodyMessage::Hello {
                port_version: PORT_VERSION,
                caps: vec![Capability::Command, Capability::Snapshot, Capability::Pop],
                body: "praxion 0.x (abc123)".to_owned(),
                run_id: "run-1".to_owned(),
            },
        ),
        (
            "rust_event.bin",
            BodyMessage::Event {
                seq: 812,
                tick: 4521,
                event: EventKind::OwnerChanged {
                    generation: 7,
                    owner: "skill:3".to_owned(),
                },
            },
        ),
        ("rust_snapshot.bin", BodyMessage::Snapshot(snapshot(3))),
        ("rust_dropped.bin", BodyMessage::Dropped { seq: 812, n: 37 }),
        (
            "rust_pong.bin",
            BodyMessage::Pong {
                seq: 812,
                tick: 4521,
            },
        ),
        (
            "rust_bye_replaced.bin",
            BodyMessage::Bye {
                reason: ByeReason::Replaced,
            },
        ),
        (
            "rust_bye_protocol.bin",
            BodyMessage::Bye {
                reason: ByeReason::Protocol,
            },
        ),
    ]
}

/// Frames the Python client writes for the same scenarios.
fn brain_golden_frames() -> Vec<(&'static str, BrainMessage)> {
    vec![
        (
            "brain_hello.bin",
            BrainMessage::Hello {
                port_version: PORT_VERSION,
            },
        ),
        (
            "brain_command.bin",
            BrainMessage::Command(Command {
                id: "c-0007".to_owned(),
                skill: "get_furnace".to_owned(),
                args: vec!["minecraft:furnace".to_owned()],
                tier: "command".to_owned(),
                expires_at_tick: 4621,
                based_on_generation: 7,
                condition: Some("count(furnace) >= 1".to_owned()),
                budget_ticks: Some(6000),
            }),
        ),
        (
            "brain_pop.bin",
            BrainMessage::Pop {
                id: "c-0008".to_owned(),
                intent_id: 3,
            },
        ),
        ("brain_resync_request.bin", BrainMessage::ResyncRequest),
        ("brain_ping.bin", BrainMessage::Ping),
    ]
}

#[test]
fn body_frames_match_the_golden_pins() {
    let update = std::env::var_os("PRAXION_UPDATE_GOLDEN").is_some();
    if update {
        fs::create_dir_all(GOLDEN_DIR).unwrap();
    }
    for (name, message) in body_golden_frames() {
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &message).unwrap();
        let path = golden_path(name);
        if update {
            fs::write(&path, &bytes).unwrap();
        }
        let golden = fs::read(&path).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(
            golden, bytes,
            "{name} is stale; rerun with PRAXION_UPDATE_GOLDEN=1 and update the Python pin"
        );
    }
}

#[test]
fn python_brain_frames_replay_exactly() {
    for (name, expected) in brain_golden_frames() {
        let golden = fs::read(golden_path(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
        let mut reader = FrameReader::new(Cursor::new(golden.clone()));
        let message = reader
            .read_json::<BrainMessage>()
            .unwrap()
            .unwrap_or_else(|| panic!("{name} holds no frame"));
        assert_eq!(
            message, expected,
            "{name} does not decode to the expected message"
        );
        assert_eq!(
            reader.read_json::<BrainMessage>().unwrap(),
            None,
            "{name} must hold exactly one frame"
        );
        let mut reencoded = Vec::new();
        write_frame(&mut reencoded, &message).unwrap();
        assert_eq!(
            reencoded, golden,
            "{name} is not canonical for the Rust decoder"
        );
    }
}
