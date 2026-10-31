//! End-to-end test: build a synthetic `*.prxcap` with Azalea's own packet
//! writers, split it into random TCP chunk sizes, and decode it again.

use std::sync::Arc;

use azalea_block::BlockState;
use azalea_buf::AzBuf;
use azalea_core::{
    entity_id::MinecraftEntityId,
    game_type::{GameMode, OptionalGameType},
    position::{BlockPos, ChunkPos, ChunkSectionBlockPos, ChunkSectionPos, Vec3},
};
use azalea_entity::LookDirection;
use azalea_protocol::{
    common::movements::PositionMoveRotation,
    packets::{
        ClientIntention, ProtocolPacket,
        common::CommonPlayerSpawnInfo,
        config::{
            ClientboundConfigPacket, ClientboundFinishConfiguration, ClientboundKeepAlive,
            ClientboundPing, ServerboundConfigPacket, ServerboundFinishConfiguration,
            ServerboundPong,
        },
        game::{
            ClientboundBlockUpdate, ClientboundCustomPayload, ClientboundForgetLevelChunk,
            ClientboundGamePacket, ClientboundLevelChunkWithLight, ClientboundLogin,
            ClientboundPlayerPosition, ClientboundSectionBlocksUpdate,
            ClientboundStartConfiguration, ServerboundClientTickEnd,
            ServerboundConfigurationAcknowledged, ServerboundGamePacket, ServerboundMovePlayerPos,
            ServerboundMovePlayerPosRot, ServerboundMovePlayerRot,
            ServerboundMovePlayerStatusOnly, ServerboundPlayerInput,
        },
        handshake::{ServerboundHandshakePacket, ServerboundIntention},
        login::{
            ClientboundLoginCompression, ClientboundLoginFinished, ClientboundLoginPacket,
            ServerboundHello, ServerboundLoginAcknowledged, ServerboundLoginPacket,
        },
    },
    write::{encode_to_network_packet, serialize_packet},
};
use azalea_protocol::packets::game::c_level_chunk_with_light::ClientboundLevelChunkPacketData;
use azalea_protocol::packets::game::c_section_blocks_update::BlockStateWithPosition;
use azalea_registry::{DataRegistry, data::DimensionKind, identifier::Identifier};
use azalea_world::Chunk;

use crate::analysis::Options;
use crate::capture::{Dir, MAGIC, Record};
use crate::world::BlockDescription;

struct Builder {
    records: Vec<Record>,
    ts: u64,
    rng: u64,
    payload_size: usize,
}

impl Builder {
    fn new() -> Self {
        Self {
            records: Vec::new(),
            ts: 1_700_000_000_000_000_000,
            rng: 0x2545_f491_4f6c_dd1d,
            payload_size: 0,
        }
    }

    fn next_random(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// Append one wire packet, split into random TCP chunk sizes. Every record
    /// of a packet carries the same timestamp; the decoder takes the timestamp
    /// of the record holding the last byte.
    fn push_wire(&mut self, dir: Dir, wire: &[u8]) {
        self.payload_size += wire.len();
        let mut offset = 0;
        while offset < wire.len() {
            let chunk = 1 + (self.next_random() % 40) as usize;
            let end = (offset + chunk).min(wire.len());
            let record = Record {
                ts_unix_ns: self.ts,
                conn_id: 0,
                dir,
                seq: self.records.len(),
                data: wire[offset..end].to_vec(),
            };
            self.records.push(record);
            offset = end;
        }
        self.ts += 1_000;
    }

    fn marker(&mut self, dir: Dir) {
        self.records.push(Record {
            ts_unix_ns: self.ts,
            conn_id: 0,
            dir,
            seq: self.records.len(),
            data: Vec::new(),
        });
        self.ts += 1_000;
    }

    fn packet<P: ProtocolPacket + std::fmt::Debug>(&mut self, dir: Dir, packet: &P, threshold: Option<u32>) {
        let raw = serialize_packet(packet).expect("serialize");
        let mut cipher = None;
        let wire = encode_to_network_packet(&raw, threshold, &mut cipher);
        self.push_wire(dir, &wire);
    }

    fn into_capture(self) -> Vec<u8> {
        let mut bytes = MAGIC.to_vec();
        for record in &self.records {
            bytes.extend_from_slice(&record.ts_unix_ns.to_le_bytes());
            bytes.extend_from_slice(&record.conn_id.to_le_bytes());
            bytes.push(record.dir.as_u8());
            bytes.extend_from_slice(&(record.data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&record.data);
        }
        bytes
    }
}

fn find_block(name: &str) -> BlockState {
    for id in 0..4096u32 {
        if let Ok(state) = BlockState::try_from(id) {
            if BlockDescription::of(state).name == name {
                return state;
            }
        }
    }
    panic!("block {name} not found in the block state table");
}

fn default_spawn_info(dimension: &str) -> CommonPlayerSpawnInfo {
    CommonPlayerSpawnInfo {
        dimension_type: DimensionKind::new_raw(0),
        dimension: Identifier::new(dimension),
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
        levels: vec![],
        max_players: 20,
        chunk_radius: 8,
        simulation_distance: 8,
        reduced_debug_info: false,
        show_death_screen: true,
        do_limited_crafting: false,
        common: default_spawn_info("minecraft:overworld"),
        online_mode: false,
        enforces_secure_chat: false,
    }
}

fn chunk_packet(x: i32, z: i32) -> ClientboundLevelChunkWithLight {
    let chunk = Chunk::default();
    let mut buffer = Vec::new();
    for section in &chunk.sections {
        section.azalea_write(&mut buffer).expect("write section");
    }
    ClientboundLevelChunkWithLight {
        x,
        z,
        chunk_data: ClientboundLevelChunkPacketData {
            heightmaps: vec![],
            buffer: Arc::new(buffer.into_boxed_slice()),
            block_entities: vec![],
        },
        light_data: Default::default(),
    }
}

fn player_position() -> ClientboundPlayerPosition {
    ClientboundPlayerPosition {
        id: 1,
        change: PositionMoveRotation {
            pos: Vec3::new(8.5, 64.0, 8.5),
            delta: Vec3::ZERO,
            look_direction: Default::default(),
        },
        relative: Default::default(),
    }
}

fn dirt_section_update(dirt: BlockState) -> ClientboundSectionBlocksUpdate {
    ClientboundSectionBlocksUpdate {
        section_pos: ChunkSectionPos { x: 0, y: 4, z: 0 },
        states: vec![BlockStateWithPosition {
            pos: ChunkSectionBlockPos { x: 2, y: 0, z: 0 },
            state: dirt,
        }],
    }
}

/// A whole session: handshake, login (compression 256), configuration, game, a
/// Play -> Configuration re-entry and back to Play.
fn build_session() -> (Vec<u8>, usize) {
    let stone = find_block("minecraft:stone");
    let dirt = find_block("minecraft:dirt");

    let mut builder = Builder::new();
    builder.marker(Dir::Opened);

    // Handshake -> login.
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

    // Login.
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

    // Configuration.
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::KeepAlive(ClientboundKeepAlive { id: 1 }),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundConfigPacket::Pong(ServerboundPong { id: 1 }),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::Ping(ClientboundPing { id: 2 }),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::FinishConfiguration(ClientboundFinishConfiguration),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundConfigPacket::FinishConfiguration(ServerboundFinishConfiguration),
        Some(256),
    );

    // Game.
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::Login(login_packet()),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos: BlockPos::new(0, 64, 0),
            block_state: stone,
        }),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::SectionBlocksUpdate(dirt_section_update(dirt)),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::PlayerPosition(player_position()),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: Vec3::new(8.6, 64.0, 8.5),
            flags: Default::default(),
        }),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::PlayerInput(ServerboundPlayerInput {
            forward: true,
            backward: false,
            left: false,
            right: true,
            jump: true,
            shift: false,
            sprint: true,
        }),
        Some(256),
    );

    // Above the compression threshold: a 600-byte payload (serialized > 256),
    // so this packet must arrive zlib-compressed.
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::CustomPayload(ClientboundCustomPayload {
            identifier: Identifier::new("minecraft:brand"),
            data: vec![0x5A; 600].into(),
        }),
        Some(256),
    );

    // Play -> configuration -> play again.
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
        &ServerboundConfigPacket::FinishConfiguration(ServerboundFinishConfiguration),
        Some(256),
    );

    // Game again. The chunk is reloaded (so earlier block updates are wiped),
    // then the same edits are applied; chunk (4,4) is loaded and forgotten.
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::Login(login_packet()),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(4, 4)),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos: BlockPos::new(0, 64, 0),
            block_state: stone,
        }),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::SectionBlocksUpdate(dirt_section_update(dirt)),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::MovePlayerPosRot(ServerboundMovePlayerPosRot {
            pos: Vec3::new(8.6, 64.0, 8.5),
            look_direction: LookDirection::new(90.0, -12.5),
            flags: Default::default(),
        }),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::MovePlayerRot(ServerboundMovePlayerRot {
            look_direction: LookDirection::new(-45.5, 33.0),
            flags: Default::default(),
        }),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::MovePlayerStatusOnly(ServerboundMovePlayerStatusOnly {
            flags: Default::default(),
        }),
        Some(256),
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
        Some(256),
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::ForgetLevelChunk(ClientboundForgetLevelChunk {
            pos: ChunkPos::new(4, 4),
        }),
        Some(256),
    );
    builder.marker(Dir::Closed);

    let payload = builder.payload_size;
    (builder.into_capture(), payload)
}

#[test]
fn synthetic_session_decodes_with_zero_errors() {
    let (capture, payload_size) = build_session();
    let options = Options {
        blocks: vec![
            BlockPos::new(0, 64, 0),
            BlockPos::new(2, 64, 0),
            BlockPos::new(0, 0, 0),
            BlockPos::new(4 * 16, 64, 4 * 16),
        ],
        dump: None,
    };
    let analysis = crate::analyze_bytes("synthetic.prxcap", &capture, &options).expect("decode");

    let text = analysis.text_report();
    assert!(
        analysis.passed(),
        "expected PASS, got FAIL\nerrors={} unconsumed={}\n{text}",
        analysis.total_errors,
        analysis.unconsumed_packets
    );
    assert_eq!(analysis.total_errors, 0, "{text}");
    assert_eq!(analysis.unconsumed_packets, 0, "{text}");
    assert!(text.ends_with("E2 RESULT: PASS\n"), "{text}");
    assert!(payload_size > 600, "the synthetic session is big enough");

    // Every packet was framed and decoded; the opened/closed markers are not
    // packets.
    let expected = 34;
    assert_eq!(analysis.total_packets, expected, "{text}");
    assert_eq!(analysis.packets.len() as u64, expected);

    // State transitions happen per direction at the packet boundary.
    let state_of = |name: &str, dir: Dir| -> crate::State {
        analysis
            .packets
            .iter()
            .find(|p| p.name == name && p.dir == dir && p.error.is_none())
            .unwrap_or_else(|| panic!("no {dir} {name} packet decoded"))
            .state
    };
    assert_eq!(
        state_of("login_finished", Dir::Clientbound),
        crate::State::Login
    );
    assert_eq!(
        state_of("login_acknowledged", Dir::Serverbound),
        crate::State::Login
    );
    assert_eq!(
        state_of("finish_configuration", Dir::Clientbound),
        crate::State::Configuration
    );
    assert_eq!(
        state_of("finish_configuration", Dir::Serverbound),
        crate::State::Configuration
    );
    assert_eq!(
        state_of("configuration_acknowledged", Dir::Serverbound),
        crate::State::Game
    );
    assert_eq!(
        state_of("move_player_pos", Dir::Serverbound),
        crate::State::Game
    );

    // Frames were processed in timestamp order across both directions.
    let index = |name: &str, dir: Dir| -> usize {
        analysis
            .packets
            .iter()
            .position(|p| p.name == name && p.dir == dir && p.error.is_none())
            .unwrap()
    };
    assert!(
        index("login_finished", Dir::Clientbound)
            < index("login_acknowledged", Dir::Serverbound)
    );
    assert!(
        index("login_acknowledged", Dir::Serverbound)
            < index("keep_alive", Dir::Clientbound)
    );

    // Group counts.
    let login_cb = analysis
        .groups
        .get(&(crate::State::Login, Dir::Clientbound))
        .expect("login clientbound group");
    assert_eq!(login_cb.counts.get("login_compression"), Some(&1));
    assert_eq!(login_cb.counts.get("login_finished"), Some(&1));

    let game_cb = analysis
        .groups
        .get(&(crate::State::Game, Dir::Clientbound))
        .expect("game clientbound group");
    assert_eq!(game_cb.counts.get("level_chunk_with_light"), Some(&3));
    assert_eq!(game_cb.counts.get("block_update"), Some(&2));

    // World model: block updates landed, untouched blocks are air, and the
    // forgotten chunk is gone.
    let block = |index: usize| analysis.blocks[index].block.as_ref();
    assert_eq!(
        block(0).map(BlockDescription::display).as_deref(),
        Some("minecraft:stone")
    );
    assert_eq!(block(1).map(|b| b.name.as_str()), Some("minecraft:dirt"));
    assert_eq!(block(2).map(|b| b.name.as_str()), Some("minecraft:air"));
    assert_eq!(
        block(3),
        None,
        "chunk (4,4) was forgotten before the end of the capture"
    );

    // E3/E4 samples.
    assert!(analysis.e3.decode_all.count > 0);
    assert!(analysis.e3.decode_game_chunk.count > 0);
    assert_eq!(analysis.e3.apply_chunk.count, 3);
    assert!(analysis.e3.apply_block.count >= 3);
    let moves = analysis.e4.per_name.get("move_player_pos").unwrap();
    assert_eq!(moves.count, 1);
    let combined = &analysis.e4.all_moves;
    assert_eq!(combined.count, 4);
    assert_eq!(
        combined.gaps.b0_10, 3,
        "packets are 1 us apart in the synthetic capture"
    );

    // JSON has the same result.
    let json = analysis.json().render();
    assert!(json.contains("\"e2_result\": \"PASS\""), "{json}");
    assert!(json.contains("\"unconsumed_packets\": 0"), "{json}");

    // The 600-byte payload really was compressed on the wire.
    let custom = analysis
        .packets
        .iter()
        .find(|p| p.name == "custom_payload" && p.dir == Dir::Clientbound)
        .unwrap();
    assert!(
        custom.wire_size < custom.size,
        "custom_payload wire {} should be smaller than packet {}",
        custom.wire_size,
        custom.size
    );
}

#[test]
fn json_strings_are_escaped() {
    // Quotes, backslashes, the shortcuts and a raw control character must all
    // produce a valid JSON string literal. Non-ASCII stays valid UTF-8.
    let escaped = crate::dump::escape_json("a\"b\\c\nd\te\rf\u{08}g\u{0c}h\u{1f}é");
    assert_eq!(
        escaped,
        "\"a\\\"b\\\\c\\nd\\te\\rf\\bg\\fh\\u001f\u{e9}\""
    );

    let plain = crate::dump::escape_json("player_input");
    assert_eq!(plain, "\"player_input\"");
}

#[test]
fn dump_contains_expected_synthetic_lines() {
    let (capture, _) = build_session();
    let filter = crate::DumpFilter::parse(
        "player_input,client_tick_end,move_player_rot,move_player_pos_rot",
    )
    .unwrap();
    let options = Options {
        blocks: Vec::new(),
        dump: Some(filter),
    };
    let analysis = crate::analyze_bytes("synthetic.prxcap", &capture, &options).expect("decode");
    assert!(analysis.passed(), "{}", analysis.text_report());

    let lines: Vec<String> = analysis
        .packets
        .iter()
        .filter(|record| record.dump.is_some())
        .map(crate::dump::render_line)
        .collect();
    assert_eq!(
        lines.len(),
        5,
        "2 client_tick_end + player_input + 2 rotation packets"
    );
    for line in &lines {
        assert!(line.starts_with('{') && line.ends_with('}'), "{line}");
        assert!(line.contains("\"state\": \"game\""), "{line}");
    }

    let line_named = |name: &str| -> &String {
        lines
            .iter()
            .find(|line| line.contains(&format!("\"name\": \"{name}\"")))
            .unwrap_or_else(|| panic!("no dump line for {name}"))
    };

    let input = line_named("player_input");
    for (field, expected) in [
        ("forward", "true"),
        ("backward", "false"),
        ("left", "false"),
        ("right", "true"),
        ("jump", "true"),
        ("shift", "false"),
        ("sprint", "true"),
    ] {
        assert!(
            input.contains(&format!("\"{field}\": {expected}")),
            "player_input line is missing {field}={expected}: {input}"
        );
    }

    // The values come from the Azalea struct, so the non-default look
    // directions set in `build_session` must show up unchanged.
    let rot = line_named("move_player_rot");
    assert!(rot.contains("\"yaw\": -45.5"), "{rot}");
    assert!(rot.contains("\"pitch\": 33"), "{rot}");
    let pos_rot = line_named("move_player_pos_rot");
    assert!(pos_rot.contains("\"yaw\": 90"), "{pos_rot}");
    assert!(pos_rot.contains("\"pitch\": -12.5"), "{pos_rot}");
    assert!(pos_rot.contains("\"dir\": \"serverbound\""), "{pos_rot}");

    // seq counts every packet of that (conn, dir) stream, so the two
    // client_tick_end lines have increasing indices and the game packets come
    // after the login/handshake ones.
    let seq_of = |line: &str| -> u64 {
        let start = line.find("\"seq\": ").unwrap() + "\"seq\": ".len();
        let end = start + line[start..].find(',').unwrap();
        line[start..end].parse().unwrap()
    };
    let ticks: Vec<&String> = lines
        .iter()
        .filter(|line| line.contains("\"name\": \"client_tick_end\""))
        .collect();
    assert_eq!(ticks.len(), 2);
    assert!(seq_of(ticks[0]) < seq_of(ticks[1]));
    assert!(seq_of(ticks[0]) >= 3, "handshake and login came first");

    // Packets outside the filter carry no dump payload at all.
    assert!(
        analysis
            .packets
            .iter()
            .filter(|record| record.name == "move_player_pos")
            .all(|record| record.dump.is_none())
    );

    // `*` selects everything that decoded, including clientbound packets.
    let all_options = Options {
        blocks: Vec::new(),
        dump: Some(crate::DumpFilter::parse("*").unwrap()),
    };
    let all = crate::analyze_bytes("synthetic.prxcap", &capture, &all_options).expect("decode");
    assert_eq!(
        all.packets.iter().filter(|r| r.dump.is_some()).count(),
        34
    );
    assert!(all.packets.iter().any(|r| {
        r.dir == Dir::Clientbound && r.name == "level_chunk_with_light" && r.dump.is_some()
    }));
}

#[test]
fn truncated_frame_is_reported() {
    let (capture, _) = build_session();
    let cut = capture.len() * 3 / 4;
    let analysis =
        crate::analyze_bytes("cut.prxcap", &capture[..cut], &Options::default()).expect("decode");
    assert!(!analysis.passed());
    assert!(
        !analysis.framing_issues.is_empty() || !analysis.format_errors.is_empty(),
        "a cut capture must report why it stopped"
    );
    assert!(analysis.text_report().contains("E2 RESULT: FAIL"));
}

#[test]
fn bad_header_is_rejected() {
    let error =
        crate::analyze_bytes("x.prxcap", b"not a capture", &Options::default()).unwrap_err();
    assert!(error.contains("PRXCAP1"), "{error}");
}

#[test]
fn empty_capture_passes_vacuously() {
    let analysis = crate::analyze_bytes("empty.prxcap", MAGIC, &Options::default()).unwrap();
    assert!(analysis.passed());
    assert_eq!(analysis.total_packets, 0);
}

fn from_hex(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn raw_login_bytes_decode_to_configuration() {
    // The first frames of a real 26.3 connection, copied from
    // spikes/captures/corpus1.prxcap connection 0 (outer VarInt lengths included),
    // split into small TCP chunks.
    let frames: &[(u8, &str)] = &[
        (0, "10008906096c6f63616c686f737463dd02"),
        (0, "19000750726178696f6e00000000000000000000000000000000"),
        (1, "03038002"),
        (
            1,
            "2b0002ba3cc7fd45363711b7e75469cc4c0d620750726178696f6e004ed32372492a4042bdbe1a6fea5b3cc5",
        ),
        (0, "020003"),
        (0, "1a00020f6d696e6563726166743a6272616e640776616e696c6c61"),
        (1, "1a00010f6d696e6563726166743a6272616e640776616e696c6c61"),
    ];

    let mut bytes = MAGIC.to_vec();
    let mut ts = 1_700_000_000_000_000_000u64;
    let mut seq = 0usize;
    for (dir_raw, hex) in frames {
        let data = from_hex(hex);
        let dir = if *dir_raw == 0 {
            Dir::Serverbound
        } else {
            Dir::Clientbound
        };
        // Split into small chunks to exercise framing.
        let mut offset = 0;
        while offset < data.len() {
            let end = (offset + 5).min(data.len());
            bytes.extend_from_slice(&ts.to_le_bytes());
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.push(dir.as_u8());
            bytes.extend_from_slice(&((end - offset) as u32).to_le_bytes());
            bytes.extend_from_slice(&data[offset..end]);
            seq += 1;
            offset = end;
        }
        ts += 1_000;
    }
    // seq unused but kept for clarity
    let _ = seq;

    let analysis =
        crate::analyze_bytes("raw-login.prxcap", &bytes, &Options::default()).expect("decode");

    assert!(
        analysis.passed(),
        "expected PASS, got FAIL\nerrors={} unconsumed={}\n{}",
        analysis.total_errors,
        analysis.unconsumed_packets,
        analysis.text_report()
    );

    let state_of = |name: &str, dir: Dir| -> crate::State {
        analysis
            .packets
            .iter()
            .find(|p| p.name == name && p.dir == dir && p.error.is_none())
            .unwrap_or_else(|| panic!("no {dir} {name} packet decoded"))
            .state
    };
    assert_eq!(
        state_of("login_compression", Dir::Clientbound),
        crate::State::Login
    );
    assert_eq!(
        state_of("login_finished", Dir::Clientbound),
        crate::State::Login
    );

    // After login_finished + login_acknowledged we must be in configuration.
    let has_config_packet = analysis.packets.iter().any(|p| {
        p.dir == Dir::Clientbound && p.state == crate::State::Configuration && p.error.is_none()
    });
    assert!(
        has_config_packet,
        "expected at least one clientbound packet decoded in configuration state"
    );
}
