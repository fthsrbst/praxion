//! The reflex engine: visibility ages, the latency gate, one active routine and
//! the frame it emits.
//!
//! The engine is not the owner of the body. It answers "is a reflex running,
//! and what does it want this tick"; the arbiter decides to acquire (which
//! bumps the generation and preempts the interpreter) and to resume.

use praxion_headless::{Body, ControlFrame};
use praxion_planner::KnownBlocks;
use praxion_world::BlockPos;

use crate::motion::Motion;
use crate::reflex::{
    LatencyClass, MELEE_COOLDOWN_TICKS, ReflexAction, ReflexKind, ReflexState, action_for,
    evaluate,
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
    /// The last tick the routine was seen to make progress toward air.
    /// A drowning reflex is not cut at `max_reflex_ticks` while it is still
    /// advancing.
    progress_tick: u64,
    /// The cell the current plan heads for: a water cell the body can breathe
    /// from, or a bank. Whether it is a water cell or a bank is not stored;
    /// reaching it with the head in air is what ends the reflex.
    air_target: Option<BlockPos>,
    /// The closest the body has come to [`Active::air_target`] on this plan.
    air_best: Option<f64>,
}

impl Active {
    fn ran_for(&self, tick: u64) -> u64 {
        tick.saturating_sub(self.started)
    }

    /// Fold this tick's state into the progress clock. A drowning reflex is
    /// making progress when its head is out of the water or when it got
    /// measurably closer to its air target. Picking a target is not progress
    /// by itself: a route switch would otherwise keep a stuck body alive
    /// forever.
    fn observe_progress(&mut self, tick: u64, view: &View, body: &Body) {
        let mut progressed = !view.head_in_water;
        let target = self.motion.air_target();
        if target != self.air_target {
            // A fresh plan picked another air cell: restart the distance
            // baseline for it. The next tick decides whether the body is
            // actually closing in.
            self.air_target = target;
            self.air_best = target.map(|cell| distance_to_cell(body, cell));
        } else if let (Some(cell), Some(best)) = (target, self.air_best) {
            let distance = distance_to_cell(body, cell);
            // `PROGRESS_EPSILON` is far below one tick of movement, so even a
            // slow horizontal swim (about 0.1 blocks a tick) counts. The best
            // distance only ever falls, so bobbing in place still runs out the
            // window.
            if distance < best - PROGRESS_EPSILON {
                self.air_best = Some(distance);
                progressed = true;
            }
        }
        if progressed {
            self.progress_tick = tick;
        }
    }
}

/// How much closer to the air target counts as progress, in blocks. A tick of
/// the body's slowest swim is about 0.1 blocks, so this only rejects jitter.
const PROGRESS_EPSILON: f64 = 1.0e-3;

/// The distance from the body's feet to the centre of an air cell.
fn distance_to_cell(body: &Body, cell: BlockPos) -> f64 {
    body.position().distance_to(cell.center())
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
    /// The same reflex continues with a new action, in place: its window was
    /// renewed (another 30 ticks of attack) or it changed what it does (an
    /// attack became a flee at low health, or a new target). No generation
    /// bump and no new owner: the body never left the reflex.
    Renewed {
        kind: ReflexKind,
        action: ReflexAction,
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
            ReflexTick::Started { frame, .. }
            | ReflexTick::Running { frame, .. }
            | ReflexTick::Renewed { frame, .. } => Some(frame),
            ReflexTick::Idle | ReflexTick::Ended { .. } => None,
        }
    }

    pub fn kind(&self) -> Option<ReflexKind> {
        match self {
            ReflexTick::Started { kind, .. }
            | ReflexTick::Running { kind, .. }
            | ReflexTick::Renewed { kind, .. }
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

    /// The body died and respawned: it is a new player in a new place. Forget
    /// everything the old life taught the reflexes (air, burning, the last
    /// safe cell, arrow headings, cooldowns, what was visible since when) and
    /// drop the running routine; a routine that walked toward the old life's
    /// safe cell would walk the new body into the world (decision 2026-11-09
    /// "Five next areas", critique finding 11).
    pub fn on_death(&mut self) {
        self.state = ReflexState::new();
        self.active = None;
    }

    pub fn active(&self) -> Option<ReflexKind> {
        self.active.as_ref().map(|active| active.kind)
    }

    /// The action the active routine is carrying out, for logs and tests.
    pub fn active_action(&self) -> Option<ReflexAction> {
        self.active.as_ref().map(|active| active.action)
    }

    /// Tell the reflexes which mob the running skill is engaged with (see
    /// [`ReflexState::set_engagement`]). The arbiter calls this whenever the
    /// skill's in-flight primitive changes.
    pub fn set_engagement(&mut self, mob: Option<&str>) {
        self.state.set_engagement(mob);
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

        // A higher-priority trigger takes the body from a running lower one:
        // an arrow or a creeper must not wait for a fight to end (decision
        // 2026-11-10). The preempted routine is dropped without a cooldown: it
        // did not end on its own, and it may come back once the higher one
        // ends.
        if let Some(active) = &self.active {
            let above = active.kind.priority();
            if let Some(started) = self.try_start(view, settings, body, tick, blocks, |kind| {
                kind.priority() > above
            }) {
                return started;
            }
        }

        if let Some(active) = &mut self.active {
            let ran_ticks = active.ran_for(tick);
            let drowning = active.kind == ReflexKind::Drowning;
            let cap = u64::from(settings.max_reflex_ticks);

            // The drowning reflex owns the body until the body can breathe:
            // the air supply is above the trigger threshold, or the body
            // stands safely on land. Clearing the water while still floating
            // is not enough: handing control back with the air still low is
            // what drowned the live run.
            if drowning {
                // A view that still has the head in water is never "safe",
                // even if the block source disagrees: the fact that matters
                // is the one the body is drowning from.
                if self.state.air() > settings.air_surface_threshold
                    || (!view.head_in_water && view.is_safe_standing(blocks))
                {
                    let kind = active.kind;
                    self.active = None;
                    return ReflexTick::Ended {
                        kind,
                        ran_ticks,
                        timed_out: false,
                    };
                }

                // A plan that finished or stalled without reaching air is not
                // the end: re-plan from where the body is now, so a route whose
                // first segment ended at a frontier keeps going. Only when no
                // plan exists does the routine fall back to holding jump.
                if active.motion.ended(tick, view, body) {
                    let fresh = if view.head_in_water {
                        Motion::start(ReflexAction::SwimUp, view, settings, body, tick, blocks)
                    } else {
                        None
                    };
                    active.motion = fresh.unwrap_or(Motion::SwimUp);
                }
                active.observe_progress(tick, view, body);

                // The nominal cap only ends a reflex that stopped making
                // progress toward air: every tick that closes the distance to
                // the target (or clears the head) restarts the window.
                if tick.saturating_sub(active.progress_tick) >= cap {
                    let kind = active.kind;
                    self.active = None;
                    self.state.set_cooldown(kind, tick + 20);
                    return ReflexTick::Ended {
                        kind,
                        ran_ticks,
                        timed_out: true,
                    };
                }
                let frame = active
                    .motion
                    .step(tick, body.generation(), body, view, settings);
                return ReflexTick::Running {
                    kind: active.kind,
                    frame,
                };
            }

            let timed_out = ran_ticks >= cap;

            // The reflex's own end condition: its trigger is gone. A flee that
            // reached the goal, a fire that went out and a creeper that left
            // the range all end here, not only at the routine's own boundary.
            // Melee holds until its exit range (`ReflexKind::holds`).
            let trigger_gone = !active.kind.holds(view, &self.state, settings);

            // A melee reflex whose trigger still holds renews in place: a
            // finished attack window gets another one, and an attack becomes a
            // flee the tick health falls to the threshold. Ending and
            // re-acquiring instead bumped the generation, moved the gaze and
            // handed the skill one tick at a time (run 9).
            let mut renewed = None;
            if active.kind == ReflexKind::Melee
                && !timed_out
                && !trigger_gone
                && let Some(wanted) = action_for(active.kind, view, &self.state, settings)
                && (!same_variant(wanted, active.action) || active.motion.ended(tick, view, body))
                && let Some(motion) = Motion::start(wanted, view, settings, body, tick, blocks)
            {
                active.motion = motion;
                active.action = wanted;
                renewed = Some(wanted);
            }

            if timed_out || trigger_gone || active.motion.ended(tick, view, body) {
                let kind = active.kind;
                self.active = None;
                if kind == ReflexKind::Melee {
                    // Any end of a fight leaves the body to the skill for a
                    // second before the reflex may take it again.
                    self.state.set_cooldown(kind, tick + MELEE_COOLDOWN_TICKS);
                } else if timed_out {
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
            let frame = active
                .motion
                .step(tick, body.generation(), body, view, settings);
            return match renewed {
                Some(action) => ReflexTick::Renewed {
                    kind: active.kind,
                    action,
                    frame,
                },
                None => ReflexTick::Running {
                    kind: active.kind,
                    frame,
                },
            };
        }

        self.try_start(view, settings, body, tick, blocks, |_| true)
            .unwrap_or(ReflexTick::Idle)
    }

    /// Start the first reflex, in priority order, that `eligible` admits and
    /// whose trigger is present, off cooldown and past its latency gate. A
    /// started reflex replaces whatever was running.
    fn try_start(
        &mut self,
        view: &View,
        settings: &Settings,
        body: &mut Body,
        tick: u64,
        blocks: &mut dyn KnownBlocks,
        eligible: impl Fn(ReflexKind) -> bool,
    ) -> Option<ReflexTick> {
        for kind in ReflexKind::ALL {
            if !eligible(kind)
                || self.state.on_cooldown(kind, tick)
                || !kind.triggered(view, &self.state, settings)
            {
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
            let air_target = motion.air_target();
            self.active = Some(Active {
                kind,
                action,
                motion,
                started: tick,
                progress_tick: tick,
                air_target,
                air_best: air_target.map(|cell| distance_to_cell(body, cell)),
            });
            return Some(ReflexTick::Started {
                kind,
                action,
                frame,
            });
        }

        None
    }
}

/// Whether two actions are the same kind of thing (an attack and an attack,
/// a flee and a flee), whatever their targets.
fn same_variant(a: ReflexAction, b: ReflexAction) -> bool {
    std::mem::discriminant(&a) == std::mem::discriminant(&b)
}

/// The human-floor gate as a number, for logs and tests: two ticks of prior
/// visibility are needed on top of the first sighting.
pub fn human_floor_ticks(_class: LatencyClass) -> u64 {
    crate::settings::HUMAN_FLOOR_TICKS
}
