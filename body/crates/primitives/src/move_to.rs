//! `move_to`: plan a walking route to a block or entity and follow it.

use azalea_core::position::{BlockPos, Vec3};
use azalea_registry::builtin::EntityKind;
use praxion_headless::ControlFrame;
use praxion_interpreter::Outcome;
use praxion_planner::{Follower, Goal, PerceptionBlocks, Planner, PlanningState, center};

use crate::common::{aim, block_target, entity_target, idle, plan_inventory};
use crate::explore::body_planner;
use crate::runner::{Ctx, IntentSpec};

/// How close `move_to` must get to a block or entity. One block of horizontal
/// or diagonal clearance, or standing on top of it, is 1.0-1.41. A radius of
/// 2.0 would also accept the cell two blocks directly below a marker, which
/// let a follower stop under a ledge instead of climbing onto it.
const MOVE_TO_RADIUS: f32 = 1.5;

#[derive(Debug)]
pub(crate) struct Move {
    target: String,
    planner: Planner,
    follower: Option<Follower>,
    observe: Option<(Vec3, u32)>,
    tries: u32,
    waited: u32,
    path_ticks: u64,
    /// The block the first plan resolved. A replan (after a stall) must not
    /// re-resolve it: swimming can put the marker behind a ledge, and losing
    /// sight of a chosen goal is not the same as the goal disappearing.
    block_goal: Option<BlockPos>,
}

impl Move {
    pub(crate) fn new(target: String) -> Self {
        Self {
            target,
            planner: body_planner(),
            follower: None,
            observe: None,
            tries: 0,
            waited: 0,
            path_ticks: 0,
            block_goal: None,
        }
    }

    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        self.waited += 1;
        if let Some((point, ticks)) = self.observe {
            let (look, aligned) = aim(ctx, point);
            frame.look = look;
            if aligned || self.waited - ticks > 20 {
                self.observe = None;
            }
            return (frame, None, None);
        }
        if let Some(follower) = &mut self.follower {
            if follower.is_done() {
                return (frame, None, Some(Outcome::Confirmed));
            }
            if follower.is_stalled() {
                let progress = follower.progress(ctx.body);
                log::debug!(
                    "move_to stalled step={}/{} pos={:?} deviation={:.3} ticks={}",
                    progress.step,
                    progress.total,
                    ctx.body.position(),
                    progress.deviation,
                    progress.ticks_on_step
                );
                self.follower = None;
                self.tries += 1;
            } else {
                let next = follower.next_step();
                let _ = next;
                let built = follower.step(ctx.tick, ctx.body.generation(), ctx.body);
                let pending = if started {
                    None
                } else {
                    Some(IntentSpec::Movement {
                        primitive: "move_to".to_owned(),
                        args: vec![self.target.clone()],
                        window: self.path_ticks + 40,
                    })
                };
                return (built, pending, None);
            }
        }
        if self.tries > 3 {
            return (frame, None, Some(Outcome::Failed("no_path".into())));
        }
        let start = ctx.body.position().to_block_pos_floor();
        let goal = match self.resolve_goal(ctx) {
            Some(goal) => goal,
            None => {
                if self.waited > 80 {
                    return (frame, None, Some(Outcome::Failed("no_target".into())));
                }
                return (frame, None, None);
            }
        };
        let inventory = plan_inventory(ctx.world);
        let state = PlanningState::new(inventory);
        let plan = {
            let mut source = PerceptionBlocks {
                perception: ctx.perception,
                world: ctx.world,
            };
            self.planner.plan(&mut source, &state, start, goal)
        };
        self.path_ticks = plan.cost.ceil().max(0.) as u64;
        log::debug!(
            "move_to plan status={:?} cost={} start={:?} goal={:?} tries={} steps={:?}",
            plan.status,
            plan.cost,
            start,
            goal,
            self.tries,
            plan.steps
                .iter()
                .map(|step| (step.kind.name(), step.from, step.to))
                .collect::<Vec<_>>()
        );
        match plan.status {
            praxion_planner::PlanStatus::Success => {
                self.follower = Some(Follower::new(plan.steps));
            }
            praxion_planner::PlanStatus::Partial { cell } => {
                self.tries += 1;
                self.observe = Some((center(cell), self.waited));
                return (frame, None, None);
            }
            praxion_planner::PlanStatus::Timeout => {
                self.tries += 1;
            }
            praxion_planner::PlanStatus::NoPath { .. } => {
                return (frame, None, Some(Outcome::Failed("no_path".into())));
            }
        }
        (frame, None, None)
    }

    fn resolve_goal(&mut self, ctx: &mut Ctx<'_>) -> Option<Goal> {
        // `Goal::Block` wants the feet in exactly that cell; a solid block (a
        // tree, a marker) can never be entered. Move *to* the block means the
        // nearest standable cell within reach of it, the same rule entities
        // use.
        if self.target.parse::<EntityKind>().is_ok() {
            // An entity moves, so its goal is resolved again for each plan.
            let (_, position) = entity_target(ctx, &self.target, 32)?;
            let cell = position.to_block_pos_floor();
            return Some(Goal::Near {
                pos: cell,
                radius: MOVE_TO_RADIUS,
            });
        }
        if self.block_goal.is_none() {
            self.block_goal = block_target(ctx, &self.target, 32);
        }
        self.block_goal.map(|pos| Goal::Near {
            pos,
            radius: MOVE_TO_RADIUS,
        })
    }
}
