//! Demo-0 primitives: perception-driven state machines that emit one
//! [`praxion_headless::ControlFrame`] per tick.
//!
//! The runner is independent of the live driver: it takes a [`Ctx`] with the
//! body, the raw store, perception and the look style, and returns a frame plus
//! the intent to register. [`LiveDriver`] connects it to the headless session,
//! the receipts engine and (for a skill) the interpreter.

mod live;
mod runner;

pub use live::{LiveDriver, LiveMode, LiveOptions, Summary};
pub use runner::{
    Ctx, IntentSpec, Runner, TickResult, answer_is_known, block_name, fact_known,
};
