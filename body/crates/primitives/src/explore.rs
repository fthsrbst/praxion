//! `explore`: walk toward a known target or the best unknown frontier until a
//! `near` fact holds.

use std::collections::VecDeque;
use std::time::Duration;

use azalea_core::position::BlockPos;
use azalea_registry::builtin::BlockKind;
use praxion_gamedata as gamedata;
use praxion_headless::ControlFrame;
use praxion_interpreter::Outcome;
use praxion_planner::{Follower, Goal, PerceptionBlocks, Planner, PlanningState, Step};
use praxion_world::World;

use crate::common::{block_name, idle, plan_inventory};
use crate::frontier::{
    FrontierChoice, near_block, nearest_known_block, rank_frontiers, rank_frontiers_from,
};
use crate::runner::{Ctx, IntentSpec, PrimitiveEvent};

/// How far `explore` looks for its target before it reports `not_found`. The
/// gathering skills use the same radius in their `not near(T, R)` guard.
pub const EXPLORE_NEAR_RADIUS: u32 = praxion_rules::registry::EXPLORE_NEAR_RADIUS;

/// How far `explore` looks for its target before it falls back to a frontier
/// of the known map.
pub const EXPLORE_SEARCH_RADIUS: u32 = praxion_rules::registry::EXPLORE_SEARCH_RADIUS;

/// The frontier scan radius. It must reach past the sight range, not stay
/// inside it: rays record seen cells out to 64 blocks, so a 64-block box on
/// open ground can be fully known and contain no frontier at all. The known
/// map keeps 96, so 80 still leaves an unknown ring for the flood to find.
const EXPLORE_FRONTIER_RADIUS: u32 = 80;
const EXPLORE_FRONTIER_VERTICAL: i32 = 12;

/// The planner goal radius when `explore` walks to a known target cell. The
/// call itself ends at [`EXPLORE_NEAR_RADIUS`]. Four blocks leaves room for a
/// cave wall's exposed face while staying inside the five-block mine reach.
const TARGET_APPROACH_RADIUS: f32 = 4.0;

/// Ticks one explore path step may take before the follower is considered
/// stuck and the frontier is rejected. Long enough for the jump nudge to free
/// a corner; still shorter than the default 200 so a 1200-tick search can try
/// several directions.
const EXPLORE_STEP_TICKS: u32 = 160;

#[derive(Debug)]
pub(crate) struct Explore {
    target: String,
    planner: Planner,
    follower: Option<Follower>,
    goal: Option<BlockPos>,
    /// Frontier cells this call has reached; new choices stay away from them.
    explored: Vec<BlockPos>,
    /// Frontier cells the planner could not reach.
    rejected: Vec<BlockPos>,
    /// Cells the follower stalled in; plans treat them as blocked so the next
    /// route does not repeat the same obstacle.
    blocked: Vec<BlockPos>,
    /// A ranked set of frontier candidates computed once, consumed by plan
    /// attempts instead of rescanning the whole map every tick.
    candidates: VecDeque<FrontierChoice>,
    /// Next tick at which the known map may be rescanned for a target or a
    /// frontier. The known map changes continuously, so this is a small,
    /// bounded refresh period rather than a cache invalidation storm.
    target_search_tick: u64,
    frontier_scan_tick: u64,
    /// How many `explore` calls this run has made, so a retry starts from a
    /// different frontier instead of repeating the same walk.
    rotation: usize,
    /// Ticks with nowhere to go. A closed or fully known pocket fails early so
    /// the skill's bounded retries can try another direction instead of
    /// sleeping through the whole 1200-tick budget.
    idle_ticks: u32,
    /// Water frontiers and paths through water are allowed. Off at first;
    /// switched on once, when no dry frontier is reachable.
    allow_water: bool,
    /// Walks back to an explored cell since the last new frontier. Two such
    /// walks on seed 101 became an endless shuttle between two cells, each
    /// resetting the idle count, so dry land never counted as exhausted.
    walk_backs: u32,
    /// A plan attempt may not start before this tick. Failed A* calls are the
    /// most expensive thing explore does, so a doomed candidate list is
    /// consumed a few ticks apart, not one per tick.
    next_plan_tick: u64,
    /// How many goals this call has reached. The first few stay nearest-first,
    /// so a nearby forest is found quickly; after that the ranking heads away
    /// from the origin instead of circling the sight ring (seed 202).
    goals: u32,
    /// Bounded diagnostics for the driver's event log.
    pub(crate) events: Vec<PrimitiveEvent>,
    waited: u32,
    budget: u32,
    path_ticks: u64,
}

/// How often `explore` refreshes its target and frontier searches. The
/// completion check runs every tick; these wider scans are the expensive part
/// and the map does not move far in a few ticks.
const EXPLORE_SEARCH_PERIOD_TICKS: u64 = 20;

/// Ticks `explore` may have no target, no ranked candidate and no follower
/// before it fails `not_found`. A closed pocket fails early instead of
/// sleeping through the 1200-tick budget.
const EXPLORE_IDLE_TICKS: u32 = 120;

/// Idle ticks after which `explore` stops insisting on dry land and plans
/// through water too (user decision, 2026-11-03). On seed 101 the body stood on
/// a beach between a three-block bank and a lake: dry-only explore had no
/// frontier for 22,000 ticks. Swimming is the planner's own move set, and the
/// drowning reflex guards it.
const EXPLORE_WATER_AFTER_IDLE_TICKS: u32 = 40;

/// Walks back to explored cells without a new frontier after which dry land
/// counts as exhausted too.
const EXPLORE_WALK_BACKS_BEFORE_WATER: u32 = 2;

/// Plan attempts per tick when consuming a ranked frontier list. One keeps a
/// slow A* out of the tick's tail; the remaining candidates wait for the next
/// tick.
const EXPLORE_PLAN_ATTEMPTS: usize = 1;

/// The planner budget one body primitive may spend inside a tick. The
/// planner's default 100 ms is a standalone-search limit; a 20 Hz body tick
/// cannot spend it, so a route that needs longer is tried again next tick.
const BODY_PLAN_BUDGET: praxion_planner::Budget =
    praxion_planner::Budget::new(Duration::from_millis(6), 30_000);

pub(crate) fn body_planner() -> Planner {
    Planner::with_budget(BODY_PLAN_BUDGET)
}

/// Explored, rejected and blocked cells kept per `explore` call. Old cells are
/// behind the search and no longer influence it; keeping the list bounded is
/// what makes the per-tick cost independent of how long the run has walked.
const EXPLORE_MEMORY_CELLS: usize = 64;

fn remember(cells: &mut Vec<BlockPos>, pos: BlockPos) {
    if cells.contains(&pos) {
        return;
    }
    if cells.len() >= EXPLORE_MEMORY_CELLS {
        cells.remove(0);
    }
    cells.push(pos);
}

/// Cells recent `explore` calls reached or rejected. A call used to forget its
/// explored set when it ended, so the next 1200-tick retry walked the same way.
/// The set is kept for the run, newest last, and bounded.
const RECENT_EXPLORED_CELLS: usize = 128;

/// The memory one run's `explore` calls share. It used to be process-wide
/// statics that two runners (parallel tests, two bodies) shared; `Runner` owns
/// one, so a run only ever sees its own history.
#[derive(Debug, Default)]
pub(crate) struct ExploreMemory {
    /// Cells recent `explore` calls reached or rejected, oldest first.
    recent: VecDeque<BlockPos>,
    /// The highest surface altitude this run's `explore` calls stood at, once
    /// one has run.
    home_y: Option<i32>,
    /// The first cell this run explored, as `(x, z)`.
    origin: Option<(i32, i32)>,
    /// How many `explore` calls this run has started.
    calls: u64,
}

impl ExploreMemory {
    /// The remembered cells, oldest first.
    fn recent(&self) -> Vec<BlockPos> {
        self.recent.iter().copied().collect()
    }

    /// Remember a cell, newest last. An already remembered cell keeps its
    /// place; a full memory drops its oldest cell.
    fn remember(&mut self, pos: BlockPos) {
        if self.recent.contains(&pos) {
            return;
        }
        if self.recent.len() >= RECENT_EXPLORED_CELLS {
            self.recent.pop_front();
        }
        self.recent.push_back(pos);
    }

    /// The highest surface altitude recent `explore` calls stood at. Wood grows
    /// on the surface; an explore that follows one-block descents into a cave
    /// can burn every budget underground, so a candidate much below this line
    /// is refused.
    fn home_y(&mut self, start: i32) -> i32 {
        let home = self.home_y.get_or_insert(start);
        *home = (*home).max(start);
        *home
    }

    /// The first cell this body explored, as (x, z). Frontier ranking pushes
    /// away from it, so a call heads outward instead of circling the nearest
    /// ring of the known map. One run is one body, so a per-run origin is
    /// exact.
    fn origin(&mut self, start: BlockPos) -> BlockPos {
        let (x, z) = *self.origin.get_or_insert((start.x, start.z));
        BlockPos::new(x, start.y, z)
    }

    /// A per-run counter so consecutive `explore` calls (a failed call and its
    /// retry, or a new skill after a failure) start from a different frontier
    /// in the ranked list. Without it, a retry repeated the same walk from an
    /// empty explored set and could burn every 1200-tick budget in the same
    /// direction.
    fn next_rotation(&mut self) -> usize {
        let rotation = (self.calls % 8) as usize;
        self.calls = self.calls.wrapping_add(1);
        rotation
    }
}

impl Explore {
    pub(crate) fn new(target: String, memory: &mut ExploreMemory) -> Result<Self, String> {
        if let Some(tag) = target.strip_prefix('#') {
            if gamedata::block_tag(tag).is_none() {
                return Err(format!("unknown block tag `#{tag}`"));
            }
        } else if target.parse::<BlockKind>().is_err() {
            return Err(format!("unknown block `{target}`"));
        }
        Ok(Self {
            target,
            planner: {
                // Explore walks; it never digs or places. A frontier that
                // needs a world change is rejected and another one is chosen,
                // so the follower never stalls on a dig/place step it cannot
                // execute (the layer above owns those primitives).
                //
                // Descending uses the planner's default, safe drop (three
                // blocks, no fall damage). An eight-block drop is survivable
                // but can land in a one-cell pit with no route back up: on
                // seed 303 that ended every later explore in the 120-tick
                // idle (runs/s9d-f11-3-seed303, seq 75 then 86+). A peak is
                // left by walking its slopes, not by dropping off a cliff.
                let mut planner = body_planner();
                planner.set_allow_world_changes(false);
                planner
            },
            follower: None,
            goal: None,
            explored: {
                // Continue from where recent calls walked, so a retry does not
                // repeat the same direction from an empty set.
                let mut explored = memory.recent();
                if explored.len() > EXPLORE_MEMORY_CELLS {
                    explored.drain(..explored.len() - EXPLORE_MEMORY_CELLS);
                }
                explored
            },
            rejected: Vec::new(),
            blocked: Vec::new(),
            candidates: VecDeque::new(),
            target_search_tick: 0,
            frontier_scan_tick: 5,
            rotation: memory.next_rotation(),
            idle_ticks: 0,
            allow_water: false,
            walk_backs: 0,
            next_plan_tick: 0,
            goals: 0,
            events: Vec::new(),
            waited: 0,
            budget: praxion_rules::registry::EXPLORE_MAX_TICKS,
            path_ticks: 0,
        })
    }

    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
        memory: &mut ExploreMemory,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let frame = idle(ctx);
        self.waited += 1;
        let start = ctx.body.position().to_block_pos_floor();
        let home = memory.home_y(start.y);
        // The completion check is cheap: a 5-block scan. The wider 32-block
        // target search runs only when a new plan is needed, never while a
        // follower is walking, so exploring keeps the 20 TPS tick rate.
        if near_block(ctx.perception, ctx.world, &self.target, EXPLORE_NEAR_RADIUS) == Some(true) {
            return (frame, None, Some(Outcome::Confirmed));
        }
        if self.waited > self.budget {
            return (frame, None, Some(Outcome::Failed("not_found".into())));
        }

        if let Some(follower) = &mut self.follower {
            if follower.is_done() {
                self.follower = None;
                self.target_search_tick = 0;
                self.frontier_scan_tick = ctx.tick.saturating_add(5);
                self.idle_ticks = 0;
                self.candidates.clear();
                if let Some(goal) = self.goal.take() {
                    memory.remember(goal);
                    remember(&mut self.explored, goal);
                    self.goals = self.goals.saturating_add(1);
                }
            } else if follower.is_stalled() {
                let progress = follower.progress(ctx.body);
                log::debug!(
                    "explore stalled step={}/{} pos={:?} deviation={:.3} ticks={} goal={:?} on_ground={} in_water={}",
                    progress.step,
                    progress.total,
                    ctx.body.position(),
                    progress.deviation,
                    progress.ticks_on_step,
                    self.goal,
                    ctx.body.on_ground(),
                    ctx.body.is_in_water(),
                );
                let feet = ctx.body.position().to_block_pos_floor();
                let cell = |pos| {
                    ctx.perception
                        .known(pos)
                        .map(|known| block_name(known.state))
                        .unwrap_or_else(|| "unknown".to_owned())
                };
                log::debug!(
                    "explore stall cells feet={} below={} head={} next={:?} next_cell={} next_below={} next_head={}",
                    cell(feet),
                    cell(BlockPos::new(feet.x, feet.y - 1, feet.z)),
                    cell(BlockPos::new(feet.x, feet.y + 1, feet.z)),
                    follower.next_cell(),
                    follower
                        .next_cell()
                        .map(&cell)
                        .unwrap_or_else(|| "-".to_owned()),
                    follower
                        .next_cell()
                        .map(|next| cell(BlockPos::new(next.x, next.y - 1, next.z)))
                        .unwrap_or_else(|| "-".to_owned()),
                    follower
                        .next_cell()
                        .map(|next| cell(BlockPos::new(next.x, next.y + 1, next.z)))
                        .unwrap_or_else(|| "-".to_owned()),
                );
                self.follower = None;
                self.target_search_tick = 0;
                self.frontier_scan_tick = 0;
                self.idle_ticks = 0;
                // Drop the rest of the old ranked list: its first entry is
                // usually the same corner, and replanning to it one tick later
                // is how the body sat at an edge for 160 ticks.
                self.candidates.clear();
                if let Some(goal) = self.goal.take() {
                    memory.remember(goal);
                    remember(&mut self.rejected, goal);
                }
                let stuck = ctx.body.position().to_block_pos_floor();
                remember(&mut self.blocked, stuck);
            } else {
                self.idle_ticks = 0;
                let built = follower.step(ctx.tick, ctx.body.generation(), ctx.body);
                let pending = if started {
                    None
                } else {
                    Some(IntentSpec::Movement {
                        primitive: "explore".to_owned(),
                        args: vec![self.target.clone()],
                        window: self.path_ticks + 40,
                    })
                };
                return (built, pending, None);
            }
        }

        // A plan starts from the feet cell. While the body is in the air that
        // cell is a space it is falling through, not a place it stands: a plan
        // made mid-fall believed in a floor that was not there and the body
        // stalled under its first step (seed 303, body at y=77.25 planning a
        // step to y=78 over air). Wait for solid ground; falling is progress,
        // not idling.
        if !ctx.body.on_ground() && !ctx.body.is_in_water() {
            self.idle_ticks = 0;
            return (frame, None, None);
        }

        // A target the known map already holds is approached directly. The
        // search is the expensive wide scan, so it refreshes on a bounded
        // period instead of every tick.
        if ctx.tick >= self.target_search_tick {
            self.target_search_tick = ctx.tick + EXPLORE_SEARCH_PERIOD_TICKS;
            let target = nearest_known_block(
                ctx.perception,
                ctx.world,
                &self.target,
                EXPLORE_SEARCH_RADIUS,
                &self.rejected,
            );
            // The target is in sight but not yet in reach: walk to it.
            if let Some((cell, _)) = target
                && !self.rejected.contains(&cell)
            {
                let plan = self.plan_to(
                    ctx,
                    start,
                    Goal::Near {
                        pos: cell,
                        radius: TARGET_APPROACH_RADIUS,
                    },
                );
                match plan.status {
                    praxion_planner::PlanStatus::Success
                        if self.allow_water || !plan_crosses_water(&plan) =>
                    {
                        self.path_ticks = plan.cost.ceil().max(0.) as u64;
                        log::debug!(
                            "explore approach target={:?} cell={:?} cost={} steps={}",
                            self.target,
                            cell,
                            plan.cost,
                            plan.steps.len()
                        );
                        self.goal = Some(cell);
                        self.follower = Some(self.follow(ctx, plan.steps));
                        self.idle_ticks = 0;
                        return (frame, None, None);
                    }
                    _ => {
                        remember(&mut self.rejected, cell);
                    }
                }
            }
        }

        // No target in sight: rank the frontiers once per refresh period and
        // consume the list with a few plan attempts per tick. An unreachable
        // frontier is rejected and the next candidate is tried; a closed room
        // simply has none and waits for its budget.
        if self.candidates.is_empty() && ctx.tick >= self.frontier_scan_tick {
            let horizontal = EXPLORE_FRONTIER_RADIUS
                .min(ctx.perception.config().known_radius)
                .max(8) as i32;
            let known = |pos: BlockPos| ctx.perception.known(pos).map(|cell| cell.state);
            // Stay nearest-first while a nearby target may still be close, then
            // walk outward from the origin rather than around the sight ring.
            const OUTWARD_AFTER_GOALS: u32 = 8;
            let ranked = if self.goals < OUTWARD_AFTER_GOALS {
                rank_frontiers(
                    start,
                    horizontal,
                    EXPLORE_FRONTIER_VERTICAL,
                    &self.explored,
                    &self.rejected,
                    known,
                    self.allow_water,
                )
            } else {
                rank_frontiers_from(
                    memory.origin(start),
                    start,
                    horizontal,
                    EXPLORE_FRONTIER_VERTICAL,
                    &self.explored,
                    &self.rejected,
                    known,
                    self.allow_water,
                )
            };
            self.candidates = ranked.into();
            self.frontier_scan_tick = ctx.tick + EXPLORE_SEARCH_PERIOD_TICKS;
            self.candidates
                .retain(|choice| !self.rejected.contains(&choice.cell));
            // Keep the best quality tier (not avoided, on level, dry) before
            // fanning out or preferring a longer leg; distance never overrides
            // a cliff or an explored edge.
            if let Some(tier) = self.candidates.front().map(|choice| choice.tier) {
                self.candidates.retain(|choice| choice.tier == tier);
            }
            if self.rotation > 0 && self.candidates.len() > 1 {
                // Start at a different eighth of the ranked ring per call, so
                // bounded retries fan out around the player instead of all
                // walking the same nearest edge.
                let shift = self.rotation * self.candidates.len() / 8;
                self.candidates
                    .rotate_left(shift.min(self.candidates.len() - 1));
            }
            // The candidate list is already nearest-first (after the
            // rotation); hopping is fine because the follower walks a whole
            // path. Long legs were tried and made the follower stall on
            // terrain, so the score is not inflated here.
            // Stay near the surface: wood does not grow in the cave a chain of
            // one-block descents leads to. Twelve blocks below the highest
            // recent surface still reaches a valley forest but not a cave.
            self.candidates
                .retain(|choice| choice.cell.y >= home - 12 && choice.cell.y <= home + 32);
        }
        if ctx.tick >= self.next_plan_tick {
            for _ in 0..EXPLORE_PLAN_ATTEMPTS {
                let Some(choice) = self.candidates.pop_front() else {
                    break;
                };
                let goal = choice.cell;
                let plan = self.plan_to(ctx, start, Goal::Block(goal));
                match plan.status {
                    praxion_planner::PlanStatus::Success
                        if self.allow_water || !plan_crosses_water(&plan) =>
                    {
                        self.path_ticks = plan.cost.ceil().max(0.) as u64;
                        log::debug!(
                            "explore plan target={:?} frontier={:?} cost={} steps={} candidates={}",
                            self.target,
                            goal,
                            plan.cost,
                            plan.steps.len(),
                            choice.candidates
                        );
                        self.goal = Some(goal);
                        self.follower = Some(self.follow(ctx, plan.steps));
                        self.idle_ticks = 0;
                        self.walk_backs = 0;
                        self.next_plan_tick = 0;
                        self.events.push(PrimitiveEvent::ExploreFrontier {
                            cell: goal,
                            score: choice.score,
                            candidates: choice.candidates,
                        });
                        return (frame, None, None);
                    }
                    _ => {
                        memory.remember(goal);
                        remember(&mut self.rejected, goal);
                        self.next_plan_tick = ctx.tick.saturating_add(5);
                    }
                }
            }
        }
        self.idle_ticks = self.idle_ticks.saturating_add(1);
        // Dry land is exhausted: allow water once and rescan, with a fresh
        // idle budget. Frontiers rejected because their only path crossed
        // water get another chance.
        if !self.allow_water
            && (self.idle_ticks >= EXPLORE_WATER_AFTER_IDLE_TICKS
                || self.walk_backs >= EXPLORE_WALK_BACKS_BEFORE_WATER)
        {
            log::debug!(
                "explore target={:?}: no dry frontier, allowing water",
                self.target
            );
            self.allow_water = true;
            self.walk_backs = 0;
            self.rejected.clear();
            self.candidates.clear();
            self.frontier_scan_tick = ctx.tick;
            self.next_plan_tick = ctx.tick;
            self.idle_ticks = 0;
            return (frame, None, None);
        }
        // A pit with no reachable frontier is not a dead end: walk back to a
        // cell a previous leg reached and scan from there. Without this the
        // body sits at the bottom of the same hole for every retry.
        if self.idle_ticks >= 10 && !self.explored.is_empty() && ctx.tick >= self.next_plan_tick {
            for _ in 0..2 {
                if self.explored.is_empty() {
                    break;
                }
                let cell = self.explored.remove(0);
                let plan = self.plan_to(
                    ctx,
                    start,
                    Goal::Near {
                        pos: cell,
                        radius: TARGET_APPROACH_RADIUS,
                    },
                );
                if plan.status == praxion_planner::PlanStatus::Success
                    && (self.allow_water || !plan_crosses_water(&plan))
                {
                    self.path_ticks = plan.cost.ceil().max(0.) as u64;
                    self.goal = Some(cell);
                    self.follower = Some(self.follow(ctx, plan.steps));
                    self.idle_ticks = 0;
                    self.walk_backs += 1;
                    self.next_plan_tick = 0;
                    return (frame, None, None);
                }
                self.next_plan_tick = ctx.tick.saturating_add(5);
            }
        }
        if self.idle_ticks > EXPLORE_IDLE_TICKS {
            return (frame, None, Some(Outcome::Failed("not_found".into())));
        }
        (frame, None, None)
    }

    /// The planning state a plan and its follower share: the run's inventory
    /// and the cells explore has stalled in.
    fn planning_state(&self, world: &World) -> PlanningState {
        let mut state = PlanningState::new(plan_inventory(world));
        state.blacklist.extend(self.blocked.iter().copied());
        state
    }

    /// Follow `steps` with the planning state the plan used.
    fn follow(&self, ctx: &mut Ctx<'_>, steps: Vec<Step>) -> Follower {
        let state = self.planning_state(ctx.world);
        let mut source = PerceptionBlocks {
            perception: ctx.perception,
            world: ctx.world,
        };
        Follower::new(steps, &mut source, &state, ctx.style)
            .with_max_ticks_per_step(EXPLORE_STEP_TICKS)
    }

    fn plan_to(&self, ctx: &mut Ctx<'_>, start: BlockPos, goal: Goal) -> praxion_planner::Plan {
        let state = self.planning_state(ctx.world);
        let mut source = PerceptionBlocks {
            perception: ctx.perception,
            world: ctx.world,
        };
        self.planner.plan(&mut source, &state, start, goal)
    }
}

/// Whether a walking plan enters water. `explore` for wood stays on land: a
/// swim can drown the body, and the skill layer has no safe water handling.
fn plan_crosses_water(plan: &praxion_planner::Plan) -> bool {
    plan.steps.iter().any(|step| {
        matches!(
            step.kind,
            praxion_planner::MoveKind::EnterWater
                | praxion_planner::MoveKind::Swim
                | praxion_planner::MoveKind::SwimUp
                | praxion_planner::MoveKind::ExitWater { .. }
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn explore_descends_only_within_the_safe_drop() {
        // Seed 303's peak: explore used an eight-block drop, landed in a
        // one-cell pit and every later call idled 120 ticks. Descending must
        // stay within the planner's safe (damage-free) three-block drop; a peak
        // is left along its slopes.
        let mut memory = ExploreMemory::default();
        let explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        assert_eq!(explore.planner.max_drop(), 3);
    }
    #[test]
    fn explore_memory_is_bounded() {
        let mut cells = Vec::new();
        for i in 0..(EXPLORE_MEMORY_CELLS as i32 * 2) {
            remember(&mut cells, at(i, 64, 0));
        }
        assert_eq!(cells.len(), EXPLORE_MEMORY_CELLS);
        assert!(cells.contains(&at(EXPLORE_MEMORY_CELLS as i32 * 2 - 1, 64, 0)));
        // Re-remembering an existing cell never grows the list.
        let before = cells.len();
        let first = cells[0];
        remember(&mut cells, first);
        assert_eq!(cells.len(), before);
    }
    #[test]
    fn consecutive_explore_calls_rotate() {
        let mut memory = ExploreMemory::default();
        let first = memory.next_rotation();
        let second = memory.next_rotation();
        assert_ne!(first, second, "a retry starts from a different frontier");
    }

    #[test]
    fn explore_memory_is_per_runner() {
        // Two runners must not share explore's history: the cells one
        // remembers and the rotations one has consumed stay invisible to the
        // other.
        let mut first = ExploreMemory::default();
        let mut second = ExploreMemory::default();
        first.remember(at(1, 64, 2));
        first.next_rotation();
        first.next_rotation();
        assert_eq!(first.recent(), vec![at(1, 64, 2)]);
        assert!(
            second.recent().is_empty(),
            "the other run remembers nothing"
        );
        assert_eq!(second.next_rotation(), 0, "the other run has not rotated");

        // The memory is bounded: it keeps the newest cells and drops the
        // oldest, and re-remembering a cell does not grow it.
        let mut memory = ExploreMemory::default();
        for i in 0..(RECENT_EXPLORED_CELLS as i32 + 5) {
            memory.remember(at(i, 64, 0));
        }
        let recent = memory.recent();
        assert_eq!(recent.len(), RECENT_EXPLORED_CELLS);
        assert_eq!(
            recent[0],
            at(5, 64, 0),
            "the five oldest cells were dropped"
        );
        assert_eq!(
            recent[RECENT_EXPLORED_CELLS - 1],
            at(RECENT_EXPLORED_CELLS as i32 + 4, 64, 0),
            "the newest cell is last"
        );
        memory.remember(at(5, 64, 0));
        assert_eq!(memory.recent().len(), RECENT_EXPLORED_CELLS);
    }
}
