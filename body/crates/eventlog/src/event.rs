//! Typed events and the small data types they carry.

use serde::{Deserialize, Serialize};

/// The version pins recorded once at the start of a run.
///
/// The body fills these from `versions/manifest.toml`; the log crate stays
/// independent of the manifest reader.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Versions {
    /// The Praxion build, or `"dev"` for an unversioned build.
    pub praxion: String,
    /// The Minecraft version, for example `"26.3"`.
    pub minecraft: String,
    /// The protocol number the forwarder accepts.
    pub protocol: u32,
    /// The pinned Azalea commit.
    pub azalea: String,
    /// The pinned Rust toolchain.
    pub rust: String,
}

/// What causally attributes evidence to an intent (architecture §5.3).
///
/// A tick at or after the start is necessary but never sufficient: the causal
/// key must match too. `Nonce` is reserved for Demo-1's injected events and is
/// unused in Demo-0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attribution {
    /// No causal key: an action whose evidence is an effect, not a block change.
    None,
    /// The client packet's block-change sequence number.
    BlockSeq { seq: u32 },
    /// A menu action's window state id and slot.
    SlotClick { state_id: u32, slot: i16 },
    /// A per-run nonce on an injected event (Demo-1, unused in Demo-0).
    Nonce { nonce: u64 },
}

/// How an action settled (architecture §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The evidence contract was met.
    Confirmed,
    /// The window closed without evidence, or evidence refuted it.
    Unconfirmed,
    /// The caller's own deadline expired before the receipt settled.
    Timeout,
    /// A reflex, take-over or new command abandoned the action.
    Preempted,
}

/// One piece of evidence that took part in a settlement.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// The evidence kind, for example `"block_air"` or `"block_changed_ack"`.
    pub kind: String,
    /// The body tick on which the observation arrived.
    pub tick: u64,
}

/// One line of the log, before the envelope fields are added.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    /// First event of a run: the pins it was played with.
    RunStarted { versions: Versions },
    /// The actuator started a primitive and registered its evidence contract.
    IntentStarted {
        intent_id: u64,
        primitive: String,
        args: Vec<String>,
        start_tick: u64,
        attribution: Attribution,
    },
    /// A receipt reached its outcome and will not change again.
    ReceiptSettled {
        receipt_id: u64,
        intent_id: u64,
        outcome: Outcome,
        evidence: Vec<EvidenceRef>,
        #[serde(default)]
        note: Option<String>,
    },
    /// Evidence arrived after its receipt had already settled.
    ///
    /// The payload field is `evidence_kind`, not `kind`: `kind` is already the
    /// event tag of every line, and JSON cannot hold both without a duplicate
    /// key.
    LateEvidence {
        receipt_id: u64,
        evidence_kind: String,
    },
    /// The followed connection lost framing or decoding.
    WorldDesync { reason: String },
    /// A bounded, aggregated entry from perception's denied-attempt ledger.
    Ledger {
        query: String,
        fact: String,
        count: u64,
        first_tick: u64,
        last_tick: u64,
    },
    /// Free-form operator or test note.
    Note { text: String },
}

/// One complete log line: the envelope plus its typed payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    /// Strictly increasing from 0, one per event.
    pub seq: u64,
    /// The body tick the event happened in.
    pub tick: u64,
    /// The arrival timestamp in nanoseconds, as the caller observed it.
    pub ts_ns: u64,
    #[serde(flatten)]
    pub event: EventKind,
}
