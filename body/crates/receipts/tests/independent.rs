//! Independent adversarial tests for receipts (step 6 review).

use std::path::{Path, PathBuf};

use azalea_block::BlockState;
use azalea_core::position::{BlockPos, Vec3};
use azalea_inventory::operations::ClickType;
use azalea_protocol::common::movements::{PositionMoveRotation, RelativeMovements};
use azalea_protocol::packets::game::{
    ClientboundBlockChangedAck, ClientboundContainerSetContent, ClientboundGamePacket, ClientboundPlayerPosition, ServerboundContainerClick, ServerboundGamePacket, s_container_click::HashedStack,
};
use azalea_registry::builtin::{BlockKind, ItemKind};
use praxion_decoder::Packet;
use praxion_eventlog::{EventKind, Outcome, Writer, read};
use praxion_world::{ItemStack, World};

use praxion_receipts::{Engine, Intent, ReceiptId};

fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/receipts-tests")
        .join(format!("independent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stone() -> BlockState {
    BlockState::from(BlockKind::Stone)
}

fn air() -> BlockState {
    BlockState::AIR
}

fn block_update(pos: BlockPos, state: BlockState) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::BlockUpdate(
        azalea_protocol::packets::game::ClientboundBlockUpdate {
            pos,
            block_state: state,
        },
    ))
}

fn block_ack(seq: u32) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::BlockChangedAck(
        ClientboundBlockChangedAck { seq },
    ))
}


fn player_position() -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::PlayerPosition(
        ClientboundPlayerPosition {
            id: 1,
            change: PositionMoveRotation {
                pos: Vec3::new(0.0, 64.0, 0.0),
                delta: Vec3::ZERO,
                look_direction: Default::default(),
            },
            relative: RelativeMovements::all_absolute(),
        },
    ))
}

fn content(container_id: i32, state_id: u32, items: Vec<ItemStack>) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::ContainerSetContent(
        ClientboundContainerSetContent {
            container_id,
            state_id,
            items,
            carried_item: ItemStack::Empty,
        },
    ))
}


fn click(container_id: i32, state_id: u32, slot: i16) -> Packet {
    Packet::PlayServerbound(ServerboundGamePacket::ContainerClick(
        ServerboundContainerClick {
            container_id,
            state_id,
            slot_num: slot,
            button_num: 0,
            click_type: ClickType::Pickup,
            changed_slots: Default::default(),
            carried_item: HashedStack(None),
        },
    ))
}

fn content_packet(
    container_id: i32,
    state_id: u32,
    items: Vec<ItemStack>,
) -> ClientboundGamePacket {
    ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
        container_id,
        state_id,
        items,
        carried_item: ItemStack::Empty,
    })
}

struct Harness {
    dir: PathBuf,
    writer: Writer,
    engine: Engine,
    world: World,
    tick: u64,
    ts_ns: u64,
}

impl Harness {
    fn new(name: &str) -> Self {
        let dir = test_dir(name);
        let writer = Writer::create(&dir).unwrap();
        let mut world = World::new();
        world.follow(0);
        Self {
            dir,
            writer,
            engine: Engine::new(),
            world,
            tick: 0,
            ts_ns: 1_793_443_244_558_717_800,
        }
    }

    fn register(&mut self, intent: Intent) -> ReceiptId {
        self.engine
            .register(&mut self.writer, self.tick, self.ts_ns, intent, &self.world)
            .unwrap()
    }

    fn apply(&mut self, packet: &Packet) {
        match packet {
            Packet::PlayClientbound(packet) => {
                self.world.apply_clientbound_play(0, packet);
            }
            Packet::PlayServerbound(packet) => {
                self.world.apply_serverbound_play(0, packet);
            }
            _ => {}
        }
    }

    fn observe_at(&mut self, tick: u64, packet: Packet) {
        self.apply(&packet);
        self.ts_ns += 1;
        self.engine
            .observe(&mut self.writer, tick, self.ts_ns, &packet, &self.world)
            .unwrap();
    }

    fn advance_at(&mut self, tick: u64) {
        self.ts_ns += 1;
        self.engine
            .advance(&mut self.writer, tick, self.ts_ns)
            .unwrap();
    }

    fn outcome(&self, receipt: ReceiptId) -> Option<Outcome> {
        self.engine.outcome(receipt)
    }

    fn events(&mut self) -> Vec<praxion_eventlog::Event> {
        self.writer.flush().unwrap();
        read(self.dir.join(praxion_eventlog::FILE_NAME)).unwrap().events
    }
}

// FINDING 1
// inside the window still confirms, because the engine keeps the first air hit
// and never clears it on a revert.
#[test]
fn dig_reverted_by_server_inside_window_does_not_confirm() {
    let mut h = Harness::new("revert-dig");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 5, 0).with_window(10));
    h.observe_at(1, block_update(pos, air()));
    h.observe_at(2, block_update(pos, stone())); // server reverts
    h.observe_at(3, block_ack(5));
    h.advance_at(10);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
}

#[test]
fn two_intents_same_block_interleaved_acks_attribute_by_seq() {
    let mut h = Harness::new("two-intents");
    let pos = BlockPos::new(1, 64, 0);
    h.tick = 0;
    let a = h.register(Intent::dig(1, pos, 5, 0).with_window(10));
    let b = h.register(Intent::dig(2, pos, 7, 0).with_window(10));
    h.observe_at(1, block_update(pos, air()));
    h.observe_at(2, block_ack(5));
    assert_eq!(h.outcome(a), Some(Outcome::Confirmed));
    assert_eq!(h.outcome(b), None);
    h.observe_at(3, block_ack(7));
    assert_eq!(h.outcome(b), Some(Outcome::Confirmed));
}

#[test]
fn higher_ack_covers_older_intent() {
    let mut h = Harness::new("higher-ack");
    let pos = BlockPos::new(2, 64, 0);
    h.tick = 0;
    let older = h.register(Intent::dig(1, pos, 5, 0).with_window(10));
    let newer = h.register(Intent::dig(2, pos, 10, 0).with_window(10));
    h.observe_at(1, block_update(pos, air()));
    h.observe_at(2, block_ack(10));
    assert_eq!(h.outcome(older), Some(Outcome::Confirmed));
    assert_eq!(h.outcome(newer), Some(Outcome::Confirmed));
}

#[test]
fn lower_ack_does_not_cover_newer_intent() {
    let mut h = Harness::new("lower-ack");
    let pos = BlockPos::new(3, 64, 0);
    h.tick = 0;
    let high = h.register(Intent::dig(1, pos, 10, 0).with_window(10));
    let low = h.register(Intent::dig(2, pos, 5, 0).with_window(10));
    h.observe_at(1, block_update(pos, air()));
    h.observe_at(2, block_ack(5));
    assert_eq!(h.outcome(low), Some(Outcome::Confirmed));
    assert_eq!(h.outcome(high), None);
    h.advance_at(10);
    assert_eq!(h.outcome(high), Some(Outcome::Unconfirmed));
}

#[test]
fn evidence_at_exact_start_tick_counts() {
    let mut h = Harness::new("exact-start");
    let pos = BlockPos::new(4, 64, 0);
    h.tick = 5;
    let receipt = h.register(Intent::dig(1, pos, 9, 5));
    h.observe_at(5, block_update(pos, air()));
    h.observe_at(5, block_ack(9));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn evidence_at_exact_window_end_counts() {
    let mut h = Harness::new("exact-end");
    let pos = BlockPos::new(5, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 11, 0).with_window(5));
    h.observe_at(5, block_update(pos, air()));
    h.observe_at(5, block_ack(11));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn move_settles_confirmed_at_window_end() {
    let mut h = Harness::new("move-end");
    h.tick = 0;
    let receipt = h.register(Intent::movement(1, "move_to", 0).with_window(5));
    h.advance_at(5);
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
    let note = h
        .events()
        .into_iter()
        .find_map(|event| match event.event {
            EventKind::ReceiptSettled { receipt_id, note, .. } if receipt_id == receipt => note,
            _ => None,
        });
    assert_eq!(note.as_deref(), Some("unrefuted"));
}

#[test]
fn unrelated_pickup_does_not_confirm_craft() {
    let mut h = Harness::new("craft-unrelated");
    h.world
        .apply_clientbound_play(0, &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]));
    h.tick = 0;
    let receipt = h.register(Intent::inventory(1, "craft", "stone", 1, 0).with_window(10));
    h.observe_at(
        1,
        content(
            0,
            1,
            vec![ItemStack::new(ItemKind::Stone, 5), ItemStack::new(ItemKind::Dirt, 1)],
        ),
    );
    h.advance_at(10);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
}

#[test]
fn same_item_pickup_confirms_craft_intent() {
    let mut h = Harness::new("craft-pickup");
    h.world
        .apply_clientbound_play(0, &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]));
    h.tick = 0;
    let receipt = h.register(Intent::inventory(1, "craft", "stone", 1, 0).with_window(10));
    h.observe_at(1, content(0, 1, vec![ItemStack::new(ItemKind::Stone, 6)]));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn move_correction_one_tick_after_window_is_late_evidence() {
    let mut h = Harness::new("move-late-corr");
    h.tick = 0;
    let receipt = h.register(Intent::movement(1, "move_to", 0).with_window(5));
    h.advance_at(5);
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
    h.observe_at(6, player_position());
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
    let late = h
        .events()
        .into_iter()
        .filter(|event| {
            matches!(
                &event.event,
                EventKind::LateEvidence { receipt_id, evidence_kind }
                    if *receipt_id == receipt && evidence_kind == "correction"
            )
        })
        .count();
    assert_eq!(late, 1);
}

#[test]
fn slot_click_reverted_by_newer_set_content() {
    let mut h = Harness::new("slot-content-revert");
    h.world
        .apply_clientbound_play(0, &content_packet(0, 1, vec![ItemStack::new(ItemKind::Stone, 1)]));
    h.tick = 0;
    let receipt = h.register(Intent::slot_click(1, 0, 0, 1, 0).with_window(5));
    h.observe_at(1, click(0, 1, 0));
    assert_eq!(h.outcome(receipt), None);
    h.observe_at(2, content(0, 2, vec![ItemStack::new(ItemKind::Stone, 1)]));
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
}

#[test]
fn property_no_confirmed_without_own_evidence() {
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn chance(&mut self, percent: u64) -> bool {
            self.next() % 100 < percent
        }
        fn pick(&mut self, n: usize) -> usize {
            (self.next() as usize) % n
        }
    }

    let n = 50usize;
    for trial in 0..20usize {
        let mut h = Harness::new(&format!("property-{trial}"));
        let mut receipts = Vec::with_capacity(n);
        for i in 0..n {
            let pos = BlockPos::new(i as i32, 64, 0);
            let seq = (i as u32) + 1;
            let r = h.register(Intent::dig(i as u64, pos, seq, 0).with_window(1000));
            receipts.push(r);
        }

        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ trial as u64);
        let mut air_observed = vec![false; n];
        let mut ack_seqs: Vec<u32> = Vec::new();
        let mut packets: Vec<Packet> = Vec::new();

        for i in 0..n {
            if rng.chance(70) {
                air_observed[i] = true;
                let pos = BlockPos::new(i as i32, 64, 0);
                packets.push(block_update(pos, air()));
            }
        }
        for i in 0..n {
            if rng.chance(70) {
                let seq = (i as u32) + 1;
                ack_seqs.push(seq);
                packets.push(block_ack(seq));
            }
        }
        // Shuffle.
        for i in (1..packets.len()).rev() {
            let j = rng.pick(i + 1);
            packets.swap(i, j);
        }
        for packet in packets {
            h.observe_at(1, packet);
        }
        h.advance_at(10_000);

        for i in 0..n {
            let seq = (i as u32) + 1;
            let has_air = air_observed[i];
            let has_covering_ack = ack_seqs.iter().any(|&s| s >= seq);
            let happened = has_air && has_covering_ack;
            if !happened {
                assert_ne!(
                    h.outcome(receipts[i]),
                    Some(Outcome::Confirmed),
                    "trial {trial}: intent {i} confirmed without own evidence"
                );
            }
        }
    }
}

#[test]
fn no_panic_on_degenerate_inputs() {
    let mut h = Harness::new("degenerate");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 1, 0).with_window(0));
    h.observe_at(0, block_update(pos, air()));
    h.observe_at(0, block_ack(1));
    let _ = h.outcome(receipt);
    h.advance_at(u64::MAX / 4);
    let _ = h.outcome(receipt);
}
