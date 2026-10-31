//! The offline decoder: framing + protocol state machine + world updates.
//!
//! One pass over the capture, per connection, with the frames of both
//! directions merged in global timestamp order (ties keep file order).
//! Everything the reports need is collected here; `report.rs` renders it.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::time::Instant;

use azalea_buf::AzBufVar;
use azalea_core::{
    position::{BlockPos, ChunkPos, Vec3},
    registry_holder::RegistryHolder,
};
use azalea_protocol::{
    packets::{
        ClientIntention, ProtocolPacket,
        common::CommonPlayerSpawnInfo,
        config::{ClientboundConfigPacket, ClientboundRegistryData, ServerboundConfigPacket},
        game::{
            ClientboundGamePacket, ClientboundLevelChunkWithLight,
            ClientboundPlayerPosition, ServerboundGamePacket,
        },
        handshake::ServerboundHandshakePacket,
        login::{ClientboundLoginPacket, ServerboundLoginPacket},
        status::{ClientboundStatusPacket, ServerboundStatusPacket},
    },
    read::compression_decoder,
};
use azalea_registry::DataRegistry;

use azalea_protocol::packets::game::c_section_blocks_update::BlockStateWithPosition;

use crate::capture::{Capture, Dir, Record};
use crate::dump::{DumpExtras, DumpFilter, PacketDump, PlayerInput};
use crate::framing::{self, Frame};
use crate::world::{BlockDescription, WorldModel};

/// The protocol state a packet was decoded in.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum State {
    Handshake,
    Status,
    Login,
    Configuration,
    Game,
}

impl State {
    pub const ALL: [State; 5] = [
        State::Handshake,
        State::Status,
        State::Login,
        State::Configuration,
        State::Game,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            State::Handshake => "handshake",
            State::Status => "status",
            State::Login => "login",
            State::Configuration => "configuration",
            State::Game => "game",
        }
    }
}

/// Packet names that E3 reports separately from steady-state packets.
pub const CHUNK_PACKET_NAMES: [&str; 3] = [
    "level_chunk_with_light",
    "chunk_batch_start",
    "chunk_batch_finished",
];

/// The serverbound packets E4 counts, in report order.
pub const MOVE_PACKET_NAMES: [&str; 5] = [
    "move_player_pos",
    "move_player_pos_rot",
    "move_player_rot",
    "move_player_status_only",
    "client_tick_end",
];

pub fn is_chunk_packet(name: &str) -> bool {
    CHUNK_PACKET_NAMES.contains(&name)
}

#[derive(Clone, Debug)]
pub struct PacketRecord {
    pub conn_id: u32,
    pub state: State,
    pub dir: Dir,
    pub id: u32,
    pub name: String,
    /// Decompressed packet size in bytes (packet id included).
    pub size: usize,
    /// Frame body size on the wire after the length prefix (maybe compressed).
    pub wire_size: usize,
    pub bytes_left: usize,
    pub error: Option<String>,
    pub ts_unix_ns: u64,
    /// Index of this packet within its `(conn_id, dir)` stream, from 0.
    pub stream_index: u64,
    pub decode_ns: u64,
    pub apply_ns: Option<u64>,
    /// Set only when `--dump` selected this packet.
    pub dump: Option<PacketDump>,
}

#[derive(Clone, Debug)]
pub struct ErrorExample {
    pub conn_id: u32,
    pub id: u32,
    pub name: String,
    pub error: String,
    pub bytes_left: usize,
}

#[derive(Clone, Debug, Default)]
pub struct GroupStats {
    pub counts: BTreeMap<String, u64>,
    pub errors: u64,
    pub unconsumed: u64,
    pub examples: Vec<ErrorExample>,
}

impl GroupStats {
    pub fn packets(&self) -> u64 {
        self.counts.values().sum()
    }
}

#[derive(Clone, Debug, Default)]
pub struct PercentilesUs {
    pub count: usize,
    pub p50: f64,
    pub p99: f64,
    pub p99_9: f64,
    pub max: f64,
}

#[derive(Clone, Debug, Default)]
pub struct E3Stats {
    pub decode_all: PercentilesUs,
    pub decode_game_steady: PercentilesUs,
    pub decode_game_chunk: PercentilesUs,
    pub apply_chunk: PercentilesUs,
    pub apply_block: PercentilesUs,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GapBins {
    pub b0_10: u64,
    pub b10_40: u64,
    pub b40_60: u64,
    pub b60_100: u64,
    pub b100_1000: u64,
    pub b_gt1000: u64,
}

impl GapBins {
    fn add(&mut self, gap_ms: f64) {
        if gap_ms < 10.0 {
            self.b0_10 += 1;
        } else if gap_ms < 40.0 {
            self.b10_40 += 1;
        } else if gap_ms < 60.0 {
            self.b40_60 += 1;
        } else if gap_ms < 100.0 {
            self.b60_100 += 1;
        } else if gap_ms < 1000.0 {
            self.b100_1000 += 1;
        } else {
            self.b_gt1000 += 1;
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MoveStats {
    pub count: u64,
    pub per_second: f64,
    pub gaps: GapBins,
}

#[derive(Clone, Debug, Default)]
pub struct E4Stats {
    /// Duration of the game state in the capture, seconds.
    pub game_duration_s: f64,
    pub per_name: BTreeMap<String, MoveStats>,
    /// The four pose packets together (`client_tick_end` excluded: it is a tick
    /// marker, not a pose report).
    pub all_moves: MoveStats,
}

#[derive(Clone, Debug)]
pub struct DimensionInfo {
    pub dimension: String,
    pub dimension_type: String,
    /// `registry` when the dimension type came from `registry_data`,
    /// `fallback` when the capture did not carry the registry entry.
    pub source: &'static str,
    pub height: u32,
    pub min_y: i32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct PlayerPos {
    pub pos: Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Debug)]
pub struct ConnectionSummary {
    pub conn_id: u32,
    pub dimension: Option<DimensionInfo>,
    pub chunks_loaded: usize,
    pub player: Option<PlayerPos>,
    pub notes: Vec<String>,
    pub stopped: Option<String>,
    /// Timestamp of the last frame processed for this connection, any state.
    pub last_ts_unix_ns: u64,
    /// Whether this connection ever entered the game state.
    pub reached_game: bool,
}

#[derive(Clone, Debug)]
pub struct BlockAnswer {
    pub pos: BlockPos,
    pub conn_id: Option<u32>,
    pub block: Option<BlockDescription>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct RecordCounts {
    pub serverbound: usize,
    pub clientbound: usize,
    pub opened: usize,
    pub closed: usize,
}

#[derive(Clone, Debug)]
pub struct Analysis {
    pub capture_name: String,
    pub records: RecordCounts,
    pub connections: Vec<ConnectionSummary>,
    pub groups: BTreeMap<(State, Dir), GroupStats>,
    pub total_packets: u64,
    pub total_errors: u64,
    pub unconsumed_packets: u64,
    pub framing_issues: Vec<framing::FramingIssue>,
    pub format_errors: Vec<String>,
    pub packets: Vec<PacketRecord>,
    pub e3: E3Stats,
    pub e4: E4Stats,
    pub blocks: Vec<BlockAnswer>,
    /// The connection whose world answers `--block` and the end-of-capture
    /// player summary: the game connection that ended last.
    pub end_world_conn: Option<u32>,
}

impl Analysis {
    pub fn passed(&self) -> bool {
        self.total_errors == 0 && self.unconsumed_packets == 0
    }

    pub fn result_line(&self) -> &'static str {
        if self.passed() {
            "E2 RESULT: PASS"
        } else {
            "E2 RESULT: FAIL"
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub blocks: Vec<BlockPos>,
    /// When set, matching packets also get a `PacketDump` for `--dump`.
    pub dump: Option<DumpFilter>,
}

/// Decode a whole capture.
pub fn analyze(capture_name: &str, capture: &Capture, options: &Options) -> Analysis {
    let mut analysis = Analysis {
        capture_name: capture_name.to_owned(),
        records: RecordCounts {
            serverbound: capture.count(Dir::Serverbound),
            clientbound: capture.count(Dir::Clientbound),
            opened: capture.count(Dir::Opened),
            closed: capture.count(Dir::Closed),
        },
        connections: Vec::new(),
        groups: BTreeMap::new(),
        total_packets: 0,
        total_errors: 0,
        unconsumed_packets: 0,
        framing_issues: Vec::new(),
        format_errors: Vec::new(),
        packets: Vec::new(),
        e3: E3Stats::default(),
        e4: E4Stats::default(),
        blocks: Vec::new(),
        end_world_conn: None,
    };

    if let Some(offset) = capture.truncated_at {
        analysis.format_errors.push(format!(
            "capture ends inside a record at byte {offset}; the records before it were decoded"
        ));
    }

    // 1. Rebuild one byte stream per (connection, direction) and frame it.
    let mut by_stream: BTreeMap<(u32, Dir), Vec<&Record>> = BTreeMap::new();
    for record in &capture.records {
        if record.dir.is_data() {
            by_stream
                .entry((record.conn_id, record.dir))
                .or_default()
                .push(record);
        }
    }

    let mut frames: Vec<Frame> = Vec::new();
    for ((conn_id, dir), records) in &by_stream {
        let (stream_frames, issue) = framing::split_direction(*conn_id, *dir, records);
        frames.extend(stream_frames);
        if let Some(issue) = issue {
            analysis.framing_issues.push(issue);
        }
    }

    // 2. Merge both directions in global timestamp order; ties keep file order.
    frames.sort_by_key(|f| (f.conn_id, f.ts_unix_ns, f.seq, f.end_offset));

    // 3. Run one state machine per connection.
    let mut timings = Timings::default();
    let mut worlds: Vec<(u32, WorldModel)> = Vec::new();
    for conn_id in capture.conn_ids() {
        let conn_frames: Vec<&Frame> = frames.iter().filter(|f| f.conn_id == conn_id).collect();
        let mut conn = ConnectionState::new(conn_id);

        for frame in conn_frames {
            conn.process_frame(frame, &mut analysis, &mut timings, options);
        }

        let world = conn.world.take();
        let chunks_loaded = world.as_ref().map(WorldModel::chunk_count).unwrap_or(0);
        if let Some(world) = world {
            worlds.push((conn_id, world));
        }
        let mut summary = conn.summary();
        summary.chunks_loaded = chunks_loaded;
        analysis.connections.push(summary);
    }

    // 4. Pick the world that is live at the end of the capture: the game
    // connection whose last frame has the newest timestamp. A `/transfer`
    // starts a second game connection, and the first one's world is stale by
    // then. Ties go to the higher connection id.
    let end_world_conn = analysis
        .connections
        .iter()
        .filter(|connection| connection.reached_game)
        .filter(|connection| {
            worlds
                .iter()
                .any(|(conn_id, _)| *conn_id == connection.conn_id)
        })
        .max_by_key(|connection| (connection.last_ts_unix_ns, connection.conn_id))
        .map(|connection| connection.conn_id);
    analysis.end_world_conn = end_world_conn;
    let end_world = end_world_conn.and_then(|conn_id| {
        worlds
            .iter()
            .find(|(world_conn_id, _)| *world_conn_id == conn_id)
            .map(|(_, world)| (conn_id, world))
    });

    // 5. Answer --block queries against that world at the end of the capture.
    for pos in &options.blocks {
        let mut answer = BlockAnswer {
            pos: *pos,
            conn_id: None,
            block: None,
            note: Some("no connection decoded a game world".to_owned()),
        };
        if let Some((conn_id, world)) = end_world {
            match world.block_state(*pos) {
                Some(state) => {
                    answer.conn_id = Some(conn_id);
                    answer.block = Some(BlockDescription::of(state));
                    answer.note = None;
                }
                None => {
                    let chunk = ChunkPos::from(*pos);
                    answer.note = Some(if world.has_chunk(chunk) {
                        format!(
                            "connection {conn_id}: y={} is outside the world ({}..{})",
                            pos.y,
                            world.min_y,
                            world.min_y + world.height as i32
                        )
                    } else {
                        format!("connection {conn_id}: chunk {chunk:?} was not loaded at the end")
                    });
                }
            }
        }
        analysis.blocks.push(answer);
    }

    analysis.e3 = timings.finish_e3();
    analysis.e4 = timings.finish_e4();
    analysis.total_packets = analysis.packets.len() as u64;
    analysis.total_errors += analysis.framing_issues.len() as u64;
    analysis.total_errors += analysis.format_errors.len() as u64;
    analysis
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

struct ConnectionState {
    conn_id: u32,
    sb_state: State,
    cb_state: State,
    /// Next packet index for each direction's stream.
    sb_stream_index: u64,
    cb_stream_index: u64,
    compression: Option<u32>,
    stopped: Option<String>,
    registries: RegistryHolder,
    world: Option<WorldModel>,
    dimension: Option<DimensionInfo>,
    player: Option<PlayerPos>,
    notes: Vec<String>,
    last_ts_unix_ns: u64,
    reached_game: bool,
}

impl ConnectionState {
    fn new(conn_id: u32) -> Self {
        Self {
            conn_id,
            sb_state: State::Handshake,
            cb_state: State::Handshake,
            sb_stream_index: 0,
            cb_stream_index: 0,
            compression: None,
            stopped: None,
            registries: RegistryHolder::default(),
            world: None,
            dimension: None,
            player: None,
            notes: Vec::new(),
            last_ts_unix_ns: 0,
            reached_game: false,
        }
    }

    fn next_stream_index(&mut self, dir: Dir) -> u64 {
        let slot = if dir == Dir::Serverbound {
            &mut self.sb_stream_index
        } else {
            &mut self.cb_stream_index
        };
        let index = *slot;
        *slot += 1;
        index
    }

    fn process_frame(
        &mut self,
        frame: &Frame,
        analysis: &mut Analysis,
        timings: &mut Timings,
        options: &Options,
    ) {
        let state = if frame.dir == Dir::Serverbound {
            self.sb_state
        } else {
            self.cb_state
        };
        self.last_ts_unix_ns = self.last_ts_unix_ns.max(frame.ts_unix_ns);
        let stream_index = self.next_stream_index(frame.dir);
        let group = analysis.groups.entry((state, frame.dir)).or_default();

        if let Some(reason) = &self.stopped {
            let message = format!("connection abandoned: {reason}");
            analysis.total_errors += 1;
            group.errors += 1;
            push_example(group, self.conn_id, u32::MAX, "<skipped>", &message, 0);
            analysis.packets.push(PacketRecord {
                conn_id: self.conn_id,
                state,
                dir: frame.dir,
                id: u32::MAX,
                name: "<skipped>".to_owned(),
                size: 0,
                wire_size: frame.wire.len(),
                bytes_left: 0,
                error: Some(message),
                ts_unix_ns: frame.ts_unix_ns,
                stream_index,
                decode_ns: 0,
                apply_ns: None,
                dump: None,
            });
            return;
        }

        // Decompress. Compression is set by `login_compression` and applies to
        // both directions from the next frame on.
        let wire_size = frame.wire.len();
        let raw = match self.compression {
            None => frame.wire.clone(),
            Some(threshold) => match compression_decoder(&mut Cursor::new(&frame.wire), threshold) {
                Ok(raw) => raw.into_vec(),
                Err(e) => {
                    let message = format!("decompression failed: {e}");
                    analysis.total_errors += 1;
                    group.errors += 1;
                    push_example(
                        group,
                        self.conn_id,
                        u32::MAX,
                        "<decompress>",
                        &message,
                        wire_size,
                    );
                    analysis.packets.push(PacketRecord {
                        conn_id: self.conn_id,
                        state,
                        dir: frame.dir,
                        id: u32::MAX,
                        name: "<decompress>".to_owned(),
                        size: wire_size,
                        wire_size,
                        bytes_left: wire_size,
                        error: Some(message),
                        ts_unix_ns: frame.ts_unix_ns,
                        stream_index,
                        decode_ns: 0,
                        apply_ns: None,
                        dump: None,
                    });
                    return;
                }
            },
        };

        let start = Instant::now();
        let decoded = decode_packet(state, frame.dir, &raw, options.dump.as_ref());
        let decode_ns = start.elapsed().as_nanos() as u64;
        timings.record_decode(decode_ns, state, decoded.as_ref().ok());

        let decoded = match decoded {
            Ok(decoded) => decoded,
            Err(failure) => {
                analysis.total_errors += 1;
                group.errors += 1;
                push_example(
                    group,
                    self.conn_id,
                    failure.id,
                    &failure.name,
                    &failure.message,
                    failure.bytes_left,
                );
                if failure.bytes_left > 0 {
                    group.unconsumed += 1;
                    analysis.unconsumed_packets += 1;
                }
                analysis.packets.push(PacketRecord {
                    conn_id: self.conn_id,
                    state,
                    dir: frame.dir,
                    id: failure.id,
                    name: failure.name,
                    size: raw.len(),
                    wire_size,
                    bytes_left: failure.bytes_left,
                    error: Some(failure.message),
                    ts_unix_ns: frame.ts_unix_ns,
                    stream_index,
                    decode_ns,
                    apply_ns: None,
                    dump: None,
                });
                return;
            }
        };

        // A packet that leaves bytes unread is a layout mismatch: report it as
        // an error and do not run its action.
        let bytes_left = raw.len().saturating_sub(decoded.size);
        let mut error = None;
        if bytes_left > 0 {
            error = Some(format!(
                "leftover bytes after packet: {bytes_left} of {} unread",
                raw.len()
            ));
        } else if decoded.id != decoded.packet_id {
            error = Some(format!(
                "packet id mismatch: read {}, packet reports {}",
                decoded.id, decoded.packet_id
            ));
        }

        let mut record = PacketRecord {
            conn_id: self.conn_id,
            state,
            dir: frame.dir,
            id: decoded.id,
            name: decoded.name.to_owned(),
            size: decoded.size,
            wire_size,
            bytes_left,
            error: error.clone(),
            ts_unix_ns: frame.ts_unix_ns,
            stream_index,
            decode_ns,
            apply_ns: None,
            dump: None,
        };

        match &error {
            Some(message) => {
                analysis.total_errors += 1;
                group.errors += 1;
                if bytes_left > 0 {
                    group.unconsumed += 1;
                    analysis.unconsumed_packets += 1;
                }
                push_example(
                    group,
                    self.conn_id,
                    decoded.id,
                    decoded.name,
                    message,
                    bytes_left,
                );
            }
            None => {
                group
                    .counts
                    .entry(record.name.clone())
                    .and_modify(|c| *c += 1)
                    .or_insert(1);
                // Only moved when the dump filter selected this packet.
                record.dump = decoded.dump;
                record.apply_ns = self.apply_action(&decoded.action, analysis, timings);
                timings.record_game(frame, state, &record.name);
            }
        }

        analysis.packets.push(record);
    }

    fn apply_action(
        &mut self,
        action: &Action,
        _analysis: &mut Analysis,
        timings: &mut Timings,
    ) -> Option<u64> {
        let mut apply_ns = None;
        match action {
            Action::None => {}
            Action::StateBoth(next) => {
                self.sb_state = *next;
                self.cb_state = *next;
                if *next == State::Game {
                    self.reached_game = true;
                }
            }
            Action::StateClientbound(next) => {
                self.cb_state = *next;
                if *next == State::Game {
                    self.reached_game = true;
                }
            }
            Action::StateServerbound(next) => {
                self.sb_state = *next;
                if *next == State::Game {
                    self.reached_game = true;
                }
            }
            Action::Compression(threshold) => {
                if *threshold < 0 {
                    self.compression = None;
                    self.notes.push(format!(
                        "login_compression carried a negative threshold ({threshold}); compression stays off"
                    ));
                } else {
                    self.compression = Some(*threshold as u32);
                }
            }
            Action::Encryption => {
                let reason = "server requested online-mode encryption; the capture is not decodable past this point";
                self.notes.push(reason.to_owned());
                self.stopped = Some(reason.to_owned());
            }
            Action::Registry(packet) => {
                self.registries.append(
                    packet.registry_id.clone(),
                    packet
                        .entries
                        .iter()
                        .cloned()
                        .map(|(ident, tag)| (ident, tag.map(Into::into)))
                        .collect(),
                );
            }
            Action::GameLogin(common) => {
                let dimension = self.resolve_dimension(common);
                if let Some(previous) = &self.dimension {
                    if previous.dimension != dimension.dimension {
                        self.notes.push(format!(
                            "dimension changed from {} to {}; the world was recreated",
                            previous.dimension, dimension.dimension
                        ));
                    }
                }
                self.notes.push(format!(
                    "dimension {} ({}, height {}, min_y {}, from {})",
                    dimension.dimension,
                    dimension.dimension_type,
                    dimension.height,
                    dimension.min_y,
                    dimension.source
                ));
                self.world = Some(WorldModel::new(dimension.height, dimension.min_y));
                self.dimension = Some(dimension);
            }
            Action::Chunk(packet) => {
                let pos = ChunkPos::new(packet.x, packet.z);
                let Some(world) = &mut self.world else {
                    self.notes.push(
                        "level_chunk_with_light arrived before the game login packet".to_owned(),
                    );
                    return None;
                };
                let start = Instant::now();
                let result = world.read_chunk(
                    pos,
                    &packet.chunk_data.buffer[..],
                    &packet.chunk_data.heightmaps,
                );
                let ns = start.elapsed().as_nanos() as u64;
                apply_ns = Some(ns);
                timings.apply_chunk.push(ns as f64 / 1000.0);
                if let Err(e) = result {
                    self.notes
                        .push(format!("chunk {pos:?} failed to parse: {e}"));
                }
            }
            Action::ForgetChunk(pos) => {
                let Some(world) = &mut self.world else {
                    return None;
                };
                let start = Instant::now();
                world.forget_chunk(*pos);
                let ns = start.elapsed().as_nanos() as u64;
                apply_ns = Some(ns);
                timings.apply_block.push(ns as f64 / 1000.0);
            }
            Action::BlockUpdate { pos, state } => {
                let Some(world) = &mut self.world else {
                    return None;
                };
                let start = Instant::now();
                let applied = world.set_block(*pos, *state);
                let ns = start.elapsed().as_nanos() as u64;
                apply_ns = Some(ns);
                timings.apply_block.push(ns as f64 / 1000.0);
                if !applied {
                    self.notes
                        .push(format!("block_update at {pos:?}: chunk not loaded"));
                }
            }
            Action::SectionBlocksUpdate { section, states } => {
                let Some(world) = &mut self.world else {
                    return None;
                };
                let start = Instant::now();
                let mut missing = false;
                for update in states {
                    let pos = BlockPos::new(
                        section.x * 16 + i32::from(update.pos.x),
                        section.y * 16 + i32::from(update.pos.y),
                        section.z * 16 + i32::from(update.pos.z),
                    );
                    if !world.set_block(pos, update.state) {
                        missing = true;
                    }
                }
                let ns = start.elapsed().as_nanos() as u64;
                apply_ns = Some(ns);
                timings.apply_block.push(ns as f64 / 1000.0);
                if missing {
                    self.notes.push(format!(
                        "section_blocks_update in {section:?}: some chunks were not loaded"
                    ));
                }
            }
            Action::PlayerPosition(packet) => {
                let player = self.player.get_or_insert_with(PlayerPos::default);
                let change = &packet.change;
                let relative = &packet.relative;
                player.pos.x = if relative.x {
                    player.pos.x + change.pos.x
                } else {
                    change.pos.x
                };
                player.pos.y = if relative.y {
                    player.pos.y + change.pos.y
                } else {
                    change.pos.y
                };
                player.pos.z = if relative.z {
                    player.pos.z + change.pos.z
                } else {
                    change.pos.z
                };
                player.yaw = if relative.y_rot {
                    player.yaw + change.look_direction.y_rot()
                } else {
                    change.look_direction.y_rot()
                };
                player.pitch = if relative.x_rot {
                    player.pitch + change.look_direction.x_rot()
                } else {
                    change.look_direction.x_rot()
                };
            }
            Action::MovePos { pos } => {
                let player = self.player.get_or_insert_with(PlayerPos::default);
                player.pos = *pos;
            }
            Action::MovePosRot { pos, yaw, pitch } => {
                let player = self.player.get_or_insert_with(PlayerPos::default);
                player.pos = *pos;
                player.yaw = *yaw;
                player.pitch = *pitch;
            }
            Action::MoveRot { yaw, pitch } => {
                let player = self.player.get_or_insert_with(PlayerPos::default);
                player.yaw = *yaw;
                player.pitch = *pitch;
            }
            Action::MoveStatus => {}
            Action::PlayerInput { .. } => {}
            Action::Note(note) => self.notes.push(note.clone()),
        }
        apply_ns
    }

    fn resolve_dimension(&self, common: &CommonPlayerSpawnInfo) -> DimensionInfo {
        if let Some((identifier, data)) = common.dimension_type(&self.registries) {
            return DimensionInfo {
                dimension: common.dimension.to_string(),
                dimension_type: identifier.to_string(),
                source: "registry",
                height: data.height,
                min_y: data.min_y,
            };
        }

        // Known packs can make the server omit registry entries (session 2
        // research). The three vanilla dimension kinds have fixed heights.
        let path = common.dimension.path().to_owned();
        let (height, min_y) = if path.contains("nether") || path.contains("end") {
            (256, 0)
        } else {
            (384, -64)
        };
        DimensionInfo {
            dimension: common.dimension.to_string(),
            dimension_type: format!("dimension_type#{}", common.dimension_type.protocol_id()),
            source: "fallback",
            height,
            min_y,
        }
    }

    fn summary(self) -> ConnectionSummary {
        ConnectionSummary {
            conn_id: self.conn_id,
            dimension: self.dimension,
            chunks_loaded: self.world.as_ref().map(WorldModel::chunk_count).unwrap_or(0),
            player: self.player,
            notes: self.notes,
            stopped: self.stopped,
            last_ts_unix_ns: self.last_ts_unix_ns,
            reached_game: self.reached_game,
        }
    }
}

fn push_example(
    group: &mut GroupStats,
    conn_id: u32,
    id: u32,
    name: &str,
    error: &str,
    bytes_left: usize,
) {
    if group.examples.len() < 5 {
        group.examples.push(ErrorExample {
            conn_id,
            id,
            name: name.to_owned(),
            error: error.to_owned(),
            bytes_left,
        });
    }
}

// ---------------------------------------------------------------------------
// Packet decoding
// ---------------------------------------------------------------------------

struct Decoded {
    id: u32,
    /// The id the packet enum reports for itself; must match `id`.
    packet_id: u32,
    name: &'static str,
    /// Bytes consumed, including the packet id.
    size: usize,
    action: Action,
    /// Set only when the dump filter selected this packet.
    dump: Option<PacketDump>,
}

struct DecodeFailure {
    id: u32,
    name: String,
    message: String,
    bytes_left: usize,
}

#[derive(Clone, Debug)]
enum Action {
    None,
    StateBoth(State),
    StateClientbound(State),
    StateServerbound(State),
    Compression(i32),
    Encryption,
    Registry(Box<ClientboundRegistryData>),
    GameLogin(Box<CommonPlayerSpawnInfo>),
    Chunk(Box<ClientboundLevelChunkWithLight>),
    ForgetChunk(ChunkPos),
    BlockUpdate {
        pos: BlockPos,
        state: azalea_block::BlockState,
    },
    SectionBlocksUpdate {
        section: azalea_core::position::ChunkSectionPos,
        states: Vec<BlockStateWithPosition>,
    },
    PlayerPosition(Box<ClientboundPlayerPosition>),
    MovePos {
        pos: Vec3,
    },
    MovePosRot {
        pos: Vec3,
        yaw: f32,
        pitch: f32,
    },
    MoveRot {
        yaw: f32,
        pitch: f32,
    },
    MoveStatus,
    /// Only used to carry the parsed `player_input` flags to the dump.
    PlayerInput {
        forward: bool,
        backward: bool,
        left: bool,
        right: bool,
        jump: bool,
        shift: bool,
        sprint: bool,
    },
    Note(String),
}

fn dump_extras(action: &Action) -> DumpExtras {
    match action {
        Action::PlayerInput {
            forward,
            backward,
            left,
            right,
            jump,
            shift,
            sprint,
        } => DumpExtras::Input(PlayerInput {
            forward: *forward,
            backward: *backward,
            left: *left,
            right: *right,
            jump: *jump,
            shift: *shift,
            sprint: *sprint,
        }),
        Action::MoveRot { yaw, pitch } | Action::MovePosRot { yaw, pitch, .. } => DumpExtras::Look {
            yaw: *yaw,
            pitch: *pitch,
        },
        _ => DumpExtras::None,
    }
}

fn decode_packet(
    state: State,
    dir: Dir,
    raw: &[u8],
    dump_filter: Option<&DumpFilter>,
) -> Result<Decoded, DecodeFailure> {
    let mut cursor = Cursor::new(raw);
    let id = match u32::azalea_read_var(&mut cursor) {
        Ok(id) => id,
        Err(e) => {
            return Err(DecodeFailure {
                id: u32::MAX,
                name: "<id>".to_owned(),
                message: format!("couldn't read the packet id: {e}"),
                bytes_left: raw.len().saturating_sub(cursor.position() as usize),
            });
        }
    };
    let id_bytes = cursor.position() as usize;

    macro_rules! read {
        ($ty:ty) => {
            match <$ty>::read(id, &mut cursor) {
                Ok(packet) => packet,
                Err(e) => {
                    return Err(DecodeFailure {
                        id,
                        name: format!("<id {id}>"),
                        message: e.to_string(),
                        bytes_left: raw.len().saturating_sub(cursor.position() as usize),
                    });
                }
            }
        };
    }

    macro_rules! finish {
        ($packet:expr, $action:expr) => {{
            let packet = $packet;
            let name = ProtocolPacket::name(&packet);
            let action = $action;
            let dump = match dump_filter {
                Some(filter) if filter.matches(name) => Some(PacketDump {
                    debug: format!("{packet:?}"),
                    extras: dump_extras(&action),
                }),
                _ => None,
            };
            Ok(Decoded {
                id,
                packet_id: ProtocolPacket::id(&packet),
                name,
                size: cursor.position() as usize,
                action,
                dump,
            })
        }};
    }

    match (state, dir) {
        (State::Handshake, Dir::Serverbound) => {
            let packet = read!(ServerboundHandshakePacket);
            let action = match &packet {
                ServerboundHandshakePacket::Intention(intention) => {
                    Action::StateBoth(match intention.intention {
                        ClientIntention::Status => State::Status,
                        // Transfer (3) also goes to login.
                        ClientIntention::Login | ClientIntention::Transfer => State::Login,
                    })
                }
            };
            finish!(packet, action)
        }
        (State::Handshake, Dir::Clientbound) => Err(DecodeFailure {
            id,
            name: "<handshake clientbound>".to_owned(),
            message: "the server never sends a clientbound handshake packet".to_owned(),
            bytes_left: raw.len().saturating_sub(id_bytes),
        }),
        (State::Status, Dir::Serverbound) => {
            let packet = read!(ServerboundStatusPacket);
            finish!(packet, Action::None)
        }
        (State::Status, Dir::Clientbound) => {
            let packet = read!(ClientboundStatusPacket);
            finish!(packet, Action::None)
        }
        (State::Login, Dir::Serverbound) => {
            let packet = read!(ServerboundLoginPacket);
            let action = match &packet {
                // `hello` is the normal offline login handshake (name + UUID);
                // only `key` is the client's answer to an online-mode encryption
                // challenge.
                ServerboundLoginPacket::Key(_) => Action::Encryption,
                ServerboundLoginPacket::LoginAcknowledged(_) => {
                    Action::StateServerbound(State::Configuration)
                }
                _ => Action::None,
            };
            finish!(packet, action)
        }
        (State::Login, Dir::Clientbound) => {
            let packet = read!(ClientboundLoginPacket);
            let action = match &packet {
                ClientboundLoginPacket::Hello(_) => Action::Encryption,
                ClientboundLoginPacket::LoginCompression(p) => {
                    Action::Compression(p.compression_threshold)
                }
                ClientboundLoginPacket::LoginFinished(_) => {
                    Action::StateClientbound(State::Configuration)
                }
                _ => Action::None,
            };
            finish!(packet, action)
        }
        (State::Configuration, Dir::Serverbound) => {
            let packet = read!(ServerboundConfigPacket);
            let action = match &packet {
                ServerboundConfigPacket::FinishConfiguration(_) => {
                    Action::StateServerbound(State::Game)
                }
                _ => Action::None,
            };
            finish!(packet, action)
        }
        (State::Configuration, Dir::Clientbound) => {
            let packet = read!(ClientboundConfigPacket);
            let action = match &packet {
                ClientboundConfigPacket::FinishConfiguration(_) => {
                    Action::StateClientbound(State::Game)
                }
                ClientboundConfigPacket::RegistryData(p) => Action::Registry(Box::new(p.clone())),
                _ => Action::None,
            };
            finish!(packet, action)
        }
        (State::Game, Dir::Serverbound) => {
            let packet = read!(ServerboundGamePacket);
            let action = match &packet {
                ServerboundGamePacket::MovePlayerPos(p) => Action::MovePos { pos: p.pos },
                ServerboundGamePacket::MovePlayerPosRot(p) => Action::MovePosRot {
                    pos: p.pos,
                    yaw: p.look_direction.y_rot(),
                    pitch: p.look_direction.x_rot(),
                },
                ServerboundGamePacket::MovePlayerRot(p) => Action::MoveRot {
                    yaw: p.look_direction.y_rot(),
                    pitch: p.look_direction.x_rot(),
                },
                ServerboundGamePacket::MovePlayerStatusOnly(_) => Action::MoveStatus,
                ServerboundGamePacket::PlayerInput(p) => Action::PlayerInput {
                    forward: p.forward,
                    backward: p.backward,
                    left: p.left,
                    right: p.right,
                    jump: p.jump,
                    shift: p.shift,
                    sprint: p.sprint,
                },
                ServerboundGamePacket::ConfigurationAcknowledged(_) => {
                    Action::StateServerbound(State::Configuration)
                }
                _ => Action::None,
            };
            finish!(packet, action)
        }
        (State::Game, Dir::Clientbound) => {
            let packet = read!(ClientboundGamePacket);
            let action = match &packet {
                ClientboundGamePacket::Login(p) => Action::GameLogin(Box::new(p.common.clone())),
                ClientboundGamePacket::LevelChunkWithLight(p) => Action::Chunk(Box::new(p.clone())),
                ClientboundGamePacket::ForgetLevelChunk(p) => Action::ForgetChunk(p.pos),
                ClientboundGamePacket::BlockUpdate(p) => Action::BlockUpdate {
                    pos: p.pos,
                    state: p.block_state,
                },
                ClientboundGamePacket::SectionBlocksUpdate(p) => Action::SectionBlocksUpdate {
                    section: p.section_pos,
                    states: p.states.clone(),
                },
                ClientboundGamePacket::PlayerPosition(p) => {
                    Action::PlayerPosition(Box::new(p.clone()))
                }
                ClientboundGamePacket::StartConfiguration(_) => {
                    Action::StateClientbound(State::Configuration)
                }
                ClientboundGamePacket::Respawn(p) => Action::Note(format!(
                    "respawn: dimension {} is not resized",
                    p.common.dimension
                )),
                _ => Action::None,
            };
            finish!(packet, action)
        }
        (_, Dir::Opened | Dir::Closed) => Err(DecodeFailure {
            id,
            name: "<marker>".to_owned(),
            message: "connection markers are not framed packets".to_owned(),
            bytes_left: raw.len(),
        }),
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Timings {
    decode_all: Vec<f64>,
    decode_game_steady: Vec<f64>,
    decode_game_chunk: Vec<f64>,
    apply_chunk: Vec<f64>,
    apply_block: Vec<f64>,
    game_start_ts: Option<u64>,
    game_end_ts: Option<u64>,
    move_ts: BTreeMap<String, Vec<u64>>,
}

impl Timings {
    fn record_decode(&mut self, ns: u64, state: State, decoded: Option<&Decoded>) {
        let us = ns as f64 / 1000.0;
        self.decode_all.push(us);
        if state == State::Game {
            let is_chunk = decoded.map(|d| is_chunk_packet(d.name)).unwrap_or(false);
            if is_chunk {
                self.decode_game_chunk.push(us);
            } else {
                self.decode_game_steady.push(us);
            }
        }
    }

    fn record_game(&mut self, frame: &Frame, state: State, name: &str) {
        if state != State::Game {
            return;
        }
        self.game_start_ts = Some(
            self.game_start_ts
                .map_or(frame.ts_unix_ns, |t| t.min(frame.ts_unix_ns)),
        );
        self.game_end_ts = Some(
            self.game_end_ts
                .map_or(frame.ts_unix_ns, |t| t.max(frame.ts_unix_ns)),
        );
        if frame.dir == Dir::Serverbound && MOVE_PACKET_NAMES.contains(&name) {
            self.move_ts
                .entry(name.to_owned())
                .or_default()
                .push(frame.ts_unix_ns);
        }
    }

    fn finish_e3(&self) -> E3Stats {
        E3Stats {
            decode_all: percentiles(self.decode_all.clone()),
            decode_game_steady: percentiles(self.decode_game_steady.clone()),
            decode_game_chunk: percentiles(self.decode_game_chunk.clone()),
            apply_chunk: percentiles(self.apply_chunk.clone()),
            apply_block: percentiles(self.apply_block.clone()),
        }
    }

    fn finish_e4(self) -> E4Stats {
        let duration_s = match (self.game_start_ts, self.game_end_ts) {
            (Some(start), Some(end)) if end > start => (end - start) as f64 / 1e9,
            _ => 0.0,
        };
        let mut e4 = E4Stats {
            game_duration_s: duration_s,
            ..E4Stats::default()
        };

        for name in MOVE_PACKET_NAMES {
            let Some(timestamps) = self.move_ts.get(name) else {
                e4.per_name.insert(name.to_owned(), MoveStats::default());
                continue;
            };
            let mut stats = MoveStats {
                count: timestamps.len() as u64,
                per_second: rate(timestamps.len() as u64, duration_s),
                gaps: GapBins::default(),
            };
            for pair in timestamps.windows(2) {
                stats.gaps.add((pair[1] - pair[0]) as f64 / 1e6);
            }
            e4.per_name.insert(name.to_owned(), stats);
        }

        let mut combined: Vec<u64> = Vec::new();
        for name in &MOVE_PACKET_NAMES[..4] {
            if let Some(timestamps) = self.move_ts.get(*name) {
                combined.extend_from_slice(timestamps);
            }
        }
        combined.sort_unstable();
        let mut gaps = GapBins::default();
        for pair in combined.windows(2) {
            gaps.add((pair[1] - pair[0]) as f64 / 1e6);
        }
        e4.all_moves = MoveStats {
            count: combined.len() as u64,
            per_second: rate(combined.len() as u64, duration_s),
            gaps,
        };
        e4
    }
}

fn rate(count: u64, duration_s: f64) -> f64 {
    if duration_s > 0.0 {
        count as f64 / duration_s
    } else {
        0.0
    }
}

fn percentiles(mut samples: Vec<f64>) -> PercentilesUs {
    if samples.is_empty() {
        return PercentilesUs::default();
    }
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let count = samples.len();
    let pick = |p: f64| -> f64 {
        let rank = (p * count as f64).ceil() as usize;
        let index = rank.clamp(1, count) - 1;
        samples[index]
    };
    PercentilesUs {
        count,
        p50: pick(0.50),
        p99: pick(0.99),
        p99_9: pick(0.999),
        max: *samples.last().unwrap(),
    }
}
