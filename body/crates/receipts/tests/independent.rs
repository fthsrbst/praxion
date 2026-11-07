use azalea_core::position::BlockPos;
use azalea_registry::builtin::ItemKind;
use praxion_decoder::Packet;
use praxion_eventlog::{EventKind, Outcome};
use praxion_world::ItemStack;

use praxion_receipts::{Engine, Intent, ReceiptId};

mod common;

use common::*;

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
fn unrelated_pickup_does_not_confirm_craft() {
    let mut h = Harness::new("craft-unrelated");
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 0, vec![ItemStack::new(ItemKind::Stone, 5)]),
    );
    h.tick = 0;
    let receipt = h.register(Intent::inventory(1, "craft", "stone", 1, 0).with_window(10));
    h.observe_at(
        1,
        content(
            0,
            1,
            vec![
                ItemStack::new(ItemKind::Stone, 5),
                ItemStack::new(ItemKind::Dirt, 1),
            ],
        ),
    );
    h.advance_at(10);
    assert_eq!(h.outcome(receipt), Some(Outcome::Unconfirmed));
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
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 1, vec![ItemStack::new(ItemKind::Stone, 1)]),
    );
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

        for (i, observed) in air_observed.iter_mut().enumerate() {
            if rng.chance(70) {
                *observed = true;
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

// --- Step 8b adversarial receipts tests ---

#[test]
fn place_early_block_update_different_position_does_not_confirm() {
    let mut h = Harness::new("place-early-diff-pos");
    let pos = BlockPos::new(0, 64, 0);
    let other = BlockPos::new(1, 64, 0);
    let seq = 5u32;
    h.tick = 0;
    // A block update for a *different* cell arrives before the intent starts.
    h.observe_at(0, block_update(other, stone()));
    h.tick = 5;
    let receipt = h.register(Intent::place(1, pos, "stone", seq, 5).with_window(10));
    // Only the ack arrives; the placed block was never observed at `pos`.
    h.observe_at(6, block_ack(seq));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn place_early_block_update_different_block_does_not_confirm() {
    let mut h = Harness::new("place-early-diff-block");
    let pos = BlockPos::new(2, 64, 0);
    let seq = 6u32;
    h.tick = 0;
    // A different block lands in the target cell before registration.
    h.observe_at(0, block_update(pos, dirt()));
    h.tick = 5;
    let receipt = h.register(Intent::place(2, pos, "stone", seq, 5).with_window(10));
    h.observe_at(6, block_ack(seq));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn place_observed_carry_stale_initial_not_air_does_not_confirm() {
    let mut h = Harness::new("place-carry-stale-initial");
    let pos = BlockPos::new(3, 64, 0);
    let seq = 7u32;
    h.tick = 0;
    let receipt = h.register(
        Intent::place(3, pos, "stone", seq, 0)
            .with_window(10)
            .with_initial_block(Some(stone()))
            .with_observed_block(Some(stone())),
    );
    h.observe_at(1, block_ack(seq));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn place_observed_carry_wrong_observed_block_does_not_confirm() {
    let mut h = Harness::new("place-carry-wrong-observed");
    let pos = BlockPos::new(4, 64, 0);
    let seq = 8u32;
    h.tick = 0;
    let receipt = h.register(
        Intent::place(4, pos, "stone", seq, 0)
            .with_window(10)
            .with_initial_block(Some(air()))
            .with_observed_block(Some(dirt())),
    );
    h.observe_at(1, block_ack(seq));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn tag_inventory_non_tag_arrival_does_not_confirm() {
    let mut h = Harness::new("tag-non-tag");
    h.world
        .apply_clientbound_play(0, &content_packet(0, 0, vec![]));
    h.tick = 0;
    let receipt = h.register(Intent::inventory(1, "eat", "#planks", 1, 0).with_window(10));
    // Only a non-tag item arrives.
    h.observe_at(1, content(0, 1, vec![ItemStack::new(ItemKind::Stone, 1)]));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn tag_inventory_leave_and_arrive_same_tick_does_not_confirm() {
    let mut h = Harness::new("tag-swap-same-tick");
    h.world.apply_clientbound_play(
        0,
        &content_packet(0, 0, vec![ItemStack::new(ItemKind::OakPlanks, 1)]),
    );
    h.tick = 0;
    let receipt = h.register(Intent::inventory(2, "eat", "#planks", 1, 0).with_window(10));
    // One plank leaves and a different plank arrives in the same content packet:
    // net count unchanged, so no delta evidence.
    h.observe_at(
        1,
        content(0, 1, vec![ItemStack::new(ItemKind::OakPlanks, 1)]),
    );
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn place_same_batch_update_without_matching_ack_does_not_confirm() {
    let mut h = Harness::new("place-piston");
    let pos = BlockPos::new(5, 64, 0);
    let seq = 9u32;
    h.tick = 0;
    let receipt = h.register(
        Intent::place(5, pos, "stone", seq, 0)
            .with_window(10)
            .with_initial_block(Some(air())),
    );
    // A same-batch update turns the cell into the placed block (piston/other
    // player), but no ack for our seq arrives.
    h.observe_at(1, block_update(pos, stone()));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn place_same_batch_update_with_lower_ack_does_not_confirm() {
    let mut h = Harness::new("place-lower-ack");
    let pos = BlockPos::new(6, 64, 0);
    let seq = 10u32;
    h.tick = 0;
    let receipt = h.register(
        Intent::place(6, pos, "stone", seq, 0)
            .with_window(10)
            .with_initial_block(Some(air())),
    );
    h.observe_at(1, block_update(pos, stone()));
    h.observe_at(1, block_ack(1));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn dig_same_batch_air_update_without_matching_ack_does_not_confirm() {
    let mut h = Harness::new("dig-piston");
    let pos = BlockPos::new(7, 64, 0);
    let seq = 11u32;
    h.tick = 0;
    let receipt = h.register(
        Intent::dig(7, pos, seq, 0)
            .with_window(10)
            .with_initial_block(Some(stone())),
    );
    // The block becomes air from a piston in the same batch.
    h.observe_at(1, block_update(pos, air()));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
}

#[test]
fn dig_same_batch_air_update_with_lower_ack_does_not_confirm() {
    let mut h = Harness::new("dig-lower-ack");
    let pos = BlockPos::new(8, 64, 0);
    let seq = 12u32;
    h.tick = 0;
    let receipt = h.register(
        Intent::dig(8, pos, seq, 0)
            .with_window(10)
            .with_initial_block(Some(stone())),
    );
    h.observe_at(1, block_update(pos, air()));
    h.observe_at(1, block_ack(1));
    h.advance_at(10);
    assert_ne!(h.outcome(receipt), Some(Outcome::Confirmed));
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
