//! The arbiter: one owner of the body (architecture §4 and §5.4, session 7).
//!
//! Tiers are `survival > command > maintenance > projects`. A need's soft
//! threshold is served at the command's next safe point; its hard threshold is
//! promoted to survival and preempts immediately. A same-tier switch needs
//! `gain > k x loss` with no `until` active. A reflex always acquires: the
//! arbiter bumps the generation, preempts the interpreter, runs the reflex
//! until its end condition or `max_reflex_ticks`, then resumes the interrupted
//! skill, which re-evaluates from the top.
//!
//! The intent stack is here too: `{intent_id, tier, skill_ref, args, state}`,
//! persisted through `praxion-eventlog` events (`intent_pushed`,
//! `intent_state`, `owner_changed`, `replan_needed`).

mod arbiter;
mod tier;

#[cfg(test)]
mod tests;

pub use arbiter::{
    Arbiter, ArbiterTick, CommandAccepted, CommandError, FrameSource, Owner,
};
pub use tier::{
    Intent, IntentStack, IntentState, NeedKind, Needs, Pushed, StackFull, Tier, arg_to_string,
};
