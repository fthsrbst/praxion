//! Demo-0 primitives: perception-driven state machines that emit one
//! [`praxion_headless::ControlFrame`] per tick.
//!
//! The runner takes a [`Ctx`] with the body, the raw store, perception and the
//! look style, and returns a frame plus the intent to register.

mod branch_mine;
mod common;
mod craft;
mod dig_down;
mod explore;
mod frontier;
mod heading;
mod mine;
mod move_to;
mod place;
mod runner;
mod small;
#[cfg(test)]
mod test_world;

pub use common::block_name;
pub use frontier::{FrontierChoice, choose_frontier, rank_frontiers};
pub use runner::{Ctx, IntentSpec, PrimitiveEvent, Runner, TickResult};
pub use small::fact_known;
