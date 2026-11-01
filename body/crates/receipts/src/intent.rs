//! What the actuator registers when it starts a primitive.

use praxion_eventlog::Attribution;
use praxion_rules::EvidenceKind;
use praxion_world::BlockPos;

/// Identifies one primitive execution across retries and replans.
pub type IntentId = u64;
/// Identifies one evidence contract. The engine allocates these in order.
pub type ReceiptId = u64;

/// Default settle windows in body ticks. Session 4 leaves the exact numbers to
/// measurement; these are the primitive records' `max_ticks` where one exists.
pub const DIG_WINDOW: u64 = 40;
pub const PLACE_WINDOW: u64 = 20;
pub const INVENTORY_WINDOW: u64 = 20;
pub const MOVE_WINDOW: u64 = 20;
pub const SLOT_CLICK_WINDOW: u64 = 20;

/// The observable effect a receipt waits for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    /// `dig`: the target block becomes air.
    BlockBecomesAir { pos: BlockPos },
    /// `place`: the target block changes to `block`.
    BlockState { pos: BlockPos, block: String },
    /// `pickup`, `craft` or `eat`: the inventory count of `item` changes by
    /// `delta` (negative for a consumed item).
    ///
    /// A pickup carries [`Attribution::None`], because the delta itself is the
    /// whole causal story. A craft carries the container state id and slot of
    /// the click that took the result; use [`Intent::craft`] for it.
    Inventory { item: String, delta: i64 },
    /// `craft`: like [`Expect::Inventory`], but only the player's storage slots
    /// count (inventory 9 to 44, or the open menu's last 36 slots). A result
    /// still sitting in the crafting result slot is not evidence that the craft
    /// reached the inventory.
    StorageInventory { item: String, delta: i64 },
    /// `move_to`, `look`, `wait`: no server correction within the window.
    Unrefuted,
    /// A menu action: this click was sent and the server did not revert it.
    SlotClick { container_id: i32, slot: i16, state_id: u32 },
    /// `attack`: the entity's hurt animation, hurt event or removal.
    EntityHurt { id: i32 },
    /// `observe`: the driver saw the fact become known.
    FactKnown { fact: String },
}

/// One registered evidence contract (§5.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Intent {
    /// The caller's intent id, persisted across retries.
    pub intent_id: IntentId,
    /// The primitive that started it, for the log.
    pub primitive: String,
    /// Human-readable arguments, for the log only.
    pub args: Vec<String>,
    /// The body tick the primitive started on.
    pub start_tick: u64,
    /// The causal key that must match in addition to the tick order.
    pub attribution: Attribution,
    /// The evidence kind declared by the primitive's contract record.
    pub evidence: EvidenceKind,
    /// How many ticks the receipt waits for its evidence.
    pub window: u64,
    /// What the evidence must show.
    pub expect: Expect,
    /// The target cell's state just before the action was planned, when the
    /// caller knows it. The receipt must compare against this rather than a
    /// snapshot taken at registration: a fast server can send the block update
    /// in the same tick batch that registers the intent.
    pub initial_block: Option<azalea_block::BlockState>,
}

impl Intent {
    /// `dig`: the block becomes air, attributed by the action's packet seq.
    pub fn dig(intent_id: IntentId, pos: BlockPos, seq: u32, start_tick: u64) -> Self {
        Self {
            intent_id,
            primitive: "mine".to_owned(),
            args: vec![format!("{}, {}, {}", pos.x, pos.y, pos.z)],
            start_tick,
            attribution: Attribution::BlockSeq { seq },
            evidence: EvidenceKind::BlockBecomesAir,
            window: DIG_WINDOW,
            expect: Expect::BlockBecomesAir { pos },
            initial_block: None,
        }
    }

    /// `place`: the block state changes to `block`, same attribution as dig.
    pub fn place(
        intent_id: IntentId,
        pos: BlockPos,
        block: impl Into<String>,
        seq: u32,
        start_tick: u64,
    ) -> Self {
        let block = block.into();
        Self {
            intent_id,
            primitive: "place".to_owned(),
            args: vec![block.clone(), format!("{}, {}, {}", pos.x, pos.y, pos.z)],
            start_tick,
            attribution: Attribution::BlockSeq { seq },
            evidence: EvidenceKind::BlockStateChanged,
            window: PLACE_WINDOW,
            expect: Expect::BlockState { pos, block },
            initial_block: None,
        }
    }

    /// `pickup` or `eat`: the inventory delta of one item.
    ///
    /// This constructor uses [`Attribution::None`], which is only correct for
    /// actions whose inventory delta is itself the evidence. A craft must use
    /// [`Intent::craft`] so the delta is tied to the click that took the
    /// result. `primitive` is free-form and is recorded in the log only.
    pub fn inventory(
        intent_id: IntentId,
        primitive: impl Into<String>,
        item: impl Into<String>,
        delta: i64,
        start_tick: u64,
    ) -> Self {
        let item = item.into();
        Self {
            intent_id,
            primitive: primitive.into(),
            args: vec![item.clone(), delta.to_string()],
            start_tick,
            attribution: Attribution::None,
            evidence: EvidenceKind::InventoryDelta,
            window: INVENTORY_WINDOW,
            expect: Expect::Inventory { item, delta },
            initial_block: None,
        }
    }

    /// `craft`: the inventory delta of the crafted item, attributed to the
    /// container state id and slot of the click that takes the result.
    ///
    /// The receipt confirms only when the delta arrives in a container packet
    /// whose state id is at or after `state_id`. A pickup or another delta
    /// without that causal link stays unrelated and cannot confirm the craft.
    pub fn craft(
        intent_id: IntentId,
        item: impl Into<String>,
        delta: i64,
        start_tick: u64,
        state_id: u32,
        slot: i16,
    ) -> Self {
        let mut intent =
            Self::inventory(intent_id, "craft", item, delta, start_tick)
                .with_attribution(Attribution::SlotClick { state_id, slot });
        intent.args.push(format!("state {state_id}"));
        intent.args.push(format!("slot {slot}"));
        intent
    }

    /// `craft`: the output reaches the player's storage, not just the crafting
    /// result slot.
    pub fn storage_inventory(
        intent_id: IntentId,
        primitive: impl Into<String>,
        item: impl Into<String>,
        delta: i64,
        start_tick: u64,
    ) -> Self {
        let item = item.into();
        Self {
            intent_id,
            primitive: primitive.into(),
            args: vec![item.clone(), delta.to_string()],
            start_tick,
            attribution: Attribution::None,
            evidence: EvidenceKind::InventoryDelta,
            window: INVENTORY_WINDOW,
            expect: Expect::StorageInventory { item, delta },
            initial_block: None,
        }
    }

    /// `move_to`, `look` or another action verified by absence of a correction.
    pub fn movement(
        intent_id: IntentId,
        primitive: impl Into<String>,
        start_tick: u64,
    ) -> Self {
        Self {
            intent_id,
            primitive: primitive.into(),
            args: Vec::new(),
            start_tick,
            attribution: Attribution::None,
            evidence: EvidenceKind::NoCorrection,
            window: MOVE_WINDOW,
            expect: Expect::Unrefuted,
            initial_block: None,
        }
    }

    /// A menu slot click, attributed by the window state id and slot.
    pub fn slot_click(
        intent_id: IntentId,
        container_id: i32,
        slot: i16,
        state_id: u32,
        start_tick: u64,
    ) -> Self {
        Self {
            intent_id,
            primitive: "click_slot".to_owned(),
            args: vec![
                format!("container {container_id}"),
                format!("slot {slot}"),
                format!("state {state_id}"),
            ],
            start_tick,
            attribution: Attribution::SlotClick { state_id, slot },
            evidence: EvidenceKind::NoCorrection,
            window: SLOT_CLICK_WINDOW,
            expect: Expect::SlotClick { container_id, slot, state_id },
            initial_block: None,
        }
    }

    /// `attack`: the attacked entity is hurt or gone.
    pub fn attack(intent_id: IntentId, entity_id: i32, start_tick: u64) -> Self {
        Self {
            intent_id,
            primitive: "attack".to_owned(),
            args: vec![format!("entity {entity_id}")],
            start_tick,
            attribution: Attribution::None,
            evidence: EvidenceKind::EntityHurt,
            window: 20,
            expect: Expect::EntityHurt { id: entity_id },
            initial_block: None,
        }
    }

    /// `observe`: the fact became known.
    pub fn fact(intent_id: IntentId, fact: impl Into<String>, start_tick: u64) -> Self {
        let fact = fact.into();
        Self {
            intent_id,
            primitive: "observe".to_owned(),
            args: vec![fact.clone()],
            start_tick,
            attribution: Attribution::None,
            evidence: EvidenceKind::FactKnown,
            window: 20,
            expect: Expect::FactKnown { fact },
            initial_block: None,
        }
    }

    /// Override the settle window (measurement, tests, corpus replay).
    pub fn with_window(mut self, window: u64) -> Self {
        self.window = window;
        self
    }

    /// Supply the target cell's state just before the action. The actuator's
    /// report carries it, so a block update the server sends in the same tick
    /// batch as the registration is still evidence of a real change.
    pub fn with_initial_block(mut self, state: Option<azalea_block::BlockState>) -> Self {
        self.initial_block = state;
        self
    }

    /// Override the causal attribution key.
    ///
    /// Constructors set this already; it exists for callers that build an
    /// intent from measured packet details (for example a craft click).
    pub fn with_attribution(mut self, attribution: Attribution) -> Self {
        self.attribution = attribution;
        self
    }

    /// Replace the free-form log arguments.
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// The tick at which the window closes.
    pub fn deadline(&self) -> u64 {
        self.start_tick.saturating_add(self.window)
    }
}
