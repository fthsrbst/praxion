//! The body's append-only event log (architecture §4, §8, §9).
//!
//! One file per run, `<run_dir>/events.jsonl`, one JSON object per line:
//!
//! ```text
//! {"seq": 0, "tick": 12, "ts_ns": 1793443244558717800, "kind": "note", "text": "..."}
//! ```
//!
//! `seq` starts at 0 and increases by one per event. A reader refuses a gap, a
//! corrupt line or a line the writer never finished, so a replay either yields
//! a gap-free prefix or tells the caller exactly where it stopped.
//!
//! The writer never reads a clock: every event carries the body tick and the
//! arrival timestamp its caller observed. It buffers a tick in memory and
//! writes the buffer when the tick changes, on [`Writer::flush`], and on drop,
//! so a crash loses at most the tick in progress.
//!
//! This crate has no game types. Receipt and ledger payloads are data that the
//! body fills in, which keeps the log readable by tests, the brain and the
//! memory projections without loading the world model.

mod event;
mod reader;
mod writer;

pub use event::{
    Attribution, Event, EventKind, EvidenceRef, Outcome, Versions,
};
pub use reader::{ReadError, Reader, Replay, read, replay};
pub use writer::{FILE_NAME, Writer};

#[cfg(test)]
mod tests;
