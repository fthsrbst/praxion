//! The teleo-reactive rule-list interpreter (architecture §6.1).
//!
//! The interpreter has no game types. It asks a [`Facts`] implementation for
//! three-valued facts and drives an [`Actuator`] with primitive calls. The
//! fake world (level 1) and, later, the headless protocol body implement the
//! same two traits.
//!
//! Each [`Interpreter::tick`] runs the §6.1 order:
//!
//! 1. preemption is an explicit call ([`Interpreter::preempt`]);
//! 2. a discrete action in flight blocks switching until it settles;
//! 3. otherwise the highest rule whose guard is `T` fires;
//! 4. if no rule is eligible, the highest `U` rule offers an
//!    [`Decision::Observe`], at most one per tick;
//! 5. if nothing is eligible, the skill fails with `no_rule`.
//!
//! Skills compose: every call is a frame on a stack, the deepest frame with a
//! `T` rule fires, a child's failure makes the calling rule ineligible for that
//! tick, and preemption suspends the whole stack. An `until` guard is a
//! commitment: `ACTION until G` keeps running the rule's action while the
//! rule's own guard is `T` and `G` is not `T` (`F` or `U`). The commitment is
//! released when `G` becomes `T`, when the own guard stops being `T`, or at
//! `max_ticks`. A primitive failure is retried, but a rule whose primitive or
//! child skill fails [`MAX_PRIMITIVE_FAILURES`] times in a row fails its skill
//! with the last kind. A success or a different rule firing resets the count.

mod interp;

use praxion_rules::{ArgValue, FactCall, PrimitiveKind};

pub use interp::{DEFAULT_MAX_TICKS, Interpreter, MAX_PRIMITIVE_FAILURES};

/// A bounded, observable interpreter transition. The driver drains these once
/// per tick and writes them to the event log; the interpreter itself never
/// logs and never grows an unbounded buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpreterEvent {
    /// A skill frame was pushed; `depth` is one-based.
    FramePushed {
        skill: String,
        depth: u32,
        args: Vec<String>,
    },
    /// A skill frame completed (`done`).
    FrameDone { skill: String, depth: u32 },
    /// A skill frame failed with `kind`.
    FrameFailed {
        skill: String,
        depth: u32,
        kind: String,
    },
}

/// A fact's three-valued truth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Truth {
    True,
    False,
    /// The fact could not be answered; the caller may `observe` it.
    Unknown(FactCall),
}

/// An int fact's value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntValue {
    Known(i64),
    Unknown(FactCall),
}

/// The world as the interpreter sees it.
pub trait Facts {
    fn bool_fact(&mut self, call: &FactCall) -> Truth;
    fn int_fact(&mut self, call: &FactCall) -> IntValue;
}

/// What a primitve reports when it settles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Confirmed,
    Failed(String),
    Timeout,
    Preempted,
}

/// A primitive call the interpreter asks the actuator to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartAction {
    pub primitive: String,
    pub args: Vec<ArgValue>,
}

/// The body-facing view of the primitive the interpreter chose this tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunAction {
    pub primitive: String,
    pub args: Vec<ArgValue>,
    pub kind: PrimitiveKind,
    pub max_ticks: u32,
}

/// One frame of the call stack in a [`Decision::Run`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameRef {
    pub skill: String,
    /// The rule selected in this frame; for parents, the rule that called the
    /// child.
    pub rule: usize,
}

/// The interpreter's output for one tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Run {
        action: RunAction,
        frame_path: Vec<FrameRef>,
    },
    /// The highest unknown rule wants this fact observed.
    Observe(FactCall),
    Done,
    Failed(String),
    /// No external action this tick: an action is in flight, the stack is
    /// suspended, a child finished, or the skill already ended.
    Waiting(String),
}

/// The actuator: starts discrete primitives, is driven every tick for
/// continuous ones, and reports outcomes.
pub trait Actuator {
    /// Starts a discrete primitive at `tick`; returns the simulated duration in
    /// ticks. The outcome is later reported by [`Actuator::poll`].
    fn start(&mut self, action: &StartAction, tick: u64) -> u32;
    /// The outcome of the in-flight discrete action, if it settled by `tick`.
    fn poll(&mut self, tick: u64) -> Option<Outcome>;
    /// Abandons the in-flight discrete action; reports `Preempted`.
    fn cancel(&mut self) -> Option<Outcome>;
    /// Drives a continuous primitive for this tick.
    fn apply(&mut self, action: &StartAction, tick: u64);
    /// The selected continuous primitive is no longer selected.
    fn release(&mut self);
}

/// Kleene negation (§6.1).
pub fn kleene_not(value: Truth) -> Truth {
    match value {
        Truth::True => Truth::False,
        Truth::False => Truth::True,
        unknown @ Truth::Unknown(_) => unknown,
    }
}

/// Kleene conjunction (§6.1). `False` dominates, then `Unknown`; the first
/// unknown is kept so the interpreter can offer it for observation.
pub fn kleene_and(left: Truth, right: Truth) -> Truth {
    match (left, right) {
        (Truth::False, _) | (_, Truth::False) => Truth::False,
        (Truth::True, Truth::True) => Truth::True,
        (unknown @ Truth::Unknown(_), Truth::True) => unknown,
        (Truth::True, unknown @ Truth::Unknown(_)) => unknown,
        (unknown @ Truth::Unknown(_), Truth::Unknown(_)) => unknown,
    }
}

/// Kleene disjunction (§6.1). `True` dominates, then `Unknown`.
pub fn kleene_or(left: Truth, right: Truth) -> Truth {
    match (left, right) {
        (Truth::True, _) | (_, Truth::True) => Truth::True,
        (Truth::False, Truth::False) => Truth::False,
        (unknown @ Truth::Unknown(_), Truth::False) => unknown,
        (Truth::False, unknown @ Truth::Unknown(_)) => unknown,
        (unknown @ Truth::Unknown(_), Truth::Unknown(_)) => unknown,
    }
}

#[cfg(test)]
mod tests;
