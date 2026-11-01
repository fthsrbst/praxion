use std::path::{Path, PathBuf};

use azalea_block::BlockState;
use azalea_core::position::{BlockPos, ChunkSectionBlockPos, ChunkSectionPos, Vec3};
use azalea_inventory::operations::ClickType;
use azalea_protocol::common::movements::{PositionMoveRotation, RelativeMovements};
use azalea_protocol::packets::game::{
    ClientboundBlockChangedAck, ClientboundBlockUpdate, ClientboundContainerSetContent,
    ClientboundContainerSetSlot, ClientboundGamePacket, ClientboundPlayerPosition,
    ClientboundSectionBlocksUpdate, ServerboundContainerClick, ServerboundGamePacket,
    c_section_blocks_update::BlockStateWithPosition, s_container_click::HashedStack,
};
use azalea_registry::builtin::{BlockKind, ItemKind};
use azalea_registry::Registry;
use praxion_decoder::Packet;
use praxion_eventlog::{Event, EventKind, Outcome, Writer, read};
use praxion_world::{ItemStack, World};

use crate::{
    Engine, Intent, ReceiptId, interpreter_outcome, LATE_RETENTION_TICKS,
};

fn test_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/receipts-tests")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn stone() -> BlockState {
    BlockState::from(BlockKind::Stone)
}

fn dirt() -> BlockState {
    BlockState::from(BlockKind::Dirt)
}

fn block_update(pos: BlockPos, state: BlockState) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
        pos,
        block_state: state,
    }))
}

fn block_ack(seq: u32) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::BlockChangedAck(
        ClientboundBlockChangedAck { seq },
    ))
}

fn section_update(pos: BlockPos, state: BlockState) -> Packet {
    let section = ChunkSectionPos::new(pos.x >> 4, pos.y >> 4, pos.z >> 4);
    let local = ChunkSectionBlockPos::new(
        (pos.x & 15) as u8,
        (pos.y & 15) as u8,
        (pos.z & 15) as u8,
    );
    Packet::PlayClientbound(ClientboundGamePacket::SectionBlocksUpdate(
        ClientboundSectionBlocksUpdate {
            section_pos: section,
            states: vec![BlockStateWithPosition { pos: local, state }],
        },
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

fn set_slot(container_id: i32, state_id: u32, slot: u16, item: ItemStack) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::ContainerSetSlot(
        ClientboundContainerSetSlot {
            container_id,
            state_id,
            slot,
            item_stack: item,
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

    fn events(&mut self) -> Vec<Event> {
        self.writer.flush().unwrap();
        read(self.dir.join(praxion_eventlog::FILE_NAME)).unwrap().events
    }
}

#[test]
fn dig_confirms_on_air_plus_matching_ack_in_either_order() {
    let mut h = Harness::new("dig-confirm");
    let pos_a = BlockPos::new(1, 64, 0);
    let pos_b = BlockPos::new(2, 64, 0);

    h.tick = 0;
    let a = h.register(Intent::dig(1, pos_a, 5, 0));
    h.observe_at(1, block_update(pos_a, BlockState::AIR));
    h.observe_at(2, block_ack(5));
    assert_eq!(h.outcome(a), Some(Outcome::Confirmed));

    let b = h.register(Intent::dig(2, pos_b, 9, 3));
    h.observe_at(4, block_ack(9));
    h.observe_at(5, section_update(pos_b, BlockState::AIR));
    assert_eq!(h.outcome(b), Some(Outcome::Confirmed));

    let events = h.events();
    let settled: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.event {
            EventKind::ReceiptSettled { receipt_id, outcome, evidence, .. } => {
                Some((*receipt_id, *outcome, evidence.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(settled.len(), 2);
    assert_eq!(settled[0].1, Outcome::Confirmed);
    assert_eq!(settled[0].2.len(), 2);
}

#[test]
fn dig_without_evidence_is_unconfirmed() {
    let mut h = Harness::new("dig-missing");
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, BlockPos::new(0, 64, 0), 1, 0));
    h.advance_at(40);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
}

#[test]
fn dig_late_evidence_is_logged_but_does_not_reopen_the_receipt() {
    let mut h = Harness::new("dig-late");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 3, 0).with_window(5));
    h.advance_at(5);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));

    h.observe_at(6, block_update(pos, BlockState::AIR));
    h.observe_at(7, block_ack(3));
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));

    let late: Vec<_> = h
        .events()
        .into_iter()
        .filter_map(|event| match event.event {
            EventKind::LateEvidence { receipt_id, evidence_kind } => {
                Some((receipt_id, evidence_kind))
            }
            _ => None,
        })
        .collect();
    assert_eq!(late.len(), 2);
    assert!(late.iter().all(|(id, _)| *id == receipt));
    assert!(late.iter().any(|(_, kind)| kind == "block_air"));
    assert!(late.iter().any(|(_, kind)| kind == "block_changed_ack"));
}

#[test]
fn evidence_on_the_deadline_tick_still_confirms() {
    let mut h = Harness::new("deadline");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 2, 0).with_window(5));
    h.observe_at(5, block_update(pos, BlockState::AIR));
    h.observe_at(5, block_ack(2));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn evidence_after_the_window_without_advance_is_late_not_confirmed() {
    let mut h = Harness::new("late-no-advance");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 3, 0).with_window(5));
    // The caller forgot to advance; the packet itself must reveal that the
    // window has closed.
    h.observe_at(6, block_update(pos, BlockState::AIR));
    h.observe_at(7, block_ack(3));
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
    let late = h
        .events()
        .into_iter()
        .filter(|event| {
            matches!(
                &event.event,
                EventKind::LateEvidence { receipt_id, .. } if *receipt_id == receipt
            )
        })
        .count();
    assert_eq!(late, 2);
}

#[test]
fn evidence_before_the_start_tick_is_ignored() {
    let mut h = Harness::new("before-start");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 10;
    let receipt = h.register(Intent::dig(1, pos, 4, 10));
    h.observe_at(9, block_update(pos, BlockState::AIR));
    h.observe_at(9, block_ack(4));
    // The matching ack after the start cannot confirm an air block that was
    // already air before the intent began.
    h.observe_at(10, block_ack(4));
    h.advance_at(50);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
}

#[test]
fn a_block_update_with_a_non_matching_ack_is_unconfirmed() {
    let mut h = Harness::new("ack-mismatch");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 10, 0));
    h.observe_at(1, block_update(pos, BlockState::AIR));
    h.observe_at(2, block_ack(9));
    h.advance_at(40);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
}

#[test]
fn preemption_settles_preempted() {
    let mut h = Harness::new("preempt");
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, BlockPos::new(0, 64, 0), 1, 0));
    assert!(h.engine.preempt(&mut h.writer, receipt, 1, h.ts_ns).unwrap());
    assert_eq!(h.outcome(receipt), Some(Outcome::Preempted));
    assert!(!h.engine.is_open(receipt));
    assert!(!h.engine.preempt(&mut h.writer, receipt, 2, h.ts_ns).unwrap());
}

#[test]
fn place_confirms_on_the_placed_state_plus_ack() {
    let mut h = Harness::new("place");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::place(1, pos, "stone", 7, 0));
    h.observe_at(1, block_update(pos, stone()));
    h.observe_at(2, block_ack(7));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));

    let wrong = BlockPos::new(1, 64, 0);
    let second = h.register(Intent::place(2, wrong, "stone", 8, 2));
    h.observe_at(3, block_update(wrong, dirt()));
    h.observe_at(4, block_ack(8));
    h.advance_at(30);
    assert_eq!(h.outcome(second), Some(Outcome::Unconfirmed));
}

#[test]
fn inventory_delta_confirms_and_late_inventory_is_logged() {
    let mut h = Harness::new("inventory");
    h.world
        .apply_clientbound_play(0, &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]));
    h.tick = 0;
    let receipt = h.register(Intent::inventory(1, "craft", "stone", 1, 0).with_window(5));
    h.observe_at(1, content(0, 1, vec![ItemStack::new(ItemKind::Stone, 6)]));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));

    let second = h.register(Intent::inventory(2, "craft", "stone", 1, 6).with_window(5));
    h.advance_at(11);
    assert_eq!(h.outcome(second), Some(Outcome::Unconfirmed));
    h.observe_at(12, content(0, 2, vec![ItemStack::new(ItemKind::Stone, 7)]));
    let late = h
        .events()
        .into_iter()
        .filter(|event| {
            matches!(
                &event.event,
                EventKind::LateEvidence { receipt_id, evidence_kind }
                    if *receipt_id == second && evidence_kind == "inventory_delta"
            )
        })
        .count();
    assert_eq!(late, 1);
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

#[test]
fn a_stale_pre_start_inventory_cannot_confirm() {
    let mut h = Harness::new("stale-inventory");
    h.world
        .apply_clientbound_play(0, &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]));
    h.tick = 10;
    let receipt = h.register(Intent::inventory(1, "pickup", "stone", 1, 10));
    // The packet is stamped before the start but reaches us after registration.
    h.observe_at(9, content(0, 1, vec![ItemStack::new(ItemKind::Stone, 6)]));
    // A later packet that does not change the count is not a new delta either.
    h.observe_at(11, content(0, 2, vec![ItemStack::new(ItemKind::Stone, 6)]));
    h.advance_at(30);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
}

#[test]
fn movement_is_unrefuted_without_a_correction_and_refuted_with_one() {
    let mut h = Harness::new("movement");
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

    let corrected = h.register(Intent::movement(2, "move_to", 6).with_window(5));
    h.observe_at(7, player_position());
    assert_eq!(h.outcome(corrected), Some(Outcome::Unconfirmed));
}

#[test]
fn slot_click_confirms_without_a_correction_and_refutes_with_one() {
    let mut h = Harness::new("slot-click");
    h.world
        .apply_clientbound_play(0, &content_packet(0, 1, vec![ItemStack::new(ItemKind::Stone, 1)]));
    h.tick = 0;
    let receipt = h.register(Intent::slot_click(1, 0, 0, 1, 0).with_window(5));
    h.observe_at(1, click(0, 1, 0));
    assert_eq!(h.outcome(receipt), None);
    h.advance_at(5);
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));

    let corrected = h.register(Intent::slot_click(2, 0, 0, 2, 6).with_window(5));
    h.observe_at(7, click(0, 2, 0));
    h.observe_at(8, set_slot(0, 3, 0, ItemStack::new(ItemKind::Stone, 1)));
    assert_eq!(h.outcome(corrected), Some(Outcome::Unconfirmed));

    let missing = h.register(Intent::slot_click(3, 0, 0, 4, 9).with_window(5));
    h.advance_at(14);
    assert_eq!(h.outcome(missing), Some(Outcome::Unconfirmed));
}

#[test]
fn interpreter_outcomes_map_as_decided() {
    assert_eq!(
        interpreter_outcome(Outcome::Confirmed),
        praxion_interpreter::Outcome::Confirmed
    );
    assert_eq!(
        interpreter_outcome(Outcome::Unconfirmed),
        praxion_interpreter::Outcome::Failed("unconfirmed".to_owned())
    );
    assert_eq!(
        interpreter_outcome(Outcome::Timeout),
        praxion_interpreter::Outcome::Timeout
    );
    assert_eq!(
        interpreter_outcome(Outcome::Preempted),
        praxion_interpreter::Outcome::Preempted
    );
}

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

    fn pick(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Default)]
struct FaultStats {
    actions: u64,
    happened: u64,
    unconfirmed_when_happened: u64,
    false_confirmed: u64,
}

#[test]
fn fault_injection_never_reports_a_false_confirmed() {
    let mut h = Harness::new("faults");
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let mut stats = FaultStats::default();

    for index in 0..10_000u64 {
        // A fresh engine per action keeps the late-evidence scan bounded; the
        // writer keeps one continuous log.
        h.engine = Engine::new();
        h.world.follow(0);
        let start = index * 50 + 10;
        h.tick = start;
        let kind = rng.pick(5);
        let mut happened = rng.chance(70);
        let receipt = match kind {
            0 => {
                let pos = BlockPos::new(index as i32, 64, 0);
                let seq = index as u32 + 1;
                let receipt = h.register(Intent::dig(index, pos, seq, start));
                if happened {
                    if rng.chance(15) {
                        // Dropped ack: the client saw the block change but the
                        // server's acknowledgement never arrived.
                        h.observe_at(start + 1, block_update(pos, BlockState::AIR));
                    } else if rng.chance(10) {
                        // A stale acknowledgement for an earlier sequence.
                        h.observe_at(start + 1, block_update(pos, BlockState::AIR));
                        h.observe_at(start + 1, block_ack(seq - 1));
                    } else {
                        if rng.chance(50) {
                            h.observe_at(start + 1, block_ack(seq));
                            h.observe_at(start + 1, block_update(pos, BlockState::AIR));
                        } else {
                            h.observe_at(start + 1, block_update(pos, BlockState::AIR));
                            h.observe_at(start + 1, block_ack(seq));
                        }
                        if rng.chance(20) {
                            // Duplicated ack.
                            h.observe_at(start + 1, block_ack(seq));
                        }
                    }
                } else {
                    // Refused dig: the server leaves the block in place.
                    h.observe_at(start + 1, block_update(pos, stone()));
                    h.observe_at(start + 2, block_ack(seq));
                }
                receipt
            }
            1 => {
                let pos = BlockPos::new(index as i32, 65, 0);
                let seq = index as u32 + 1;
                let receipt = h.register(Intent::place(index, pos, "stone", seq, start));
                if happened {
                    if rng.chance(15) {
                        h.observe_at(start + 1, block_update(pos, stone()));
                    } else {
                        h.observe_at(start + 1, block_update(pos, stone()));
                        h.observe_at(start + 1, block_ack(seq));
                        if rng.chance(20) {
                            h.observe_at(start + 1, block_ack(seq));
                        }
                    }
                } else {
                    // Refused place: a different block remains.
                    h.observe_at(start + 1, block_update(pos, dirt()));
                    h.observe_at(start + 2, block_ack(seq));
                }
                receipt
            }
            2 => {
                let item = ItemKind::from_u32(1 + (index % 1000) as u32).unwrap();
                h.world.apply_clientbound_play(
                    0,
                    &content_packet(0, 0, vec![ItemStack::new(item, 5)]),
                );
                let name = item.to_str();
                let receipt = h.register(Intent::inventory(index, "pickup", name, 1, start));
                if happened {
                    h.observe_at(start + 1, content(0, 1, vec![ItemStack::new(item, 6)]));
                } else if rng.chance(50) {
                    // A stale inventory packet stamped before the start.
                    h.observe_at(start.saturating_sub(1), content(0, 1, vec![ItemStack::new(item, 6)]));
                }
                receipt
            }
            3 => {
                // Movement is client-authoritative: the action happens unless
                // the server corrects it, so the ground truth is always "it
                // happened".
                happened = true;
                let receipt = h.register(Intent::movement(index, "move_to", start));
                if rng.chance(30) {
                    h.observe_at(start + 1, player_position());
                }
                receipt
            }
            _ => {
                h.world.apply_clientbound_play(
                    0,
                    &content_packet(0, 1, vec![ItemStack::new(ItemKind::Stone, 1)]),
                );
                let receipt = h.register(Intent::slot_click(index, 0, 0, 1, start));
                if happened {
                    h.observe_at(start + 1, click(0, 1, 0));
                } else if rng.chance(50) {
                    h.observe_at(start + 1, click(0, 1, 0));
                    h.observe_at(start + 2, set_slot(0, 2, 0, ItemStack::new(ItemKind::Stone, 1)));
                }
                receipt
            }
        };

        h.advance_at(start + 49);
        let outcome = h.outcome(receipt).expect("every action settles");
        stats.actions += 1;
        if happened {
            stats.happened += 1;
            if outcome == Outcome::Unconfirmed {
                stats.unconfirmed_when_happened += 1;
            }
        }
        if outcome == Outcome::Confirmed && !happened {
            stats.false_confirmed += 1;
        }
    }

    let rate = if stats.happened == 0 {
        0.0
    } else {
        stats.unconfirmed_when_happened as f64 / stats.happened as f64
    };
    eprintln!(
        "fault injection: actions={} happened={} false_confirmed={} unconfirmed_when_happened={} rate={:.3}",
        stats.actions,
        stats.happened,
        stats.false_confirmed,
        stats.unconfirmed_when_happened,
        rate
    );
    assert_eq!(stats.false_confirmed, 0, "a confirmed receipt must have happened");
    assert_eq!(stats.actions, 10_000);
}

#[test]
fn late_retention_is_positive() {
    assert!(LATE_RETENTION_TICKS > 0);
}
