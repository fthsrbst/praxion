//! The reflex engine: visibility ages, the latency gate, one active routine and
//! the frame it emits.
//!
//! The engine is not the owner of the body. It answers "is a reflex running,
//! and what does it want this tick"; the arbiter decides to acquire (which
//! bumps the generation and preempts the interpreter) and to resume.

use praxion_headless::{Body, ControlFrame};
use praxion_planner::KnownBlocks;

use crate::motion::Motion;
use crate::reflex::{
    LatencyClass, ReflexAction, ReflexKind, ReflexState, evaluate,
};
use crate::settings::Settings;
use crate::view::View;

/// One reflex that owns the body right now.
#[derive(Clone, Debug)]
struct Active {
    kind: ReflexKind,
    action: ReflexAction,
    motion: Motion,
    started: u64,
}

impl Active {
    fn ran_for(&self, tick: u64) -> u64 {
        tick.saturating_sub(self.started)
    }
}

/// What the engine reports for one tick.
#[derive(Clone, Debug)]
pub enum ReflexTick {
    /// No reflex is running and none may start.
    Idle,
    /// A reflex just acquired the body. Its frame is already for the new
    /// generation.
    Started {
        kind: ReflexKind,
        action: ReflexAction,
        frame: ControlFrame,
    },
    /// The same reflex continues.
    Running {
        kind: ReflexKind,
        frame: ControlFrame,
    },
    /// The reflex ended this tick; the body may go back to a skill.
    Ended {
        kind: ReflexKind,
        ran_ticks: u64,
        /// `true` when the max-ticks bound, not the end condition, stopped it.
        timed_out: bool,
    },
}

impl ReflexTick {
    pub fn frame(&self) -> Option<&ControlFrame> {
        match self {
            ReflexTick::Started { frame, .. } | ReflexTick::Running { frame, .. } => Some(frame),
            ReflexTick::Idle | ReflexTick::Ended { .. } => None,
        }
    }

    pub fn kind(&self) -> Option<ReflexKind> {
        match self {
            ReflexTick::Started { kind, .. }
            | ReflexTick::Running { kind, .. }
            | ReflexTick::Ended { kind, .. } => Some(*kind),
            ReflexTick::Idle => None,
        }
    }
}

/// Holds the reflex memory and the active routine.
#[derive(Clone, Debug, Default)]
pub struct Engine {
    state: ReflexState,
    active: Option<Active>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> &ReflexState {
        &self.state
    }

    pub fn state_mut(&mut self) -> &mut ReflexState {
        &mut self.state
    }

    pub fn active(&self) -> Option<ReflexKind> {
        self.active.as_ref().map(|active| active.kind)
    }

    /// The action the active routine is carrying out, for logs and tests.
    pub fn active_action(&self) -> Option<ReflexAction> {
        self.active.as_ref().map(|active| active.action)
    }

    /// Run one tick. The caller must have started perception's tick and built
    /// the [`View`] from it.
    ///
    /// On `Started` the body's generation is bumped: every frame built before
    /// the reflex is stale, exactly like a take-over. The arbiter reads
    /// [`Body::generation`] after this call.
    pub fn poll(
        &mut self,
        view: &View,
        settings: &Settings,
        body: &mut Body,
        tick: u64,
        blocks: &mut dyn KnownBlocks,
    ) -> ReflexTick {
        self.state.observe(tick, view, settings, blocks);

        if let Some(active) = &mut self.active {
            let ran_ticks = active.ran_for(tick);
            let timed_out = ran_ticks >= u64::from(settings.max_reflex_ticks);
            // The reflex's own end condition: its trigger is gone. A flee that
            // reached the goal, a fire that went out and a creeper that left
            // the range all end here, not only at the routine's own boundary.
            let trigger_gone = !active.kind.triggered(view, &self.state, settings);
            if timed_out || trigger_gone || active.motion.ended(tick, view, body) {
                let kind = active.kind;
                self.active = None;
                if timed_out {
                    // Do not immediately re-acquire the same reflex: give the
                    // skill a few ticks or the threat a chance to move.
                    self.state.set_cooldown(kind, tick + 20);
                }
                return ReflexTick::Ended {
                    kind,
                    ran_ticks,
                    timed_out,
                };
            }
            let frame = active.motion.step(tick, body.generation(), body, view, settings);
            return ReflexTick::Running {
                kind: active.kind,
                frame,
            };
        }

        for kind in ReflexKind::ALL {
            if self.state.on_cooldown(kind, tick) || !kind.triggered(view, &self.state, settings) {
                continue;
            }
            let class = kind.latency_class(view);
            if !self.state.may_act(kind, class, tick) {
                continue;
            }
            let Some(action) = evaluate(kind, view, &self.state, settings) else {
                continue;
            };
            let Some(mut motion) = Motion::start(action, view, settings, body, tick, blocks) else {
                // Nothing the body can do right now (no path, no food). Do not
                // spin on it every tick.
                self.state.set_cooldown(kind, tick + 20);
                continue;
            };
            body.bump_generation();
            let frame = motion.step(tick, body.generation(), body, view, settings);
            self.active = Some(Active {
                kind,
                action,
                motion,
                started: tick,
            });
            return ReflexTick::Started {
                kind,
                action,
                frame,
            };
        }

        ReflexTick::Idle
    }
}

/// The human-floor gate as a number, for logs and tests: two ticks of prior
/// visibility are needed on top of the first sighting.
pub fn human_floor_ticks(_class: LatencyClass) -> u64 {
    crate::settings::HUMAN_FLOOR_TICKS
}
