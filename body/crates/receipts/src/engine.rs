//! Evidence matching and receipt settlement.

use std::collections::BTreeMap;

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
    container_before: Option<Vec<SlotItem>>,
    block_hit: Option<EvidenceRef>,
    ack_hit: Option<EvidenceRef>,
    inventory_hit: Option<EvidenceRef>,
    click_hit: Option<EvidenceRef>,
    correction_hit: Option<EvidenceRef>,
}

#[derive(Clone, Debug)]
struct Settled {
    receipt_id: ReceiptId,
    intent_id: IntentId,
    outcome: Outcome,
    late_until: u64,
    /// Dropped once `late_until` has passed; the compact outcome stays for the
    /// rest of the run.
    receipt: Option<Receipt>,
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
        let container_before = match &intent.expect {
            Expect::SlotClick { container_id, .. } => {
                container_snapshot(world, *container_id)
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
            container_before,
            block_hit: None,
            ack_hit: None,
            inventory_hit: None,
            click_hit: None,
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
                    self.container_before = container_snapshot(world, *container_id);
                }
            }
            Expect::Unrefuted => {}
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
                self.observe_inventory(packet, tick, world, &item, delta)
            }
            Expect::Unrefuted => self.observe_movement(packet, tick),
            Expect::SlotClick { container_id, slot, state_id } => {
                let container_id = *container_id;
                let slot = *slot;
                let state_id = *state_id;
                self.observe_slot(packet, tick, container_id, slot, state_id)
            }
        }
    }

    fn observe_block(&mut self, packet: &Packet, tick: u64) {
        let Packet::PlayClientbound(packet) = packet else {
            return;
        };
        match packet {
            ClientboundGamePacket::BlockUpdate(update) => {
                if let Some(kind) = self.block_matches(update.pos, update.block_state) {
                    self.record_block(kind, tick);
                }
            }
            ClientboundGamePacket::SectionBlocksUpdate(update) => {
                for entry in &update.states {
                    let Some(pos) = section_block_pos(update.section_pos, entry.pos) else {
                        continue;
                    };
                    if let Some(kind) = self.block_matches(pos, entry.state) {
                        self.record_block(kind, tick);
                        break;
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

    fn content_reverted(&self, items: &[praxion_world::ItemStack], slot: i16) -> bool {
        if let Some(before) = &self.container_before {
            let after: Vec<SlotItem> = items.iter().map(SlotItem::of).collect();
            if *before == after {
                return true;
            }
        }
        let Some(index) = usize::try_from(slot).ok() else {
            return false;
        };
        let item = items.get(index).map(SlotItem::of).unwrap_or(SlotItem::EMPTY);
        self.slot_reverted(item)
    }

    fn slot_reverted(&self, item: SlotItem) -> bool {
        // Without a captured pre-click slot we cannot prove the server kept the
        // old value; treat a newer state as a correction rather than risk a
        // false confirmed.
        match &self.slot_before {
            Some(before) => *before == item,
            None => true,
        }
    }

    fn evidence(&self) -> Vec<EvidenceRef> {
        let mut evidence = Vec::new();
        for hit in [
            &self.block_hit,
            &self.ack_hit,
            &self.inventory_hit,
            &self.click_hit,
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
        }
    }

    /// The evidence kind a late packet would have supplied, if it matches this
    /// receipt's contract.
    fn late_kind(&mut self, packet: &Packet, world: &World) -> Option<&'static str> {
        match &self.intent.expect {
            Expect::BlockBecomesAir { .. } | Expect::BlockState { .. } => {
                let Packet::PlayClientbound(packet) = packet else {
                    return None;
                };
                match packet {
                    ClientboundGamePacket::BlockUpdate(update) => {
                        if self.block_hit.is_some() {
                            return None;
                        }
                        self.block_matches(update.pos, update.block_state)
                    }
                    ClientboundGamePacket::SectionBlocksUpdate(update) => {
                        if self.block_hit.is_some() {
                            return None;
                        }
                        update.states.iter().find_map(|entry| {
                            let pos = section_block_pos(update.section_pos, entry.pos)?;
                            self.block_matches(pos, entry.state)
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
                let difference = current - self.baseline_count;
                let satisfied = if *delta >= 0 {
                    difference >= *delta
                } else {
                    difference <= *delta
                };
                (changed && satisfied).then_some("inventory_delta")
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
        }
    }
}

/// Registers intents and settles their receipts.
#[derive(Debug, Default)]
pub struct Engine {
    open: BTreeMap<ReceiptId, Receipt>,
    settled: Vec<Settled>,
    next_receipt: ReceiptId,
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
        // A packet that arrives after its window closed must not settle the
        // receipt as if it were in time, even if the caller forgot to call
        // `advance` for the intervening ticks. A packet exactly on the deadline
        // still gets its chance: the window includes that tick.
        self.settle_expired(log, tick, ts_ns, false)?;
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
                log.receipt_settled(
                    tick,
                    ts_ns,
                    receipt.id,
                    receipt.intent.intent_id,
                    outcome,
                    receipt.evidence(),
                    note,
                )?;
                self.settled.push(Settled {
                    receipt_id: receipt.id,
                    intent_id: receipt.intent.intent_id,
                    outcome,
                    late_until: tick.saturating_add(LATE_RETENTION_TICKS),
                    receipt: Some(receipt),
                });
            }
        }

        let mut late = Vec::new();
        // Every receipt settled before this packet (already settled, or just
        // timed out by the call above) sees it as late evidence. Receipts that
        // settled *because of* this packet start at `timeout_end` and are not
        // candidates.
        for settled in &mut self.settled[..timeout_end] {
            if tick > settled.late_until {
                settled.receipt = None;
                continue;
            }
            let Some(receipt) = settled.receipt.as_mut() else {
                continue;
            };
            if let Some(kind) = receipt.late_kind(packet, world) {
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
        self.settle_expired(log, tick, ts_ns, true)
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
            log.receipt_settled(
                tick,
                ts_ns,
                receipt.id,
                receipt.intent.intent_id,
                outcome,
                receipt.evidence(),
                note,
            )?;
            self.settled.push(Settled {
                receipt_id: receipt.id,
                intent_id: receipt.intent.intent_id,
                outcome,
                late_until: tick.saturating_add(LATE_RETENTION_TICKS),
                receipt: Some(receipt),
            });
        }
        Ok(())
    }

    /// Settle one open receipt `preempted`. Returns whether it was open.
    pub fn preempt(
        &mut self,
        log: &mut Writer,
        receipt_id: ReceiptId,
        tick: u64,
        ts_ns: u64,
    ) -> std::io::Result<bool> {
        let Some(receipt) = self.open.remove(&receipt_id) else {
            return Ok(false);
        };
        log.receipt_settled(
            tick,
            ts_ns,
            receipt.id,
            receipt.intent.intent_id,
            Outcome::Preempted,
            receipt.evidence(),
            Some("preempted".to_owned()),
        )?;
        self.settled.push(Settled {
            receipt_id: receipt.id,
            intent_id: receipt.intent.intent_id,
            outcome: Outcome::Preempted,
            late_until: tick.saturating_add(LATE_RETENTION_TICKS),
            receipt: Some(receipt),
        });
        Ok(true)
    }

    /// The outcome of a settled receipt.
    pub fn outcome(&self, receipt_id: ReceiptId) -> Option<Outcome> {
        self.settled
            .iter()
            .find(|settled| settled.receipt_id == receipt_id)
            .map(|settled| settled.outcome)
    }

    /// The outcome of the newest settlement of an intent.
    pub fn outcome_for_intent(&self, intent_id: IntentId) -> Option<Outcome> {
        self.settled
            .iter()
            .rev()
            .find(|settled| settled.intent_id == intent_id)
            .map(|settled| settled.outcome)
    }

    pub fn is_open(&self, receipt_id: ReceiptId) -> bool {
        self.open.contains_key(&receipt_id)
    }

    pub fn open_count(&self) -> usize {
        self.open.len()
    }

    pub fn settled_count(&self) -> usize {
        self.settled.len()
    }

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

fn container_snapshot(world: &World, container_id: i32) -> Option<Vec<SlotItem>> {
    if container_id == 0 {
        return Some(world.inventory().iter().map(SlotItem::of).collect());
    }
    let container = world.open_container()?;
    if container.id() != container_id {
        return None;
    }
    Some(container.slots().iter().map(SlotItem::of).collect())
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
