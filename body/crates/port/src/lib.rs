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
//! - `pop { id, intent_id }`
//! - `resync_request`
//! - `ping`
//!
//! Every body message except `hello` and `bye` carries `seq`: an event's
//! message `seq` is the event-log `seq`; the other messages carry the last
//! event `seq` known when they were sent. A `resync_request` is answered with
//! one `snapshot` at seq S and events with `seq > S` follow.
//!
//! The crate is standalone: [`PortServer`] runs the network side on its own
//! thread, and the driver (wired in step A2) calls [`PortServer::publish`],
//! [`PortServer::publish_snapshot`] and [`PortServer::poll_incoming`] from its
//! tick thread. None of them waits for the network: publishing is a bounded
//! `try_send`, and an overflow is reported with `dropped(n)` followed by a
//! fresh snapshot.
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
    ConnectionState, PortConfig, PortServer, PortStats, PublishOutcome, DEFAULT_QUEUE_CAPACITY,
};
pub use wire::{
    BodyMessage, BrainMessage, ByeReason, Capability, Command, Facts, PORT_VERSION, Snapshot,
    StackEntry,
};

#[cfg(test)]
mod tests;
