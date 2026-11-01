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
    /// `pickup`, `craft`, `eat`: the inventory count of `item` changes by
    /// `delta` (negative for a consumed item).
    Inventory { item: String, delta: i64 },
    /// `move_to`, `look`, `wait`: no server correction within the window.
    Unrefuted,
    /// A menu action: this click was sent and the server did not revert it.
    SlotClick { container_id: i32, slot: i16, state_id: u32 },
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
        }
    }

    /// `pickup`, `craft` or `eat`: the inventory delta of one item.
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
        }
    }

    /// Override the settle window (measurement, tests, corpus replay).
    pub fn with_window(mut self, window: u64) -> Self {
        self.window = window;
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
