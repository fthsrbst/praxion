//! Demo-0 primitives: perception-driven state machines that emit one
//! [`praxion_headless::ControlFrame`] per tick.
//!
//! The runner takes a [`Ctx`] with the body, the raw store, perception and the
//! look style, and returns a frame plus the intent to register.

mod runner;

pub use runner::{
    Ctx, FrontierChoice, IntentSpec, PrimitiveEvent, Runner, TickResult, block_name,
    choose_frontier, fact_known, rank_frontiers,
};
