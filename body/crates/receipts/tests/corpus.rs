//! Replay a real capture through decoder -> world -> receipts.
//!
//! The intents are synthesised from the serverbound `player_action` and
//! `use_item_on` packets: a start-destroy action becomes a dig on the packet's
//! block position, and a use-on-block becomes a place on the adjacent block
//! with the currently carried block item as the expected state. The body tick
//! advances on every `client_tick_end`.
//!
//! The capture is machine-local and ignored by git, so a machine without it
//! skips with an explicit message instead of failing.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use azalea_core::direction::Direction;
use azalea_core::entity_id::MinecraftEntityId;
use azalea_core::game_type::{GameMode, OptionalGameType};
use azalea_protocol::packets::{
    ClientIntention, ProtocolPacket,
    common::CommonPlayerSpawnInfo,
    config::{
        ClientboundConfigPacket, ClientboundFinishConfiguration, ServerboundConfigPacket,
        ServerboundFinishConfiguration,
    },
    game::{
        ClientboundGamePacket, ClientboundLogin, ClientboundSetHeldSlot, ServerboundClientTickEnd,
        ServerboundGamePacket, ServerboundPlayerAction, ServerboundUseItemOn,
        s_interact::InteractionHand, s_player_action::Action as PlayerActionKind,
        s_use_item_on::BlockHit,
    },
    handshake::{ServerboundHandshakePacket, ServerboundIntention},
    login::{
        ClientboundLoginCompression, ClientboundLoginFinished, ClientboundLoginPacket,
        ServerboundHello, ServerboundLoginAcknowledged, ServerboundLoginPacket,
    },
};
use azalea_protocol::write::{encode_to_network_packet, serialize_packet};
use azalea_registry::DataRegistry;
use azalea_registry::builtin::{BlockKind, ItemKind};
use azalea_registry::data::DimensionKind;
use azalea_registry::identifier::Identifier;
use praxion_decoder::{Decoder, Dir, Event, Packet};
use praxion_eventlog::{EventKind, Outcome, Writer, read};
use praxion_receipts::{Engine, Intent, ReceiptId};
use praxion_tee::{Kind, Reader, Record, write_header, write_record};
use praxion_world::{BlockPos, ItemStack, World};

mod common;

struct ActionInfo {
    kind: &'static str,
    receipt: ReceiptId,
    pos: BlockPos,
    seq: u32,
    expected: String,
}

struct Replay {
    world: World,
    followed: Option<u32>,
    engine: Engine,
    writer: Writer,
    tick: u64,
    last_ts_ns: u64,
    next_intent: u64,
    actions: Vec<ActionInfo>,
    skipped_use_item: u64,
    serverbound_names: BTreeMap<String, u64>,
}

impl Replay {
    fn new(run_dir: PathBuf) -> Self {
        let writer = Writer::create(&run_dir).unwrap();
        let mut world = World::new();
        world.follow(0);
        Self {
            world,
            followed: Some(0),
            engine: Engine::new(),
            writer,
            tick: 0,
            last_ts_ns: 0,
            next_intent: 0,
            actions: Vec::new(),
            skipped_use_item: 0,
            serverbound_names: BTreeMap::new(),
        }
    }

    fn process(&mut self, event: &Event) {
        match event {
            Event::Opened { conn_id, .. } => self.world.opened(*conn_id),
            Event::Closed { conn_id, .. } => self.world.closed(*conn_id),
            Event::Desync(desync) => {
                self.world.invalidate(desync.conn_id, desync.reason.clone());
            }
            Event::Decoded(decoded) => {
                self.last_ts_ns = decoded.ts_ns;
                match &decoded.packet {
                    Packet::ConfigurationClientbound(packet) => {
                        self.world.apply_clientbound_config(decoded.conn_id, packet);
                    }
                    Packet::PlayClientbound(packet) => {
                        if decoded.packet.name() == "login"
                            && self.followed != Some(decoded.conn_id)
                        {
                            self.world.follow(decoded.conn_id);
                            self.followed = Some(decoded.conn_id);
                        }
                        self.world.apply_clientbound_play(decoded.conn_id, packet);
                        if self.followed == Some(decoded.conn_id) {
                            self.observe(decoded.ts_ns, &decoded.packet);
                        }
                    }
                    Packet::PlayServerbound(packet) => {
                        self.world.apply_serverbound_play(decoded.conn_id, packet);
                        if self.followed != Some(decoded.conn_id) {
                            return;
                        }
                        *self
                            .serverbound_names
                            .entry(decoded.packet.name().to_owned())
                            .or_insert(0) += 1;
                        self.observe_serverbound(decoded.ts_ns, packet);
                    }
                    _ => {}
                }
            }
        }
    }

    fn observe(&mut self, ts_ns: u64, packet: &Packet) {
        self.engine
            .observe(&mut self.writer, self.tick, ts_ns, packet, &self.world)
            .unwrap();
    }

    fn observe_serverbound(&mut self, ts_ns: u64, packet: &ServerboundGamePacket) {
        match packet {
            ServerboundGamePacket::PlayerAction(action)
                if action.action == PlayerActionKind::StartDestroyBlock =>
            {
                let id = self.next_intent;
                self.next_intent += 1;
                let intent = Intent::dig(id, action.pos, action.seq, self.tick);
                let receipt = self
                    .engine
                    .register(&mut self.writer, self.tick, ts_ns, intent, &self.world)
                    .unwrap();
                self.actions.push(ActionInfo {
                    kind: "dig",
                    receipt,
                    pos: action.pos,
                    seq: action.seq,
                    expected: "air".to_owned(),
                });
            }
            ServerboundGamePacket::UseItemOn(use_on) => {
                let carried = held_item(&self.world);
                let item = carried.kind().to_str();
                let short = item.strip_prefix("minecraft:").unwrap_or(item);
                if carried.is_empty() || BlockKind::from_str(short).is_err() {
                    self.skipped_use_item += 1;
                } else {
                    let direction = use_on.block_hit.direction;
                    let pos = BlockPos::new(
                        use_on.block_hit.block_pos.x + direction.x(),
                        use_on.block_hit.block_pos.y + direction.y(),
                        use_on.block_hit.block_pos.z + direction.z(),
                    );
                    let id = self.next_intent;
                    self.next_intent += 1;
                    let intent = Intent::place(id, pos, short, use_on.seq, self.tick);
                    let receipt = self
                        .engine
                        .register(&mut self.writer, self.tick, ts_ns, intent, &self.world)
                        .unwrap();
                    self.actions.push(ActionInfo {
                        kind: "place",
                        receipt,
                        pos,
                        seq: use_on.seq,
                        expected: short.to_owned(),
                    });
                }
            }
            _ => {}
        }
        let packet = Packet::PlayServerbound(packet.clone());
        self.engine
            .observe(&mut self.writer, self.tick, ts_ns, &packet, &self.world)
            .unwrap();
        if matches!(
            packet,
            Packet::PlayServerbound(ServerboundGamePacket::ClientTickEnd(_))
        ) {
            self.engine
                .advance(&mut self.writer, self.tick, ts_ns)
                .unwrap();
            self.tick += 1;
        }
    }

    fn finish(&mut self) {
        // Close every receipt still open at the end of the capture; the real
        // body would have kept waiting, but the table needs one outcome each.
        self.engine
            .advance(&mut self.writer, self.tick + 1_000, self.last_ts_ns)
            .unwrap();
        self.writer.flush().unwrap();
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// The item in the selected hotbar slot. A player container lists its menu
/// slots 0..45, so hotbar slot `n` is inventory index `36 + n`.
fn held_item(world: &World) -> &praxion_world::ItemStack {
    let slot = world.selected_hotbar_slot().unwrap_or(0) as usize;
    world
        .inventory()
        .get(36 + slot)
        .unwrap_or_else(|| world.carried_item())
}

fn replay_capture(name: &str) -> Option<Replay> {
    replay_path(&repo_root().join("spikes/captures").join(name), name)
}

fn replay_path(path: &Path, label: &str) -> Option<Replay> {
    if !path.is_file() {
        eprintln!(
            "SKIP corpus: {} does not exist on this machine",
            path.display()
        );
        return None;
    }
    let file = File::open(path).unwrap();
    let reader = Reader::new(BufReader::new(file)).unwrap();
    let mut decoder = Decoder::new();
    // Whole-capture replay: decode every record in timestamp order.
    decoder.set_reorder_window(60_000_000_000);

    let label: String = label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let run_dir = common::test_dir(&format!("corpus-{label}"));
    let mut replay = Replay::new(run_dir);
    for record in reader {
        let record = record.unwrap();
        for event in decoder.feed(&record) {
            replay.process(&event);
        }
    }
    for event in decoder.finish() {
        replay.process(&event);
    }
    replay.finish();
    Some(replay)
}

fn report(name: &str, replay: &Replay) {
    let mut by_kind: BTreeMap<&str, BTreeMap<Outcome, u64>> = BTreeMap::new();
    for action in &replay.actions {
        let outcome = replay.engine.outcome(action.receipt).unwrap();
        *by_kind
            .entry(action.kind)
            .or_default()
            .entry(outcome)
            .or_insert(0) += 1;
    }

    eprintln!(
        "{name}: actions={} (skipped use_item_on without a block item: {}), final tick {}",
        replay.actions.len(),
        replay.skipped_use_item,
        replay.tick
    );
    eprintln!("  action  confirmed  unconfirmed  preempted  total");
    for kind in ["dig", "place"] {
        let outcomes = by_kind.get(kind);
        let confirmed = outcomes
            .and_then(|outcomes| outcomes.get(&Outcome::Confirmed))
            .copied()
            .unwrap_or(0);
        let unconfirmed = outcomes
            .and_then(|outcomes| outcomes.get(&Outcome::Unconfirmed))
            .copied()
            .unwrap_or(0);
        let preempted = outcomes
            .and_then(|outcomes| outcomes.get(&Outcome::Preempted))
            .copied()
            .unwrap_or(0);
        eprintln!(
            "  {kind:<6}  {confirmed:>9}  {unconfirmed:>11}  {preempted:>9}  {:>5}",
            confirmed + unconfirmed + preempted
        );
    }
    for (kind, outcomes) in &by_kind {
        if *kind == "dig" || *kind == "place" {
            continue;
        }
        let confirmed = outcomes.get(&Outcome::Confirmed).copied().unwrap_or(0);
        let unconfirmed = outcomes.get(&Outcome::Unconfirmed).copied().unwrap_or(0);
        let preempted = outcomes.get(&Outcome::Preempted).copied().unwrap_or(0);
        eprintln!(
            "  {kind:<6}  {confirmed:>9}  {unconfirmed:>11}  {preempted:>9}  {:>5}",
            confirmed + unconfirmed + preempted
        );
    }

    if replay.actions.is_empty() {
        eprintln!("  no player_action or use_item_on packets in this capture");
        eprintln!("  serverbound packets seen:");
        for (name, count) in replay.serverbound_names() {
            eprintln!("    {name}: {count}");
        }
        return;
    }

    // One note per unconfirmed action, from the log the engine wrote.
    let log = read(replay.writer.path().to_path_buf()).unwrap();
    let mut notes = BTreeMap::new();
    for event in &log.events {
        if let EventKind::ReceiptSettled {
            receipt_id, note, ..
        } = &event.event
        {
            notes.insert(*receipt_id, note.clone());
        }
    }

    let mut any = false;
    for action in &replay.actions {
        let outcome = replay.engine.outcome(action.receipt).unwrap();
        if outcome == Outcome::Unconfirmed {
            any = true;
            eprintln!(
                "  unconfirmed: {} at ({}, {}, {}) seq {} expected {} note {:?}",
                action.kind,
                action.pos.x,
                action.pos.y,
                action.pos.z,
                action.seq,
                action.expected,
                notes.get(&action.receipt).cloned().flatten()
            );
        }
    }
    if !any {
        eprintln!("  every action settled confirmed");
    }
}

impl Replay {
    fn serverbound_names(&self) -> &BTreeMap<String, u64> {
        &self.serverbound_names
    }
}

#[test]
fn corpus4_replay() {
    let Some(replay) = replay_capture("corpus4.prxcap") else {
        return;
    };
    report("corpus4.prxcap", &replay);
}

#[test]
fn e2e_capture_replay() {
    let path = repo_root().join("logs/t5a/e2e.prxcap");
    let Some(replay) = replay_path(&path, "t5a-e2e") else {
        return;
    };
    report("logs/t5a/e2e.prxcap", &replay);
}

/// If corpus4 ever lacks actions (a capture can be overwritten), this test
/// names the capture that does have them. It decodes every capture and is
/// therefore ignored by default; run it with `--ignored`.
#[test]
#[ignore = "decodes every capture; run on demand"]
fn find_captures_with_player_actions() {
    let dir = repo_root().join("spikes/captures");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!(
            "SKIP corpus: {} does not exist on this machine",
            dir.display()
        );
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".prxcap"))
        .collect();
    names.sort();
    if names.is_empty() {
        eprintln!("SKIP corpus: no *.prxcap files in {}", dir.display());
        return;
    }
    for name in names {
        if let Some(replay) = replay_capture(&name) {
            let digs = replay.actions.iter().filter(|a| a.kind == "dig").count();
            let places = replay.actions.iter().filter(|a| a.kind == "place").count();
            eprintln!(
                "{name}: digs={digs} places={places} skipped_use_item_on={}",
                replay.skipped_use_item
            );
        }
    }
}

// --- A synthetic capture -------------------------------------------------
//
// corpus4 turned out to contain no player_action or use_item_on packets on
// this machine, so the action-synthesis path is exercised by a capture built
// here: a minimal login, one dig and one place, both answered correctly.

struct CaptureBuilder {
    records: Vec<Record>,
    ts: u64,
    conn_id: u32,
}

impl CaptureBuilder {
    fn new() -> Self {
        Self {
            records: Vec::new(),
            ts: 1_793_443_244_558_717_800,
            conn_id: 0,
        }
    }

    fn packet<P: ProtocolPacket + Debug>(&mut self, dir: Dir, packet: &P, threshold: Option<u32>) {
        let raw = serialize_packet(packet).expect("serialize");
        let mut cipher = None;
        let wire = encode_to_network_packet(&raw, threshold, &mut cipher);
        self.records.push(Record {
            ts_ns: self.ts,
            conn_id: self.conn_id,
            kind: match dir {
                Dir::Serverbound => Kind::Serverbound,
                Dir::Clientbound => Kind::Clientbound,
            },
            data: wire,
        });
        self.ts += 1_000;
    }

    fn marker(&mut self, kind: Kind) {
        self.records.push(Record {
            ts_ns: self.ts,
            conn_id: self.conn_id,
            kind,
            data: Vec::new(),
        });
        self.ts += 1_000;
    }

    fn write(&self, path: &Path) {
        let mut out = Vec::new();
        write_header(&mut out).unwrap();
        for record in &self.records {
            write_record(&mut out, record).unwrap();
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, out).unwrap();
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

fn write_synthetic_capture(path: &Path) {
    let threshold = Some(256);
    let mut builder = CaptureBuilder::new();
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
        threshold,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundConfigPacket::FinishConfiguration(ClientboundFinishConfiguration),
        threshold,
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundConfigPacket::FinishConfiguration(ServerboundFinishConfiguration),
        threshold,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::Login(login_packet()),
        threshold,
    );

    // A stone stack in hotbar slot 0, so the place has an expected block.
    let mut items = vec![ItemStack::Empty; 46];
    items[36] = ItemStack::new(ItemKind::Stone, 5);
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::ContainerSetContent(
            azalea_protocol::packets::game::ClientboundContainerSetContent {
                container_id: 0,
                state_id: 0,
                items,
                carried_item: ItemStack::Empty,
            },
        ),
        threshold,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::SetHeldSlot(ClientboundSetHeldSlot { slot: 0 }),
        threshold,
    );

    // Tick 0 ends.
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
        threshold,
    );

    // Dig at (1, 64, 0): the server answers with air and a matching ack.
    let dig_pos = BlockPos::new(1, 64, 0);
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::PlayerAction(ServerboundPlayerAction {
            action: PlayerActionKind::StartDestroyBlock,
            pos: dig_pos,
            direction: Direction::Up,
            seq: 7,
        }),
        threshold,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::BlockUpdate(
            azalea_protocol::packets::game::ClientboundBlockUpdate {
                pos: dig_pos,
                block_state: praxion_world::BlockState::AIR,
            },
        ),
        threshold,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::BlockChangedAck(
            azalea_protocol::packets::game::ClientboundBlockChangedAck { seq: 7 },
        ),
        threshold,
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
        threshold,
    );

    // Place against (1, 63, 0) on its top face: the new block is (1, 64, 0).
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::UseItemOn(ServerboundUseItemOn {
            hand: InteractionHand::MainHand,
            block_hit: BlockHit {
                block_pos: BlockPos::new(1, 63, 0),
                direction: Direction::Up,
                location: praxion_world::Vec3::new(1.5, 64.0, 0.5),
                inside: false,
                world_border: false,
            },
            seq: 8,
        }),
        threshold,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::BlockUpdate(
            azalea_protocol::packets::game::ClientboundBlockUpdate {
                pos: dig_pos,
                block_state: praxion_world::BlockState::from(BlockKind::Stone),
            },
        ),
        threshold,
    );
    builder.packet(
        Dir::Clientbound,
        &ClientboundGamePacket::BlockChangedAck(
            azalea_protocol::packets::game::ClientboundBlockChangedAck { seq: 8 },
        ),
        threshold,
    );
    builder.packet(
        Dir::Serverbound,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
        threshold,
    );
    builder.marker(Kind::Closed);
    builder.write(path);
}

#[test]
fn synthetic_capture_replays_dig_and_place_as_confirmed() {
    let path = repo_root()
        .join("body/target/receipts-tests")
        .join(format!("synthetic-{}.prxcap", std::process::id()));
    write_synthetic_capture(&path);
    let Some(replay) = replay_path(&path, "synthetic") else {
        panic!("the synthetic capture must replay");
    };
    report("synthetic.prxcap", &replay);
    assert_eq!(replay.actions.len(), 2, "one dig and one place");
    for action in &replay.actions {
        assert_eq!(
            replay.engine.outcome(action.receipt),
            Some(Outcome::Confirmed),
            "{} at {:?} should confirm",
            action.kind,
            action.pos
        );
    }
    let log = read(replay.writer.path().to_path_buf()).unwrap();
    assert!(log.events.iter().any(|event| matches!(
        &event.event,
        EventKind::ReceiptSettled {
            outcome: Outcome::Confirmed,
            ..
        }
    )));
}
