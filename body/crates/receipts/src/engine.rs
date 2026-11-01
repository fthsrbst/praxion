//! Evidence matching and receipt settlement.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use azalea_core::position::{ChunkSectionBlockPos, ChunkSectionPos};
use azalea_protocol::packets::game::{ClientboundGamePacket, ServerboundGamePacket};
use azalea_registry::builtin::ItemKind;
use praxion_decoder::Packet;
use praxion_eventlog::{Attribution, EvidenceRef, Outcome, Writer};
use praxion_world::{BlockDescription, BlockPos, World};

use crate::intent::{Expect, Intent, IntentId, ReceiptId};

/// How long after settlement a receipt's contract stays available for the
/// late-evidence check. Beyond this the receipt is still replayable from the
/// event log and its outcome is still readable, but the engine no longer scans
/// it on every packet. 6000 ticks is five minutes of play.
pub const LATE_RETENTION_TICKS: u64 = 6_000;

/// A settlement as seen from outside the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settlement {
    pub receipt_id: ReceiptId,
    pub intent_id: IntentId,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SlotItem {
    kind: ItemKind,
    count: i32,
}

impl SlotItem {
    const EMPTY: Self = Self { kind: ItemKind::Air, count: 0 };

    fn of(stack: &praxion_world::ItemStack) -> Self {
        Self { kind: stack.kind(), count: stack.count() }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BlockSnapshot {
    state: azalea_block::BlockState,
}

#[derive(Clone, Debug)]
struct Receipt {
    id: ReceiptId,
    intent: Intent,
    initial_block: Option<BlockSnapshot>,
    baseline_count: i64,
    inventory_last: i64,
    slot_before: Option<SlotItem>,
    block_hit: Option<EvidenceRef>,
    ack_hit: Option<EvidenceRef>,
    inventory_hit: Option<EvidenceRef>,
    click_hit: Option<EvidenceRef>,
    hurt_hit: Option<EvidenceRef>,
    fact_hit: Option<EvidenceRef>,
    correction_hit: Option<EvidenceRef>,
}

#[derive(Clone, Debug)]
struct Settled {
    receipt_id: ReceiptId,
    intent_id: IntentId,
    outcome: Outcome,
    /// The last tick at which this receipt still takes part in the
    /// late-evidence scan. After this it is dropped from the queue and from
    /// the outcome maps.
    late_until: u64,
    /// Monotonic settlement order, used to keep the per-intent outcome map from
    /// removing a newer settlement when an older one expires.
    order: u64,
    receipt: Receipt,
}

/// A compact outcome answer kept in the O(1) lookup maps.
#[derive(Clone, Copy, Debug)]
struct StoredOutcome {
    outcome: Outcome,
    late_until: u64,
    order: u64,
}

impl Receipt {
    fn new(id: ReceiptId, intent: Intent, world: &World) -> Self {
        let initial_block = match &intent.expect {
            Expect::BlockBecomesAir { pos } | Expect::BlockState { pos, .. } => {
                world.block_state(*pos).map(|state| BlockSnapshot { state })
            }
            _ => None,
        };
        let baseline_count = match &intent.expect {
            Expect::Inventory { item, .. } => item_count(world, item),
            _ => 0,
        };
        let slot_before = match &intent.expect {
            Expect::SlotClick { container_id, slot, .. } => {
                slot_item(world, *container_id, *slot)
            }
            _ => None,
        };
        Self {
            id,
            intent,
            initial_block,
            baseline_count,
            inventory_last: baseline_count,
            slot_before,
            block_hit: None,
            ack_hit: None,
            inventory_hit: None,
            click_hit: None,
            hurt_hit: None,
            fact_hit: None,
            correction_hit: None,
        }
    }

    /// A packet that arrived before the intent started may still change the
    /// world the intent sees (the decoder reorders by timestamp, and a stale
    /// inventory/slot packet can reach us late). Fold it into the baseline so
    /// it can never become evidence, but never let it settle anything.
    fn observe_before_start(&mut self, packet: &Packet, world: &World) {
        match &self.intent.expect {
            Expect::BlockBecomesAir { pos } | Expect::BlockState { pos, .. } => {
                if let Packet::PlayClientbound(packet) = packet {
                    let state = match packet {
                        ClientboundGamePacket::BlockUpdate(update) if update.pos == *pos => {
                            Some(update.block_state)
                        }
                        ClientboundGamePacket::SectionBlocksUpdate(update) => {
                            update.states.iter().find_map(|entry| {
                                (section_block_pos(update.section_pos, entry.pos) == Some(*pos))
                                    .then_some(entry.state)
                            })
                        }
                        _ => None,
                    };
                    if let Some(state) = state {
                        self.initial_block = Some(BlockSnapshot { state });
                    }
                }
            }
            Expect::Inventory { item, .. } => {
                if is_inventory_packet(packet) {
                    let current = item_count(world, item);
                    self.baseline_count = current;
                    self.inventory_last = current;
                }
            }
            Expect::SlotClick { container_id, slot, .. } => {
                if is_container_state_packet(packet) {
                    self.slot_before = slot_item(world, *container_id, *slot);
                }
            }
            Expect::Unrefuted => {}
            Expect::EntityHurt { .. } | Expect::FactKnown { .. } => {}
        }
    }

    fn observe(
        &mut self,
        packet: &Packet,
        tick: u64,
        world: &World,
    ) -> Option<(Outcome, Option<String>)> {
        match &self.intent.expect {
            Expect::BlockBecomesAir { .. } | Expect::BlockState { .. } => {
                self.observe_block(packet, tick);
                (self.block_hit.is_some() && self.ack_hit.is_some())
                    .then_some((Outcome::Confirmed, None))
            }
            Expect::Inventory { item, delta } => {
                let item = item.clone();
                let delta = *delta;
                let attribution = self.intent.attribution;
                self.observe_inventory(packet, tick, world, &item, delta, attribution)
            }
            Expect::Unrefuted => self.observe_movement(packet, tick),
            Expect::SlotClick { container_id, slot, state_id } => {
                let container_id = *container_id;
                let slot = *slot;
                let state_id = *state_id;
                self.observe_slot(packet, tick, container_id, slot, state_id)
            }
            Expect::EntityHurt { id } => self.observe_entity_hurt(packet, tick, *id),
            // `observe` has no packet evidence; the driver confirms it when its
            // own perception sees the fact.
            Expect::FactKnown { .. } => None,
        }
    }

    fn observe_block(&mut self, packet: &Packet, tick: u64) {
        let Packet::PlayClientbound(packet) = packet else {
            return;
        };
        match packet {
            ClientboundGamePacket::BlockUpdate(update) => {
                if self.block_reverted(update.pos, update.block_state) {
                    // The block is no longer in the state this intent needs.
                    // Clearing the hit means a later ack cannot confirm a dig
                    // whose block the server put back.
                    self.block_hit = None;
                } else if let Some(kind) = self.block_matches(update.pos, update.block_state) {
                    self.record_block(kind, tick);
                }
            }
            ClientboundGamePacket::SectionBlocksUpdate(update) => {
                for entry in &update.states {
                    let Some(pos) = section_block_pos(update.section_pos, entry.pos) else {
                        continue;
                    };
                    if self.block_reverted(pos, entry.state) {
                        self.block_hit = None;
                    } else if let Some(kind) = self.block_matches(pos, entry.state) {
                        self.record_block(kind, tick);
                    }
                }
            }
            ClientboundGamePacket::BlockChangedAck(ack) => {
                if let Attribution::BlockSeq { seq } = self.intent.attribution
                    && ack.seq >= seq
                {
                    self.ack_hit.get_or_insert(EvidenceRef {
                        kind: "block_changed_ack".to_owned(),
                        tick,
                    });
                }
            }
            _ => {}
        }
    }

    fn block_matches(
        &self,
        pos: BlockPos,
        state: azalea_block::BlockState,
    ) -> Option<&'static str> {
        match &self.intent.expect {
            Expect::BlockBecomesAir { pos: target } if *target == pos => {
                if !state.is_air() {
                    return None;
                }
                // A block that was already air at the start is not evidence of
                // this dig; a redundant or stale update must not confirm it.
                if self
                    .initial_block
                    .as_ref()
                    .is_some_and(|block| block.state.is_air())
                {
                    return None;
                }
                Some("block_air")
            }
            Expect::BlockState { pos: target, block } if *target == pos => {
                let name = BlockDescription::of(state).name().to_owned();
                if normalize(&name) != normalize(block) {
                    return None;
                }
                // The state must actually change: placing the same state over
                // itself is not evidence.
                if self
                    .initial_block
                    .as_ref()
                    .is_some_and(|initial| initial.state == state)
                {
                    return None;
                }
                Some("block_state")
            }
            _ => None,
        }
    }

    /// Whether an update at `pos` puts the block back out of the state this
    /// intent needs. For a dig that is any non-air state; for a place it is any
    /// state other than the block that was placed.
    fn block_reverted(&self, pos: BlockPos, state: azalea_block::BlockState) -> bool {
        match &self.intent.expect {
            Expect::BlockBecomesAir { pos: target } => *target == pos && !state.is_air(),
            Expect::BlockState { pos: target, block } => {
                *target == pos
                    && normalize(BlockDescription::of(state).name()) != normalize(block)
            }
            _ => false,
        }
    }

    fn record_block(&mut self, kind: &'static str, tick: u64) {
        self.block_hit.get_or_insert(EvidenceRef { kind: kind.to_owned(), tick });
    }

    fn observe_inventory(
        &mut self,
        packet: &Packet,
        tick: u64,
        world: &World,
        item: &str,
        delta: i64,
        attribution: Attribution,
    ) -> Option<(Outcome, Option<String>)> {
        if !is_inventory_packet(packet) || !world.status().is_valid() {
            return None;
        }
        let current = item_count(world, item);
        let changed = current != self.inventory_last;
        self.inventory_last = current;
        if !changed {
            return None;
        }
        let difference = current - self.baseline_count;
        let satisfied = if delta >= 0 {
            difference >= delta
        } else {
            difference <= delta
        };
        if !inventory_attributed(packet, attribution) {
            // A pickup or another unrelated inventory change happened while
            // the craft window was open. Fold it into the baseline so it can
            // never be credited to the craft later.
            self.baseline_count = current;
            return None;
        }
        if satisfied {
            self.inventory_hit = Some(EvidenceRef {
                kind: "inventory_delta".to_owned(),
                tick,
            });
            Some((Outcome::Confirmed, None))
        } else {
            None
        }
    }

    fn observe_movement(
        &mut self,
        packet: &Packet,
        tick: u64,
    ) -> Option<(Outcome, Option<String>)> {
        if let Packet::PlayClientbound(ClientboundGamePacket::PlayerPosition(_)) = packet {
            self.correction_hit = Some(EvidenceRef {
                kind: "correction".to_owned(),
                tick,
            });
            return Some((Outcome::Unconfirmed, Some("corrected".to_owned())));
        }
        None
    }

    fn observe_slot(
        &mut self,
        packet: &Packet,
        tick: u64,
        container_id: i32,
        slot: i16,
        state_id: u32,
    ) -> Option<(Outcome, Option<String>)> {
        match packet {
            Packet::PlayServerbound(ServerboundGamePacket::ContainerClick(click)) => {
                if click.container_id == container_id
                    && click.slot_num == slot
                    && click.state_id == state_id
                {
                    self.click_hit.get_or_insert(EvidenceRef {
                        kind: "container_click".to_owned(),
                        tick,
                    });
                }
            }
            Packet::PlayClientbound(ClientboundGamePacket::ContainerSetContent(content)) => {
                if content.container_id == container_id
                    && content.state_id > state_id
                    && self.content_reverted(&content.items, slot)
                {
                    self.correction_hit = Some(EvidenceRef {
                        kind: "correction".to_owned(),
                        tick,
                    });
                }
            }
            Packet::PlayClientbound(ClientboundGamePacket::ContainerSetSlot(set)) => {
                if set.container_id == container_id
                    && set.state_id > state_id
                    && set.slot == u16::try_from(slot).ok()?
                    && self.slot_reverted(SlotItem::of(&set.item_stack))
                {
                    self.correction_hit = Some(EvidenceRef {
                        kind: "correction".to_owned(),
                        tick,
                    });
                }
            }
            _ => {}
        }
        if self.correction_hit.is_some() {
            Some((Outcome::Unconfirmed, Some("corrected".to_owned())))
        } else {
            None
        }
    }

    /// `EntityHurt`: the hurt animation, the hurt entity event (status 2) or the
    /// entity's removal settles the attack receipt.
    fn observe_entity_hurt(
        &mut self,
        packet: &Packet,
        tick: u64,
        id: i32,
    ) -> Option<(Outcome, Option<String>)> {
        let Packet::PlayClientbound(packet) = packet else {
            return None;
        };
        let hit = match packet {
            ClientboundGamePacket::HurtAnimation(hurt) => hurt.id.0 == id,
            ClientboundGamePacket::EntityEvent(event) => {
                event.entity_id.0 == id && event.event_id == 2
            }
            ClientboundGamePacket::RemoveEntities(remove) => {
                remove.entity_ids.iter().any(|entity| entity.0 == id)
            }
            _ => false,
        };
        if hit {
            self.hurt_hit = Some(EvidenceRef {
                kind: "entity_hurt".to_owned(),
                tick,
            });
            Some((Outcome::Confirmed, None))
        } else {
            None
        }
    }

    fn content_reverted(&self, items: &[praxion_world::ItemStack], slot: i16) -> bool {
        // Only a slot we actually captured before the click can be reverted.
        // Without that snapshot a newer container packet proves nothing.
        let Some(index) = usize::try_from(slot).ok() else {
            return false;
        };
        let item = items.get(index).map(SlotItem::of).unwrap_or(SlotItem::EMPTY);
        self.slot_reverted(item)
    }

    fn slot_reverted(&self, item: SlotItem) -> bool {
        // Finding 5: a container packet is a correction only when a pre-click
        // slot was captured and the packet restores it. A missing snapshot is
        // not evidence of a revert, so the packet is ignored.
        self.slot_before.as_ref().is_some_and(|before| *before == item)
    }

    fn evidence(&self) -> Vec<EvidenceRef> {
        let mut evidence = Vec::new();
        for hit in [
            &self.block_hit,
            &self.ack_hit,
            &self.inventory_hit,
            &self.click_hit,
            &self.hurt_hit,
            &self.fact_hit,
            &self.correction_hit,
        ] {
            if let Some(hit) = hit {
                evidence.push(hit.clone());
            }
        }
        evidence
    }

    fn timeout_outcome(&self) -> (Outcome, Option<String>) {
        match &self.intent.expect {
            Expect::BlockBecomesAir { .. } | Expect::BlockState { .. } => {
                if self.block_hit.is_some() && self.ack_hit.is_some() {
                    return (Outcome::Confirmed, None);
                }
                let note = match (self.block_hit.is_some(), self.ack_hit.is_some()) {
                    (true, false) => "missing_ack",
                    (false, true) => "missing_block",
                    _ => "no_evidence",
                };
                (Outcome::Unconfirmed, Some(note.to_owned()))
            }
            Expect::Inventory { .. } => {
                (Outcome::Unconfirmed, Some("no_inventory_delta".to_owned()))
            }
            Expect::Unrefuted => (Outcome::Confirmed, Some("unrefuted".to_owned())),
            Expect::SlotClick { .. } => {
                if self.click_hit.is_some() {
                    (Outcome::Confirmed, Some("no_correction".to_owned()))
                } else {
                    (Outcome::Unconfirmed, Some("no_click".to_owned()))
                }
            }
            Expect::EntityHurt { .. } => {
                (Outcome::Unconfirmed, Some("no_hurt".to_owned()))
            }
            Expect::FactKnown { .. } => {
                (Outcome::Unconfirmed, Some("fact_unknown".to_owned()))
            }
        }
    }

    /// Evidence a late block update carries. A late revert is refuting late
    /// evidence: the receipt stays settled, but the log records that the block
    /// did not stay in the state the intent needed.
    fn late_block(&mut self, pos: BlockPos, state: azalea_block::BlockState) -> Option<&'static str> {
        if self.block_reverted(pos, state) {
            return self
                .block_hit
                .take()
                .map(|_| "block_reverted");
        }
        if self.block_hit.is_none() {
            return self.block_matches(pos, state);
        }
        None
    }

    /// The evidence kind a late packet would have supplied, if it matches this
    /// receipt's contract.
    fn late_kind(&mut self, packet: &Packet, world: &World) -> Option<&'static str> {
        let attribution = self.intent.attribution;
        match &self.intent.expect {
            Expect::BlockBecomesAir { .. } | Expect::BlockState { .. } => {
                let Packet::PlayClientbound(packet) = packet else {
                    return None;
                };
                match packet {
                    ClientboundGamePacket::BlockUpdate(update) => {
                        self.late_block(update.pos, update.block_state)
                    }
                    ClientboundGamePacket::SectionBlocksUpdate(update) => {
                        update.states.iter().find_map(|entry| {
                            let pos = section_block_pos(update.section_pos, entry.pos)?;
                            self.late_block(pos, entry.state)
                        })
                    }
                    ClientboundGamePacket::BlockChangedAck(ack) => {
                        if self.ack_hit.is_some() {
                            return None;
                        }
                        if let Attribution::BlockSeq { seq } = self.intent.attribution
                            && ack.seq >= seq
                        {
                            Some("block_changed_ack")
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            }
            Expect::Inventory { item, delta } => {
                if self.inventory_hit.is_some()
                    || !is_inventory_packet(packet)
                    || !world.status().is_valid()
                {
                    return None;
                }
                let current = item_count(world, item);
                let changed = current != self.inventory_last;
                self.inventory_last = current;
                if !changed {
                    return None;
                }
                if !inventory_attributed(packet, attribution) {
                    self.baseline_count = current;
                    return None;
                }
                let difference = current - self.baseline_count;
                let satisfied = if *delta >= 0 {
                    difference >= *delta
                } else {
                    difference <= *delta
                };
                satisfied.then_some("inventory_delta")
            }
            Expect::Unrefuted => {
                if self.correction_hit.is_some() {
                    return None;
                }
                match packet {
                    Packet::PlayClientbound(ClientboundGamePacket::PlayerPosition(_)) => {
                        Some("correction")
                    }
                    _ => None,
                }
            }
            Expect::SlotClick { container_id, slot, state_id } => match packet {
                Packet::PlayServerbound(ServerboundGamePacket::ContainerClick(click)) => {
                    (self.click_hit.is_none()
                        && click.container_id == *container_id
                        && click.slot_num == *slot
                        && click.state_id == *state_id)
                        .then_some("container_click")
                }
                Packet::PlayClientbound(ClientboundGamePacket::ContainerSetContent(content)) => {
                    (self.correction_hit.is_none()
                        && content.container_id == *container_id
                        && content.state_id > *state_id
                        && self.content_reverted(&content.items, *slot))
                    .then_some("correction")
                }
                Packet::PlayClientbound(ClientboundGamePacket::ContainerSetSlot(set)) => {
                    (self.correction_hit.is_none()
                        && set.container_id == *container_id
                        && set.state_id > *state_id
                        && set.slot == u16::try_from(*slot).ok()?
                        && self.slot_reverted(SlotItem::of(&set.item_stack)))
                    .then_some("correction")
                }
                _ => None,
            },
            Expect::EntityHurt { id } => match packet {
                Packet::PlayClientbound(ClientboundGamePacket::HurtAnimation(hurt))
                    if hurt.id.0 == *id && self.hurt_hit.is_none() =>
                {
                    Some("entity_hurt")
                }
                Packet::PlayClientbound(ClientboundGamePacket::EntityEvent(event))
                    if event.entity_id.0 == *id
                        && event.event_id == 2
                        && self.hurt_hit.is_none() =>
                {
                    Some("entity_hurt")
                }
                Packet::PlayClientbound(ClientboundGamePacket::RemoveEntities(remove))
                    if remove.entity_ids.iter().any(|entity| entity.0 == *id)
                        && self.hurt_hit.is_none() =>
                {
                    Some("entity_hurt")
                }
                _ => None,
            },
            Expect::FactKnown { .. } => None,
        }
    }
}

/// Registers intents and settles their receipts.
///
/// Settled receipts live in a queue ordered by the tick on which they stop
/// being scanned for late evidence. Once that tick passes they are dropped, so
/// the per-packet work depends on the retention window rather than on how many
/// actions the run has performed. Outcomes stay reachable through two maps
/// (receipt id and intent id) for the same window; afterwards a lookup answers
/// [`Outcome::Expired`] instead of falling back to a full scan.
#[derive(Debug, Default)]
pub struct Engine {
    open: BTreeMap<ReceiptId, Receipt>,
    settled: VecDeque<Settled>,
    receipt_outcomes: HashMap<ReceiptId, StoredOutcome>,
    intent_outcomes: HashMap<IntentId, StoredOutcome>,
    /// Intent ids whose last retained outcome has expired. Kept so an intent
    /// lookup can answer `expired` without scanning the settlement history.
    expired_intents: BTreeSet<IntentId>,
    next_receipt: ReceiptId,
    next_order: u64,
    /// The greatest tick the caller has passed so far. Used to expire entries
    /// even when an observation arrives out of order.
    last_tick: u64,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one intent and log `intent_started`. The receipt id is the
    /// engine's own counter; the intent id stays the caller's.
    ///
    /// Call this before the primitive's packet is sent: a slot click is itself
    /// evidence, and the engine only sees packets fed to [`Engine::observe`]
    /// after registration.
    pub fn register(
        &mut self,
        log: &mut Writer,
        tick: u64,
        ts_ns: u64,
        intent: Intent,
        world: &World,
    ) -> std::io::Result<ReceiptId> {
        self.last_tick = self.last_tick.max(tick);
        self.prune_settled();
        let receipt_id = self.next_receipt;
        self.next_receipt = self.next_receipt.saturating_add(1);
        let receipt = Receipt::new(receipt_id, intent, world);
        log.intent_started(
            tick,
            ts_ns,
            receipt.intent.intent_id,
            receipt.intent.primitive.clone(),
            receipt.intent.args.clone(),
            receipt.intent.start_tick,
            receipt.intent.attribution,
        )?;
        self.open.insert(receipt_id, receipt);
        Ok(receipt_id)
    }

    /// Feed one decoded packet to every open receipt and to the late-evidence
    /// check of every settled one.
    ///
    /// `world` must already have the packet applied for the followed
    /// connection; the engine reads inventory and container state from it.
    pub fn observe(
        &mut self,
        log: &mut Writer,
        tick: u64,
        ts_ns: u64,
        packet: &Packet,
        world: &World,
    ) -> std::io::Result<()> {
        self.last_tick = self.last_tick.max(tick);
        self.prune_settled();
        // A packet that arrives after its window closed must not settle the
        // receipt as if it were in time, even if the caller forgot to call
        // `advance` for the intervening ticks. A packet exactly on the deadline
        // still gets its chance: the window includes that tick.
        self.settle_expired(log, tick, ts_ns, false)?;
        self.prune_settled();
        let timeout_end = self.settled.len();

        let ids: Vec<ReceiptId> = self.open.keys().copied().collect();
        for id in ids {
            let Some(receipt) = self.open.get_mut(&id) else {
                continue;
            };
            if tick < receipt.intent.start_tick {
                receipt.observe_before_start(packet, world);
                continue;
            }
            if let Some((outcome, note)) = receipt.observe(packet, tick, world) {
                let receipt = self.open.remove(&id).expect("receipt was present");
                self.record_settlement(log, tick, ts_ns, receipt, outcome, note)?;
            }
        }

        let mut late = Vec::new();
        // Every receipt settled before this packet (already settled, or just
        // timed out by the call above) sees it as late evidence. Receipts that
        // settled *because of* this packet start at `timeout_end` and are not
        // candidates.
        for settled in self.settled.iter_mut().take(timeout_end) {
            if let Some(kind) = settled.receipt.late_kind(packet, world) {
                late.push((settled.receipt_id, kind));
            }
        }
        for (receipt_id, kind) in late {
            log.late_evidence(tick, ts_ns, receipt_id, kind)?;
        }
        Ok(())
    }

    /// Close every receipt whose window ended at or before `tick`.
    pub fn advance(
        &mut self,
        log: &mut Writer,
        tick: u64,
        ts_ns: u64,
    ) -> std::io::Result<()> {
        self.last_tick = self.last_tick.max(tick);
        self.prune_settled();
        self.settle_expired(log, tick, ts_ns, true)?;
        self.prune_settled();
        Ok(())
    }

    fn settle_expired(
        &mut self,
        log: &mut Writer,
        tick: u64,
        ts_ns: u64,
        at_deadline: bool,
    ) -> std::io::Result<()> {
        let ids: Vec<ReceiptId> = self
            .open
            .iter()
            .filter(|(_, receipt)| {
                let deadline = receipt.intent.deadline();
                if at_deadline {
                    tick >= deadline
                } else {
                    tick > deadline
                }
            })
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let receipt = self.open.remove(&id).expect("receipt was present");
            let (outcome, note) = receipt.timeout_outcome();
            self.record_settlement(log, tick, ts_ns, receipt, outcome, note)?;
        }
        Ok(())
    }

    /// Log one settlement, insert it into both outcome maps and enqueue it for
    /// the late-evidence scan until its retention tick.
    fn record_settlement(
        &mut self,
        log: &mut Writer,
        tick: u64,
        ts_ns: u64,
        receipt: Receipt,
        outcome: Outcome,
        note: Option<String>,
    ) -> std::io::Result<()> {
        log.receipt_settled(
            tick,
            ts_ns,
            receipt.id,
            receipt.intent.intent_id,
            outcome,
            receipt.evidence(),
            note,
        )?;
        let order = self.next_order;
        self.next_order = self.next_order.saturating_add(1);
        let late_until = tick.saturating_add(LATE_RETENTION_TICKS);
        let stored = StoredOutcome { outcome, late_until, order };
        self.receipt_outcomes.insert(receipt.id, stored);
        self.intent_outcomes.insert(receipt.intent.intent_id, stored);
        self.expired_intents.remove(&receipt.intent.intent_id);
        self.push_settled(Settled {
            receipt_id: receipt.id,
            intent_id: receipt.intent.intent_id,
            outcome,
            late_until,
            order,
            receipt,
        });
        Ok(())
    }

    /// Keep the scan queue ordered by expiry tick. Settlements normally append
    /// in order; a preemption or an out-of-order observation may arrive
    /// earlier, so insert it at its sorted position.
    fn push_settled(&mut self, settled: Settled) {
        if self
            .settled
            .back()
            .map_or(true, |back| back.late_until <= settled.late_until)
        {
            self.settled.push_back(settled);
            return;
        }
        let index = self
            .settled
            .iter()
            .position(|entry| entry.late_until > settled.late_until)
            .unwrap_or(self.settled.len());
        self.settled.insert(index, settled);
    }

    /// Drop every settlement whose retention tick has passed, from the scan
    /// queue and from the outcome maps. This is what keeps per-packet work
    /// proportional to the retention window, not to the run length.
    fn prune_settled(&mut self) {
        while let Some(front) = self.settled.front() {
            if self.last_tick <= front.late_until {
                break;
            }
            let settled = self.settled.pop_front().expect("the front entry exists");
            if self
                .receipt_outcomes
                .get(&settled.receipt_id)
                .is_some_and(|stored| stored.order == settled.order)
            {
                self.receipt_outcomes.remove(&settled.receipt_id);
            }
            if self
                .intent_outcomes
                .get(&settled.intent_id)
                .is_some_and(|stored| stored.order == settled.order)
            {
                self.intent_outcomes.remove(&settled.intent_id);
                self.expired_intents.insert(settled.intent_id);
            }
        }
    }

    /// Settle one open receipt `preempted`. Returns whether it was open.
    pub fn preempt(
        &mut self,
        log: &mut Writer,
        receipt_id: ReceiptId,
        tick: u64,
        ts_ns: u64,
    ) -> std::io::Result<bool> {
        self.last_tick = self.last_tick.max(tick);
        self.prune_settled();
        let Some(receipt) = self.open.remove(&receipt_id) else {
            return Ok(false);
        };
        self.record_settlement(
            log,
            tick,
            ts_ns,
            receipt,
            Outcome::Preempted,
            Some("preempted".to_owned()),
        )?;
        self.prune_settled();
        Ok(true)
    }

    /// Settle one open receipt `confirmed` from evidence the caller observed
    /// outside the packet stream (an `observe` fact becoming known). Returns
    /// whether it was open.
    pub fn confirm(
        &mut self,
        log: &mut Writer,
        receipt_id: ReceiptId,
        tick: u64,
        ts_ns: u64,
        note: impl Into<String>,
    ) -> std::io::Result<bool> {
        self.last_tick = self.last_tick.max(tick);
        self.prune_settled();
        let Some(mut receipt) = self.open.remove(&receipt_id) else {
            return Ok(false);
        };
        receipt.fact_hit = Some(EvidenceRef {
            kind: "fact_known".to_owned(),
            tick,
        });
        self.record_settlement(
            log,
            tick,
            ts_ns,
            receipt,
            Outcome::Confirmed,
            Some(note.into()),
        )?;
        self.prune_settled();
        Ok(true)
    }

    /// The outcome of a settled receipt, or `expired` once its retention window
    /// has passed. An unknown receipt id gives `None`.
    pub fn outcome(&self, receipt_id: ReceiptId) -> Option<Outcome> {
        if let Some(stored) = self.receipt_outcomes.get(&receipt_id) {
            return Some(if self.last_tick > stored.late_until {
                Outcome::Expired
            } else {
                stored.outcome
            });
        }
        // A receipt id below the counter that is neither open nor retained was
        // settled and has already expired.
        if !self.open.contains_key(&receipt_id) && receipt_id < self.next_receipt {
            return Some(Outcome::Expired);
        }
        None
    }

    /// The outcome of the newest settlement of an intent, or `expired` once
    /// that settlement's retention window has passed.
    pub fn outcome_for_intent(&self, intent_id: IntentId) -> Option<Outcome> {
        if let Some(stored) = self.intent_outcomes.get(&intent_id) {
            return Some(if self.last_tick > stored.late_until {
                Outcome::Expired
            } else {
                stored.outcome
            });
        }
        if self.expired_intents.contains(&intent_id) {
            return Some(Outcome::Expired);
        }
        None
    }

    pub fn is_open(&self, receipt_id: ReceiptId) -> bool {
        self.open.contains_key(&receipt_id)
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    /// How many settlements are still retained for the late-evidence scan.
    pub fn settled_count(&self) -> usize {
        self.settled.len()
    }

    /// The settlements still inside the retention window, oldest first.
    pub fn settlements(&self) -> impl Iterator<Item = Settlement> + '_ {
        self.settled.iter().map(|settled| Settlement {
            receipt_id: settled.receipt_id,
            intent_id: settled.intent_id,
            outcome: settled.outcome,
        })
    }
}

fn is_inventory_packet(packet: &Packet) -> bool {
    matches!(
        packet,
        Packet::PlayClientbound(
            ClientboundGamePacket::ContainerSetContent(_)
                | ClientboundGamePacket::ContainerSetSlot(_)
                | ClientboundGamePacket::SetPlayerInventory(_)
        )
    )
}

fn is_container_state_packet(packet: &Packet) -> bool {
    matches!(
        packet,
        Packet::PlayClientbound(
            ClientboundGamePacket::ContainerSetContent(_)
                | ClientboundGamePacket::ContainerSetSlot(_)
        )
    )
}

/// The container state id an inventory delta arrived with, when it has one.
fn container_state_id(packet: &Packet) -> Option<u32> {
    match packet {
        Packet::PlayClientbound(ClientboundGamePacket::ContainerSetContent(content)) => {
            Some(content.state_id)
        }
        Packet::PlayClientbound(ClientboundGamePacket::ContainerSetSlot(slot)) => {
            Some(slot.state_id)
        }
        _ => None,
    }
}

/// Whether this inventory packet may supply the intent's delta.
///
/// Pickups carry `Attribution::None`: the inventory delta is their whole
/// causal story. A craft carries the state id of the click that took the
/// result, so only a container packet at or after that state id is attributed.
/// Everything else is unrelated and must not confirm.
fn inventory_attributed(packet: &Packet, attribution: Attribution) -> bool {
    match attribution {
        Attribution::None => true,
        Attribution::SlotClick { state_id, .. } => {
            container_state_id(packet).is_some_and(|packet_state| packet_state >= state_id)
        }
        Attribution::BlockSeq { .. } | Attribution::Nonce { .. } => false,
    }
}

fn item_count(world: &World, item: &str) -> i64 {
    let wanted: Option<ItemKind> = item.parse().ok();
    world
        .inventory()
        .iter()
        .chain(std::iter::once(world.carried_item()))
        .filter(|stack| match &wanted {
            Some(wanted) => stack.kind() == *wanted,
            None => normalize(&stack.kind().to_str()) == normalize(item),
        })
        .map(|stack| i64::from(stack.count()))
        .sum()
}

fn slot_item(world: &World, container_id: i32, slot: i16) -> Option<SlotItem> {
    let index = usize::try_from(slot).ok()?;
    if container_id == 0 {
        return world.inventory().get(index).map(SlotItem::of);
    }
    let container = world.open_container()?;
    if container.id() != container_id {
        return None;
    }
    container.slots().get(index).map(SlotItem::of)
}

fn section_block_pos(section: ChunkSectionPos, pos: ChunkSectionBlockPos) -> Option<BlockPos> {
    let x = section.x.checked_mul(16)?.checked_add(i32::from(pos.x))?;
    let y = section.y.checked_mul(16)?.checked_add(i32::from(pos.y))?;
    let z = section.z.checked_mul(16)?.checked_add(i32::from(pos.z))?;
    Some(BlockPos::new(x, y, z))
}

fn normalize(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_owned()
}
