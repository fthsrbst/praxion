//! Receipts: per-action evidence contracts and causal attribution
//! (architecture §5.3, session 4).
//!
//! An intent is registered when a primitive starts. It carries the primitive's
//! declared evidence kind, the body tick it started on, a causal attribution
//! key, and a settle window. The engine observes decoded packets (plus the
//! world the caller already applied them to) and settles the receipt when the
//! evidence arrives or the window closes, whichever is first.
//!
//! The rules that matter:
//!
//! - a tick at or after the start is necessary but not sufficient: the causal
//!   key (block-change seq, or the click's state id and slot) must match too;
//! - evidence before the start tick is ignored; it may only adjust the
//!   baseline so it cannot become evidence later;
//! - late evidence never reopens a receipt and is logged as `late_evidence`;
//! - no match gives `unconfirmed`; a preempted intent gives `preempted`.
//!
//! The crate never reads a clock: the caller passes the body tick and the
//! arrival timestamp for every observation.

mod engine;
mod intent;

pub use engine::{Engine, LATE_RETENTION_TICKS, Settlement};
pub use intent::{
    DIG_WINDOW, Expect, INVENTORY_WINDOW, Intent, IntentId, MOVE_WINDOW, PLACE_WINDOW,
    ReceiptId, SLOT_CLICK_WINDOW,
};
pub use praxion_eventlog::{Attribution, Outcome};
pub use praxion_rules::EvidenceKind;

/// Map a receipt outcome onto the interpreter's primitive outcome.
///
/// `confirmed` stays `confirmed`; `unconfirmed` is a typed failure
/// (`failed(unconfirmed)`); `timeout` and `preempted` pass through.
pub fn interpreter_outcome(outcome: Outcome) -> praxion_interpreter::Outcome {
    match outcome {
        Outcome::Confirmed => praxion_interpreter::Outcome::Confirmed,
        Outcome::Unconfirmed => praxion_interpreter::Outcome::Failed("unconfirmed".to_owned()),
        Outcome::Timeout => praxion_interpreter::Outcome::Timeout,
        Outcome::Preempted => praxion_interpreter::Outcome::Preempted,
    }
}

#[cfg(test)]
mod tests;
