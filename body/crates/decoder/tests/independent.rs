//! Independent tests for the decoder (t3b review).
//!
//! These are written by the reviewer, not the crate author, and exercise
//! properties the author's tests do not: chunk-boundary invariance across a
//! whole capture, per-connection isolation of a Gap, and connection reuse
//! after Close, plus an oracle cross-check against the last packets of
//! corpus4/corpus5.

use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;


use azalea_protocol::{
    packets::{
        ClientIntention,
        game::{ClientboundGamePacket, ServerboundGamePacket},
        handshake::{ServerboundHandshakePacket, ServerboundIntention},
        status::{ServerboundStatusPacket, ServerboundStatusRequest},
    },
    write::{encode_to_network_packet, serialize_packet},
};
use praxion_decoder::{Decoder, Event, Packet};
use praxion_tee::{Kind, Reader, Record};
use praxion_world::World;

// corpus5 world reset check uses World already.

fn repo_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn read_records(path: &PathBuf) -> Option<Vec<Record>> {
    let file = File::open(path).ok()?;
    let reader = Reader::new(BufReader::new(file)).ok()?;
    Some(reader.map(Result::unwrap).collect())
}

fn next_rng(seed: &mut u64) -> u64 {
    let mut x = *seed;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *seed = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}

fn rechunk_records(records: &[Record], seed: &mut u64) -> Vec<Record> {
    let mut out = Vec::new();
    for record in records {
        match record.kind {
            Kind::Serverbound | Kind::Clientbound => {
                if record.data.is_empty() {
                    out.push(record.clone());
                    continue;
                }
                let mut offset = 0;
                while offset < record.data.len() {
                    let chunk = 1 + (next_rng(seed) % 97) as usize;
                    let end = (offset + chunk).min(record.data.len());
                    out.push(Record {
                        ts_ns: record.ts_ns,
                        conn_id: record.conn_id,
                        kind: record.kind,
                        data: record.data[offset..end].to_vec(),
                    });
                    offset = end;
                }
            }
            _ => out.push(record.clone()),
        }
    }
    out
}

fn decoded_sequence(records: &[Record]) -> (Vec<(u64, u32, &'static str, &'static str, String, u32)>, usize) {
    let mut decoder = Decoder::new();
    decoder.set_reorder_window(60_000_000_000);
    let mut sequence = Vec::new();
    let mut desyncs = 0usize;
    for record in records {
        for event in decoder.feed(record) {
            match event {
                Event::Decoded(decoded) => {
                    sequence.push((
                        decoded.ts_ns,
                        decoded.conn_id,
                        decoded.dir.as_str(),
                        decoded.state.as_str(),
                        decoded.packet.name().to_owned(),
                        decoded.packet.id(),
                    ));
                }
                Event::Desync(_) => desyncs += 1,
                _ => {}
            }
        }
    }
    for event in decoder.finish() {
        match event {
            Event::Decoded(decoded) => {
                sequence.push((
                    decoded.ts_ns,
                    decoded.conn_id,
                    decoded.dir.as_str(),
                    decoded.state.as_str(),
                    decoded.packet.name().to_owned(),
                    decoded.packet.id(),
                ));
            }
            Event::Desync(_) => desyncs += 1,
            _ => {}
        }
    }
    (sequence, desyncs)
}

#[test]
fn corpus1_rechunked_decodes_identically() {
    let path = repo_root().join("spikes/captures/corpus1.prxcap");
    let Some(records) = read_records(&path) else {
        eprintln!("SKIP: corpus1.prxcap not present");
        return;
    };

    let (original, original_desyncs) = decoded_sequence(&records);

    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    for run in 0..5 {
        let mut s = seed;
        let rechunked = rechunk_records(&records, &mut s);
        seed = s;
        let (rechunked_seq, rechunked_desyncs) = decoded_sequence(&rechunked);
        assert_eq!(
            original_desyncs, rechunked_desyncs,
            "run {run}: desync count differs"
        );
        assert_eq!(
            original, rechunked_seq,
            "run {run}: decoded packet sequence differs after re-chunking"
        );
    }
}

fn handshake_frame() -> Vec<u8> {
    let packet = ServerboundHandshakePacket::Intention(ServerboundIntention {
        protocol_version: 777,
        hostname: "localhost".to_owned(),
        port: 25565,
        intention: ClientIntention::Status,
    });
    let raw = serialize_packet(&packet).expect("serialize handshake");
    encode_to_network_packet(&raw, None, &mut None)
}

fn status_request_frame() -> Vec<u8> {
    let packet = ServerboundStatusPacket::StatusRequest(ServerboundStatusRequest);
    let raw = serialize_packet(&packet).expect("serialize status request");
    encode_to_network_packet(&raw, None, &mut None)
}

#[test]
fn gap_desyncs_one_connection_second_keeps_decoding() {
    let hs = handshake_frame();
    let sr = status_request_frame();

    let mut records = Vec::new();
    // Connection 0: decodes one handshake, then a gap desyncs it.
    records.push(Record { ts_ns: 1, conn_id: 0, kind: Kind::Opened, data: Vec::new() });
    records.push(Record { ts_ns: 2, conn_id: 0, kind: Kind::Serverbound, data: hs.clone() });
    records.push(Record { ts_ns: 3, conn_id: 0, kind: Kind::Gap { dropped_bytes: 7 }, data: Vec::new() });
    // Connection 1 starts after the gap and must still decode.
    records.push(Record { ts_ns: 4, conn_id: 1, kind: Kind::Opened, data: Vec::new() });
    records.push(Record { ts_ns: 5, conn_id: 1, kind: Kind::Serverbound, data: hs.clone() });
    records.push(Record { ts_ns: 6, conn_id: 1, kind: Kind::Serverbound, data: sr.clone() });

    let mut decoder = Decoder::new();
    let mut events = Vec::new();
    for record in &records {
        events.extend(decoder.feed(record));
    }
    events.extend(decoder.finish());

    let desyncs: Vec<&praxion_decoder::Desync> = events
        .iter()
        .filter_map(|e| match e {
            Event::Desync(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(desyncs.len(), 1, "expected exactly one desync");
    assert_eq!(desyncs[0].conn_id, 0);

    let decoded: Vec<&praxion_decoder::Decoded> = events
        .iter()
        .filter_map(|e| match e {
            Event::Decoded(d) => Some(d),
            _ => None,
        })
        .collect();
    // Connection 0 decoded its handshake before the gap; connection 1 decoded
    // both of its packets after the gap.
    assert_eq!(
        decoded.iter().filter(|d| d.conn_id == 0).count(),
        1,
        "connection 0 should have decoded only its pre-gap handshake"
    );
    assert_eq!(
        decoded.iter().filter(|d| d.conn_id == 1).count(),
        2,
        "connection 1 should keep decoding after the gap on connection 0"
    );
}

#[test]
fn closed_half_frame_and_reopened_conn_starts_clean() {
    let hs = handshake_frame();

    let mut records = Vec::new();
    records.push(Record { ts_ns: 1, conn_id: 0, kind: Kind::Opened, data: Vec::new() });
    // A frame that claims 10 bytes but only 3 arrive, then Closed.
    records.push(Record { ts_ns: 2, conn_id: 0, kind: Kind::Serverbound, data: vec![10, 1, 2, 3] });
    records.push(Record { ts_ns: 3, conn_id: 0, kind: Kind::Closed, data: Vec::new() });
    // Reuse the same conn id: should start clean and decode again.
    records.push(Record { ts_ns: 4, conn_id: 0, kind: Kind::Opened, data: Vec::new() });
    records.push(Record { ts_ns: 5, conn_id: 0, kind: Kind::Serverbound, data: hs });

    let mut decoder = Decoder::new();
    let mut events = Vec::new();
    for record in &records {
        events.extend(decoder.feed(record));
    }
    events.extend(decoder.finish());

    let desyncs = events.iter().filter(|e| matches!(e, Event::Desync(_))).count();
    let decoded = events.iter().filter(|e| matches!(e, Event::Decoded(_))).count();
    assert_eq!(desyncs, 1, "the truncated frame at Close must desync");
    assert_eq!(decoded, 1, "the reopened connection must decode cleanly");
}

#[test]
fn oracle_corpus4_corpus5_final_state_matches_last_packets() {
    for name in ["corpus4.prxcap", "corpus5.prxcap"] {
        let path = repo_root().join("spikes/captures").join(name);
        let Some(records) = read_records(&path) else {
            eprintln!("SKIP: {name} not present");
            return;
        };

        let mut decoder = Decoder::new();
        decoder.set_reorder_window(60_000_000_000);
        let mut world = World::new();
        let mut followed = None;

        // Track the last serverbound move position and last set_health value
        // seen in stream order, so we can compare them with the world model.
        let mut last_sb_pos: Option<(f64, f64, f64)> = None;
        let mut last_health: Option<f32> = None;

        for record in &records {
            for event in decoder.feed(record) {
                if let Event::Decoded(decoded) = &event {
                    match &decoded.packet {
                        Packet::PlayServerbound(ServerboundGamePacket::MovePlayerPos(p)) => {
                            last_sb_pos = Some((p.pos.x, p.pos.y, p.pos.z));
                        }
                        Packet::PlayClientbound(ClientboundGamePacket::SetHealth(p)) => {
                            last_health = Some(p.health);
                        }
                        _ => {}
                    }
                }
                match &event {
                    Event::Opened { conn_id, .. } => world.opened(*conn_id),
                    Event::Desync(desync) => world.invalidate(desync.conn_id, desync.reason.clone()),
                    Event::Decoded(decoded) => match &decoded.packet {
                        Packet::ConfigurationClientbound(pkt) => {
                            world.apply_clientbound_config(decoded.conn_id, pkt);
                        }
                        Packet::PlayClientbound(pkt) => {
                            if decoded.packet.name() == "login" && followed != Some(decoded.conn_id) {
                                world.follow(decoded.conn_id);
                                followed = Some(decoded.conn_id);
                            }
                            world.apply_clientbound_play(decoded.conn_id, pkt);
                        }
                        Packet::PlayServerbound(pkt) => {
                            world.apply_serverbound_play(decoded.conn_id, pkt);
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        for event in decoder.finish() {
            if let Event::Decoded(decoded) = &event {
                match &decoded.packet {
                    Packet::PlayServerbound(ServerboundGamePacket::MovePlayerPos(p)) => {
                        last_sb_pos = Some((p.pos.x, p.pos.y, p.pos.z));
                    }
                    Packet::PlayClientbound(ClientboundGamePacket::SetHealth(p)) => {
                        last_health = Some(p.health);
                    }
                    _ => {}
                }
            }
            match &event {
                Event::Opened { conn_id, .. } => world.opened(*conn_id),
                Event::Desync(desync) => world.invalidate(desync.conn_id, desync.reason.clone()),
                Event::Decoded(decoded) => match &decoded.packet {
                    Packet::ConfigurationClientbound(pkt) => {
                        world.apply_clientbound_config(decoded.conn_id, pkt);
                    }
                    Packet::PlayClientbound(pkt) => {
                        if decoded.packet.name() == "login" && followed != Some(decoded.conn_id) {
                            world.follow(decoded.conn_id);
                            followed = Some(decoded.conn_id);
                        }
                        world.apply_clientbound_play(decoded.conn_id, pkt);
                    }
                    Packet::PlayServerbound(pkt) => {
                        world.apply_serverbound_play(decoded.conn_id, pkt);
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        assert!(world.status().is_valid(), "{name}: world invalid");

        // Health: world must match the capture's last set_health.
        let health = world.health().expect("health");
        assert_eq!(
            health.health(),
            last_health.expect("last set_health"),
            "{name}: health mismatch"
        );

        // Position: the world model tracks the last serverbound move packet's
        // position (the client-reported position). Compare against it.
        let pos = world.player_position().expect("position");
        let (lx, ly, lz) = last_sb_pos.expect("last serverbound move");
        assert_eq!(pos.x, lx, "{name}: x mismatch");
        assert_eq!(pos.y, ly, "{name}: y mismatch");
        assert_eq!(pos.z, lz, "{name}: z mismatch");
    }
}
#[test]
fn corpus5_reconnect_world_does_not_keep_old_chunks() {
    let path = repo_root().join("spikes/captures/corpus5.prxcap");
    let Some(records) = read_records(&path) else {
        eprintln!("SKIP: corpus5.prxcap not present");
        return;
    };

    let mut decoder = Decoder::new();
    decoder.set_reorder_window(60_000_000_000);
    let mut world = World::new();
    let mut followed = None;
    let mut play_logins = Vec::new();
    let mut reset_verified = false;

    for record in &records {
        for event in decoder.feed(record) {
            if let Event::Decoded(d) = &event {
                if d.packet.name() == "login" && matches!(d.packet, Packet::PlayClientbound(_)) {
                    play_logins.push(d.conn_id);
                }
            }
            match &event {
                Event::Opened { conn_id, .. } => world.opened(*conn_id),
                Event::Desync(desync) => world.invalidate(desync.conn_id, desync.reason.clone()),
                Event::Decoded(decoded) => match &decoded.packet {
                    Packet::ConfigurationClientbound(pkt) => {
                        world.apply_clientbound_config(decoded.conn_id, pkt);
                    }
                    Packet::PlayClientbound(pkt) => {
                        if decoded.packet.name() == "login" && followed != Some(decoded.conn_id) {
                            if followed.is_some() {
                                // Follow resets the world; verify before applying
                                // the new connection's login packet.
                                world.follow(decoded.conn_id);
                                followed = Some(decoded.conn_id);
                                assert_eq!(
                                    world.chunk_count(),
                                    0,
                                    "reconnect did not clear old chunks"
                                );
                                assert_eq!(world.entities().count(), 0);
                                reset_verified = true;
                            } else {
                                world.follow(decoded.conn_id);
                                followed = Some(decoded.conn_id);
                            }
                        }
                        world.apply_clientbound_play(decoded.conn_id, pkt);
                    }
                    Packet::PlayServerbound(pkt) => {
                        world.apply_serverbound_play(decoded.conn_id, pkt);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
    }
    for event in decoder.finish() {
        match &event {
            Event::Opened { conn_id, .. } => world.opened(*conn_id),
            Event::Desync(desync) => world.invalidate(desync.conn_id, desync.reason.clone()),
            Event::Decoded(decoded) => match &decoded.packet {
                Packet::ConfigurationClientbound(pkt) => {
                    world.apply_clientbound_config(decoded.conn_id, pkt);
                }
                Packet::PlayClientbound(pkt) => {
                    if decoded.packet.name() == "login" && followed != Some(decoded.conn_id) {
                        if followed.is_some() {
                            world.follow(decoded.conn_id);
                            followed = Some(decoded.conn_id);
                            assert_eq!(world.chunk_count(), 0);
                            assert_eq!(world.entities().count(), 0);
                            reset_verified = true;
                        } else {
                            world.follow(decoded.conn_id);
                            followed = Some(decoded.conn_id);
                        }
                    }
                    world.apply_clientbound_play(decoded.conn_id, pkt);
                }
                Packet::PlayServerbound(pkt) => {
                    world.apply_serverbound_play(decoded.conn_id, pkt);
                }
                _ => {}
            },
            _ => {}
        }
    }

    assert!(play_logins.len() >= 2, "expected reconnect, got {:?}", play_logins);
    assert_eq!(followed, play_logins.last().copied());
    assert!(reset_verified, "reconnect reset was never observed");
    assert!(world.status().is_valid());
}
