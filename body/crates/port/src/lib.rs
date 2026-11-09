//! The body port v0: the brain's only channel into the body.
//!
//! Specification: `docs/proposals/2026-11-08-demo2-loop.md` §2 (transport and
//! handshake 2.1, message set 2.2, liveness and back-pressure 2.5) and
//! `docs/architecture.md` §5.4.
//!
//! Transport: loopback TCP, one brain connection. Each frame is a 4-byte
//! big-endian length followed by UTF-8 JSON. Frames larger than 1 MiB close
//! the connection. The body listens; the brain connects. The body sends its
//! `hello` first and then waits for the brain's `hello`. A second connection
//! replaces the first: the old one gets `bye { reason: "replaced" }`.
//!
//! Body to brain messages:
//!
//! - `hello { port_version, caps, body, run_id }`
//! - `event { seq, tick, event }` — an event-log line, `seq` is its log `seq`
//! - `snapshot { seq, tick, generation, owner, stack, facts, stale_ticks }`
//! - `dropped { seq, n }` — `n` queued events were discarded
//! - `pong { seq, tick }`
//! - `bye { reason }`
//!
//! Brain to body messages:
//!
//! - `hello { port_version }`
//! - `command { id, skill, args, tier, expires_at_tick, based_on_generation,
//!   condition, budget_ticks }`
//! - `pop { id, intent_id }` — cancel the running intent, or remove a done or
//!   suspended one (architecture §5.4 "stop and cancel always work")
//! - `resync_request`
//! - `ping`
//!
//! Every body message except `hello` and `bye` carries `seq`: an event's
//! message `seq` is the event-log `seq`; the other messages carry the last
//! event `seq` known when they were sent. A `resync_request` is answered with
//! one `snapshot` at seq S and events with `seq > S` follow.
//!
//! The crate is standalone: [`PortServer`] runs the network side on its own
//! thread, and the driver calls [`PortServer::publish`],
//! [`PortServer::publish_snapshot`] and [`PortServer::poll_incoming`] from its
//! tick thread. None of them waits for the network: publishing is a bounded
//! `try_send`, and an overflow is reported with `dropped(n)` followed by a
//! fresh snapshot. [`forwards`] is the event filter the driver applies before
//! publishing, and [`PortServer::last_brain_message`] gives it the 3 s silence
//! rule.
//!
//! # Example
//!
//! ```no_run
//! use praxion_port::{PortConfig, PortServer};
//!
//! # fn main() -> std::io::Result<()> {
//! let server = PortServer::bind(
//!     "127.0.0.1:0".parse().unwrap(),
//!     PortConfig { body: "praxion dev".to_owned(), run_id: "run-1".to_owned() },
//! )?;
//! println!("brain connects to {}", server.local_addr());
//! # Ok(())
//! # }
//! ```

mod frame;
mod server;
mod wire;

pub use frame::{FrameError, FrameReader, MAX_FRAME_SIZE, write_frame};
pub use server::{
    ConnectionState, DEFAULT_QUEUE_CAPACITY, PortConfig, PortServer, PortStats, PublishOutcome,
};
pub use wire::{
    BodyMessage, BrainMessage, ByeReason, Capability, Command, Facts, PORT_VERSION, SlotFact, SayPriority,
    Snapshot, StackEntry,
};

/// Whether one event-log line is forwarded to the brain (proposal §2.2).
///
/// The filter is deliberately narrow: the brain needs the intent lifecycle,
/// the owner changes, wakes, command receipts and the root skill frame. High
/// rate evidence (`rule_selected`, `primitive_finished`, `explore_frontier`,
/// receipt and ledger lines) stays in `events.jsonl` for the post-hoc digest.
pub fn forwards(event: &praxion_eventlog::EventKind) -> bool {
    use praxion_eventlog::EventKind;
    match event {
        EventKind::IntentPushed { .. }
        | EventKind::IntentState { .. }
        | EventKind::OwnerChanged { .. }
        | EventKind::ReplanNeeded { .. }
        | EventKind::WakeBrain { .. }
        | EventKind::CommandReceipt { .. }
        | EventKind::ChatReceived { .. }
        | EventKind::ChatSent { .. }
        | EventKind::ChatDropped { .. }
        | EventKind::RunStarted { .. } => true,
        // Notes are debug text (the gaze writes one almost every tick); they
        // filled the 256-message queue and a goal's `settled` receipt was lost
        // in the overflow (live port test 2026-11-08). The brain never reads them.
        EventKind::SkillFrame { depth, .. } => *depth == 1,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
