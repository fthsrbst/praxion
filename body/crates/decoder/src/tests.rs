use std::fmt::Debug;

use azalea_buf::AzBufVar;
use azalea_core::{
    entity_id::MinecraftEntityId,
    game_type::{GameMode, OptionalGameType},
};
use azalea_protocol::{
    packets::{
        ClientIntention, ProtocolPacket,
        common::CommonPlayerSpawnInfo,
        config::{ClientboundConfigPacket, ClientboundFinishConfiguration, ClientboundKeepAlive},
        game::{
            ClientboundGamePacket, ClientboundLogin, ClientboundStartConfiguration,
            ServerboundClientTickEnd, ServerboundConfigurationAcknowledged, ServerboundGamePacket,
        },
        handshake::{ServerboundHandshakePacket, ServerboundIntention},
        login::{
            ClientboundLoginCompression, ClientboundLoginFinished, ClientboundLoginPacket,
            ServerboundHello, ServerboundLoginAcknowledged, ServerboundLoginPacket,
        },
    },
    write::{compression_encoder, encode_to_network_packet, serialize_packet},
};
use azalea_registry::{
    DataRegistry,
    data::DimensionKind,
    identifier::Identifier,
};
use praxion_tee::{Kind, Record};

use crate::{Decoder, Dir, Event, Packet, State};

struct Builder {
    records: Vec<Record>,
    ts: u64,
    conn_id: u32,
    rng: u64,
}

impl Builder {
    fn new() -> Self {
        Self {
            records: Vec::new(),
            ts: 1_700_000_000_000_000_000,
            conn_id: 0,
            rng: 0x2545_f491_4f6c_dd1d,
        }
    }

    fn next_random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Append one wire packet split into random TCP chunks.
    fn push_wire(&mut self, dir: Dir, wire: &[u8]) {
        let mut offset = 0;
        while offset < wire.len() {
            let chunk = 1 + (self.next_random() % 40) as usize;
            let end = (offset + chunk).min(wire.len());
            self.records.push(Record {
                ts_ns: self.ts,
                conn_id: self.conn_id,
                kind: match dir {
                    Dir::Serverbound => Kind::Serverbound,
                    Dir::Clientbound => Kind::Clientbound,
                },
                data: wire[offset..end].to_vec(),
            });
            offset = end;
        }
        self.ts += 1_000;
    }

    fn packet<P: ProtocolPacket + Debug>(&mut self, dir: Dir, packet: &P, threshold: Option<u32>) {
        let raw = serialize_packet(packet).expect("serialize");
        let mut cipher = None;
        let wire = encode_to_network_packet(&raw, threshold, &mut cipher);
        self.push_wire(dir, &wire);
    }

    fn marker(&mut self, kind: Kind) {
        self.records.push(Record { ts_ns: self.ts, conn_id: self.conn_id, kind, data: Vec::new() });
        self.ts += 1_000;
    }

    fn gap(&mut self, dropped_bytes: u64) {
        self.records.push(Record {
            ts_ns: self.ts,
            conn_id: self.conn_id,
            kind: Kind::Gap { dropped_bytes },
            data: Vec::new(),
        });
        self.ts += 1_000;
    }

    fn decode(&self) -> (Decoder, Vec<Event>) {
        let mut decoder = Decoder::new();
        let mut events = Vec::new();
        for record in &self.records {
            events.extend(decoder.feed(record));
        }
        events.extend(decoder.finish());
        (decoder, events)
    }
}

fn spawn_info() -> CommonPlayerSpawnInfo {
    CommonPlayerSpawnInfo {
        dimension_type: DimensionKind::new_raw(0),
        dimension: Identifier::new("minecraft:overworld"),
        seed: 0,
        game_type: GameMode::Survival,
        previous_game_type: OptionalGameType(None),
        is_debug: false,
        is_flat: false,
        last_death_location: None,
        portal_cooldown: 0,
        sea_level: 63,
    }
}

fn login_packet() -> ClientboundLogin {
    ClientboundLogin {
        player_id: MinecraftEntityId(1),
        hardcore: false,
        levels: Vec::new(),
        max_players: 20,
        chunk_radius: 8,
        simulation_distance: 8,
        reduced_debug_info: false,
        show_death_screen: true,
        do_limited_crafting: false,
        common: spawn_info(),
        online_mode: false,
        enforces_secure_chat: false,
    }
}

/// A whole session: handshake, login (compression 256), configuration, play,
/// a Play -> Configuration re-entry and back to Play.
fn add_session(builder: &mut Builder) {
    builder.marker(Kind::Opened);

    builder.packet(
        Dir::Serverbound,
        &ServerboundHandshakePacket::Intention(ServerboundIntention {
            protocol_version: 777,
            hostname: "localhost".to_owned(),
            port: 25565,
            intention: ClientIntention::Login,
        }),
        None,
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundLoginPacket::Hello(ServerboundHello {
            name: "tester".to_owned(),
            profile_id: Default::default(),
        }),
        None,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundLoginPacket::LoginCompression(ClientboundLoginCompression {
            compression_threshold: 256,
        }),
        None,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundLoginPacket::LoginFinished(ClientboundLoginFinished {
            game_profile: Default::default(),
            session_id: Default::default(),
        }),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundLoginPacket::LoginAcknowledged(ServerboundLoginAcknowledged),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::KeepAlive(ClientboundKeepAlive { id: 1 }),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::FinishConfiguration(ClientboundFinishConfiguration),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &azalea_protocol::packets::config::ServerboundConfigPacket::FinishConfiguration(
            azalea_protocol::packets::config::ServerboundFinishConfiguration,
        ),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::Login(login_packet()),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
        Some(256),
    );

    // Play -> Configuration -> Play again.
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::StartConfiguration(ClientboundStartConfiguration),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::ConfigurationAcknowledged(ServerboundConfigurationAcknowledged),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::KeepAlive(ClientboundKeepAlive { id: 3 }),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::FinishConfiguration(ClientboundFinishConfiguration),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &azalea_protocol::packets::config::ServerboundConfigPacket::FinishConfiguration(
            azalea_protocol::packets::config::ServerboundFinishConfiguration,
        ),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::Login(login_packet()),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
        Some(256),
    );

    builder.marker(Kind::Closed);
}

fn decoded(events: &[Event]) -> Vec<&crate::Decoded> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Decoded(decoded) => Some(decoded),
            _ => None,
        })
        .collect()
}

#[test]
fn session_states_and_reentry() {
    let mut builder = Builder::new();
    add_session(&mut builder);
    let (decoder, events) = builder.decode();

    assert!(!events.iter().any(|e| matches!(e, Event::Desync(_))), "{events:?}");
    let packets = decoded(&events);
    assert_eq!(packets.len(), 17);

    let state_of = |name: &str, dir: Dir| -> State {
        packets
            .iter()
            .find(|p| p.packet.name() == name && p.dir == dir)
            .unwrap_or_else(|| panic!("no {dir} {name}"))
            .state
    };
    assert_eq!(state_of("login_finished", Dir::Clientbound), State::Login);
    assert_eq!(state_of("login_acknowledged", Dir::Serverbound), State::Login);
    assert_eq!(state_of("finish_configuration", Dir::Clientbound), State::Configuration);
    assert_eq!(state_of("finish_configuration", Dir::Serverbound), State::Configuration);
    assert_eq!(state_of("configuration_acknowledged", Dir::Serverbound), State::Play);
    assert_eq!(state_of("start_configuration", Dir::Clientbound), State::Play);
    assert_eq!(state_of("client_tick_end", Dir::Serverbound), State::Play);

    // Re-entry: the second finish_configuration is in Configuration again.
    let finishes: Vec<&crate::Decoded> = packets
        .iter()
        .filter(|p| p.packet.name() == "finish_configuration" && p.dir == Dir::Clientbound)
        .copied()
        .collect();
    assert_eq!(finishes.len(), 2);
    assert_eq!(finishes[1].state, State::Configuration);

    let stats = decoder.stats();
    assert_eq!(stats[&0].packets, 17);
    assert!(stats[&0].frames >= 16);
    assert!(stats[&0].decode_failures.is_empty());
}

#[test]
fn compression_can_be_off() {
    let mut builder = Builder::new();
    builder.marker(Kind::Opened);
    builder.packet(
        Dir::Serverbound,
        &ServerboundHandshakePacket::Intention(ServerboundIntention {
            protocol_version: 777,
            hostname: "localhost".to_owned(),
            port: 25565,
            intention: ClientIntention::Login,
        }),
        None,
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundLoginPacket::Hello(ServerboundHello {
            name: "tester".to_owned(),
            profile_id: Default::default(),
        }),
        None,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundLoginPacket::LoginFinished(ClientboundLoginFinished {
            game_profile: Default::default(),
            session_id: Default::default(),
        }),
        None,
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundLoginPacket::LoginAcknowledged(ServerboundLoginAcknowledged),
        None,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::FinishConfiguration(ClientboundFinishConfiguration),
        None,
    );
    builder.packet(
        Dir::Serverbound,
        &azalea_protocol::packets::config::ServerboundConfigPacket::FinishConfiguration(
            azalea_protocol::packets::config::ServerboundFinishConfiguration,
        ),
        None,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::Login(login_packet()),
        None,
    );
    builder.marker(Kind::Closed);

    let (_, events) = builder.decode();
    assert!(!events.iter().any(|e| matches!(e, Event::Desync(_))), "{events:?}");
    assert_eq!(decoded(&events).len(), 7);
}

#[test]
fn gap_desyncs_and_no_packets_follow() {
    let mut builder = Builder::new();
    builder.marker(Kind::Opened);
    builder.packet(
        Dir::Serverbound,
        &ServerboundHandshakePacket::Intention(ServerboundIntention {
            protocol_version: 777,
            hostname: "localhost".to_owned(),
            port: 25565,
            intention: ClientIntention::Login,
        }),
        None,
    );
    builder.gap(1234);
    builder.packet(
        Dir::Serverbound,
        &ServerboundLoginPacket::Hello(ServerboundHello {
            name: "tester".to_owned(),
            profile_id: Default::default(),
        }),
        None,
    );

    let (_, events) = builder.decode();
    let desyncs: Vec<&crate::Desync> = events
        .iter()
        .filter_map(|event| match event {
            Event::Desync(desync) => Some(desync),
            _ => None,
        })
        .collect();
    assert_eq!(desyncs.len(), 1);
    assert!(desyncs[0].reason.contains("gap"), "{}", desyncs[0].reason);
    assert_eq!(decoded(&events).len(), 1, "only the handshake decoded");
}

#[test]
fn truncated_frame_at_close_is_a_desync() {
    let mut builder = Builder::new();
    builder.marker(Kind::Opened);
    // A frame that claims 10 bytes but only 3 arrive.
    builder.push_wire(Dir::Clientbound, &[10, 1, 2, 3]);
    builder.marker(Kind::Closed);

    let (_, events) = builder.decode();
    assert!(matches!(events[0], Event::Opened { .. }));
    assert!(matches!(events[1], Event::Desync(_)));
    assert!(matches!(events[2], Event::Closed { .. }));
    assert!(decoded(&events).is_empty());
}

#[test]
fn opened_starts_a_fresh_connection() {
    let mut builder = Builder::new();
    builder.marker(Kind::Opened);
    builder.gap(1);
    builder.marker(Kind::Opened);
    builder.packet(
        Dir::Serverbound,
        &ServerboundHandshakePacket::Intention(ServerboundIntention {
            protocol_version: 777,
            hostname: "localhost".to_owned(),
            port: 25565,
            intention: ClientIntention::Login,
        }),
        None,
    );

    let (_, events) = builder.decode();
    assert!(events.iter().any(|e| matches!(e, Event::Desync(_))));
    assert_eq!(decoded(&events).len(), 1, "the second session decodes again");
}

#[test]
fn swing_animation_decodes_corpus3_bytes() {
    // corpus3.prxcap has swing_animation frames `7b c4 1c 00 01 06`:
    // packet id 0x7b, entity id 3652 as a VarInt, main hand, whack, 6 ticks.
    use azalea_inventory::components::SwingAnimationKind;
    use azalea_protocol::packets::game::{ClientboundSwingAnimation, s_interact::InteractionHand};

    let raw = [0x7b, 0xc4, 0x1c, 0x00, 0x01, 0x06];
    let (packet, consumed) = crate::decode::decode(State::Play, Dir::Clientbound, &raw).unwrap();
    assert_eq!(consumed, raw.len());
    match packet {
        Packet::PlayClientbound(ClientboundGamePacket::SwingAnimation(animation)) => {
            assert_eq!(
                animation,
                ClientboundSwingAnimation {
                    entity_id: MinecraftEntityId(3652),
                    hand: InteractionHand::MainHand,
                    animation: azalea_inventory::components::SwingAnimation {
                        kind: SwingAnimationKind::Whack,
                        duration: 6,
                    },
                }
            );
        }
        other => panic!("expected swing_animation, got {}", other.name()),
    }
}

#[test]
fn swing_animation_encoder_matches_corpus3_frame() {
    // The encoder must write the same frame the 26.3 server writes: the entity
    // id is a VarInt. Default SwingAnimation is whack with a 6 tick duration.
    use azalea_protocol::packets::game::{ClientboundSwingAnimation, s_interact::InteractionHand};

    let packet = ClientboundSwingAnimation {
        entity_id: MinecraftEntityId(3652),
        hand: InteractionHand::MainHand,
        animation: Default::default(),
    };
    let wire = serialize_packet(&ClientboundGamePacket::SwingAnimation(packet.clone())).unwrap();
    assert_eq!(&wire[..], &[0x7b, 0xc4, 0x1c, 0x00, 0x01, 0x06]);

    let (decoded, consumed) = crate::decode::decode(State::Play, Dir::Clientbound, &wire).unwrap();
    assert_eq!(consumed, wire.len());
    match decoded {
        Packet::PlayClientbound(ClientboundGamePacket::SwingAnimation(round_trip)) => {
            assert_eq!(round_trip, packet);
        }
        other => panic!("expected swing_animation, got {}", other.name()),
    }
}

#[test]
fn empty_data_record_does_nothing() {
    let mut builder = Builder::new();
    builder.marker(Kind::Opened);
    builder.push_wire(Dir::Clientbound, &[]);
    let (_, events) = builder.decode();
    assert!(events.iter().all(|e| matches!(e, Event::Opened { .. })));
}

// --- Decompression (review findings 1, 3 and 4) ---------------------------

/// `Opened` and the packets up to the login compression packet, so a test can
/// append crafted frames that the connection will try to decompress.
fn compression_session(builder: &mut Builder, threshold: i32) {
    builder.marker(Kind::Opened);
    builder.packet(
        Dir::Serverbound,
        &ServerboundHandshakePacket::Intention(ServerboundIntention {
            protocol_version: 777,
            hostname: "localhost".to_owned(),
            port: 25565,
            intention: ClientIntention::Login,
        }),
        None,
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundLoginPacket::Hello(ServerboundHello {
            name: "tester".to_owned(),
            profile_id: Default::default(),
        }),
        None,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundLoginPacket::LoginCompression(ClientboundLoginCompression {
            compression_threshold: threshold,
        }),
        None,
    );
}

/// Wrap a frame body in its VarInt length prefix.
fn frame(body: &[u8]) -> Vec<u8> {
    let mut wire = Vec::new();
    (body.len() as u32)
        .azalea_write_var(&mut wire)
        .expect("write frame length");
    wire.extend_from_slice(body);
    wire
}

/// The bare zlib stream that Azalea's encoder produces for `data` (the data
/// length VarInt is stripped).
fn zlib_stream(data: &[u8]) -> Vec<u8> {
    let encoded = compression_encoder(data, 0).expect("compress");
    let mut index = 0;
    while index < encoded.len() && encoded[index] & 0x80 != 0 {
        index += 1;
    }
    encoded[index + 1..].to_vec()
}

/// A frame body that declares `declared` bytes but carries the zlib stream of
/// `data`.
fn lying_compressed_frame(declared: u32, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::new();
    declared
        .azalea_write_var(&mut body)
        .expect("write declared length");
    body.extend_from_slice(&zlib_stream(data));
    frame(&body)
}

fn desync_reasons(events: &[Event]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|event| match event {
            Event::Desync(desync) => Some(desync.reason.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn decompression_bomb_desyncs_with_bounded_output() {
    // A frame that declares 64 KiB but whose stream expands to four times that.
    // The inflater must stop at the declared size instead of expanding the
    // whole stream into memory, and the frame must desync rather than decode.
    let declared = 64 * 1024u32;
    let bomb = vec![0u8; 4 * declared as usize];
    let stream = zlib_stream(&bomb);
    assert!(stream.len() < bomb.len(), "the bomb compresses to {} bytes", stream.len());

    // The cap is what bounds the memory: the decoder reads at most
    // `declared + 1` bytes, so the 256 KiB the stream holds never fits.
    let mut builder = Builder::new();
    compression_session(&mut builder, 256);
    builder.push_wire(Dir::Clientbound, &lying_compressed_frame(declared, &bomb));
    let (decoder, events) = builder.decode();

    assert_eq!(desync_reasons(&events), vec!["decompressed size mismatch"]);
    assert_eq!(decoded(&events).len(), 3, "only the login packets decoded");
    let stats = &decoder.stats()[&0];
    assert_eq!(stats.decode_failures.values().sum::<u64>(), 1);
}

#[test]
fn decompressed_size_smaller_than_declared_is_a_size_mismatch() {
    let payload = vec![7u8; 4096];
    let mut builder = Builder::new();
    compression_session(&mut builder, 256);
    // Declares 5000 bytes, the stream holds 4096.
    builder.push_wire(Dir::Clientbound, &lying_compressed_frame(5000, &payload));
    let (_, events) = builder.decode();
    assert_eq!(desync_reasons(&events), vec!["decompressed size mismatch"]);
}

#[test]
fn negative_compression_threshold_desyncs() {
    // u32::try_from(-1) is None, so the old code turned compression off and then
    // read compressed frames as raw bytes. A negative threshold has no protocol
    // meaning we can honour: the connection stops instead.
    let mut builder = Builder::new();
    compression_session(&mut builder, -1);
    let (_, events) = builder.decode();
    let reasons = desync_reasons(&events);
    assert_eq!(reasons.len(), 1, "{events:?}");
    assert!(
        reasons[0].contains("negative compression threshold (-1)"),
        "{}",
        reasons[0]
    );
    // The login compression packet itself decoded; nothing after it can.
    assert_eq!(decoded(&events).len(), 3);
}

#[test]
fn unread_bytes_never_underflow() {
    // Finding 3: a frame that reports more consumed bytes than it has must not
    // panic in debug through an underflow, and a normal leftover counts.
    assert_eq!(crate::unread_bytes(10, 4), 6);
    assert_eq!(crate::unread_bytes(10, 10), 0);
    assert_eq!(crate::unread_bytes(10, 11), 0);
}

#[test]
fn trailing_bytes_in_a_frame_desync() {
    // A frame that carries bytes past its packet is a desync, never a guessed
    // packet. Azalea's own reader refuses the leftovers only in debug builds
    // (`#[cfg(debug_assertions)]` in azalea-protocol-macros); in release, the
    // profile the body runs, `unread_bytes` is the check that fires. Either
    // reason is the same observable outcome.
    let mut builder = Builder::new();
    builder.marker(Kind::Opened);
    let mut raw = serialize_packet(&ServerboundHandshakePacket::Intention(ServerboundIntention {
        protocol_version: 777,
        hostname: "localhost".to_owned(),
        port: 25565,
        intention: ClientIntention::Login,
    }))
    .expect("serialize")
    .to_vec();
    raw.extend_from_slice(&[0xaa, 0xbb]);
    builder.push_wire(Dir::Serverbound, &frame(&raw));

    let (_, events) = builder.decode();
    let reasons = desync_reasons(&events);
    assert_eq!(reasons.len(), 1, "{events:?}");
    let expected = if cfg!(debug_assertions) {
        "Leftover data"
    } else {
        "intention left 2 of 18 bytes unread"
    };
    assert!(reasons[0].contains(expected), "{}", reasons[0]);
    assert!(decoded(&events).is_empty(), "no packet is guessed from trailing bytes");
}

#[test]
fn inflate_round_trips_zlib_streams() {
    // zlib blocks come in three shapes: stored (incompressible noise), dynamic
    // Huffman (runs and text) and fixed Huffman (short inputs). All of them run
    // through our decompression path instead of Azalea's, so every shape is
    // pinned here, not only in the corpus tests.
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut noise = Vec::with_capacity(200_000);
    for _ in 0..200_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        noise.push(seed as u8);
    }
    let runs = vec![0x5au8; 150_000];
    let text = b"the quick brown fox jumps over the lazy dog. ".repeat(400);

    for payload in [noise, runs, text, b"tiny".to_vec()] {
        let encoded = compression_encoder(&payload, 1).expect("compress");
        let decoded = crate::decompress::decompress(&encoded, 1).expect("decompress");
        assert_eq!(decoded, payload);
    }

    // The zlib trailer is checked: a flipped byte in the Adler-32 is refused.
    let mut broken = compression_encoder(b"checksum me", 1).expect("compress");
    let last = broken.len() - 1;
    broken[last] ^= 0xff;
    assert!(crate::decompress::decompress(&broken, 1).is_err());
}
