//! `explore`: walk toward a known target or the best unknown frontier until a
//! `near` fact holds.

use std::collections::VecDeque;
use std::time::Duration;

use azalea_core::position::BlockPos;
use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_gamedata as gamedata;
use praxion_headless::{ControlFrame, Look};
use praxion_interpreter::Outcome;
use praxion_perception::{Caller, FactKind, QueryKind};
use praxion_planner::{Follower, Goal, PerceptionBlocks, Planner, PlanningState, Step};
use praxion_world::{BlockState, World};

use crate::common::{block_name, idle, plan_food, plan_inventory};
use crate::frontier::{
    FrontierChoice, FrontierScan, near_block, nearest_known_block, scan_frontiers,
};
use crate::heading::SearchHeading;
use crate::runner::{Ctx, IntentSpec, PrimitiveEvent};

/// How far `explore` looks for its target before it reports `not_found`. The
/// gathering skills use the same radius in their `not near(T, R)` guard.
pub const EXPLORE_NEAR_RADIUS: u32 = praxion_rules::registry::EXPLORE_NEAR_RADIUS;

/// How far `explore` looks for its target before it falls back to a frontier
/// of the known map.
pub const EXPLORE_SEARCH_RADIUS: u32 = praxion_rules::registry::EXPLORE_SEARCH_RADIUS;

/// How close a mob must be for `explore(mob)` to end (decision 2026-11-09
/// "Five next areas": `hunt` and `get_food` search for animals the same way).
pub const EXPLORE_ENTITY_RADIUS: u32 = 24;

/// Ticks between two mob checks of one `explore(mob)` call.
pub const EXPLORE_ENTITY_PERIOD_TICKS: u64 = 10;

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

/// The nearest best-tier frontier must lie closer than this many blocks for the
/// once-per-call climb to a lookout. A frontier that near means terrain, not
/// sight range, ends the known map, so a higher view can show what lies past
/// it; a far one means the view already reaches as far as the sight range
/// (decision 2026-11-09, "Explore may climb once per call for a view").
const LOOKOUT_NEAR_FRONTIER: i64 = 32;

/// Ticks one explore path step may take before the follower is considered
/// stuck and the frontier is rejected. Long enough for the jump nudge to free
/// a corner; still shorter than the default 200 so a 1200-tick search can try
/// several directions.
const EXPLORE_STEP_TICKS: u32 = 160;

/// Hard cap on one look-around: four held quarters must fit in five seconds
/// even when the gaze is slow. One turn at the start of a call, plus at most one more on
/// arrival at a lookout (decision 2026-11-09).
const LOOK_AROUND_MAX_TICKS: u64 = 100;

/// How close the body's yaw must be to a held quarter before the quarter
/// counts as arrived, in degrees.
const LOOK_AROUND_TOLERANCE_DEG: f32 = 10.0;

/// Cue walks one call may attempt. A cue is a one-shot hint, not a substitute
/// for exploring: after two attempts the search only considers targets and
/// frontiers. An attempt is one `approach` call, whether a plan succeeds or
/// not.
const MAX_CUE_ATTEMPTS: u32 = 2;

/// One look-around: four quarter turns held long enough for a view scan each.
/// The turn starts where the body already looks, so the first quarter needs no
/// movement.
#[derive(Debug)]
struct LookAround {
    start_tick: u64,
    start_yaw: f32,
    quarter: u8,
    /// Tick the body first came within tolerance of the held quarter. The
    /// quarter is held for one scan period from here, so the scan sees it.
    arrived_tick: Option<u64>,
}

impl LookAround {
    fn new(tick: u64, yaw: f32) -> Self {
        Self {
            start_tick: tick,
            start_yaw: yaw,
            quarter: 0,
            arrived_tick: None,
        }
    }

    /// The yaw this quarter holds, in `0..360`.
    fn aim(&self) -> f32 {
        (self.start_yaw + f32::from(self.quarter) * 90.0).rem_euclid(360.0)
    }

    /// Advance the held aim when the gaze arrived and the scan period passed.
    /// Returns whether the turn is still running. A turn never outlives
    /// [`LOOK_AROUND_MAX_TICKS`], even if the gaze never reaches an aim.
    fn advance(&mut self, tick: u64, body_yaw: f32, period: u64) -> bool {
        if tick.saturating_sub(self.start_tick) >= LOOK_AROUND_MAX_TICKS {
            return false;
        }
        let within =
            praxion_planner::wrap_degrees(self.aim() - body_yaw).abs() <= LOOK_AROUND_TOLERANCE_DEG;
        if within && self.arrived_tick.is_none() {
            self.arrived_tick = Some(tick);
        }
        if let Some(arrived) = self.arrived_tick
            && within
            && tick.saturating_sub(arrived) >= period
        {
            if self.quarter < 3 {
                self.quarter += 1;
                self.arrived_tick = None;
            } else {
                return false;
            }
        }
        true
    }
}

#[derive(Debug)]
pub(crate) struct Explore {
    target: String,
    /// Set when the target is a mob, not a block: the call then ends when one
    /// is within [`EXPLORE_ENTITY_RADIUS`], checked every
    /// [`EXPLORE_ENTITY_PERIOD_TICKS`] (an entity query costs rays).
    entity: Option<EntityKind>,
    planner: Planner,
    follower: Option<Follower>,
    goal: Option<BlockPos>,
    /// What the follower is walking to. A walk to a frontier or a cue keeps
    /// looking for something better on the search period, as a person who
    /// spots the tree turns toward it instead of finishing the detour.
    chase: Chase,
    /// When nothing is known yet, the call first turns once on the spot, so
    /// the view scan sees every side before a frontier is picked; a person
    /// looks around before walking off. Armed at the start of a call and at most
    /// once more on arrival at a lookout (decision 2026-11-09).
    look_around: Option<LookAround>,
    /// Whether this call already made its look-around decision. It prevents a
    /// later failed seek from arming a turn after the first leg.
    look_around_started: bool,
    /// Whether this call already climbed to a lookout (or tried and failed).
    /// The climb happens at most once per call (decision 2026-11-09).
    lookout_done: bool,
    /// Cue approaches this call has attempted, successful plans or not.
    cue_attempts: u32,
    /// Cue cells this call reached, rejected or found already at. Kept apart
    /// from `explored`: a cue must not steer the frontier ranking or the
    /// cross-call memory, and the shared ring must not silently re-enable a
    /// cue that was already used.
    cue_done: Vec<BlockPos>,
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

/// Walks back without a new frontier after which the call gives up and lets
/// its idle budget run out. Without a cap a body near its explored cells
/// walked back and forth for whole calls and never reported `not_found`
/// (seed 101 live run 2026-11-08).
const EXPLORE_MAX_WALK_BACKS: u32 = 4;

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
    /// The direction the search keeps from call to call; see
    /// [`SearchHeading`].
    heading: SearchHeading,
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
}

/// Ticks one `explore(stone)` call may walk before it fails `not_found`: a
/// quarter of the usual budget. Stone in flat terrain lies under the dirt and
/// no ray reaches it from the surface, so walking finds it only where it
/// outcrops, and a person with a pickaxe digs (`find_stone`) before they walk.
/// The walk is the last resort of `get_cobblestone`; at the full budget its
/// three failed calls cost three minutes (run B, 2026-11-12), at this one
/// forty-five seconds.
pub const STONE_EXPLORE_TICKS: u32 = 300;

/// The tick budget of one `explore(target)` call.
fn explore_budget(target: &str) -> u32 {
    if target == "stone" {
        STONE_EXPLORE_TICKS
    } else {
        praxion_rules::registry::EXPLORE_MAX_TICKS
    }
}

impl Explore {
    pub(crate) fn new(target: String, memory: &mut ExploreMemory) -> Result<Self, String> {
        if let Some(tag) = target.strip_prefix('#') {
            if gamedata::block_tag(tag).is_none() {
                return Err(format!("unknown block tag `#{tag}`"));
            }
        } else if target.parse::<BlockKind>().is_err() && target.parse::<EntityKind>().is_err() {
            return Err(format!("unknown block or mob `{target}`"));
        }
        // A name that is both a block and a mob does not exist in vanilla; the
        // block reading wins if one ever appears.
        let entity = if target.starts_with('#') || target.parse::<BlockKind>().is_ok() {
            None
        } else {
            target.parse::<EntityKind>().ok()
        };
        // A mob target is the exploration half of a hunt, and the body only
        // hunts the loot-table allowlist: creepers and Endermen are never
        // hunted (decision 2026-11-09 "Five next areas"). `explore(creeper)`
        // cannot be told from a block at load time, so it is refused when the
        // call starts (review finding 4).
        if entity.is_some() && !gamedata::huntable(&target) {
            return Err(format!(
                "`{target}` is not hunted; explore may only seek mobs on the hunt allowlist"
            ));
        }
        let budget = explore_budget(&target);
        Ok(Self {
            target,
            entity,
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
            chase: Chase::Frontier,
            look_around: None,
            look_around_started: false,
            lookout_done: false,
            cue_attempts: 0,
            cue_done: Vec::new(),
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
            idle_ticks: 0,
            allow_water: false,
            walk_backs: 0,
            next_plan_tick: 0,
            events: Vec::new(),
            waited: 0,
            budget,
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
        // The completion check is cheap: a 5-block scan. The wider target
        // search runs on `EXPLORE_SEARCH_PERIOD_TICKS`, also while walking to a
        // frontier or a cue; it walks remembered sections, not the cube.
        if let Some(kind) = self.entity {
            if self.waited == 1 || self.waited.is_multiple_of(EXPLORE_ENTITY_PERIOD_TICKS as u32) {
                let found = ctx
                    .perception
                    .nearest_entity(Caller::Other, ctx.world, kind, EXPLORE_ENTITY_RADIUS);
                if found.is_known() {
                    return (frame, None, Some(Outcome::Confirmed));
                }
            }
        } else if near_block(ctx.perception, ctx.world, &self.target, EXPLORE_NEAR_RADIUS)
            == Some(true)
        {
            return (frame, None, Some(Outcome::Confirmed));
        }
        if self.waited > self.budget {
            return (frame, None, Some(Outcome::Failed("not_found".into())));
        }

        // A first tick in the air must not consume the one look-around: the
        // body cannot turn while it falls, but it may arm the turn as soon as
        // it lands. In water the gate is evaluated (and refused) so a sinking
        // body does not arm the same turn on every tick.
        if !self.look_around_started && (ctx.body.on_ground() || ctx.body.is_in_water()) {
            self.look_around_started = true;
            // Only a body standing on solid, dry ground may turn on the spot.
            // A body that stands still in water sinks; on seed 101 the turn
            // cost 100 ticks of sinking, the drowning reflex preempted the
            // call, and the retry armed the same turn again, 106 times.
            if look_around_allowed(ctx.body.on_ground(), ctx.body.is_in_water()) {
                let target_known = nearest_known_block(
                    ctx.perception,
                    ctx.world,
                    &self.target,
                    EXPLORE_SEARCH_RADIUS,
                    &self.rejected,
                )
                .is_some();
                if !target_known {
                    self.look_around = Some(LookAround::new(ctx.tick, ctx.body.look().y_rot()));
                }
            } else {
                // The start look-around was refused, so "it found nothing"
                // cannot hold: a call that began in water gets no lookout.
                self.lookout_done = true;
            }
        }

        // While walking to a frontier or a cue, keep looking: a target (or,
        // on a frontier walk, a cue) seen on the way replaces the detour.
        if self.chase != Chase::Target
            && ctx.tick >= self.target_search_tick
            && ctx.body.on_ground()
            && self
                .follower
                .as_ref()
                .is_some_and(|follower| !follower.is_done() && !follower.is_stalled())
        {
            self.target_search_tick = ctx.tick + EXPLORE_SEARCH_PERIOD_TICKS;
            self.seek(ctx, start);
        }

        if let Some(follower) = &mut self.follower {
            if follower.is_done() {
                self.follower = None;
                self.target_search_tick = 0;
                self.frontier_scan_tick = ctx.tick.saturating_add(5);
                self.idle_ticks = 0;
                self.candidates.clear();
                if let Some(goal) = self.goal.take() {
                    if self.chase == Chase::Cue {
                        // Cue cells have their own per-call memory. They must
                        // not seed the frontier-avoid ring or the ranking.
                        self.note_cue_done(goal);
                    } else if self.chase == Chase::Lookout {
                        // A viewpoint is not an explored frontier: it enters
                        // neither memory nor the heading. The view scan sees
                        // the land from up here only if the body looks around
                        // again, so the turn is armed once more. It waits for
                        // solid ground if the arrival tick is airborne.
                        self.look_around = Some(LookAround::new(ctx.tick, ctx.body.look().y_rot()));
                    } else if self.chase == Chase::WalkBack {
                        // The cell was taken out of `explored` to walk back to
                        // it; putting it back would make it the next walk-back
                        // target and the body would shuttle between two cells.
                    } else {
                        memory.remember(goal);
                        remember(&mut self.explored, goal);
                        // Only a real frontier arrival measures the heading's
                        // progress; a cue or target leg is not exploration.
                        if self.chase == Chase::Frontier {
                            memory.heading.arrived(goal);
                        }
                    }
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
                    if self.chase == Chase::Cue {
                        self.note_cue_done(goal);
                    } else if self.chase != Chase::Lookout {
                        // A stalled climb only costs the climb (`lookout_done`
                        // is already set); the cell is no frontier to reject.
                        memory.remember(goal);
                        remember(&mut self.rejected, goal);
                    }
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
        // period instead of every tick. A target or cue found now ends the
        // look-around.
        if ctx.tick >= self.target_search_tick {
            self.target_search_tick = ctx.tick + EXPLORE_SEARCH_PERIOD_TICKS;
            if self.seek(ctx, start) {
                self.look_around = None;
                return (frame, None, None);
            }
        }
        // Hold each quarter of the horizon for one view-scan period after the
        // gaze reaches it, so the scan sees every side once. The whole turn is
        // capped, because a slow gaze must not eat the call's budget.
        let period = ctx.perception.config().view_scan_period.max(1);
        let mut aim = None;
        // The ground can give way mid-turn (sand, a flood): stop turning then.
        let still_allowed = look_around_allowed(ctx.body.on_ground(), ctx.body.is_in_water());
        if self.look_around.is_some() && !still_allowed {
            log::debug!("explore look-around dropped: the body is no longer on dry ground");
        }
        if let Some(turn) = self.look_around.as_mut()
            && still_allowed
            && turn.advance(ctx.tick, ctx.body.look().y_rot(), period)
        {
            aim = Some(turn.aim());
        }
        if let Some(yaw) = aim {
            let mut frame = frame;
            frame.look = Look::Aim { yaw, pitch: 0.0 };
            return (frame, None, None);
        }
        self.look_around = None;

        // No target in sight: rank the frontiers once per refresh period and
        // consume the list with a few plan attempts per tick. An unreachable
        // frontier is rejected and the next candidate is tried; a closed room
        // simply has none and waits for its budget.
        if self.candidates.is_empty() && ctx.tick >= self.frontier_scan_tick {
            let horizontal = EXPLORE_FRONTIER_RADIUS
                .min(ctx.perception.config().known_radius)
                .max(8) as i32;
            let known = |pos: BlockPos| ctx.perception.known(pos).map(|cell| cell.state);
            // Keep only cells whose column is open to the sky. Wood does not
            // grow in the cave a chain of one-block descents leads to, and an
            // open column is the real property that tells a valley from a
            // cave. The altitude band that stood in for it dropped every
            // valley once the body walked down from a peak (seed 303: spawned
            // at y 99, at y 88 every frontier toward the trees was refused).
            // `choice.cell` is the feet cell (`frontier::standable` keeps the
            // body cell and checks its floor), so the scan starts at its head.
            // It runs inside the scan, before the tier cut and the cap: a best
            // tier made only of covered cells emptied the list while open
            // cells waited in a lower tier (seed 101, 2026-11-08: 106
            // `not_found` in a row).
            let sky_reads = std::cell::Cell::new(0_u32);
            let scan = scan_frontiers(
                start,
                horizontal,
                EXPLORE_FRONTIER_VERTICAL,
                &self.explored,
                &self.rejected,
                known,
                self.allow_water,
                |cell| {
                    sky_reads.set(sky_reads.get() + 1);
                    // The raw column is a privileged read (heightmap and
                    // blocks the body may never have seen), so perception
                    // serves it and the ledger counts it after the scan; the
                    // judgement above it stays here.
                    let column = ctx.perception.privileged_column(
                        ctx.world,
                        cell.x,
                        cell.z,
                        cell.y.saturating_add(1),
                        SKY_SCAN_BLOCKS,
                    );
                    open_to_sky(
                        cell,
                        column.as_ref().map(|column| column.surface),
                        |pos| column.as_ref().and_then(|column| column.state_at(pos.y)),
                    )
                },
            );
            ctx.perception.ledger_privileged(
                QueryKind::SkyColumn,
                FactKind::Column,
                sky_reads.get(),
            );
            if !self.lookout_done && self.plan_lookout(ctx, start, &scan) {
                self.frontier_scan_tick = ctx.tick + EXPLORE_SEARCH_PERIOD_TICKS;
                return (frame, None, None);
            }
            let heading = memory.heading.direction(start, &scan.sector_weights());
            self.candidates = scan.rank(heading).into();
            self.frontier_scan_tick = ctx.tick + EXPLORE_SEARCH_PERIOD_TICKS;
            log::debug!(
                "explore frontiers ranked={} heading={:?}",
                self.candidates.len(),
                heading
            );
            // Keep the best quality tier (not avoided, on level, dry) before
            // fanning out or preferring a longer leg; distance never overrides
            // a cliff or an explored edge.
            if let Some(tier) = self.candidates.front().map(|choice| choice.tier) {
                self.candidates.retain(|choice| choice.tier == tier);
            }
            // The list is ordered farthest-forward first along the heading.
            // A leg the follower cannot finish is rejected and the next
            // candidate is tried.
        }
        if ctx.tick >= self.next_plan_tick {
            for _ in 0..EXPLORE_PLAN_ATTEMPTS {
                let Some(choice) = self.candidates.pop_front() else {
                    break;
                };
                let goal = choice.cell;
                let plan = self.plan_to(ctx, start, Goal::Block(goal));
                match plan.status {
                    // The body already stands on this frontier: it is explored,
                    // not a goal to walk to (no follower, no goal count). It is
                    // also the only cell an empty successful plan may name.
                    praxion_planner::PlanStatus::Success
                        if plan.steps.is_empty() && empty_plan_is_here(start, goal) =>
                    {
                        memory.remember(goal);
                        remember(&mut self.explored, goal);
                    }
                    // `Goal::Block` means exactly that cell, so an empty plan to
                    // any other cell cannot be "already there". Treat it as a
                    // planner fault, reject the cell and keep going: a broken
                    // empty plan must never freeze explore (seed 101 live run
                    // 2026-11-08 marked frontiers explored without moving).
                    praxion_planner::PlanStatus::Success if plan.steps.is_empty() => {
                        log::warn!(
                            "explore: empty successful plan from {:?} to {:?}; rejecting the cell",
                            start,
                            goal
                        );
                        memory.remember(goal);
                        remember(&mut self.rejected, goal);
                        self.next_plan_tick = ctx.tick.saturating_add(5);
                    }
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
                        self.chase = Chase::Frontier;
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
        if self.idle_ticks >= 10
            && self.walk_backs < EXPLORE_MAX_WALK_BACKS
            && !self.explored.is_empty()
            && ctx.tick >= self.next_plan_tick
        {
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
                // An empty plan means the body is already there: a walk back
                // would end on the next tick and only reset the idle budget.
                if plan.status == praxion_planner::PlanStatus::Success
                    && !plan.steps.is_empty()
                    && (self.allow_water || !plan_crosses_water(&plan))
                {
                    log::debug!(
                        "explore walk back target={:?} cell={:?} steps={} walk_backs={}",
                        self.target,
                        cell,
                        plan.steps.len(),
                        self.walk_backs
                    );
                    self.path_ticks = plan.cost.ceil().max(0.) as u64;
                    self.goal = Some(cell);
                    self.chase = Chase::WalkBack;
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
            log::debug!(
                "explore not_found target={:?} pos={:?} on_ground={} in_water={} allow_water={} explored={} walk_backs={}",
                self.target,
                ctx.body.position(),
                ctx.body.on_ground(),
                ctx.body.is_in_water(),
                self.allow_water,
                self.explored.len(),
                self.walk_backs
            );
            // Nothing reachable ahead: turn instead of retrying the same way.
            memory.heading.give_up();
            return (frame, None, Some(Outcome::Failed("not_found".into())));
        }
        (frame, None, None)
    }

    /// The once-per-call climb: when the look-around found nothing and the
    /// nearest best-tier frontier is near, walk to the highest reachable
    /// lookout so the look-around can see over the terrain that ends the known
    /// map. Returns whether a walk started. The climb is spent once the gate
    /// opens and a lookout exists, whether or not the plan succeeds, so a
    /// failed climb is not retried on the next scan.
    fn plan_lookout(&mut self, ctx: &mut Ctx<'_>, start: BlockPos, scan: &FrontierScan) -> bool {
        let nearest = scan.nearest_best_distance_sq();
        if !lookout_gate_open(nearest) {
            log::debug!(
                "explore lookout skipped: nearest best-tier frontier at {:?} blocks",
                nearest.map(|distance_sq| (distance_sq as f64).sqrt().round())
            );
            return false;
        }
        let Some(choice) = scan.lookout() else {
            log::debug!("explore lookout skipped: no reachable viewpoint in the window");
            return false;
        };
        self.lookout_done = true;
        let goal = choice.cell;
        let plan = self.plan_to(ctx, start, Goal::Block(goal));
        if plan.status != praxion_planner::PlanStatus::Success
            || plan.steps.is_empty()
            || (!self.allow_water && plan_crosses_water(&plan))
        {
            log::debug!("explore lookout {goal:?} abandoned: no walkable plan");
            return false;
        }
        log::debug!(
            "explore lookout cell={:?} rise={} nearest_frontier={:.0} steps={}",
            goal,
            goal.y - start.y,
            nearest.map_or(0.0, |distance_sq| (distance_sq as f64).sqrt()),
            plan.steps.len()
        );
        self.path_ticks = plan.cost.ceil().max(0.) as u64;
        self.goal = Some(goal);
        self.chase = Chase::Lookout;
        self.follower = Some(self.follow(ctx, plan.steps));
        self.idle_ticks = 0;
        self.next_plan_tick = 0;
        true
    }

    /// The planning state a plan and its follower share: the run's inventory
    /// and the cells explore has stalled in.
    fn planning_state(&self, world: &World, food: Option<u32>) -> PlanningState {
        let mut state = PlanningState::new(plan_inventory(world));
        state.food = food;
        state.blacklist.extend(self.blocked.iter().copied());
        state
    }

    /// Follow `steps` with the planning state the plan used.
    fn follow(&self, ctx: &mut Ctx<'_>, steps: Vec<Step>) -> Follower {
        let food = plan_food(ctx);
        let state = self.planning_state(ctx.world, food);
        let mut source = PerceptionBlocks {
            perception: ctx.perception,
            world: ctx.world,
        };
        Follower::new(steps, &mut source, &state, ctx.style)
            .with_max_ticks_per_step(EXPLORE_STEP_TICKS)
    }

    /// Plan to `cell` and start the follower. A failed plan rejects the cell
    /// for the next search. A successful plan with no steps means the body is
    /// already there: it starts no follower, and a cue is marked done, so an
    /// already-reached cell cannot become a fake goal every tick. Returns
    /// whether a walk started.
    fn approach(
        &mut self,
        ctx: &mut Ctx<'_>,
        start: BlockPos,
        cell: BlockPos,
        chase: Chase,
    ) -> bool {
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
            if plan.steps.is_empty() {
                if chase == Chase::Cue {
                    self.note_cue_done(cell);
                }
                return false;
            }
            self.path_ticks = plan.cost.ceil().max(0.) as u64;
            log::debug!(
                "explore approach target={:?} cell={:?} cost={} steps={}",
                self.target,
                cell,
                plan.cost,
                plan.steps.len()
            );
            self.goal = Some(cell);
            self.chase = chase;
            self.follower = Some(self.follow(ctx, plan.steps));
            self.idle_ticks = 0;
            true
        } else {
            if chase == Chase::Cue {
                self.note_cue_done(cell);
            } else {
                remember(&mut self.rejected, cell);
            }
            false
        }
    }

    /// Look for the target, then (unless a cue walk is already under way) for a
    /// cue, and approach the first one found. A known target that cannot be
    /// reached falls back to the frontiers, not to a cue.
    fn seek(&mut self, ctx: &mut Ctx<'_>, start: BlockPos) -> bool {
        let target = nearest_known_block(
            ctx.perception,
            ctx.world,
            &self.target,
            EXPLORE_SEARCH_RADIUS,
            &self.rejected,
        );
        if let Some((cell, _)) = target {
            return !self.rejected.contains(&cell)
                && self.approach(ctx, start, cell, Chase::Target);
        }
        if self.chase == Chase::Cue && self.follower.is_some() {
            return false;
        }
        // A cue is a one-shot hint: at most two walks per call. After that the
        // search only considers targets and frontiers, so a harvested canopy
        // cannot consume the whole budget.
        if self.cue_attempts >= MAX_CUE_ATTEMPTS {
            return false;
        }
        // Leaves are see-through, so the view scan records no canopy: only the
        // sweep knows leaf cells, and only within its radius. A cue farther
        // out would be a stale sweep fact the body has walked away from.
        let sweep_radius = ctx.perception.config().sweep_radius;
        let cue = cue_goal(&self.target, sweep_radius, |tag, radius| {
            let mut exclude = self.rejected.clone();
            exclude.extend(self.cue_done.iter().copied());
            exclude.extend(self.explored.iter().copied());
            nearest_known_block(ctx.perception, ctx.world, tag, radius, &exclude)
                .map(|(cell, _)| cell)
        });
        let Some(cell) = cue else {
            return false;
        };
        self.cue_attempts += 1;
        self.approach(ctx, start, cell, Chase::Cue)
    }

    /// Mark a cue cell reached or rejected. The list is per call and never
    /// feeds the frontier ranking or the cross-call memory.
    fn note_cue_done(&mut self, cell: BlockPos) {
        if !self.cue_done.contains(&cell) {
            self.cue_done.push(cell);
        }
    }

    fn plan_to(&self, ctx: &mut Ctx<'_>, start: BlockPos, goal: Goal) -> praxion_planner::Plan {
        let food = plan_food(ctx);
        let state = self.planning_state(ctx.world, food);
        let mut source = PerceptionBlocks {
            perception: ctx.perception,
            world: ctx.world,
        };
        self.planner.plan(&mut source, &state, start, goal)
    }
}

/// What the follower is walking to; see [`Explore::chase`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chase {
    Frontier,
    /// A walk to a cue cell, not a frontier: it neither counts as a goal nor
    /// enters the frontier memory.
    Cue,
    Target,
    /// The one climb per call to a viewpoint; like a cue it is not a frontier
    /// arrival and enters no memory.
    Lookout,
    /// A walk back to a cell an earlier leg reached, so the search can scan
    /// from there. It is not a frontier arrival either.
    WalkBack,
}

/// The tag whose known cells point toward `target` when `target` itself is not
/// known. Logs grow in leaf canopies, so a log search that sees no log can
/// still walk to a canopy and look under it. This is the only cue; it is not a
/// general mapping.
fn cue_for(target: &str) -> Option<&'static str> {
    let tag = target.strip_prefix('#')?;
    // `#logs` is the general tag; a wood type names its own (`#oak_logs`).
    let is_log = tag == "logs" || tag.ends_with("_log") || tag.ends_with("_logs");
    is_log.then_some("#leaves")
}

/// The cell a cued search walks to, if the cue tag is known within `radius`.
/// The caller passes the sweep radius: leaves are see-through, so the view scan
/// never records a far canopy. `nearest` is the perception lookup; it receives
/// the cue tag and the radius so this decision stays independent of the
/// ray-free map scan.
fn cue_goal(
    target: &str,
    radius: u32,
    mut nearest: impl FnMut(&str, u32) -> Option<BlockPos>,
) -> Option<BlockPos> {
    nearest(cue_for(target)?, radius)
}

/// Whether the nearest best-tier frontier (squared distance, `None` without
/// one) is close enough to try a lookout: strictly under
/// [`LOOKOUT_NEAR_FRONTIER`] blocks.
fn lookout_gate_open(nearest_distance_sq: Option<i64>) -> bool {
    nearest_distance_sq.is_some_and(|distance_sq| distance_sq < LOOKOUT_NEAR_FRONTIER.pow(2))
}

/// Whether an empty successful plan means the body is already at the goal.
///
/// `Goal::Block` names exactly one cell, so the only cell an empty success can
/// legitimately mean is the start cell. Any other empty success is a planner
/// fault and the frontier loop rejects it instead of treating it as explored.
fn empty_plan_is_here(start: BlockPos, goal: BlockPos) -> bool {
    goal == start
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

/// Whether a call may start with the look-around turn. A body that stands
/// still out of the ground sinks, and a body that stands still in water
/// drowns: only solid, dry ground is a place to turn on.
fn look_around_allowed(on_ground: bool, in_water: bool) -> bool {
    on_ground && !in_water
}

/// Blocks above the head a column may hold before the cell counts as covered.
/// A roof farther up than this is a mountain over a cave, not a canopy.
const SKY_SCAN_BLOCKS: i32 = 48;

// Explore asks perception for at most the states one privileged read serves:
// a larger window would silently truncate, a smaller one would waste reads.
const _: () = assert!(SKY_SCAN_BLOCKS as usize <= praxion_perception::COLUMN_BLOCKS);

/// Whether the column above the body cell `feet` is open to the sky.
///
/// `surface` is the column's [`World::surface_y`], the first free y above its
/// highest non-air block. `None` means the chunk or its heightmap is not
/// loaded, which is not a fact about the column: the cell is kept, and the
/// planner rejects it if it is unreachable. `block` looks a world cell up.
///
/// The scan starts at the head cell, one above `feet`: frontier ranking checks
/// only the feet cell and its floor, so a covered head must also count. Every
/// block from there up to the surface must be open; a surface farther than
/// [`SKY_SCAN_BLOCKS`] above counts as covered without a scan.
fn open_to_sky(
    feet: BlockPos,
    surface: Option<i32>,
    block: impl Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let Some(surface) = surface else {
        return true;
    };
    let first = feet.y.saturating_add(1);
    if surface.saturating_sub(first) > SKY_SCAN_BLOCKS {
        return false;
    }
    (first..surface).all(|y| block(BlockPos::new(feet.x, y, feet.z)).is_some_and(sky_open))
}

/// Whether a block above a frontier cell still leaves its column open.
///
/// Air, leaves, logs and the vegetation the game replaces when a tree grows
/// in it leave the ground under a canopy in daylight. Water is in that
/// `replaceable` tag too and stays open: a water frontier is the last resort
/// the planner swims, and the drowning reflex guards it, so refusing it here
/// would silently disable the water fallback. Lava and fire are hazards and
/// cover the column, and anything else does too.
fn sky_open(state: BlockState) -> bool {
    if state.is_air() {
        return true;
    }
    // Kelp and other waterlogged plants are not in `replaceable`; the planner
    // swims through them, so they leave a water column open too.
    if praxion_planner::blocks::is_water(state) {
        return true;
    }
    let name = gamedata::normalize(praxion_planner::blocks::name(state));
    gamedata::block_in_tag(name, "leaves")
        || gamedata::block_in_tag(name, "logs")
        || (gamedata::block_in_tag(name, "replaceable")
            && !praxion_planner::blocks::is_hazard(state))
}

#[cfg(test)]
mod tests {
    #[test]
    fn exploring_for_stone_gets_a_short_budget_everything_else_the_full_one() {
        use super::*;
        assert_eq!(explore_budget("stone"), STONE_EXPLORE_TICKS);
        const { assert!(STONE_EXPLORE_TICKS * 3 <= 1200) };
        for other in ["#logs", "oak_log", "coal_ore", "cow", "cobblestone"] {
            assert_eq!(
                explore_budget(other),
                praxion_rules::registry::EXPLORE_MAX_TICKS,
                "{other}"
            );
        }
        let mut memory = ExploreMemory::default();
        let explore = Explore::new("stone".into(), &mut memory).expect("stone is a block");
        assert_eq!(explore.budget, STONE_EXPLORE_TICKS);
    }

    use std::collections::HashMap;

    use azalea_core::position::Vec3;
    use praxion_headless::{Blocks, Body, Held};
    use praxion_perception::Perception;
    use praxion_world::BlockState;

    use super::*;

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    /// Solid ground at and below y = 63, air above: enough for the body to
    /// stand on and for explore's ground gate to pass.
    struct Ground;

    impl Blocks for Ground {
        fn block(&self, pos: BlockPos) -> Option<BlockState> {
            (pos.y <= 63).then_some(BlockState::from(BlockKind::Stone))
        }

        fn ready(&self, _position: Vec3) -> bool {
            true
        }
    }

    /// Stone at and below y = 63 with a water column from y = 64 to y = 70: a
    /// body in it is in water and cannot stand, the seed 101 case.
    struct Sea;

    impl Blocks for Sea {
        fn block(&self, pos: BlockPos) -> Option<BlockState> {
            Some(if pos.y <= 63 {
                BlockState::from(BlockKind::Stone)
            } else if pos.y <= 70 {
                BlockState::from(BlockKind::Water)
            } else {
                BlockState::from(BlockKind::Air)
            })
        }

        fn ready(&self, _position: Vec3) -> bool {
            true
        }
    }

    fn ctx<'a>(
        tick: u64,
        body: &'a Body,
        world: &'a World,
        perception: &'a mut Perception,
    ) -> Ctx<'a> {
        Ctx {
            tick,
            body,
            world,
            perception,
            style: praxion_planner::Personality::default(),
        }
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
    fn a_log_search_with_only_leaves_known_walks_to_the_leaves() {
        // Only leaves are known within the sweep radius; no log is known. The
        // cue must ask for leaves at the sweep radius (leaves pass, so the view
        // scan never records a far canopy) and return that cell as the goal,
        // before the frontier scan gets a chance.
        let sweep_radius = 32;
        let leaves = at(0, 65, 20);
        let mut asked = None;
        let goal = cue_goal("#logs", sweep_radius, |tag, radius| {
            asked = Some((tag.to_owned(), radius));
            (tag == "#leaves" && radius == sweep_radius).then_some(leaves)
        });
        assert_eq!(asked, Some(("#leaves".to_owned(), sweep_radius)));
        assert_eq!(
            goal,
            Some(leaves),
            "the leaves cell is the goal, not a frontier"
        );
        assert_eq!(cue_for("#oak_log"), Some("#leaves"));
        assert_eq!(cue_for("#oak_logs"), Some("#leaves"));
        assert_eq!(cue_for("#stone"), None);
        assert_eq!(cue_for("oak_log"), None, "a block name is not a tag");
        assert_eq!(cue_goal("#logs", sweep_radius, |_, _| None), None);
    }

    #[test]
    fn explore_does_not_finish_on_a_stone_that_holds_back_water() {
        use crate::test_world::{set_block, swept, world};

        // Seed 202: the only stone in reach was a lake bed's. If `near` reports
        // it, explore returns instantly and the skill calls `mine`, which
        // refuses the wet cell and fails with `no_block`.
        let mut wet_world = world();
        let wet = BlockPos::new(0, 64, 2);
        set_block(&mut wet_world, wet, BlockState::from(BlockKind::Stone));
        set_block(
            &mut wet_world,
            BlockPos::new(0, 65, 2),
            BlockState::from(BlockKind::Water),
        );
        let perception = swept(&wet_world);
        assert!(
            perception.known(wet).is_some(),
            "the sweep reached the wet stone"
        );
        assert_eq!(
            near_block(&perception, &wet_world, "stone", EXPLORE_NEAR_RADIUS),
            Some(false),
            "explore must not complete on a cell it would refuse to mine"
        );

        // A dry stone counts, so the false above is the wetness, not the map.
        let mut dry_world = world();
        set_block(&mut dry_world, wet, BlockState::from(BlockKind::Stone));
        let perception = swept(&dry_world);
        assert_eq!(
            near_block(&perception, &dry_world, "stone", EXPLORE_NEAR_RADIUS),
            Some(true)
        );
    }

    #[test]
    fn explore_does_not_complete_on_the_log_the_body_placed_to_climb() {
        use crate::test_world::{set_block, swept, world};
        use praxion_perception::scaffold;

        // Review finding 1: explore's `near` and `mine`'s walk share one
        // known-map search. A scaffold log is real `#logs`, so without the
        // shared exclusion explore would answer "already near" for a column
        // whose only log is the one the body put there.
        let mut log_world = world();
        let placed = BlockPos::new(0, 64, 2);
        set_block(&mut log_world, placed, BlockState::from(BlockKind::OakLog));
        let perception = swept(&log_world);
        assert_eq!(
            near_block(&perception, &log_world, "#logs", EXPLORE_NEAR_RADIUS),
            Some(true),
            "a real log is a target"
        );
        scaffold::note(placed);
        assert_eq!(
            near_block(&perception, &log_world, "#logs", EXPLORE_NEAR_RADIUS),
            Some(false),
            "the log the body placed is not a target"
        );
        scaffold::reset();
    }

    #[test]
    fn an_empty_plan_is_already_here_only_at_the_start_cell() {
        // `explore` asks for `Goal::Block`, which means exactly one cell. An
        // empty successful plan can only mean the body already stands on it;
        // every other empty success is a planner fault and must be rejected,
        // not recorded as explored.
        let start = at(-251, 66, -103);
        assert!(empty_plan_is_here(start, start));
        assert!(!empty_plan_is_here(start, at(-250, 66, -103)));
        assert!(!empty_plan_is_here(start, at(-251, 66, -104)));
        assert!(!empty_plan_is_here(start, at(-251, 67, -103)));
    }

    #[test]
    fn a_cue_cell_already_within_reach_starts_no_follower() {
        // Standing next to a known leaf: the planner succeeds with no steps.
        // That is "already there", not a goal: no follower, no goal, and the
        // chase mode must not change.
        let mut memory = ExploreMemory::default();
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        let body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        let start = at(0, 64, 0);
        let leaf = at(2, 64, 0);
        let mut ctx = ctx(0, &body, &world, &mut perception);
        let chase_before = explore.chase;
        assert!(
            !explore.approach(&mut ctx, start, leaf, Chase::Cue),
            "an empty plan is not a walk"
        );
        assert!(explore.follower.is_none(), "no follower for zero steps");
        assert!(explore.goal.is_none(), "a cue is not a goal");
        assert_eq!(explore.chase, chase_before, "the chase mode is unchanged");
        assert!(
            explore.cue_done.contains(&leaf),
            "the reached cue cell is remembered in its own list"
        );
    }

    #[test]
    fn look_around_holds_four_quarters_and_ends_within_its_cap() {
        // No target is known, so the call turns once. The body follows every
        // aim immediately, as a fast gaze would, and the turn must still visit
        // all four quarters and finish inside the cap.
        let mut memory = ExploreMemory::default();
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..5 {
            body.step(&Held::NONE, &Ground);
        }
        assert!(body.on_ground(), "the body stands on the test ground");
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        let start_yaw = body.look().y_rot();
        let mut quarters = Vec::new();
        let mut ended = None;
        for tick in 0..=LOOK_AROUND_MAX_TICKS {
            let frame = {
                let mut ctx = ctx(tick, &body, &world, &mut perception);
                explore.tick(&mut ctx, false, &mut memory).0
            };
            if let Look::Aim { yaw, .. } = frame.look {
                let quarter = ((yaw - start_yaw).rem_euclid(360.0) / 90.0).round() as i32;
                if !quarters.contains(&quarter) {
                    quarters.push(quarter);
                }
                frame
                    .apply(&mut body, tick)
                    .expect("the look-around frame applies");
            } else if !quarters.is_empty() {
                ended = Some(tick);
                break;
            }
        }
        assert_eq!(quarters, vec![0, 1, 2, 3], "all four quarters were held");
        let ended = ended.expect("the turn ends on its own");
        assert!(
            ended <= LOOK_AROUND_MAX_TICKS,
            "the turn took {ended} ticks"
        );
    }

    #[test]
    fn a_look_around_is_not_armed_in_water_or_in_the_air() {
        // Seed 101: the call turned on the spot while the body sank in water,
        // the drowning reflex preempted it, and the retry repeated the turn.
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        body.step(&Held::NONE, &Sea);
        assert!(body.is_in_water(), "the body is in the water column");
        assert!(
            !look_around_allowed(body.on_ground(), body.is_in_water()),
            "a body in water may not turn on the spot"
        );

        let mut memory = ExploreMemory::default();
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        {
            let mut ctx = ctx(0, &body, &world, &mut perception);
            explore.tick(&mut ctx, false, &mut memory);
        }
        assert!(
            explore.look_around.is_none(),
            "the call does not arm a turn while the body is in water"
        );
        assert!(
            explore.look_around_started,
            "the gate was evaluated in water, so the call does not retry it"
        );
        assert!(
            explore.lookout_done,
            "a refused start look-around leaves no lookout for the call"
        );

        // An airborne body stands nowhere either; it must fall, not turn. The
        // first tick must not consume the one look-around: the body may land
        // and turn then.
        let mut body = Body::new(Vec3::new(0.5, 80.0, 0.5));
        body.step(&Held::NONE, &Ground);
        assert!(!body.on_ground(), "the body is falling");
        assert!(!body.is_in_water());
        assert!(
            !look_around_allowed(body.on_ground(), body.is_in_water()),
            "a falling body may not turn on the spot"
        );

        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        {
            let mut ctx = ctx(0, &body, &world, &mut perception);
            explore.tick(&mut ctx, false, &mut memory);
        }
        assert!(
            !explore.look_around_started,
            "an airborne first tick must not consume the look-around"
        );
        assert!(explore.look_around.is_none());

        // Land the body and tick again: the turn arms now.
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..5 {
            body.step(&Held::NONE, &Ground);
        }
        assert!(body.on_ground(), "the body stands on the test ground");
        {
            let mut ctx = ctx(1, &body, &world, &mut perception);
            explore.tick(&mut ctx, false, &mut memory);
        }
        assert!(explore.look_around_started);
        assert!(
            explore.look_around.is_some(),
            "the first grounded tick arms the look-around"
        );
    }

    /// The column predicate over a map whose unlisted cells are air.
    fn column_open(feet: BlockPos, surface: i32, map: &HashMap<BlockPos, BlockState>) -> bool {
        open_to_sky(feet, Some(surface), |pos| {
            Some(map.get(&pos).copied().unwrap_or_else(air))
        })
    }

    fn air() -> BlockState {
        BlockState::from(BlockKind::Air)
    }

    #[test]
    fn a_column_is_open_only_under_air_a_canopy_or_vegetation() {
        let feet = at(0, 64, 0);
        // The scan starts at the head cell, y = 65.
        let mut head = HashMap::new();
        head.insert(at(0, 65, 0), BlockState::from(BlockKind::Stone));
        assert!(!column_open(feet, 66, &head), "a stone head cell covers it");

        let mut stone = HashMap::new();
        for y in 66..=68 {
            stone.insert(at(0, y, 0), BlockState::from(BlockKind::Stone));
        }
        assert!(
            !column_open(feet, 69, &stone),
            "three stone blocks cover it"
        );

        let mut canopy = HashMap::new();
        canopy.insert(at(0, 66, 0), BlockState::from(BlockKind::OakLeaves));
        canopy.insert(at(0, 67, 0), BlockState::from(BlockKind::OakLeaves));
        canopy.insert(at(0, 68, 0), BlockState::from(BlockKind::OakLog));
        assert!(
            column_open(feet, 69, &canopy),
            "a leaf and log canopy still lets the ground be reached"
        );

        let open = HashMap::new();
        assert!(column_open(feet, 66, &open), "nothing above the head");
        assert!(
            column_open(feet, 65 + SKY_SCAN_BLOCKS, &open),
            "the whole scan window is air"
        );

        let mut overhang = HashMap::new();
        overhang.insert(at(0, 66 + 59, 0), BlockState::from(BlockKind::Stone));
        assert!(
            !column_open(feet, 66 + 60, &overhang),
            "a 60-block overhang counts as covered"
        );

        // No heightmap is not a fact about the column: the cell is kept for the
        // planner, which rejects a chunk it cannot reach anyway.
        assert!(open_to_sky(feet, None, |_| None));

        // Water is in the `replaceable` tag and stays open (a water frontier
        // is the swimming last resort); lava and fire are hazards and cover.
        let mut water = HashMap::new();
        water.insert(at(0, 66, 0), BlockState::from(BlockKind::Water));
        assert!(column_open(feet, 67, &water), "a water column stays open");
        let mut kelp = HashMap::new();
        kelp.insert(at(0, 66, 0), BlockState::from(BlockKind::KelpPlant));
        assert!(
            column_open(feet, 67, &kelp),
            "kelp is swimmable water, not a roof"
        );
        for kind in [BlockKind::Lava, BlockKind::Fire] {
            let mut hazard = HashMap::new();
            hazard.insert(at(0, 66, 0), BlockState::from(kind));
            assert!(!column_open(feet, 67, &hazard), "{kind:?} covers the cell");
        }
    }

    #[test]
    fn explore_memory_is_per_runner() {
        // Two runners must not share explore's history: the cells and the
        // heading one has stay invisible to the other.
        let mut first = ExploreMemory::default();
        let mut second = ExploreMemory::default();
        first.remember(at(1, 64, 2));
        first.heading.direction(at(0, 64, 0), &[1; 8]);
        assert_eq!(first.recent(), vec![at(1, 64, 2)]);
        assert!(
            second.recent().is_empty(),
            "the other run remembers nothing"
        );
        assert_eq!(
            second.heading.direction(at(0, 64, 0), &[0; 8]),
            None,
            "the other run has no heading"
        );

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

    /// Height of the test pyramid above the floor at `(x, z)`: a 5-block
    /// stair pyramid whose top is `(8, 8)`, one block per ring.
    fn pyramid_height(x: i32, z: i32) -> i32 {
        (5 - (x - 8).abs().max((z - 8).abs())).max(0)
    }

    /// The block the pyramid world holds at `pos`: floor to y = 63, the pyramid
    /// on it and air elsewhere.
    fn pyramid_block(pos: BlockPos) -> BlockState {
        if pos.y <= 63 + pyramid_height(pos.x, pos.z) {
            BlockState::from(BlockKind::Stone)
        } else {
            air()
        }
    }

    /// Lets the body walk the pyramid world.
    struct PyramidBlocks;

    impl Blocks for PyramidBlocks {
        fn block(&self, pos: BlockPos) -> Option<BlockState> {
            Some(pyramid_block(pos))
        }

        fn ready(&self, _position: Vec3) -> bool {
            true
        }
    }

    /// A one-chunk world with a stair pyramid (top `(8, 69, 8)`, +5 above the
    /// start) and a swept perception of it; `with_pyramid` false leaves flat
    /// ground. The body stands at the origin on the floor.
    fn pyramid_world(with_pyramid: bool) -> (World, Perception, Body) {
        use crate::test_world::{set_block, swept, world};

        let mut world = world();
        for x in 0..16 {
            for z in 0..16 {
                let top = if with_pyramid {
                    63 + pyramid_height(x, z)
                } else {
                    63
                };
                for y in 63..=top {
                    set_block(&mut world, at(x, y, z), BlockState::from(BlockKind::Stone));
                }
            }
        }
        let mut perception = swept(&world);
        for _ in 0..300 {
            perception.begin_tick(&world);
            perception.sweep(&world);
        }
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..5 {
            body.step(&Held::NONE, &PyramidBlocks);
        }
        assert!(body.on_ground());
        (world, perception, body)
    }

    /// An explore call whose planner may take seconds: the body budget is six
    /// wall-clock milliseconds, which a loaded test machine can exceed, and a
    /// timed-out lookout plan spends the climb.
    fn patient_explore(memory: &mut ExploreMemory) -> Explore {
        let mut explore = Explore::new("#logs".into(), memory).expect("a known block tag");
        let mut planner = Planner::with_budget(praxion_planner::Budget::new(
            Duration::from_secs(10),
            200_000,
        ));
        planner.set_allow_world_changes(false);
        explore.planner = planner;
        explore
    }

    /// Ticks `explore` with the body following every frame until `stop` holds
    /// or the tick range ends. Returns the tick `stop` first held at.
    fn drive(
        explore: &mut Explore,
        memory: &mut ExploreMemory,
        (world, perception, body): (&World, &mut Perception, &mut Body),
        range: std::ops::Range<u64>,
        mut stop: impl FnMut(&Explore) -> bool,
    ) -> Option<u64> {
        for tick in range {
            let frame = {
                let mut ctx = ctx(tick, body, world, perception);
                explore.tick(&mut ctx, true, memory).0
            };
            frame.apply(body, tick).expect("the frame applies");
            body.step(&frame.held, &PyramidBlocks);
            if stop(explore) {
                return Some(tick);
            }
        }
        None
    }

    #[test]
    fn a_near_frontier_and_a_mound_climb_once_then_look_around_again() {
        let (world, mut perception, mut body) = pyramid_world(true);
        let mut memory = ExploreMemory::default();
        let mut explore = patient_explore(&mut memory);
        let top = at(8, 69, 8);

        // The look-around finds nothing, the nearest frontier is the chunk
        // edge beside the start, and the pyramid is a reachable +5 viewpoint.
        let planned = drive(
            &mut explore,
            &mut memory,
            (&world, &mut perception, &mut body),
            0..300,
            |e| e.lookout_done && e.follower.is_some(),
        );
        assert!(planned.is_some(), "the lookout walk starts");
        assert_eq!(explore.chase, Chase::Lookout);
        assert_eq!(explore.goal, Some(top), "the pyramid top is the viewpoint");
        assert!(explore.look_around.is_none(), "the first turn is over");

        // On arrival the look-around is armed once more.
        let arrived = drive(
            &mut explore,
            &mut memory,
            (&world, &mut perception, &mut body),
            planned.unwrap() + 1..900,
            |e| e.follower.is_none(),
        );
        assert!(arrived.is_some(), "the body reaches the top");
        assert_eq!(body.position().to_block_pos_floor(), top);
        assert!(
            explore.look_around.is_some(),
            "arrival re-arms the look-around"
        );
        assert!(
            memory.recent().is_empty() && !explore.explored.contains(&top),
            "a viewpoint is not an explored frontier"
        );

        // The turn runs out and normal exploring resumes: no second climb, no
        // second turn.
        let rearmed = std::cell::Cell::new(0_u32);
        let second_climb = std::cell::Cell::new(false);
        let mut was_turning = true;
        drive(
            &mut explore,
            &mut memory,
            (&world, &mut perception, &mut body),
            arrived.unwrap() + 1..arrived.unwrap() + 400,
            |e| {
                let turning = e.look_around.is_some();
                if turning && !was_turning {
                    rearmed.set(rearmed.get() + 1);
                }
                was_turning = turning;
                if e.chase == Chase::Lookout && e.follower.is_some() {
                    second_climb.set(true);
                }
                false
            },
        );
        assert_eq!(rearmed.get(), 0, "the look-around is not armed again");
        assert!(!second_climb.get(), "the call climbs once");
    }

    #[test]
    fn flat_ground_near_the_frontier_plans_no_lookout() {
        let (world, mut perception, mut body) = pyramid_world(false);
        let mut memory = ExploreMemory::default();
        let mut explore = patient_explore(&mut memory);
        let climbed = drive(
            &mut explore,
            &mut memory,
            (&world, &mut perception, &mut body),
            0..200,
            |e| e.chase == Chase::Lookout,
        );
        assert_eq!(climbed, None, "no viewpoint, no climb");
        assert!(!explore.lookout_done, "the climb is not spent");
    }

    #[test]
    fn the_lookout_gate_opens_only_for_a_frontier_under_32_blocks() {
        assert!(!lookout_gate_open(None), "no frontier, no gate");
        assert!(lookout_gate_open(Some(31 * 31)));
        assert!(!lookout_gate_open(Some(32 * 32)), "32 blocks is not closer");
        assert!(!lookout_gate_open(Some(60 * 60)));

        // The same decision through `Explore`: a big known platform with a
        // +6 stair mound, the edge of the known map 50 blocks out (far) or
        // 25 blocks out (near).
        let scan_of = |half: i32| {
            let mut map: HashMap<BlockPos, BlockState> = HashMap::new();
            for x in -half..=half {
                for z in -half..=half {
                    let rise = (6 - (x - 8).abs().max(z.abs())).clamp(0, 6);
                    for y in 63..=64 + rise + 1 {
                        let state = if y <= 63 + rise {
                            BlockState::from(BlockKind::Stone)
                        } else {
                            air()
                        };
                        map.insert(at(x, y, z), state);
                    }
                }
            }
            scan_frontiers(
                at(0, 64, 0),
                80,
                EXPLORE_FRONTIER_VERTICAL,
                &[],
                &[],
                move |pos| map.get(&pos).copied(),
                false,
                |_| true,
            )
        };
        let body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        let mut memory = ExploreMemory::default();

        let far = scan_of(50);
        assert!(far.lookout().is_some(), "the mound is there either way");
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        {
            let mut ctx = ctx(0, &body, &world, &mut perception);
            explore.plan_lookout(&mut ctx, at(0, 64, 0), &far);
        }
        assert!(!explore.lookout_done, "a far frontier keeps the climb");

        let near = scan_of(25);
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        {
            let mut ctx = ctx(0, &body, &world, &mut perception);
            explore.plan_lookout(&mut ctx, at(0, 64, 0), &near);
        }
        assert!(
            explore.lookout_done,
            "a near frontier spends the climb, even if the plan then fails"
        );
        assert!(
            explore.follower.is_none(),
            "an empty perception cannot plan"
        );
    }

    #[test]
    fn cue_walks_stop_at_max_cue_attempts_with_a_leaf_still_known() {
        use crate::test_world::{set_block, swept, world};

        // Three distinct known leaf cells inside the sweep radius: a cue is
        // always available, so only the cap can stop the third walk.
        let mut world = world();
        let leaves = [at(0, 70, 2), at(2, 70, 2), at(2, 70, 0)];
        for cell in leaves {
            set_block(&mut world, cell, BlockState::from(BlockKind::OakLeaves));
        }
        let mut perception = swept(&world);
        for cell in leaves {
            // The sweep covers a rotating slice of the eye's section, so a
            // specific cell can fall outside the ten ticks; record the leaf
            // like a confirmed change to make the canopy known.
            perception.note_changed(&world, cell);
            assert!(
                perception.known(cell).is_some(),
                "the known map holds the leaf at {cell:?}"
            );
        }

        let mut memory = ExploreMemory::default();
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        let body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        let start = at(0, 64, 0);
        for _ in 0..MAX_CUE_ATTEMPTS {
            {
                let mut ctx = ctx(0, &body, &world, &mut perception);
                explore.seek(&mut ctx, start);
            }
            // Reset the leg, not only the cap: the `Cue && follower` guard
            // would refuse the second call for another reason.
            explore.follower = None;
            explore.chase = Chase::Frontier;
        }
        assert_eq!(
            explore.cue_attempts, MAX_CUE_ATTEMPTS,
            "each call spends one cue attempt"
        );

        // The third leaf is still known: a fourth cue would exist if the cap
        // did not refuse it.
        assert!(
            nearest_known_block(
                &perception,
                &world,
                "#leaves",
                EXPLORE_SEARCH_RADIUS,
                &explore.cue_done,
            )
            .is_some(),
            "a known leaf is still left"
        );
        {
            let mut ctx = ctx(0, &body, &world, &mut perception);
            assert!(
                !explore.seek(&mut ctx, start),
                "the third seek is refused by the cap, not by a missing cue"
            );
        }
        assert_eq!(explore.cue_attempts, MAX_CUE_ATTEMPTS);
    }

    #[test]
    fn a_capped_walk_back_does_not_consume_the_explored_cell() {
        let mut memory = ExploreMemory::default();
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..5 {
            body.step(&Held::NONE, &Ground);
        }
        assert!(body.on_ground());
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());

        explore.look_around_started = true;
        explore.lookout_done = true;
        explore.target_search_tick = 1_000;
        explore.frontier_scan_tick = 1_000;
        explore.allow_water = true;
        explore.idle_ticks = 10;
        explore.walk_backs = EXPLORE_MAX_WALK_BACKS;
        explore.explored = vec![at(1, 64, 0)];

        {
            let mut ctx = ctx(0, &body, &world, &mut perception);
            explore.tick(&mut ctx, false, &mut memory);
        }
        assert_eq!(
            explore.explored,
            vec![at(1, 64, 0)],
            "a capped walk-back must not remove the cell it would not walk to"
        );
        assert_eq!(
            explore.walk_backs, EXPLORE_MAX_WALK_BACKS,
            "no walk-back was counted"
        );
        assert!(explore.follower.is_none());
    }

    #[test]
    fn an_empty_walk_back_plan_drops_the_cell_and_waits_five_ticks() {
        let mut memory = ExploreMemory::default();
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..5 {
            body.step(&Held::NONE, &Ground);
        }
        assert!(body.on_ground());
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());

        explore.look_around_started = true;
        explore.lookout_done = true;
        explore.target_search_tick = 1_000;
        explore.frontier_scan_tick = 1_000;
        explore.allow_water = true;
        explore.idle_ticks = 10;
        explore.walk_backs = 1;
        // Within TARGET_APPROACH_RADIUS of the start: the plan succeeds with no
        // steps, so a walk back would end on the next tick.
        explore.explored = vec![at(2, 64, 0)];
        let tick = 20;

        {
            let mut ctx = ctx(tick, &body, &world, &mut perception);
            explore.tick(&mut ctx, false, &mut memory);
        }
        assert!(
            !explore.explored.contains(&at(2, 64, 0)),
            "an already-reached cell is not a walk-back target"
        );
        assert_eq!(
            explore.next_plan_tick,
            tick + 5,
            "the next plan attempt waits five ticks"
        );
        assert_eq!(explore.walk_backs, 1, "no walk started, so none counted");
        assert!(explore.follower.is_none());
    }

    #[test]
    fn a_target_becoming_visible_mid_turn_ends_the_look_around() {
        use crate::test_world::{set_block, swept, world_with};

        // A stone floor the planner can walk on; no log is known yet, so the
        // first tick arms the turn.
        let mut world = world_with(1, 63, |_| BlockState::from(BlockKind::Stone));
        let mut perception = swept(&world);
        let mut memory = ExploreMemory::default();
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..5 {
            body.step(&Held::NONE, &Ground);
        }
        assert!(body.on_ground());

        let first = {
            let mut ctx = ctx(0, &body, &world, &mut perception);
            explore.tick(&mut ctx, false, &mut memory).0
        };
        assert!(explore.look_around.is_some(), "the first tick arms the turn");
        assert!(matches!(first.look, Look::Aim { .. }));

        // A log appears in view, beyond the approach radius so the seek starts
        // a real walk, and the sweep records it.
        let log = at(0, 64, 8);
        set_block(&mut world, log, BlockState::from(BlockKind::OakLog));
        perception.begin_tick(&world);
        perception.sweep(&world);
        assert!(perception.known(log).is_some(), "the log is known now");

        // The next search period runs while the turn is still active: the
        // target wins, the turn ends, and the frame walks instead of turning.
        let frame = {
            let mut ctx = ctx(20, &body, &world, &mut perception);
            explore.tick(&mut ctx, false, &mut memory).0
        };
        assert!(
            explore.look_around.is_none(),
            "a target in sight ends the look-around"
        );
        assert!(
            !matches!(frame.look, Look::Aim { .. }),
            "the frame does not keep turning once the target is known"
        );
        assert!(explore.follower.is_some(), "the log starts a walk");
    }

    #[test]
    fn a_frontier_scan_ledgers_its_privileged_column_reads() {
        use crate::test_world::{swept, world_with};

        // A stone plain with no log in sight: the frontier scan is the only
        // thing this tick can do, and it reads one privileged column per
        // candidate.
        let world = world_with(2, 63, |_| BlockState::from(BlockKind::Stone));
        let mut perception = swept(&world);
        assert!(
            perception
                .ledger()
                .get(QueryKind::SkyColumn, FactKind::Column)
                .is_none(),
            "nothing has asked for a column yet"
        );
        let mut memory = ExploreMemory::default();
        let mut explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..5 {
            body.step(&Held::NONE, &Ground);
        }
        assert!(body.on_ground());

        // Skip the start turn and the target search so this tick reaches the
        // frontier scan.
        explore.look_around_started = true;
        explore.target_search_tick = 1_000;
        explore.frontier_scan_tick = 0;
        let tick = perception.tick();
        {
            let mut ctx = ctx(tick, &body, &world, &mut perception);
            explore.tick(&mut ctx, false, &mut memory);
        }

        let entry = perception
            .ledger()
            .get(QueryKind::SkyColumn, FactKind::Column)
            .expect("the frontier scan ledgers the columns it reads");
        assert!(entry.count() > 0, "every scanned candidate is counted");
        assert_eq!(entry.first_tick(), tick);
        assert_eq!(entry.last_tick(), tick);
        assert_eq!(
            perception.ledger().len(),
            1,
            "one entry per (query, fact), not one per read"
        );
    }

    #[test]
    fn a_mob_target_ends_the_call_when_one_is_near_and_unknown_names_are_refused() {
        use crate::test_world::{add_entity, feet, swept, world};
        use azalea_registry::builtin::EntityKind;
        use praxion_planner::Personality;

        let mut memory = ExploreMemory::default();
        assert!(Explore::new("unicorn".into(), &mut memory).is_err());
        // The mob half of explore may only seek what the body hunts (decision
        // 2026-11-09 "Five next areas": never a creeper or an Enderman).
        assert!(Explore::new("creeper".into(), &mut memory).is_err());
        assert!(Explore::new("enderman".into(), &mut memory).is_err());
        assert!(Explore::new("cow".into(), &mut memory).is_ok());

        let run = |with_cow: bool| {
            let mut world = world();
            if with_cow {
                add_entity(&mut world, 5, EntityKind::Cow, Vec3::new(5.5, 64.0, 0.5));
            }
            let mut perception = swept(&world);
            let body = Body::new(feet());
            let mut memory = ExploreMemory::default();
            let mut explore = Explore::new("cow".into(), &mut memory).expect("a mob target");
            let mut ctx = Ctx {
                tick: 20,
                body: &body,
                world: &world,
                perception: &mut perception,
                style: Personality::default(),
            };
            explore.tick(&mut ctx, false, &mut memory).2
        };
        assert_eq!(run(true), Some(Outcome::Confirmed), "a cow five blocks away");
        assert_eq!(run(false), None, "no cow: the call goes on searching");
    }
}
