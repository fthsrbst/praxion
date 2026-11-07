//! Demo-0 primitives: perception-driven state machines that emit one
//! [`praxion_headless::ControlFrame`] per tick.
//!
//! The runner takes a [`Ctx`] with the body, the raw store, perception and the
//! look style, and returns a frame plus the intent to register.

mod common;
mod craft;
mod explore;
mod frontier;
mod mine;
mod move_to;
mod place;
mod runner;
mod small;

pub use common::block_name;
pub use frontier::{FrontierChoice, choose_frontier, rank_frontiers};
pub use runner::{Ctx, IntentSpec, PrimitiveEvent, Runner, TickResult};
pub use small::fact_known;
