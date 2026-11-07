//! The tiny motion routines a reflex owns. No skills, no brain, no network:
//! an action becomes a `ControlFrame` for this tick and nothing else.
//!
//! Planner-backed routines are `Flee`, `StepBack`, `ToWater` and `OffFire`.
//! They plan once when the reflex acquires (never in the hot per-tick path)
//! and then follow the returned steps. The fall guard runs on every frame a
//! reflex emits: a reflex never walks the body off a drop deeper than three.

use praxion_headless::{Body, Click, ControlFrame, Held, Look};
use praxion_planner::{
    Follower, Goal, Inventory, MoveKind, Personality, Planner, PlanStatus, PlanningState, Step,
    WALK_TOLERANCE_DEG, aim, look_towards,
};
use praxion_planner::KnownBlocks;
use praxion_world::{BlockPos, Vec3};

use crate::reflex::ReflexAction;
use crate::settings::Settings;
use crate::view::{Cell, View};

/// The deepest drop a reflex may walk off. The planner's `max_drop` is the
/// same number (session 6).
pub const SAFE_DROP: u8 = 3;

/// How long a strafe or an attack runs at most, in ticks.
const STRAFE_TICKS: u64 = 8;
const ATTACK_TICKS: u64 = 30;
const EAT_TICKS: u64 = 4;

/// One running routine.
#[derive(Clone, Debug)]
pub enum Motion {
    Follow {
        /// Boxed because a `Follower` dwarfs the other variants.
        follower: Box<Follower>,
        /// Give up if the follower has not reached the last step by then.
        deadline: u64,
        /// A flee sprints; a careful step does not.
        sprint: bool,
    },
    Strafe { right: bool, until: u64 },
    SwimUp,
    Attack { target: i32, until: u64 },
    Eat { until: u64, clicked: bool },
    /// The body is already in the water the fire routine wanted.
    Wait { until: u64 },
}

impl Motion {
    /// Turn an action into a routine. `None` means there is nothing to do: the
    /// body is already there, no path exists, or every direct step would enter
    /// a cell the body may not enter (an unknown cell, lava, fire, a wall).
    pub fn start(
        action: ReflexAction,
        view: &View,
        settings: &Settings,
        body: &Body,
        tick: u64,
        blocks: &mut dyn KnownBlocks,
    ) -> Option<Motion> {
        match action {
            ReflexAction::Flee { from, distance } => {
                let away = horizontal_away(view.position, from);
                let target = view.position + away * f64::from(distance.max(1.0));
                let goal = Goal::Near {
                    pos: target.to_block_pos_floor(),
                    radius: 1.5,
                };
                let away_cell = if away.x.abs() >= away.z.abs() {
                    view.feet.east(away.x.signum() as i32)
                } else {
                    view.feet.south(away.z.signum() as i32)
                };
                let side = if away.x.abs() >= away.z.abs() {
                    view.feet.south(1)
                } else {
                    view.feet.east(1)
                };
                Self::path(goal, body, tick, blocks, true).or_else(|| {
                    Self::single_step(view, &[away_cell, side, view.feet.up(1)], true, blocks)
                })
            }
            ReflexAction::StepBack { to } => Self::path(Goal::Block(to), body, tick, blocks, false)
                .or_else(|| Self::single_step(view, &steps_from(view, to), false, blocks)),
            ReflexAction::ToWater { pos } => {
                if view.feet_in_water {
                    Some(Motion::Wait { until: tick + 1 })
                } else {
                    Self::path(Goal::Block(pos), body, tick, blocks, false).or_else(|| {
                        let toward = cardinal_towards(view.feet, pos);
                        Self::single_step(view, &[toward], false, blocks)
                    })
                }
            }
            ReflexAction::OffFire { to } => Self::path(Goal::Block(to), body, tick, blocks, false)
                .or_else(|| Self::single_step(view, &steps_from(view, to), false, blocks)),
            ReflexAction::Strafe { right } => Some(Motion::Strafe {
                right,
                until: tick + STRAFE_TICKS,
            }),
            ReflexAction::SwimUp => Some(Motion::SwimUp),
            ReflexAction::Attack { target, .. } => Some(Motion::Attack {
                target,
                until: tick + ATTACK_TICKS.min(u64::from(settings.max_reflex_ticks)),
            }),
            ReflexAction::Eat { .. } => Some(Motion::Eat {
                until: tick + EAT_TICKS,
                clicked: false,
            }),
        }
    }

    fn path(
        goal: Goal,
        body: &Body,
        tick: u64,
        blocks: &mut dyn KnownBlocks,
        sprint: bool,
    ) -> Option<Motion> {
        let mut planner = Planner::new();
        planner.set_max_drop(i32::from(SAFE_DROP));
        // A reflex never digs or places: those are world changes with receipts.
        planner.set_allow_world_changes(false);
        let start = body.position().to_block_pos_floor();
        let state = PlanningState::new(Inventory::empty());
        let plan = planner.plan(blocks, &state, start, goal);
        let reachable = matches!(plan.status, PlanStatus::Success | PlanStatus::Partial { .. });
        if !reachable || plan.steps.is_empty() {
            return None;
        }
        let ticks = plan.cost.ceil().max(1.0) as u64 + 40;
        Some(Motion::Follow {
            follower: Box::new(
                Follower::new(plan.steps, blocks, &state, Personality::default())
                    .with_max_ticks_per_step(60),
            ),
            deadline: tick + ticks,
            sprint,
        })
    }

    /// One direct step, for a cell the planner could not reach in one segment.
    ///
    /// Every candidate is checked through the perception seam before it is
    /// used: a cell the body may not enter (an unknown cell, lava, fire, a
    /// solid block) is skipped, and an unknown cell below it means the fall
    /// guard will refuse the step anyway. The first candidate that is known
    /// safe wins; when none is, `None` says "stay put", which is always safer
    /// than stepping into a cell nobody saw.
    fn single_step(
        view: &View,
        candidates: &[BlockPos],
        sprint: bool,
        blocks: &mut dyn KnownBlocks,
    ) -> Option<Motion> {
        let to = candidates
            .iter()
            .copied()
            .find(|cell| *cell != view.feet && Cell::probe(blocks, *cell).is_safe_to_enter())?;
        let step = Step {
            from: view.feet,
            to,
            kind: MoveKind::Walk,
            primitives: Vec::new(),
            cost: 1.0,
        };
        Some(Motion::Follow {
            follower: Box::new(Follower::new(
                vec![step],
                blocks,
                &PlanningState::default(),
                Personality::default(),
            )),
            deadline: u64::MAX,
            sprint,
        })
    }

    /// This tick's frame.
    pub fn step(
        &mut self,
        tick: u64,
        generation: u32,
        body: &Body,
        view: &View,
        settings: &Settings,
    ) -> ControlFrame {
        let mut frame = match self {
            Motion::Follow {
                follower, sprint, ..
            } => {
                let mut frame = follower.step(tick, generation, body);
                if *sprint && frame.held.forward {
                    frame.held.sprint = true;
                } else if !*sprint {
                    // The reflex flag still owns sprint: a careful step never
                    // starts one, even though the follower may sprint on its
                    // own flat runs.
                    frame.held.sprint = false;
                }
                frame
            }
            Motion::Strafe { right, .. } => {
                let mut frame = ControlFrame::new(tick, generation, Held::NONE);
                if *right {
                    frame.held.right = true;
                } else {
                    frame.held.left = true;
                }
                frame
            }
            Motion::SwimUp => {
                let mut frame = ControlFrame::new(tick, generation, Held::NONE);
                frame.held.jump = true;
                frame
            }
            Motion::Attack { target, .. } => {
                let mut frame = ControlFrame::new(tick, generation, Held::NONE);
                if let Some(mob) = attack_target(view, *target) {
                    let target_point = mob.position + Vec3::new(0.0, 1.0, 0.0);
                    let (look, aligned) =
                        look_towards(body.look(), view.eye, target_point, WALK_TOLERANCE_DEG);
                    frame.look = look;
                    let reach = view.horizontal_to(mob);
                    if aligned && reach <= 3.2 {
                        frame.clicks.push(Click::Attack);
                    } else if reach > 3.2 {
                        frame.held.forward = true;
                        frame.held.sprint = true;
                    }
                } else {
                    // The target is gone; the engine will end the routine.
                    frame.look = Look::None;
                }
                frame
            }
            Motion::Eat { clicked, .. } => {
                let mut frame = ControlFrame::new(tick, generation, Held::NONE);
                if !*clicked {
                    frame.clicks.push(Click::Use);
                    *clicked = true;
                }
                frame
            }
            Motion::Wait { .. } => ControlFrame::new(tick, generation, Held::NONE),
        };
        let _ = settings;
        guard_drop(&mut frame, view);
        frame
    }

    /// Whether the routine's end condition is true at `tick`.
    pub fn ended(&self, tick: u64, view: &View, body: &Body) -> bool {
        match self {
            Motion::Follow {
                follower,
                deadline,
                ..
            } => follower.is_done() || follower.is_stalled() || tick >= *deadline,
            Motion::Strafe { until, .. } => tick >= *until,
            Motion::SwimUp => !view.head_in_water,
            Motion::Attack { target, until } => {
                tick >= *until
                    || attack_target(view, *target)
                        .is_none_or(|mob| view.horizontal_to(mob) > 6.0)
            }
            Motion::Eat { until, .. } => tick >= *until,
            Motion::Wait { until } => tick >= *until || body.is_in_water(),
        }
    }
}

/// The fall guard: a moving frame whose next cell drops more than three blocks,
/// or holds lava, becomes a sneak-and-hold frame. Sneaking keeps the body on
/// the edge in vanilla, so the reflex stops instead of falling or burning.
///
/// `next_drop` is `DROP_PROBE` for a column nobody saw, so an unknown cell is
/// refused here as well: the guard never moves the body over a floor it cannot
/// name.
fn guard_drop(frame: &mut ControlFrame, view: &View) {
    if frame.held.moves() && (view.next_drop > SAFE_DROP || view.next_cell_lava) {
        frame.held = Held {
            sneak: true,
            ..Held::NONE
        };
    }
}

fn attack_target(view: &View, target: i32) -> Option<crate::view::EntityView> {
    view.melee
        .filter(|mob| mob.id == target)
        .or_else(|| view.creeper.filter(|mob| mob.id == target))
}

/// The unit horizontal direction away from `from`.
fn horizontal_away(position: Vec3, from: Vec3) -> Vec3 {
    let dx = position.x - from.x;
    let dz = position.z - from.z;
    let length = (dx * dx + dz * dz).sqrt();
    if length < 1.0e-6 {
        Vec3::new(0.0, 0.0, 1.0)
    } else {
        Vec3::new(dx / length, 0.0, dz / length)
    }
}

/// The cells a step-away action tries, in order: the cell the action asked
/// for, then the four neighbours. The caller's preferred cell keeps its
/// meaning, and the neighbours are only there for "a known safe alternative
/// exists" when the preferred one is lava, fire or unknown. Each candidate is
/// still checked by [`Motion::single_step`], so a neighbour that is just as
/// unsafe is refused as well.
fn steps_from(view: &View, to: BlockPos) -> [BlockPos; 5] {
    [
        to,
        view.feet.north(1),
        view.feet.south(1),
        view.feet.east(1),
        view.feet.west(1),
    ]
}

/// One cardinal step from `from` toward `to`.
fn cardinal_towards(from: BlockPos, to: BlockPos) -> BlockPos {
    let dx = to.x - from.x;
    let dz = to.z - from.z;
    if dx.abs() >= dz.abs() && dx != 0 {
        from.east(dx.signum())
    } else if dz != 0 {
        from.south(dz.signum())
    } else {
        from
    }
}

/// Aim from `eye` at `target` without the delta wrapper. Kept for tests and
/// for the arbiter's Enderman guard, which needs the same convention.
pub fn yaw_pitch_towards(eye: Vec3, target: Vec3) -> (f32, f32) {
    aim(eye, target)
}
