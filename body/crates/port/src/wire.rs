//! The v0 wire messages (proposal §2.2).
//!
//! The enums are tagged by `t`. They are the complete set A1 needs: a message
//! kind that the proposal does not list is not added here.

use std::collections::BTreeMap;

use praxion_eventlog::EventKind;
use serde::{Deserialize, Serialize};

/// The port version this crate speaks. A brain that says another number is
/// refused with `bye { reason: "version" }`.
pub const PORT_VERSION: u16 = 0;

/// What a connection can be asked to do. The v0 body always advertises all
/// three.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Accept `command` and `pop`.
    Command,
    /// Send `snapshot` on demand.
    Snapshot,
    /// Release an intent: cancel it when it runs, remove it when it is done or
    /// suspended.
    Pop,
}

/// Why the body closes a connection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    /// The brain spoke another `port_version`.
    Version,
    /// A newer connection replaced this one (proposal §2.1).
    Replaced,
    /// The body is shutting down.
    Shutdown,
    /// The first frame was not a valid `hello`.
    Protocol,
}

/// One entry of the body's intent stack.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackEntry {
    pub intent_id: u64,
    /// `survival`, `command`, `maintenance` or `projects`.
    pub tier: String,
    pub skill: String,
    pub args: Vec<String>,
    /// `running`, `suspended`, `frozen_for_carry_on` or `done`.
    pub state: String,
}

/// One occupied slot of the player's own container (decision 2026-11-09
/// "Console": the panel shows the 36 storage/hotbar slots, armour and offhand).
///
/// Slot numbers follow vanilla's `InventoryMenu`: 0 result, 1-4 grid, 5-8
/// armour, 9-35 storage, 36-44 hotbar, 45 offhand. The list is additive and
/// skipped when empty, so `PORT_VERSION` stays 0: an old brain ignores it and a
/// new brain on an old body gets an empty list. Durability is left off until
/// the owner decides whether the panel shows wear (open question 5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotFact {
    pub slot: u8,
    /// The item id without the `minecraft:` namespace.
    pub item: String,
    pub count: u32,
}

/// The body facts a snapshot carries (proposal §2.2, §2.5).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    /// Half-hearts are possible, so this is fractional.
    pub health: f32,
    pub food: u32,
    pub air: u32,
    pub is_night: bool,
    pub day_time: u64,
    pub dimension: String,
    pub pos: [i32; 3],
    pub inventory: BTreeMap<String, u32>,
    /// The hand, 0..=8; absent before the first held-slot fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_hotbar_slot: Option<u8>,
    /// Occupied slots only, ascending; absent when the inventory is empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub slots: Vec<SlotFact>,
}

/// State on demand: connect, `resync_request` and every wake.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// The last event-log `seq` the snapshot reflects.
    pub seq: u64,
    pub tick: u64,
    pub generation: u32,
    /// `idle`, `skill:<id>`, `reflex:<kind>` or `maintenance:<need>`.
    pub owner: String,
    pub stack: Vec<StackEntry>,
    pub facts: Facts,
    /// Ticks since the newest fact in the snapshot was observed.
    pub stale_ticks: u64,
}

/// A brain directive (proposal §2.2, §2.4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    /// Chosen by the brain, unique per brain run, echoed in every receipt.
    pub id: String,
    pub skill: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// v0 has only `command`.
    pub tier: String,
    /// After this body tick the command never starts; it settles `expired`.
    pub expires_at_tick: u64,
    /// Rejected when it is not the body's current generation.
    pub based_on_generation: u32,
    /// Optional guard text the body evaluates when the goal settles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// Per-goal tick limit; the body settles `failed(budget)` when it runs out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_ticks: Option<u64>,
}

/// A message the body sends to the brain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum BodyMessage {
    /// First frame of the connection: the body speaks first (proposal §2.1).
    Hello {
        port_version: u16,
        caps: Vec<Capability>,
        /// For example `"praxion 0.x (abc123)"`.
        body: String,
        run_id: String,
    },
    /// One event-log line. `seq` is the event log's own `seq`.
    Event { seq: u64, tick: u64, event: EventKind },
    /// State on demand. The `seq` is the cut: events with a larger `seq` follow.
    Snapshot(Snapshot),
    /// `n` events were discarded; a fresh snapshot follows.
    Dropped { seq: u64, n: u64 },
    /// Answer to a brain `ping`.
    Pong { seq: u64, tick: u64 },
    /// The body closes the connection.
    Bye { reason: ByeReason },
}

/// A message the brain sends to the body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum BrainMessage {
    Hello { port_version: u16 },
    Command(Command),
    /// Release an intent: `running` is cancelled (the command settles
    /// `cancelled`), `done` or `suspended` is removed from the stack.
    Pop { id: String, intent_id: u64 },
    /// Ask for a snapshot at the current cut.
    ResyncRequest,
    Ping,
}
