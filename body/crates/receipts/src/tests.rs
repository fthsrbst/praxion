use azalea_block::BlockState;
use azalea_core::entity_id::{MinecraftEntityId, OptionalEntityId};
use azalea_core::position::{BlockPos, ChunkSectionBlockPos, ChunkSectionPos};
use azalea_protocol::packets::game::{
    ClientboundContainerSetSlot, ClientboundDamageEvent, ClientboundGamePacket,
    ClientboundOpenScreen, ClientboundSectionBlocksUpdate,
    c_section_blocks_update::BlockStateWithPosition,
};
use azalea_registry::Registry;
use azalea_registry::builtin::{ItemKind, MenuKind};
use praxion_decoder::Packet;
use praxion_eventlog::{EventKind, Outcome};
use praxion_world::ItemStack;

use crate::{Engine, Intent, LATE_RETENTION_TICKS, ReceiptId, interpreter_outcome};

#[path = "../tests/common/mod.rs"]
mod common;

use common::*;

/// The 26.3 damage packet a hit produces (the live capture showed this, not a
/// hurt animation, for a NoAI zombie).
fn damage_event(id: i32) -> Packet {
    Packet::PlayClientbound(ClientboundGamePacket::DamageEvent(ClientboundDamageEvent {
        entity_id: MinecraftEntityId(id),
        source_type_id: 0,
        source_cause_id: OptionalEntityId(None),
        source_direct_id: OptionalEntityId(None),
        source_position: None,
    }))
}

fn section_update(pos: BlockPos, state: BlockState) -> Packet {
    let section = ChunkSectionPos::new(pos.x >> 4, pos.y >> 4, pos.z >> 4);
    let local =
        ChunkSectionBlockPos::new((pos.x & 15) as u8, (pos.y & 15) as u8, (pos.z & 15) as u8);
    Packet::PlayClientbound(ClientboundGamePacket::SectionBlocksUpdate(
        ClientboundSectionBlocksUpdate {
            section_pos: section,
            states: vec![BlockStateWithPosition { pos: local, state }],
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
            EventKind::ReceiptSettled {
                receipt_id,
                outcome,
                evidence,
                ..
            } => Some((*receipt_id, *outcome, evidence.clone())),
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
            EventKind::LateEvidence {
                receipt_id,
                evidence_kind,
            } => Some((receipt_id, evidence_kind)),
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
    assert!(
        h.engine
            .preempt(&mut h.writer, receipt, 1, h.ts_ns)
            .unwrap()
    );
    assert_eq!(h.outcome(receipt), Some(Outcome::Preempted));
    assert!(!h.engine.is_open(receipt));
    assert!(
        !h.engine
            .preempt(&mut h.writer, receipt, 2, h.ts_ns)
            .unwrap()
    );
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
fn a_same_batch_block_update_confirms_with_the_pre_action_state() {
    // The server answered the use in the tick batch that registered the
    // intent, so the world already holds the placed block when the receipt is
    // created. The explicit pre-action state keeps the update as evidence.
    let mut h = Harness::new("place-same-batch");
    let pos = BlockPos::new(0, 64, 0);
    h.apply(&block_update(pos, stone()));
    h.tick = 0;
    let receipt =
        h.register(Intent::place(1, pos, "stone", 7, 0).with_initial_block(Some(BlockState::AIR)));
    h.observe_at(1, block_update(pos, stone()));
    h.observe_at(1, block_ack(7));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));

    // Placing the state that was already there is still not a change.
    h.apply(&block_update(pos, dirt()));
    let unchanged =
        h.register(Intent::place(2, pos, "dirt", 8, 1).with_initial_block(Some(dirt())));
    h.observe_at(2, block_update(pos, dirt()));
    h.observe_at(2, block_ack(8));
    h.advance_at(30);
    assert_eq!(h.outcome(unchanged), Some(Outcome::Unconfirmed));
}

#[test]
fn a_place_update_before_registration_still_confirms_with_its_ack() {
    // The live driver registers a place from the previous tick's report; the
    // server's block update can already be in the world. The actuator's
    // pre-click air snapshot plus the current state is the update evidence.
    let mut h = Harness::new("place-before-register");
    let pos = BlockPos::new(0, 64, 0);
    h.tick = 0;
    let receipt = h.register(
        Intent::place(1, pos, "stone", 7, 0)
            .with_initial_block(Some(BlockState::AIR))
            .with_observed_block(Some(stone())),
    );
    h.observe_at(1, block_ack(7));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));

    // The same observed state with no proof it was air before the click is
    // not evidence, so a bare ack cannot confirm it.
    let unchanged = h.register(
        Intent::place(2, pos, "stone", 8, 1)
            .with_initial_block(Some(stone()))
            .with_observed_block(Some(stone())),
    );
    h.observe_at(2, block_ack(8));
    h.advance_at(30);
    assert_eq!(h.outcome(unchanged), Some(Outcome::Unconfirmed));
}

#[test]
fn a_dig_update_before_registration_still_confirms_with_its_ack() {
    // Captured from a live run (seed 101, step 9c): the dig at (-352, 74,
    // -144) started on tick 2641 with a stone pre-click snapshot, the server's
    // block update was already in the world when the intent registered on
    // 2642, and the ack (seq 217) arrived on 2643. The old receipt saw only
    // the ack and settled `unconfirmed(missing_block)` although the block was
    // air.
    let mut h = Harness::new("dig-before-register");
    let pos = BlockPos::new(-352, 74, -144);
    h.apply(&block_update(pos, BlockState::AIR));
    h.tick = 2642;
    let receipt = h.register(
        Intent::dig(15, pos, 217, 2641)
            .with_window(160)
            .with_initial_block(Some(stone()))
            .with_observed_block(Some(BlockState::AIR)),
    );
    h.observe_at(2643, block_ack(217));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
    let evidence: Vec<_> = h
        .events()
        .into_iter()
        .filter_map(|event| match event.event {
            EventKind::ReceiptSettled { evidence, .. } => Some(evidence),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        evidence
            .iter()
            .any(|hit| hit.kind == "block_air" && hit.tick == 2641),
        "the pre-click stone plus the observed air is the block evidence: {evidence:?}"
    );
    assert!(
        evidence
            .iter()
            .any(|hit| hit.kind == "block_changed_ack" && hit.tick == 2643),
        "the ack still has to arrive: {evidence:?}"
    );

    // A dig whose block was already air before the click is not evidence.
    let already = h.register(
        Intent::dig(16, pos, 218, 2644)
            .with_window(160)
            .with_initial_block(Some(BlockState::AIR))
            .with_observed_block(Some(BlockState::AIR)),
    );
    h.observe_at(2645, block_ack(218));
    h.advance_at(2820);
    assert_eq!(h.outcome(already), Some(Outcome::Unconfirmed));
}

#[test]
fn the_live_place_order_confirms_with_the_observed_state_and_the_same_batch_ack() {
    // The live driver registers the place from the previous tick's applied
    // report, after the world feed has already applied the block update. The
    // receipt therefore starts with the actuator's pre-click air snapshot and
    // the placed state the driver observed, then folds the same batch's block
    // update and ack. This is the exact order captured from a stocked run.
    let mut h = Harness::new("place-live-order");
    let pos = BlockPos::new(-276, 71, -111);
    h.apply(&block_update(pos, stone()));
    h.tick = 303;
    let receipt = h.register(
        Intent::place(1, pos, "stone", 7, 300)
            .with_initial_block(Some(BlockState::AIR))
            .with_observed_block(Some(stone())),
    );
    h.observe_at(303, block_update(pos, stone()));
    h.observe_at(303, block_ack(7));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));

    let evidence: Vec<_> = h
        .events()
        .into_iter()
        .filter_map(|event| match event.event {
            EventKind::ReceiptSettled { evidence, .. } => Some(evidence),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(
        evidence
            .iter()
            .any(|hit| hit.kind == "block_state" && hit.tick == 300),
        "block evidence comes from the pre-action air plus the observed placed state: {evidence:?}"
    );
    assert!(
        evidence
            .iter()
            .any(|hit| hit.kind == "block_changed_ack" && hit.tick == 303),
        "the same batch's ack completes the place: {evidence:?}"
    );
}

#[test]
fn attack_confirms_on_a_damage_event_for_the_target() {
    let mut h = Harness::new("attack-damage-event");
    h.tick = 0;
    let receipt = h.register(Intent::attack(1, 63, 0));
    h.observe_at(1, damage_event(63));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));

    let other = h.register(Intent::attack(2, 64, 0));
    h.observe_at(2, damage_event(63));
    h.advance_at(30);
    assert_eq!(h.outcome(other), Some(Outcome::Unconfirmed));
}

#[test]
fn a_craft_result_in_the_open_menu_player_slots_counts_as_storage() {
    // A crafting table's player inventory lives in the menu's last 36 slots,
    // and `World::inventory` is not updated while a menu is open. The storage
    // receipt must read the menu's copy.
    let mut h = Harness::new("craft-open-menu");
    h.apply(&Packet::PlayClientbound(ClientboundGamePacket::OpenScreen(
        ClientboundOpenScreen {
            container_id: 1,
            menu_type: MenuKind::Crafting,
            title: "Crafting".into(),
        },
    )));
    h.apply(&content(1, 0, vec![ItemStack::Empty; 46]));
    h.tick = 0;
    let receipt = h.register(Intent::storage_inventory(
        1,
        "craft",
        "wooden_pickaxe",
        1,
        0,
    ));
    h.observe_at(
        1,
        set_slot(1, 1, 45, ItemStack::new(ItemKind::WoodenPickaxe, 1)),
    );
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn inventory_delta_confirms_and_late_inventory_is_logged() {
    let mut h = Harness::new("inventory");
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]),
    );
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

#[test]
fn a_tag_inventory_receipt_accepts_any_item_in_the_tag() {
    // The data-driven skills craft `#planks`; the receipt must count every
    // wood type in the generated item tag, not look for a literal `#planks`.
    let mut h = Harness::new("inventory-tag");
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 0, vec![ItemStack::new(ItemKind::OakPlanks, 4)]),
    );
    h.tick = 0;
    let receipt = h.register(Intent::inventory(1, "craft", "#planks", 4, 0));
    h.observe_at(
        1,
        content(0, 1, vec![ItemStack::new(ItemKind::OakPlanks, 8)]),
    );
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));

    let other = h.register(Intent::inventory(2, "craft", "#planks", 1, 1));
    h.observe_at(2, content(0, 2, vec![ItemStack::new(ItemKind::Stick, 8)]));
    h.advance_at(30);
    assert_eq!(
        h.outcome(other),
        Some(Outcome::Unconfirmed),
        "a stick is not a plank"
    );
}

#[test]
fn a_stale_pre_start_inventory_cannot_confirm() {
    let mut h = Harness::new("stale-inventory");
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]),
    );
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
    let note = h.events().into_iter().find_map(|event| match event.event {
        EventKind::ReceiptSettled {
            receipt_id, note, ..
        } if receipt_id == receipt => note,
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
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 1, vec![ItemStack::new(ItemKind::Stone, 1)]),
    );
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
                    h.observe_at(
                        start.saturating_sub(1),
                        content(0, 1, vec![ItemStack::new(item, 6)]),
                    );
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
                    h.observe_at(
                        start + 2,
                        set_slot(0, 2, 0, ItemStack::new(ItemKind::Stone, 1)),
                    );
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
        stats.actions, stats.happened, stats.false_confirmed, stats.unconfirmed_when_happened, rate
    );
    assert_eq!(
        stats.false_confirmed, 0,
        "a confirmed receipt must have happened"
    );
    assert_eq!(stats.actions, 10_000);
}

#[test]
fn late_retention_is_positive() {
    assert!(LATE_RETENTION_TICKS > 0);
}

#[test]
fn dig_revert_inside_the_window_clears_the_air_hit() {
    let mut h = Harness::new("revert-inside");
    let pos = BlockPos::new(10, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 5, 0).with_window(10));
    h.observe_at(1, block_update(pos, BlockState::AIR));
    h.observe_at(2, block_update(pos, stone()));
    h.observe_at(3, block_ack(5));
    h.advance_at(10);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
    let note = h.events().into_iter().find_map(|event| match event.event {
        EventKind::ReceiptSettled {
            receipt_id, note, ..
        } if receipt_id == receipt => note,
        _ => None,
    });
    assert_eq!(note.as_deref(), Some("missing_block"));
}

#[test]
fn dig_revert_after_settlement_is_late_evidence() {
    let mut h = Harness::new("revert-late");
    let pos = BlockPos::new(11, 64, 0);
    h.tick = 0;
    let receipt = h.register(Intent::dig(1, pos, 5, 0).with_window(5));
    h.observe_at(1, block_update(pos, BlockState::AIR));
    h.observe_at(2, block_ack(5));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
    h.advance_at(5);
    h.observe_at(6, block_update(pos, stone()));
    // The settled outcome never reopens, but the log records the refutation.
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
    let late = h
        .events()
        .into_iter()
        .filter(|event| {
            matches!(
                &event.event,
                EventKind::LateEvidence { receipt_id, evidence_kind }
                    if *receipt_id == receipt && evidence_kind == "block_reverted"
            )
        })
        .count();
    assert_eq!(late, 1);
}

#[test]
fn craft_is_attributed_to_the_click_that_took_the_result() {
    let mut h = Harness::new("craft-attribution");
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]),
    );
    h.tick = 0;
    let receipt = h.register(Intent::craft(1, "stone", 1, 0, 10, 0).with_window(10));
    assert_eq!(
        h.events().into_iter().find_map(|event| match event.event {
            EventKind::IntentStarted {
                intent_id,
                attribution,
                ..
            } if intent_id == 1 => {
                Some(attribution)
            }
            _ => None,
        }),
        Some(praxion_eventlog::Attribution::SlotClick {
            state_id: 10,
            slot: 0
        })
    );
    // A pickup of the same item before the click's state id is unrelated; it
    // must not confirm the craft and must not count toward its delta.
    h.observe_at(1, content(0, 9, vec![ItemStack::new(ItemKind::Stone, 6)]));
    assert_eq!(h.outcome(receipt), None);
    // The result arrives in a packet at the click's state id: this is the
    // crafted delta.
    h.observe_at(2, content(0, 10, vec![ItemStack::new(ItemKind::Stone, 7)]));
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn craft_before_the_click_state_id_does_not_confirm() {
    let mut h = Harness::new("craft-before-click");
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]),
    );
    h.tick = 0;
    let receipt = h.register(Intent::craft(1, "stone", 1, 0, 10, 0).with_window(10));
    h.observe_at(1, content(0, 9, vec![ItemStack::new(ItemKind::Stone, 6)]));
    h.advance_at(10);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
}

#[test]
fn container_packet_without_a_pre_click_slot_is_not_a_correction() {
    let mut h = Harness::new("missing-slot");
    h.tick = 0;
    // Container 7 is not open, so no pre-click slot can be captured.
    let receipt = h.register(Intent::slot_click(1, 7, 0, 1, 0).with_window(5));
    h.observe_at(1, click(7, 1, 0));
    // A newer full-content packet must be ignored rather than read as a
    // revert of a slot we never saw.
    h.observe_at(2, content(7, 2, vec![ItemStack::new(ItemKind::Stone, 1)]));
    assert_eq!(h.outcome(receipt), None);
    h.advance_at(5);
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
}

fn settle_movement_batch(h: &mut Harness, count: u64) {
    for index in 0..count {
        h.register(Intent::movement(index, "move_to", 0).with_window(0));
    }
    h.advance_at(0);
}

fn ns_per_observe(h: &mut Harness, iterations: u64) -> f64 {
    let packet = block_ack(u32::MAX);
    let started = std::time::Instant::now();
    for _ in 0..iterations {
        h.engine
            .observe(&mut h.writer, 7_001, h.ts_ns, &packet, &h.world)
            .unwrap();
        h.ts_ns += 1;
    }
    started.elapsed().as_nanos() as f64 / iterations as f64
}

#[test]
fn one_hundred_thousand_settled_receipts_do_not_slow_the_packet_scan() {
    let iterations = 100_000u64;

    let mut small = Harness::new("retention-1k");
    settle_movement_batch(&mut small, 1_000);
    small.advance_at(7_000);
    assert_eq!(small.engine.settled_count(), 0);
    assert_eq!(small.outcome(0), Some(Outcome::Expired));
    assert_eq!(small.engine.outcome_for_intent(0), Some(Outcome::Expired));
    let small_ns = ns_per_observe(&mut small, iterations);

    let mut large = Harness::new("retention-100k");
    settle_movement_batch(&mut large, 100_000);
    large.advance_at(7_000);
    assert_eq!(large.engine.settled_count(), 0);
    assert_eq!(large.outcome(0), Some(Outcome::Expired));
    assert_eq!(large.outcome(99_999), Some(Outcome::Expired));
    assert_eq!(
        large.engine.outcome_for_intent(99_999),
        Some(Outcome::Expired)
    );
    let large_ns = ns_per_observe(&mut large, iterations);

    eprintln!(
        "retention perf: 1k settled {small_ns:.1} ns/observe, 100k settled {large_ns:.1} ns/observe ({iterations} observations each)"
    );
    assert!(
        large_ns <= small_ns * 4.0 + 100.0,
        "per-packet cost grew with total settled receipts: 1k={small_ns:.1} ns, 100k={large_ns:.1} ns"
    );
}

#[test]
fn outcome_lookups_expire_at_the_retention_boundary() {
    let mut h = Harness::new("retention-boundary");
    h.tick = 0;
    let receipt = h.register(Intent::movement(7, "move_to", 0).with_window(0));
    h.advance_at(0);
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
    assert_eq!(h.engine.outcome_for_intent(7), Some(Outcome::Confirmed));

    // The retention window includes its final tick.
    h.advance_at(LATE_RETENTION_TICKS);
    assert_eq!(h.outcome(receipt), Some(Outcome::Confirmed));
    assert_eq!(h.engine.outcome_for_intent(7), Some(Outcome::Confirmed));

    h.advance_at(LATE_RETENTION_TICKS + 1);
    assert_eq!(h.outcome(receipt), Some(Outcome::Expired));
    assert_eq!(h.engine.outcome_for_intent(7), Some(Outcome::Expired));
    assert_eq!(h.engine.settled_count(), 0);
}
