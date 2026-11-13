//! `dig_down`, `branch_mine` and `climb_out`: dig a walkable one-wide passage
//! toward a block that no ray can see, or back up to the open sky. All three
//! run on the [`Digger`] state machine; they differ in their route (see
//! [`Route`]). The two ways down plan with [`plan_step`], the way up with
//! [`plan_climb`].
//!
//! # `dig_down`
//!
//! Stone lies three to six blocks under the dirt of flat terrain and no ray
//! ever reaches it from the surface, so `explore` cannot find it and, by
//! decision, may not dig. This primitive does what a person does: it cuts a
//! diagonal staircase down along one axis until the wanted block is known
//! within mining reach, then returns `confirmed` so the caller's next rule
//! (`mine`) takes over.
//!
//! # `branch_mine`
//!
//! Ores sit inside rock, so a surface search never sees them. `branch_mine`
//! first cuts the same staircase down to the ore's best height, then tunnels
//! level, in legs that turn at right angles, until the ore is in sight.
//!
//! # `climb_out`
//!
//! The way back up (`go_to_surface`): a body that tunnelled down stands under
//! rock that no skill walks out of. `climb_out` cuts a staircase UP, the mirror
//! of the descent, until the body stands under open sky ([`praxion_facts::sky_above`]).
//! It walks an open stair when one is there (its own descent staircase, a cave
//! slope) and digs only where the way is closed. See [`plan_climb`] for the
//! step and its safety rules.
//!
//! # One step (down)
//!
//! One step goes from the feet cell `F = (x, y, z)` to the next column
//! `(x + dx, ., z + dz)`:
//!
//! ```text
//!   y+1   .  [A]     A: clearance, so the body fits while it walks in
//!   y     F  [B]     B: head room of the new stair
//!   y-1  [S] [T]     T: the new feet cell, one lower (Slope::Down)
//!   y-2  [S] [floor] floor: must be solid, so the fall is one block
//! ```
//!
//! A level step (`Slope::Level`) has no S and T row: B is the new feet cell and
//! the floor is the block under it.
//!
//! The cells (A, B and T, or A and B) are dug top-down (a gravity block above A then drains
//! into the open cells one dig at a time, never onto a cell still closed
//! below it), then the body walks onto T. The block under the body is never
//! dug. Every dug block gets exactly one dig intent and receipt, like `mine`.

use azalea_core::position::BlockPos;
use praxion_headless::{Click, ControlFrame};
use praxion_facts::sky_above;
use praxion_interpreter::Outcome;
use praxion_perception::exposes_fluid;
use praxion_planner::{blocks, center};
use praxion_world::BlockState;

use crate::branch_mine::{BRANCH_MAX_TICKS, Branch, Candidates};
use crate::common::{HandSelect, ToolStep, aim_precise, hold_tool, idle, pickup_frame};
use crate::frontier::nearest_known_block;
use crate::mine::block_gone;
use crate::move_to::Move;
use crate::runner::{Ctx, IntentSpec};

/// How deep the staircase may go below where the call started.
pub(crate) const MAX_DEPTH: i32 = 12;

/// How many blocks one `dig_down` call may dig, refills of falling sand
/// included.
pub(crate) const MAX_DUGS: u32 = 40;

/// Level steps a `branch_mine` call may walk without digging one block. In an
/// open cave the branch route can cycle through open cells forever: every
/// candidate is a walk, so the dig budget never grows. The cap turns that loop
/// into a bounded `stuck` failure. It is four tunnel legs (see
/// [`crate::branch_mine::LEG_STEPS`]), far more than any real cave crossing;
/// `dig_down` always descends, so it never takes a level step and the counter
/// stays at zero for it.
pub(crate) const MAX_DRY_LEVEL_STEPS: u32 = 64;

/// Ticks one `climb_out` call may run. A stair up is three digs (about 35 ticks
/// each in stone) and the walk onto it: about 120 ticks a block, so 3,000 ticks
/// climb some twenty-five blocks of closed rock, the depth a `branch_mine`
/// tunnel reaches from the surface twice over. Past it the call fails
/// `no_way_up`, between two digs like the other caps.
pub(crate) const CLIMB_MAX_TICKS: u32 = 3000;

/// How many blocks one `climb_out` call may dig: three a stair, forty stairs.
pub(crate) const CLIMB_MAX_DUGS: u32 = 120;

/// The failure kind of a climb that cannot go on: every axis refused, or a cap
/// reached.
const NO_WAY_UP: &str = "no_way_up";

/// The window of each dig receipt, as in `mine`.
const DIG_WINDOW: u64 = 160;

/// Ticks a target may wait for its dig to register (tool, aim) before the
/// call gives up as `stuck`.
const TARGET_WAIT_TICKS: u32 = 80;

/// Ticks one walk onto the next stair may take.
const WALK_TICKS: u32 = 60;

/// Ticks the body may stay in the air before the call gives up.
const AIRBORNE_TICKS: u32 = 60;

/// Ticks to wait for a hanging gravity block to fall into the open column.
const SETTLE_TICKS: u32 = 40;

/// Idle ticks after a dig under a gravity block, so the falling block lands
/// before the next plan reads the cells.
const GRAVITY_COOLDOWN_TICKS: u32 = 10;

/// How far from where the body stands `dig_down` looks for another cell to
/// start the staircase from when it refuses the one it stands on. A person who
/// cannot dig here steps a few blocks aside; five is the reach of the
/// stone-near guard (`near(stone, 5)`).
pub(crate) const RELOCATE_RADIUS: i32 = 5;

/// How many times one `dig_down` call may step aside. Two: the second try
/// covers a spot that proved bad only once its neighbours were in view; past
/// it the ground around is not diggable and the call says so with the refusal.
const MAX_RELOCATIONS: u32 = 2;

/// Ticks one walk to another start cell may take (about ten seconds).
const RELOCATE_TICKS: u32 = 200;

/// An axis direction as `(dx, dz)`.
pub(crate) type Dir = (i32, i32);

/// Why a staircase step cannot be taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Refusal {
    /// Removing a cell would let water or lava reach it.
    Fluid,
    /// The floor of the new stair is not solid (or hazardous), a cell is
    /// unloaded, or a hazard sits in the way.
    UnsafeDrop,
    /// A cell cannot be broken (bedrock).
    NoBlock,
    /// The stair would be deeper than [`MAX_DEPTH`].
    TooDeep,
}

impl Refusal {
    pub(crate) fn kind(self) -> &'static str {
        match self {
            Self::Fluid => "fluid",
            Self::UnsafeDrop => "unsafe_drop",
            Self::NoBlock => "no_block",
            Self::TooDeep => "too_deep",
        }
    }
}

/// What the next move of the staircase is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StairStep {
    /// Dig this cell.
    Dig(BlockPos),
    /// The cells are clear but a gravity block hangs over them: wait for it.
    Settle,
    /// The cells are clear: walk onto this feet cell.
    Walk(BlockPos),
    Refuse(Refusal),
}

/// Whether `state` falls when the cell under it is empty.
fn is_gravity(state: BlockState) -> bool {
    let name = blocks::name(state);
    matches!(
        name,
        "sand"
            | "red_sand"
            | "gravel"
            | "suspicious_sand"
            | "suspicious_gravel"
            | "dragon_egg"
            | "anvil"
            | "chipped_anvil"
            | "damaged_anvil"
    ) || name.ends_with("concrete_powder")
}

fn offset(pos: BlockPos, dx: i32, dy: i32, dz: i32) -> BlockPos {
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

/// How a step changes the feet height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slope {
    /// The new feet cell is one lower: a stair going down.
    Down,
    /// The new feet cell is at the same height: a tunnel.
    Level,
}

impl Slope {
    /// How many blocks lower the new feet cell is.
    fn drop(self) -> i32 {
        match self {
            Self::Down => 1,
            Self::Level => 0,
        }
    }
}

/// The next move from the feet cell `feet` along `dir`. Pure over a block
/// lookup, so the safety rules are tested without a world. A step whose new
/// feet cell would lie below `min_y` is refused as too deep.
///
/// Refusals are decided before the first dig of a step, so a step that cannot
/// be finished is never started.
pub(crate) fn plan_step(
    feet: BlockPos,
    dir: Dir,
    slope: Slope,
    min_y: i32,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> StairStep {
    let (dx, dz) = dir;
    let drop = slope.drop();
    let tread = offset(feet, dx, -drop, dz);
    let floor = offset(feet, dx, -drop - 1, dz);
    if tread.y < min_y {
        return StairStep::Refuse(Refusal::TooDeep);
    }

    let Some(floor_state) = lookup(floor) else {
        return StairStep::Refuse(Refusal::UnsafeDrop);
    };
    if blocks::fluid(floor_state).is_some() {
        return StairStep::Refuse(Refusal::Fluid);
    }
    // The fall onto the new feet cell is at most one block: its floor must hold.
    if !blocks::is_solid(floor_state) || blocks::is_hazard(floor_state) {
        return StairStep::Refuse(Refusal::UnsafeDrop);
    }

    // Top-down: clearance, head room, and for a stair the new feet cell. Three
    // cells at most, so a fixed array keeps the per-tick path allocation-free.
    let mut cells = [feet; 3];
    let mut count = 0;
    for dy in (-drop..=1).rev() {
        cells[count] = offset(feet, dx, dy, dz);
        count += 1;
    }
    let mut first_closed = None;
    for &cell in &cells[..count] {
        let Some(state) = lookup(cell) else {
            return StairStep::Refuse(Refusal::UnsafeDrop);
        };
        if blocks::fluid(state).is_some() {
            return StairStep::Refuse(Refusal::Fluid);
        }
        if blocks::is_passable(state) {
            continue;
        }
        if blocks::is_hazard(state) {
            return StairStep::Refuse(Refusal::UnsafeDrop);
        }
        if !blocks::is_breakable(state) {
            return StairStep::Refuse(Refusal::NoBlock);
        }
        // Breaking a cell that holds back water or lava floods the stair.
        if exposes_fluid(cell, lookup) {
            return StairStep::Refuse(Refusal::Fluid);
        }
        first_closed.get_or_insert(cell);
    }
    if let Some(cell) = first_closed {
        return StairStep::Dig(cell);
    }
    // Everything is open. A gravity block above the clearance cell falls into
    // it as soon as the cell below goes empty: let it land and dig it again.
    if lookup(offset(feet, dx, 2, dz)).is_some_and(is_gravity) {
        return StairStep::Settle;
    }
    StairStep::Walk(tread)
}

/// The next move of a climb from the feet cell `feet` along `dir`: one stair UP.
///
/// ```text
///   y+2  [A] [C]     A: clearance over the head, so the body can rise
///   y+1   .  [T]     C: head room of the new stair
///   y     F  [S]     T: the new feet cell, one higher
///                    S: its floor, the block beside the feet: must be solid
/// ```
///
/// The cells (C, A, T) are dug top-down. It is the mirror of the descent, with
/// the roof where the descent has the floor: no cell is opened under fluid, and
/// none under a block that falls (sand, gravel), because that block would drop
/// into the stair on the body. Refusals are decided before the first dig, and
/// nothing is dug when the stair is already open (a [`StairStep::Walk`]).
///
/// Pure over a block lookup, like [`plan_step`].
pub(crate) fn plan_climb(
    feet: BlockPos,
    dir: Dir,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> StairStep {
    let (dx, dz) = dir;
    let tread = offset(feet, dx, 1, dz);
    let floor = offset(feet, dx, 0, dz);

    let Some(floor_state) = lookup(floor) else {
        return StairStep::Refuse(Refusal::UnsafeDrop);
    };
    if blocks::fluid(floor_state).is_some() {
        return StairStep::Refuse(Refusal::Fluid);
    }
    // The body stands on it: the block must hold and must not hurt.
    if !blocks::is_solid(floor_state) || blocks::is_hazard(floor_state) {
        return StairStep::Refuse(Refusal::UnsafeDrop);
    }

    let cells = [
        offset(feet, dx, 2, dz),
        offset(feet, 0, 2, 0),
        tread,
    ];
    let mut first_closed = None;
    for &cell in &cells {
        let Some(state) = lookup(cell) else {
            return StairStep::Refuse(Refusal::UnsafeDrop);
        };
        if blocks::fluid(state).is_some() {
            return StairStep::Refuse(Refusal::Fluid);
        }
        if blocks::is_passable(state) {
            continue;
        }
        if blocks::is_hazard(state) {
            return StairStep::Refuse(Refusal::UnsafeDrop);
        }
        if !blocks::is_breakable(state) {
            return StairStep::Refuse(Refusal::NoBlock);
        }
        // Breaking a cell that holds back water or lava floods the stair; the
        // cell above counts (a lake over the roof falls in).
        if exposes_fluid(cell, lookup) {
            return StairStep::Refuse(Refusal::Fluid);
        }
        first_closed.get_or_insert(cell);
    }
    if let Some(cell) = first_closed {
        // The block over the cell about to open must stay where it is. Sand or
        // gravel would fall into the stair; an unloaded cell is not known to
        // be safe. (The cell over the tread is the head room, dug before it.)
        match lookup(offset(cell, 0, 1, 0)) {
            Some(above) if !is_gravity(above) => {}
            _ => return StairStep::Refuse(Refusal::UnsafeDrop),
        }
        return StairStep::Dig(cell);
    }
    StairStep::Walk(tread)
}

/// Whether the body can stand in `cell`: room for its feet and head, a solid
/// floor under it that is no hazard.
fn standable(cell: BlockPos, lookup: &impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    lookup(cell).is_some_and(blocks::is_passable)
        && lookup(offset(cell, 0, 1, 0)).is_some_and(blocks::is_passable)
        && lookup(offset(cell, 0, -1, 0))
            .is_some_and(|floor| blocks::is_solid(floor) && !blocks::is_hazard(floor))
}

/// Whether a climbing body at `feet` is out of the ground: it can walk away on
/// the level, to a side cell it can stand in. Open sky alone is not enough: the
/// stair the climb cut is itself open to the sky a few blocks below the ground
/// (its head rooms stack into a trench), and a body in that trench has no side
/// cell to step to but the stair behind it, whose floor is a block lower.
fn is_out(feet: BlockPos, lookup: &impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    axis_order((1, 0))
        .into_iter()
        .any(|(dx, dz)| standable(offset(feet, dx, 0, dz), lookup))
}

/// The nearest cell within [`RELOCATE_RADIUS`] of `feet` (not `feet` itself,
/// which was refused) the body can stand in and from which at least one axis of
/// the staircase is not refused. Nearest by horizontal distance, ties by
/// position so the answer is deterministic; a cell at the same height or one
/// lower or higher, so the walk there is a stroll, not a climb. Pure over a
/// block lookup, so the choice is tested without a world.
pub(crate) fn relocation_cell(
    feet: BlockPos,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<BlockPos> {
    let mut columns: Vec<(i32, i32)> = (-RELOCATE_RADIUS..=RELOCATE_RADIUS)
        .flat_map(|dx| (-RELOCATE_RADIUS..=RELOCATE_RADIUS).map(move |dz| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0) && dx * dx + dz * dz <= RELOCATE_RADIUS * RELOCATE_RADIUS)
        .collect();
    columns.sort_by_key(|&(dx, dz)| (dx * dx + dz * dz, dx, dz));
    for (dx, dz) in columns {
        for dy in [0, -1, 1] {
            let cell = offset(feet, dx, dy, dz);
            if !standable(cell, lookup) {
                continue;
            }
            let min_y = cell.y - MAX_DEPTH;
            let digs = axis_order((1, 0)).into_iter().any(|dir| {
                !matches!(
                    plan_step(cell, dir, Slope::Down, min_y, lookup),
                    StairStep::Refuse(_)
                )
            });
            if digs {
                return Some(cell);
            }
        }
    }
    None
}

/// The axis direction a yaw (degrees, Minecraft convention: 0 faces south)
/// points at, rounded.
fn facing(yaw: f32) -> Dir {
    let radians = yaw.to_radians();
    let (dx, dz) = (-radians.sin(), radians.cos());
    if dx.abs() > dz.abs() {
        (dx.signum() as i32, 0)
    } else {
        (0, dz.signum() as i32)
    }
}

/// The four axes, the facing one first.
pub(crate) fn axis_order(first: Dir) -> [Dir; 4] {
    let (dx, dz) = first;
    [(dx, dz), (-dz, dx), (-dx, -dz), (dz, -dx)]
}

/// The dig target the call is breaking now.
#[derive(Debug)]
struct Target {
    cell: BlockPos,
    /// The runner registered the dig intent.
    started: bool,
    /// The receipt settled `confirmed`.
    settled: bool,
    /// A gravity block hung over the cell when it was chosen.
    gravity_over: bool,
    /// Ticks spent before the intent registered.
    waited: u32,
}

/// Where the digger goes next: the one thing `dig_down` and `branch_mine`
/// differ in.
#[derive(Debug)]
enum Route {
    /// `dig_down`: one axis, always down, at most [`MAX_DEPTH`] below the
    /// start. The axis is fixed once a step has been accepted.
    Staircase(Option<Dir>),
    /// `branch_mine`: down to the ore's height, then level legs.
    Branch(Branch),
    /// `climb_out`: stairs up, along the axis of the last accepted step (an
    /// axis only prefers; any axis the planner does not refuse may be taken).
    Climb(Option<Dir>),
}

impl Route {
    /// Which way the next step slopes from feet height `feet_y`.
    fn slope(&self, feet_y: i32) -> Slope {
        match self {
            Self::Staircase(_) => Slope::Down,
            Self::Branch(branch) => branch.slope(feet_y),
            // Not read by the climb: it plans with `plan_climb` and the
            // level-step counters never see it.
            Self::Climb(_) => Slope::Down,
        }
    }

    /// The step along `dir` from `feet`.
    fn plan(
        &self,
        feet: BlockPos,
        dir: Dir,
        slope: Slope,
        min_y: i32,
        lookup: &impl Fn(BlockPos) -> Option<BlockState>,
    ) -> StairStep {
        match self {
            Self::Climb(_) => plan_climb(feet, dir, lookup),
            _ => plan_step(feet, dir, slope, min_y, lookup),
        }
    }

    /// The lowest feet height a step may reach.
    fn min_y(&self, start_y: i32) -> i32 {
        match self {
            Self::Staircase(_) => start_y - MAX_DEPTH,
            Self::Branch(_) | Self::Climb(_) => i32::MIN,
        }
    }

    /// The axes to try for the next step, best first.
    fn candidates(&self, facing: Dir) -> Candidates {
        match self {
            Self::Staircase(Some(dir)) => Candidates::single(*dir),
            Self::Staircase(None) => Candidates::from_axes(axis_order(facing), None),
            Self::Branch(branch) => branch.candidates(facing),
            Self::Climb(Some(dir)) => Candidates::from_axes(axis_order(*dir), None),
            Self::Climb(None) => Candidates::from_axes(axis_order(facing), None),
        }
    }

    /// A step along `dir` was accepted.
    fn accept(&mut self, dir: Dir) {
        match self {
            Self::Staircase(fixed) | Self::Climb(fixed) => *fixed = Some(dir),
            Self::Branch(branch) => branch.accept(dir),
        }
    }

    /// The body is about to walk onto the new feet cell of a `slope` step.
    fn walked(&mut self, slope: Slope) {
        if let Self::Branch(branch) = self {
            branch.walked(slope);
        }
    }

    /// How many blocks a call that started at `start_y` may dig, and the
    /// failure when it runs out.
    fn dig_budget(&self, start_y: i32) -> (u32, &'static str) {
        match self {
            Self::Staircase(_) => (MAX_DUGS, "no_block"),
            Self::Branch(branch) => (branch.dig_budget(start_y), "not_found"),
            Self::Climb(_) => (CLIMB_MAX_DUGS, NO_WAY_UP),
        }
    }

    /// The tick cap of the route, and the failure when it passes: the routes
    /// that dig through rock without a natural end (see [`BRANCH_MAX_TICKS`],
    /// [`CLIMB_MAX_TICKS`]).
    fn tick_cap(&self) -> Option<(u32, &'static str)> {
        match self {
            Self::Staircase(_) => None,
            Self::Branch(_) => Some((BRANCH_MAX_TICKS, "not_found")),
            Self::Climb(_) => Some((CLIMB_MAX_TICKS, NO_WAY_UP)),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Digger {
    /// The wanted block, as in `mine(block)`.
    wanted: String,
    /// Feet height at the first tick.
    start_y: Option<i32>,
    route: Route,
    dug: u32,
    /// Ticks the call has run; see [`BRANCH_MAX_TICKS`].
    ticks: u32,
    /// Level steps walked since the last accepted dig, of the branch route.
    /// Reset by every dig; see [`MAX_DRY_LEVEL_STEPS`].
    dry_level_steps: u32,
    target: Option<Target>,
    /// The feet cell the body is walking onto, and the ticks spent.
    walking: Option<(BlockPos, u32)>,
    hand: HandSelect,
    tool_waited: u32,
    settle_waited: u32,
    cooldown: u32,
    airborne: u32,
    /// Walks to another start cell made so far; see [`MAX_RELOCATIONS`].
    relocations: u32,
    /// The walk to another start cell, while one is under way.
    relocating: Option<Relocation>,
}

/// The walk `dig_down` makes to a start cell it can dig from.
#[derive(Debug)]
struct Relocation {
    walk: Move,
    ticks: u32,
    /// Why the cell the body stood on was refused: the failure kind if the
    /// walk does not get there.
    refusal: Refusal,
}

impl Digger {
    /// `dig_down(block)`.
    pub(crate) fn dig_down(wanted: String) -> Self {
        Self::new(wanted, Route::Staircase(None))
    }

    /// `branch_mine(block)`.
    pub(crate) fn branch_mine(wanted: String) -> Self {
        let branch = Branch::new(&wanted);
        Self::new(wanted, Route::Branch(branch))
    }

    /// `climb_out()`: no wanted block, the sky is the goal.
    pub(crate) fn climb_out() -> Self {
        Self::new(String::new(), Route::Climb(None))
    }

    fn new(wanted: String, route: Route) -> Self {
        Self {
            wanted,
            start_y: None,
            route,
            dug: 0,
            ticks: 0,
            dry_level_steps: 0,
            target: None,
            walking: None,
            hand: HandSelect::default(),
            tool_waited: 0,
            settle_waited: 0,
            cooldown: 0,
            airborne: 0,
            relocations: 0,
            relocating: None,
        }
    }

    /// Whether the call's goal is met: the wanted block is known within mining
    /// reach, or (for a climb) the body is out of the ground ([`is_out`]).
    fn reached(
        &self,
        ctx: &mut Ctx<'_>,
        feet: BlockPos,
        lookup: &impl Fn(BlockPos) -> Option<BlockState>,
    ) -> bool {
        if matches!(self.route, Route::Climb(_)) {
            return sky_above(ctx.perception, ctx.world) == Some(true) && is_out(feet, lookup);
        }
        nearest_known_block(
            ctx.perception,
            ctx.world,
            &self.wanted,
            praxion_rules::registry::EXPLORE_NEAR_RADIUS,
            &[],
        )
        .is_some()
    }

    /// The runner registered the pending dig intent.
    pub(crate) fn note_registered(&mut self) {
        if let Some(target) = &mut self.target {
            target.started = true;
        }
    }

    /// A dig receipt settled `confirmed`.
    pub(crate) fn note_settled(&mut self) {
        if let Some(target) = &mut self.target {
            target.settled = true;
        }
    }

    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        self.ticks = self.ticks.saturating_add(1);
        if self.target.is_some() {
            return self.tick_target(ctx);
        }
        let frame = idle(ctx);
        // The stop condition is checked between digs only: a dig in flight
        // always settles its receipt first.
        let feet = ctx.body.position().to_block_pos_floor();
        let world = ctx.world;
        let lookup = |pos: BlockPos| world.block_state(pos);
        if self.walking.is_none() && self.reached(ctx, feet, &lookup) {
            return (frame, None, Some(Outcome::Confirmed));
        }
        // The tick cap of the routes that have one, checked between digs like
        // the stop condition: a dig in flight settles its receipt first.
        if let Some((cap, kind)) = self.route.tick_cap()
            && self.ticks > cap
        {
            return (frame, None, Some(Outcome::Failed(kind.into())));
        }
        if self.relocating.is_some() {
            return self.tick_relocation(ctx, frame);
        }
        if self.cooldown > 0 {
            self.cooldown -= 1;
            return (frame, None, None);
        }
        if !ctx.body.on_ground() {
            self.airborne += 1;
            if self.airborne > AIRBORNE_TICKS {
                return (frame, None, Some(Outcome::Failed("unsafe_drop".into())));
            }
            // A climb jumps onto each stair: the forward key stays held in the
            // air until the body lands on it, or the jump falls back at the
            // foot of the next block (a body that lets go at the top of the
            // jump has no speed left to clear the edge).
            if matches!(self.route, Route::Climb(_))
                && let Some((tread, _)) = self.walking
            {
                return (pickup_frame(ctx, center(tread)), None, None);
            }
            return (frame, None, None);
        }
        self.airborne = 0;
        if let Some((tread, ticks)) = &mut self.walking {
            if feet == *tread {
                self.walking = None;
                return (frame, None, None);
            }
            *ticks += 1;
            if *ticks > WALK_TICKS {
                return (frame, None, Some(Outcome::Failed("stuck".into())));
            }
            return (pickup_frame(ctx, center(*tread)), None, None);
        }

        let start_y = *self.start_y.get_or_insert(feet.y);
        let slope = self.route.slope(feet.y);
        let min_y = self.route.min_y(start_y);
        let dirs = self.route.candidates(facing(ctx.body.look().y_rot()));
        let mut first_refusal = None;
        // The first axis the planner does not refuse. A climb looks on for an
        // open stair first: walking up its own descent costs no dig.
        let climbing = matches!(self.route, Route::Climb(_));
        let mut chosen: Option<(Dir, StairStep)> = None;
        for dir in dirs.iter().copied() {
            let step = self.route.plan(feet, dir, slope, min_y, &lookup);
            if let StairStep::Refuse(refusal) = step {
                first_refusal.get_or_insert(refusal);
                continue;
            }
            let open = matches!(step, StairStep::Walk(_));
            if chosen.is_none() || open {
                chosen = Some((dir, step));
            }
            if open || !climbing {
                break;
            }
        }
        if let Some((dir, step)) = chosen {
            self.route.accept(dir);
            match step {
                StairStep::Dig(cell) => {
                    let (budget, exhausted) = self.route.dig_budget(start_y);
                    if self.dug >= budget {
                        return (frame, None, Some(Outcome::Failed(exhausted.into())));
                    }
                    // A dig restarts the dry-step count: the route is making
                    // progress through rock, not cycling through open cells.
                    self.dry_level_steps = 0;
                    self.settle_waited = 0;
                    self.tool_waited = 0;
                    self.target = Some(Target {
                        cell,
                        started: false,
                        settled: false,
                        gravity_over: lookup(offset(cell, 0, 1, 0)).is_some_and(is_gravity),
                        waited: 0,
                    });
                }
                StairStep::Settle => {
                    self.settle_waited += 1;
                    if self.settle_waited > SETTLE_TICKS {
                        return (frame, None, Some(Outcome::Failed("unsafe_drop".into())));
                    }
                }
                StairStep::Walk(tread) => {
                    self.settle_waited = 0;
                    self.route.walked(slope);
                    // Only the branch route ever walks level (a staircase
                    // always descends), so this cap leaves `dig_down` alone.
                    if slope == Slope::Level {
                        self.dry_level_steps += 1;
                        if self.dry_level_steps >= MAX_DRY_LEVEL_STEPS {
                            return (frame, None, Some(Outcome::Failed("stuck".into())));
                        }
                    }
                    self.walking = Some((tread, 0));
                }
                StairStep::Refuse(_) => unreachable!("refusals continue above"),
            }
            return (frame, None, None);
        }
        // Every axis refused. A person who cannot dig where they stand steps
        // a few blocks aside and digs there; they do not walk the surface for
        // minutes (run B, 2026-11-12: three `unsafe_drop` refusals at one spot
        // in as many ticks, then three minutes of exploring, then a stair dug
        // at the first cell of the new ground). Only a staircase that has dug
        // nothing yet steps aside, and only for a refusal another cell can
        // cure: a drop or water here says nothing about the ground five blocks
        // away, `too_deep` and bedrock say the same everywhere.
        if let Some(refusal) = first_refusal
            && matches!(refusal, Refusal::UnsafeDrop | Refusal::Fluid)
            && matches!(self.route, Route::Staircase(_))
            && self.dug == 0
            && self.relocations < MAX_RELOCATIONS
            && let Some(cell) = relocation_cell(feet, &lookup)
        {
            log::debug!(
                "dig_down: {:?} refused at {feet:?}; stepping aside to {cell:?}",
                refusal.kind()
            );
            self.relocations += 1;
            self.relocating = Some(Relocation {
                walk: Move::to_cell(cell),
                ticks: 0,
                refusal,
            });
            return (frame, None, None);
        }
        let kind = if climbing {
            NO_WAY_UP
        } else {
            first_refusal.map_or("no_block", Refusal::kind)
        };
        (frame, None, Some(Outcome::Failed(kind.into())))
    }

    /// One tick of the walk to another start cell. Arriving starts the
    /// staircase over from there (depth is measured from the new cell); a walk
    /// that fails or runs out of ticks fails the call with the refusal that
    /// sent the body off.
    fn tick_relocation(
        &mut self,
        ctx: &mut Ctx<'_>,
        idle_frame: ControlFrame,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let relocation = self.relocating.as_mut().expect("checked by the caller");
        relocation.ticks += 1;
        let refusal = relocation.refusal;
        if relocation.ticks > RELOCATE_TICKS {
            return (idle_frame, None, Some(Outcome::Failed(refusal.kind().into())));
        }
        // No movement intent: the walk is one step of this primitive's own
        // work, and the dig receipts stay the only receipts of the call.
        let (frame, _, outcome) = relocation.walk.tick(ctx, true);
        match outcome {
            None => (frame, None, None),
            Some(Outcome::Confirmed) => {
                self.relocating = None;
                self.start_y = None;
                self.route = Route::Staircase(None);
                (frame, None, None)
            }
            Some(_) => (idle_frame, None, Some(Outcome::Failed(refusal.kind().into()))),
        }
    }

    /// Break the current target: tool, aim, click until its receipt settles.
    fn tick_target(
        &mut self,
        ctx: &mut Ctx<'_>,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        let target = self.target.as_mut().expect("checked by the caller");
        let cell = target.cell;
        let gone = block_gone(ctx.world.block_state(cell));
        match (gone, target.started, target.settled) {
            // It vanished before the runner registered a dig: select again.
            (true, false, _) => {
                self.target = None;
                return (frame, None, None);
            }
            // Broken and settled: one more block dug.
            (true, true, true) => {
                self.dug += 1;
                if target.gravity_over {
                    self.cooldown = GRAVITY_COOLDOWN_TICKS;
                }
                self.target = None;
                return (frame, None, None);
            }
            // Broken; stop clicking (the next block must not be hit) until
            // the receipt settles.
            (true, true, false) => return (frame, None, None),
            // The receipt settled but the cell holds a block again (falling
            // sand or gravel): dig it under a new receipt.
            (false, true, true) => {
                target.started = false;
                target.settled = false;
                target.waited = 0;
                self.tool_waited = 0;
            }
            (false, _, _) => {}
        }
        let target = self.target.as_mut().expect("still set");
        if !target.started {
            target.waited += 1;
            if target.waited > TARGET_WAIT_TICKS {
                return (frame, None, Some(Outcome::Failed("stuck".into())));
            }
            if let Some(state) = ctx.world.block_state(cell) {
                match hold_tool(
                    ctx,
                    state,
                    &mut self.hand,
                    &mut self.tool_waited,
                    &mut frame,
                ) {
                    ToolStep::Ready => {}
                    ToolStep::Pending => return (frame, None, None),
                    ToolStep::Missing => {
                        return (frame, None, Some(Outcome::Failed("needs_tool".into())));
                    }
                }
            }
        }
        let (look, aligned) = aim_precise(ctx, center(cell));
        frame.look = look;
        if !aligned {
            return (frame, None, None);
        }
        frame.clicks.push(Click::Attack);
        // Keep sending the spec until the runner registers it.
        let pending = (!target.started).then_some(IntentSpec::Dig {
            pos: cell,
            window: DIG_WINDOW,
        });
        (frame, pending, None)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use azalea_registry::builtin::BlockKind;

    use super::*;

    /// A tiny world: a default block per height plus overrides.
    struct Map {
        cells: HashMap<BlockPos, BlockKind>,
        /// `(top_y, kind)` layers from the surface down; column independent.
        layers: Vec<(i32, BlockKind)>,
    }

    impl Map {
        /// Dirt up to `surface`, stone from `stone_top` down, air above.
        fn flat(surface: i32, stone_top: i32) -> Self {
            Self {
                cells: HashMap::new(),
                layers: vec![(surface, BlockKind::Dirt), (stone_top, BlockKind::Stone)],
            }
        }

        fn kind(&self, pos: BlockPos) -> BlockKind {
            if let Some(kind) = self.cells.get(&pos) {
                return *kind;
            }
            // Layers are listed top-down; the last one reaching `pos.y` wins.
            let mut found = BlockKind::Air;
            for &(top, kind) in &self.layers {
                if pos.y <= top {
                    found = kind;
                }
            }
            found
        }

        fn lookup(&self) -> impl Fn(BlockPos) -> Option<BlockState> + '_ {
            |pos| Some(BlockState::from(self.kind(pos)))
        }

        fn set(&mut self, pos: BlockPos, kind: BlockKind) {
            self.cells.insert(pos, kind);
        }
    }

    const EAST: Dir = (1, 0);
    /// The lowest feet height of a `dig_down` that started at y=64.
    const DOWN_FLOOR: i32 = 64 - MAX_DEPTH;

    /// Run the planner like the primitive does: dig every `Dig`, walk every
    /// `Walk`, stop on a refusal, or when `stop` holds for the feet cell.
    fn descend(
        map: &mut Map,
        mut feet: BlockPos,
        dir: Dir,
        stop: impl Fn(BlockPos) -> bool,
    ) -> (BlockPos, Vec<BlockPos>, Option<Refusal>) {
        let start_y = feet.y;
        let mut dug = Vec::new();
        for _ in 0..200 {
            if stop(feet) {
                return (feet, dug, None);
            }
            let step = plan_step(feet, dir, Slope::Down, start_y - MAX_DEPTH, &map.lookup());
            match step {
                StairStep::Dig(cell) => {
                    dug.push(cell);
                    map.set(cell, BlockKind::Air);
                }
                StairStep::Walk(tread) => feet = tread,
                StairStep::Settle => panic!("unexpected settle"),
                StairStep::Refuse(refusal) => return (feet, dug, Some(refusal)),
            }
        }
        panic!("the staircase did not end");
    }

    #[test]
    fn flat_dirt_over_stone_is_dug_down_to_the_stone() {
        // Surface (dirt) top at y=63, stone from y=59 down: stone is 4 under
        // the ground. The body stands at feet y=64.
        let mut map = Map::flat(63, 59);
        let start = BlockPos::new(0, 64, 0);
        // The stone is in reach once the feet are 2 above it.
        let (feet, dug, refusal) = descend(&mut map, start, EAST, |feet| feet.y <= 61);
        assert_eq!(refusal, None);
        assert_eq!(feet, BlockPos::new(3, 61, 0), "three stairs down");
        // Every dug cell is a distinct block; the cell under the start never.
        let mut unique = dug.clone();
        unique.sort_by_key(|pos| (pos.x, pos.y, pos.z));
        unique.dedup();
        assert_eq!(unique.len(), dug.len(), "one dig per block: {dug:?}");
        assert!(!dug.contains(&BlockPos::new(0, 63, 0)), "the support stays");
        // The stairs are walkable: head room and the floor hold at each foot.
        for step in 1..=3 {
            let x = step;
            let y = 64 - step;
            assert_eq!(map.kind(BlockPos::new(x, y, 0)), BlockKind::Air);
            assert_eq!(map.kind(BlockPos::new(x, y + 1, 0)), BlockKind::Air);
            assert_ne!(map.kind(BlockPos::new(x, y - 1, 0)), BlockKind::Air);
        }
    }

    #[test]
    fn the_first_step_digs_three_cells_top_down() {
        let map = Map::flat(63, 59);
        let feet = BlockPos::new(0, 64, 0);
        let step = plan_step(feet, EAST, Slope::Down, DOWN_FLOOR, &map.lookup());
        // Surface terrain: the clearance (y=65) and head (y=64) cells are air,
        // so the first dig is the dirt at the new feet cell.
        assert_eq!(step, StairStep::Dig(BlockPos::new(1, 63, 0)));
    }

    #[test]
    fn water_next_to_a_cell_refuses_the_step() {
        let mut map = Map::flat(63, 59);
        map.set(BlockPos::new(1, 63, 1), BlockKind::Water);
        let step = plan_step(
            BlockPos::new(0, 64, 0),
            EAST,
            Slope::Down,
            DOWN_FLOOR,
            &map.lookup(),
        );
        assert_eq!(step, StairStep::Refuse(Refusal::Fluid));
    }

    #[test]
    fn water_above_the_stair_refuses_the_step() {
        let mut map = Map::flat(63, 59);
        map.set(BlockPos::new(1, 64, 0), BlockKind::Water);
        let step = plan_step(
            BlockPos::new(0, 64, 0),
            EAST,
            Slope::Down,
            DOWN_FLOOR,
            &map.lookup(),
        );
        assert_eq!(step, StairStep::Refuse(Refusal::Fluid), "a water cell");
    }

    #[test]
    fn lava_below_the_new_stair_refuses_the_step() {
        let mut map = Map::flat(63, 59);
        map.set(BlockPos::new(1, 62, 0), BlockKind::Lava);
        let step = plan_step(
            BlockPos::new(0, 64, 0),
            EAST,
            Slope::Down,
            DOWN_FLOOR,
            &map.lookup(),
        );
        assert_eq!(step, StairStep::Refuse(Refusal::Fluid), "lava is the floor");
    }

    /// A dirt flat with a hole under every neighbour of the start cell: each
    /// of the four first steps lands on a floor that is air.
    fn ridge_map() -> Map {
        let mut map = Map::flat(63, 59);
        for dx in -1..=1 {
            for dz in -1..=1 {
                for y in 60..=62 {
                    map.set(BlockPos::new(dx, y, dz), BlockKind::Air);
                }
            }
        }
        map
    }

    #[test]
    fn a_start_cell_where_every_step_drops_is_refused_on_all_four_axes() {
        let map = ridge_map();
        let feet = BlockPos::new(0, 64, 0);
        for dir in axis_order(EAST) {
            assert_eq!(
                plan_step(feet, dir, Slope::Down, DOWN_FLOOR, &map.lookup()),
                StairStep::Refuse(Refusal::UnsafeDrop),
                "{dir:?}"
            );
        }
    }

    #[test]
    fn the_relocation_cell_is_the_nearest_standable_one_with_a_step_that_digs() {
        let map = ridge_map();
        let feet = BlockPos::new(0, 64, 0);
        let cell = relocation_cell(feet, &map.lookup()).expect("a cell next door");
        // The hole is one block wide on every side, so the cell one block west
        // (the first of the nearest, by position) stands on dirt and its own
        // westward step lands on dirt two blocks from the hole.
        assert_eq!(cell, BlockPos::new(-1, 64, 0));
        let step = (0..4)
            .map(|i| axis_order(EAST)[i])
            .map(|dir| plan_step(cell, dir, Slope::Down, cell.y - MAX_DEPTH, &map.lookup()))
            .any(|step| !matches!(step, StairStep::Refuse(_)));
        assert!(step, "{cell:?} digs");
        // Deterministic: asked again, the same cell.
        assert_eq!(relocation_cell(feet, &map.lookup()), Some(cell));
    }

    #[test]
    fn a_start_with_no_diggable_cell_around_has_no_relocation() {
        // Water everywhere within five blocks: every cell is a fluid refusal.
        let mut map = Map::flat(63, 59);
        for dx in -6..=6 {
            for dz in -6..=6 {
                map.set(BlockPos::new(dx, 63, dz), BlockKind::Water);
            }
        }
        assert_eq!(relocation_cell(BlockPos::new(0, 64, 0), &map.lookup()), None);
        // Unloaded ground is no evidence of a safe cell either.
        let unloaded = |_: BlockPos| None;
        assert_eq!(relocation_cell(BlockPos::new(0, 64, 0), &unloaded), None);
    }

    #[test]
    fn the_relocation_stays_inside_its_radius_and_on_a_floor() {
        let mut map = Map::flat(63, 59);
        // Only one good column exists, six blocks off: out of range.
        for dx in -8..=8 {
            for dz in -8..=8 {
                if (dx, dz) != (6, 0) {
                    map.set(BlockPos::new(dx, 63, dz), BlockKind::Lava);
                }
            }
        }
        assert_eq!(relocation_cell(BlockPos::new(0, 64, 0), &map.lookup()), None);
    }

    #[test]
    fn a_hole_under_the_new_stair_is_an_unsafe_drop() {
        let mut map = Map::flat(63, 59);
        // The new feet cell is open and nothing under it for two blocks.
        for y in 61..=63 {
            map.set(BlockPos::new(1, y, 0), BlockKind::Air);
        }
        let step = plan_step(
            BlockPos::new(0, 64, 0),
            EAST,
            Slope::Down,
            DOWN_FLOOR,
            &map.lookup(),
        );
        assert_eq!(step, StairStep::Refuse(Refusal::UnsafeDrop));
    }

    #[test]
    fn an_unloaded_cell_is_an_unsafe_drop() {
        let map = Map::flat(63, 59);
        let lookup = |pos: BlockPos| (pos.x != 1).then(|| BlockState::from(map.kind(pos)));
        let step = plan_step(
            BlockPos::new(0, 64, 0),
            EAST,
            Slope::Down,
            DOWN_FLOOR,
            &lookup,
        );
        assert_eq!(step, StairStep::Refuse(Refusal::UnsafeDrop));
    }

    #[test]
    fn bedrock_cannot_be_dug() {
        let mut map = Map::flat(63, 59);
        map.set(BlockPos::new(1, 63, 0), BlockKind::Bedrock);
        let step = plan_step(
            BlockPos::new(0, 64, 0),
            EAST,
            Slope::Down,
            DOWN_FLOOR,
            &map.lookup(),
        );
        assert_eq!(step, StairStep::Refuse(Refusal::NoBlock));
    }

    #[test]
    fn the_depth_cap_stops_the_staircase() {
        // Dirt all the way down: nothing but the cap ends the staircase.
        let mut map = Map::flat(63, -100);
        let start = BlockPos::new(0, 64, 0);
        let (feet, _, refusal) = descend(&mut map, start, EAST, |_| false);
        assert_eq!(refusal, Some(Refusal::TooDeep));
        assert_eq!(feet.y, 64 - MAX_DEPTH, "twelve blocks below the start");
        // Three cells per stair at most: the dig cap covers a full descent.
        assert!(3 * MAX_DEPTH as u32 <= MAX_DUGS);
    }

    #[test]
    fn a_sand_column_is_dug_top_down_and_waited_for() {
        let mut map = Map::flat(63, 59);
        let feet = BlockPos::new(0, 64, 0);
        // Sand stands over the clearance cell of the first stair (y=66) and
        // the clearance cell itself (y=65) is sand too.
        map.set(BlockPos::new(1, 65, 0), BlockKind::Sand);
        map.set(BlockPos::new(1, 66, 0), BlockKind::Sand);
        // The top of the stair is dug first, so the sand above cannot fall
        // into a cell that is still closed below it.
        assert_eq!(
            plan_step(feet, EAST, Slope::Down, DOWN_FLOOR, &map.lookup()),
            StairStep::Dig(BlockPos::new(1, 65, 0))
        );
        // After the sand at y=65 is dug, the one at y=66 drops into it: the
        // cell is closed again and is dug again (a new receipt).
        map.set(BlockPos::new(1, 65, 0), BlockKind::Air);
        map.set(BlockPos::new(1, 63, 0), BlockKind::Air);
        map.set(BlockPos::new(1, 64, 0), BlockKind::Air);
        // Everything is open but the sand still hangs over the column.
        assert_eq!(
            plan_step(feet, EAST, Slope::Down, DOWN_FLOOR, &map.lookup()),
            StairStep::Settle
        );
        map.set(BlockPos::new(1, 66, 0), BlockKind::Air);
        map.set(BlockPos::new(1, 65, 0), BlockKind::Sand);
        assert_eq!(
            plan_step(feet, EAST, Slope::Down, DOWN_FLOOR, &map.lookup()),
            StairStep::Dig(BlockPos::new(1, 65, 0)),
            "the refill is dug again"
        );
        map.set(BlockPos::new(1, 65, 0), BlockKind::Air);
        assert_eq!(
            plan_step(feet, EAST, Slope::Down, DOWN_FLOOR, &map.lookup()),
            StairStep::Walk(BlockPos::new(1, 63, 0))
        );
    }

    /// Stone from y=63 down with a one-wide, two-high tunnel along +x at y=64
    /// and y=65 (feet y=64) from x=1 to `end`.
    fn tunnel_map(end: i32) -> Map {
        let mut map = Map::flat(70, 70);
        for x in 1..=end {
            map.set(BlockPos::new(x, 64, 0), BlockKind::Air);
            map.set(BlockPos::new(x, 65, 0), BlockKind::Air);
        }
        map
    }

    fn level(map: &Map, feet: BlockPos) -> StairStep {
        plan_step(feet, EAST, Slope::Level, i32::MIN, &map.lookup())
    }

    #[test]
    fn a_level_step_digs_the_head_cell_then_the_feet_cell() {
        let map = Map::flat(70, 70);
        let feet = BlockPos::new(0, 64, 0);
        assert_eq!(
            level(&map, feet),
            StairStep::Dig(BlockPos::new(1, 65, 0)),
            "top-down: head first"
        );
        let mut map = map;
        map.set(BlockPos::new(1, 65, 0), BlockKind::Air);
        assert_eq!(level(&map, feet), StairStep::Dig(BlockPos::new(1, 64, 0)));
        map.set(BlockPos::new(1, 64, 0), BlockKind::Air);
        assert_eq!(level(&map, feet), StairStep::Walk(BlockPos::new(1, 64, 0)));
    }

    #[test]
    fn a_level_step_never_digs_the_floor() {
        let mut map = Map::flat(70, 70);
        let feet = BlockPos::new(0, 64, 0);
        let mut dug = Vec::new();
        for _ in 0..4 {
            match level(&map, feet) {
                StairStep::Dig(cell) => {
                    dug.push(cell);
                    map.set(cell, BlockKind::Air);
                }
                StairStep::Walk(_) => break,
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(dug.len(), 2, "only head room and the feet cell");
        assert!(dug.iter().all(|cell| cell.y >= 64));
    }

    #[test]
    fn an_open_tunnel_is_walked_without_digging() {
        let map = tunnel_map(3);
        assert_eq!(
            level(&map, BlockPos::new(0, 64, 0)),
            StairStep::Walk(BlockPos::new(1, 64, 0))
        );
        assert_eq!(
            level(&map, BlockPos::new(2, 64, 0)),
            StairStep::Walk(BlockPos::new(3, 64, 0))
        );
    }

    #[test]
    fn a_level_step_needs_a_solid_floor() {
        let mut map = tunnel_map(1);
        // A hole under the next tread: a pit, not a floor.
        map.set(BlockPos::new(2, 64, 0), BlockKind::Air);
        map.set(BlockPos::new(2, 65, 0), BlockKind::Air);
        map.set(BlockPos::new(2, 63, 0), BlockKind::Air);
        assert_eq!(
            level(&map, BlockPos::new(1, 64, 0)),
            StairStep::Refuse(Refusal::UnsafeDrop)
        );
    }

    #[test]
    fn a_level_step_refuses_fluid() {
        let feet = BlockPos::new(0, 64, 0);
        // Lava is the floor.
        let mut map = Map::flat(70, 70);
        map.set(BlockPos::new(1, 63, 0), BlockKind::Lava);
        assert_eq!(level(&map, feet), StairStep::Refuse(Refusal::Fluid));
        // Water sits beside the feet cell and would flood it once dug.
        let mut map = Map::flat(70, 70);
        map.set(BlockPos::new(1, 64, 1), BlockKind::Water);
        assert_eq!(level(&map, feet), StairStep::Refuse(Refusal::Fluid));
        // Water above the head cell.
        let mut map = Map::flat(70, 70);
        map.set(BlockPos::new(1, 66, 0), BlockKind::Water);
        assert_eq!(level(&map, feet), StairStep::Refuse(Refusal::Fluid));
    }

    #[test]
    fn a_level_step_into_bedrock_is_refused() {
        let mut map = Map::flat(70, 70);
        map.set(BlockPos::new(1, 64, 0), BlockKind::Bedrock);
        assert_eq!(
            level(&map, BlockPos::new(0, 64, 0)),
            StairStep::Refuse(Refusal::NoBlock)
        );
    }

    #[test]
    fn gravel_over_a_level_step_is_waited_for() {
        let mut map = tunnel_map(1);
        map.set(BlockPos::new(2, 64, 0), BlockKind::Air);
        map.set(BlockPos::new(2, 65, 0), BlockKind::Air);
        map.set(BlockPos::new(2, 66, 0), BlockKind::Gravel);
        assert_eq!(level(&map, BlockPos::new(1, 64, 0)), StairStep::Settle);
    }

    #[test]
    fn the_floor_height_bounds_a_step() {
        let map = Map::flat(70, 70);
        let step = plan_step(
            BlockPos::new(0, 64, 0),
            EAST,
            Slope::Level,
            65,
            &map.lookup(),
        );
        assert_eq!(step, StairStep::Refuse(Refusal::TooDeep));
    }

    /// A stone world to `top` with the pocket the body stands in at `feet`
    /// (feet and head cell open), and a way to read a step up from there.
    fn climb_map(top: i32, feet: BlockPos) -> Map {
        let mut map = Map {
            cells: HashMap::new(),
            layers: vec![(top, BlockKind::Stone)],
        };
        map.set(feet, BlockKind::Air);
        map.set(BlockPos::new(feet.x, feet.y + 1, feet.z), BlockKind::Air);
        map
    }

    fn climb(map: &Map, feet: BlockPos) -> StairStep {
        plan_climb(feet, EAST, &map.lookup())
    }

    #[test]
    fn a_climb_digs_the_head_room_over_the_stair_first_then_the_clearance_then_the_tread() {
        let feet = BlockPos::new(0, 56, 0);
        let mut map = climb_map(63, feet);
        let mut order = Vec::new();
        loop {
            match climb(&map, feet) {
                StairStep::Dig(cell) => {
                    order.push(cell);
                    map.set(cell, BlockKind::Air);
                }
                StairStep::Walk(tread) => {
                    assert_eq!(tread, BlockPos::new(1, 57, 0), "one block up, one along");
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        // Top-down: the two y+2 cells (head room of the new stair, clearance
        // over the body), then the tread. The floor beside the feet is never
        // dug.
        assert_eq!(
            order,
            [
                BlockPos::new(1, 58, 0),
                BlockPos::new(0, 58, 0),
                BlockPos::new(1, 57, 0),
            ]
        );
        assert!(!order.contains(&BlockPos::new(1, 56, 0)), "the floor stays");
    }

    #[test]
    fn an_open_stair_is_walked_without_a_dig() {
        let feet = BlockPos::new(0, 56, 0);
        let mut map = climb_map(63, feet);
        for cell in [
            BlockPos::new(1, 58, 0),
            BlockPos::new(0, 58, 0),
            BlockPos::new(1, 57, 0),
        ] {
            map.set(cell, BlockKind::Air);
        }
        assert_eq!(climb(&map, feet), StairStep::Walk(BlockPos::new(1, 57, 0)));
    }

    #[test]
    fn a_climb_needs_a_solid_floor_beside_the_feet() {
        let feet = BlockPos::new(0, 56, 0);
        for (kind, refusal) in [
            (BlockKind::Air, Refusal::UnsafeDrop),
            (BlockKind::Lava, Refusal::Fluid),
            (BlockKind::Water, Refusal::Fluid),
            (BlockKind::MagmaBlock, Refusal::UnsafeDrop),
        ] {
            let mut map = climb_map(63, feet);
            map.set(BlockPos::new(1, 56, 0), kind);
            assert_eq!(climb(&map, feet), StairStep::Refuse(refusal), "{kind:?}");
        }
        // An unloaded floor is not known to hold.
        let lookup = |pos: BlockPos| (pos != BlockPos::new(1, 56, 0)).then(|| BlockState::from(BlockKind::Stone));
        assert_eq!(
            plan_climb(feet, EAST, &lookup),
            StairStep::Refuse(Refusal::UnsafeDrop)
        );
    }

    #[test]
    fn a_climb_never_opens_a_cell_under_or_beside_fluid() {
        let feet = BlockPos::new(0, 56, 0);
        // Water in a cell of the stair itself.
        let mut map = climb_map(63, feet);
        map.set(BlockPos::new(1, 58, 0), BlockKind::Water);
        assert_eq!(climb(&map, feet), StairStep::Refuse(Refusal::Fluid));
        // Water or lava over the first cell to open (a lake over the roof
        // falls into the stair), and beside the second.
        for fluid in [BlockKind::Water, BlockKind::Lava] {
            let mut over = climb_map(63, feet);
            over.set(BlockPos::new(1, 59, 0), fluid);
            assert_eq!(climb(&over, feet), StairStep::Refuse(Refusal::Fluid), "{fluid:?} over");
            let mut beside = climb_map(63, feet);
            beside.set(BlockPos::new(1, 58, 1), fluid);
            assert_eq!(climb(&beside, feet), StairStep::Refuse(Refusal::Fluid), "{fluid:?} beside");
        }
    }

    #[test]
    fn a_climb_never_opens_a_cell_under_a_block_that_falls() {
        let feet = BlockPos::new(0, 56, 0);
        for kind in [BlockKind::Sand, BlockKind::Gravel, BlockKind::RedSand] {
            let mut map = climb_map(63, feet);
            map.set(BlockPos::new(1, 59, 0), kind);
            assert_eq!(
                climb(&map, feet),
                StairStep::Refuse(Refusal::UnsafeDrop),
                "{kind:?} over the first cell"
            );
        }
        // Once the first cell is open the second is checked on its own: gravel
        // over the clearance cell refuses the step half way.
        let mut map = climb_map(63, feet);
        map.set(BlockPos::new(1, 58, 0), BlockKind::Air);
        map.set(BlockPos::new(0, 59, 0), BlockKind::Gravel);
        assert_eq!(climb(&map, feet), StairStep::Refuse(Refusal::UnsafeDrop));
        // Sand that is the cell to dig, with solid rock over it, is dug: it
        // falls nowhere.
        let mut map = climb_map(63, feet);
        map.set(BlockPos::new(1, 58, 0), BlockKind::Sand);
        assert_eq!(climb(&map, feet), StairStep::Dig(BlockPos::new(1, 58, 0)));
        // An unloaded cell over the one to open is not known to be safe.
        let lookup = |pos: BlockPos| {
            (pos != BlockPos::new(1, 59, 0)).then(|| BlockState::from(BlockKind::Stone))
        };
        assert_eq!(
            plan_climb(feet, EAST, &lookup),
            StairStep::Refuse(Refusal::UnsafeDrop)
        );
    }

    #[test]
    fn a_climb_refuses_bedrock_and_hazards_in_the_stair() {
        let feet = BlockPos::new(0, 56, 0);
        let mut map = climb_map(63, feet);
        map.set(BlockPos::new(0, 58, 0), BlockKind::Bedrock);
        map.set(BlockPos::new(1, 58, 0), BlockKind::Air);
        assert_eq!(climb(&map, feet), StairStep::Refuse(Refusal::NoBlock));
        let mut map = climb_map(63, feet);
        map.set(BlockPos::new(1, 57, 0), BlockKind::MagmaBlock);
        assert_eq!(climb(&map, feet), StairStep::Refuse(Refusal::UnsafeDrop));
    }

    #[test]
    fn gravity_blocks_are_recognised() {
        for kind in [BlockKind::Sand, BlockKind::RedSand, BlockKind::Gravel] {
            assert!(is_gravity(BlockState::from(kind)), "{kind:?}");
        }
        assert!(is_gravity(BlockState::from(BlockKind::WhiteConcretePowder)));
        assert!(!is_gravity(BlockState::from(BlockKind::Dirt)));
        assert!(!is_gravity(BlockState::from(BlockKind::Sandstone)));
    }

    #[test]
    fn the_facing_axis_comes_first() {
        // Yaw 0 faces south (+z), 90 west (-x), 180 north, -90 east (+x).
        assert_eq!(facing(0.0), (0, 1));
        assert_eq!(facing(90.0), (-1, 0));
        assert_eq!(facing(180.0), (0, -1));
        assert_eq!(facing(-90.0), (1, 0));
        assert_eq!(facing(30.0), (0, 1), "rounded to the nearest axis");
        let order = axis_order((1, 0));
        assert_eq!(order[0], (1, 0));
        let mut all = order.to_vec();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 4, "four distinct axes");
    }
}

/// `branch_mine` driven the way production drives it: the primitive is started
/// through [`Runner`], one persistent [`Body`] is stepped with the frames the
/// primitive emits, and every dig intent is registered and settled by the
/// runner, as the body driver does.
///
/// The `Digger` lives inside the runner and `Runner` hands back only the
/// outcome, so these tests pin its counters through what it produces (the dig
/// intents and the feet path), never by reading its private fields.
#[cfg(test)]
mod digger_tests {
    use std::collections::HashSet;

    use azalea_core::position::Vec3;
    use azalea_registry::builtin::{BlockKind, ItemKind};
    use praxion_headless::{Body, Click, ControlFrame, Held, Look};
    use praxion_interpreter::{Outcome, StartAction};
    use praxion_perception::{BlockFilter, BlockRegion, Caller, Perception};
    use praxion_planner::Personality;
    use praxion_rules::registry::EXPLORE_NEAR_RADIUS;
    use praxion_rules::ArgValue;
    use praxion_world::World;

    use super::*;
    use crate::runner::Runner;
    use crate::test_world::{hold, move_body, set_block, world_with};

    const EAST_YAW: f32 = -90.0;

    /// What one driven call did.
    struct Run {
        /// Every feet cell the body stood on, in order, the start first.
        path: Vec<BlockPos>,
        /// Every cell a dig intent broke, in order.
        dug: Vec<BlockPos>,
        outcome: Option<Outcome>,
        /// The perception the call read, for the confirm-side assertions.
        perception: Perception,
    }

    /// What the server does with a registered dig. `Confirmed` is the normal
    /// path; `Unsettled` leaves the receipt hanging, the state production must
    /// bound outside the primitive.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Receipt {
        Confirmed,
        Unsettled,
    }

    /// A runner with one `branch_mine(ore)` started through `Runner::start`, as
    /// the interpreter starts it.
    fn branch_mine(ore: &str) -> Runner {
        let mut runner = Runner::new();
        runner
            .start(&StartAction {
                primitive: "branch_mine".to_owned(),
                args: vec![ArgValue::Name(ore.to_owned())],
            })
            .expect("branch_mine starts");
        runner
    }

    fn feet_position(feet: BlockPos) -> Vec3 {
        Vec3::new(
            f64::from(feet.x) + 0.5,
            f64::from(feet.y),
            f64::from(feet.z) + 0.5,
        )
    }

    /// Set the body's look through the frame path, like the actuator does.
    fn set_body_look(body: &mut Body, yaw: f32, pitch: f32) {
        let mut frame = ControlFrame::new(0, body.generation(), Held::NONE);
        frame.look = Look::Aim { yaw, pitch };
        frame.apply(body, 0).expect("a look frame applies");
    }

    /// One driven call: where the body starts, how it looks, what the server
    /// does with its receipts, which block the stand-in observer watches, and
    /// the tick cap.
    struct Call {
        start: BlockPos,
        look: (f32, f32),
        receipt: Receipt,
        watch: Option<BlockKind>,
        max_ticks: u64,
        /// Run the background sweep each tick, as production does, so the
        /// known map the planner walks over fills in.
        sweep: bool,
    }

    impl Call {
        /// A call east from `start` whose receipts confirm, as in production.
        fn new(start: BlockPos) -> Self {
            Self {
                start,
                look: (EAST_YAW, 0.0),
                receipt: Receipt::Confirmed,
                watch: None,
                max_ticks: 1500,
                sweep: false,
            }
        }
    }

    /// Drive the runner's active primitive like the body driver and the server
    /// do, for the call's tick cap or until `stop` sees the feet path:
    ///
    /// - one persistent [`Body`] from `start`, moved only by stepping the
    ///   physics with the held controls of each frame the primitive emits; its
    ///   real position is the position the server and perception see;
    /// - the frame applied and the body stepped once per tick, as the actuator
    ///   does;
    /// - `watch` answers the wanted block once per tick for a region around the
    ///   eye, the deterministic stand-in for the background sweep (the sweep is
    ///   not what these tests guard); a refused query fails the test;
    /// - a dig intent is registered through `Runner::registered`, the server
    ///   turns its cell air, and `Runner::settle(Confirmed)` forwards that only
    ///   — `Runner::settle` is the only writer of `Digger::note_settled`, and it
    ///   does so solely for `Confirmed`, as the driver does. With
    ///   [`Receipt::Unsettled`] the settle is never sent.
    fn drive(
        world: &mut World,
        runner: &mut Runner,
        call: Call,
        stop: impl Fn(&[BlockPos]) -> bool,
    ) -> Run {
        let Call {
            start,
            look,
            receipt,
            watch,
            max_ticks,
            sweep,
        } = call;
        let mut body = Body::new(feet_position(start));
        set_body_look(&mut body, look.0, look.1);
        let mut perception = Perception::new(praxion_perception::Config::default());
        let mut path = vec![start];
        let mut dug = Vec::new();
        let mut outcome = None;
        let mut intent = 0;
        for tick in 0..max_ticks {
            // The server, and perception with it, follows the body physics put
            // at this position.
            move_body(world, body.position());
            perception.begin_tick(world);
            if sweep {
                perception.sweep(world);
            }
            if let Some(kind) = watch
                && let Some(eye) = perception.eye_cell(world)
            {
                let region = BlockRegion::around(eye, EXPLORE_NEAR_RADIUS);
                let answer =
                    perception.blocks_in(Caller::Other, world, region, BlockFilter::Kind(kind));
                assert!(
                    answer.value().is_some(),
                    "the watch query was refused: {answer:?}"
                );
            }
            let result = {
                let mut ctx = Ctx {
                    tick,
                    body: &body,
                    world: &*world,
                    perception: &mut perception,
                    style: Personality::default(),
                };
                runner.tick(&mut ctx)
            };
            let frame = &result.frame;
            // The actuator applies the frame and steps the physics once.
            frame.apply(&mut body, tick).expect("the digger frame applies");
            body.step(&frame.held, world);
            if let Some(pending) = &result.pending {
                let IntentSpec::Dig { pos, .. } = pending else {
                    panic!("the digger emitted a non-dig intent");
                };
                let pos = *pos;
                assert!(
                    frame.clicks.contains(&Click::Attack),
                    "a dig intent carries its click"
                );
                dug.push(pos);
                intent += 1;
                runner.registered(intent);
                set_block(world, pos, BlockState::from(BlockKind::Air));
                if receipt == Receipt::Confirmed {
                    runner.settle(Outcome::Confirmed);
                }
            }
            // A cell the body really stood in: grounded, with solid support
            // beneath it. The tick it steps over a just-dug cell it is still
            // flagged `on_ground` while hanging over the hole, and that cell is
            // not a floor it stood on.
            if body.on_ground() {
                let feet = body.position().to_block_pos_floor();
                let support = BlockPos::new(feet.x, feet.y - 1, feet.z);
                let standing = world
                    .block_state(support)
                    .is_some_and(praxion_planner::blocks::is_solid);
                if standing && path.last() != Some(&feet) {
                    path.push(feet);
                }
            }
            if let Some(result) = result.outcome {
                outcome = Some(result);
                break;
            }
            if stop(&path) {
                break;
            }
        }
        Run {
            path,
            dug,
            outcome,
            perception,
        }
    }

    fn stone_below(top: i32) -> impl Fn(BlockPos) -> BlockState {
        move |pos| {
            if pos.y <= top {
                BlockState::from(BlockKind::Stone)
            } else {
                BlockState::from(BlockKind::Air)
            }
        }
    }

    /// A runner with one `dig_down(block)` started through `Runner::start`.
    fn dig_down_call(block: &str) -> Runner {
        let mut runner = Runner::new();
        runner
            .start(&StartAction {
                primitive: "dig_down".to_owned(),
                args: vec![ArgValue::Name(block.to_owned())],
            })
            .expect("dig_down starts");
        runner
    }

    /// Run B, 2026-11-12: `dig_down` refused the cell the body stood on
    /// (`unsafe_drop`, three times in three ticks), the skill then explored the
    /// surface for three minutes, and a stair dug at the first cell of the new
    /// ground worked. The body steps a block aside instead and digs there.
    #[test]
    fn a_refused_start_steps_aside_and_digs_the_stair_from_the_next_cell() {
        let start = BlockPos::new(24, 64, 24);
        let mut world = world_with(3, 63, move |pos| {
            let hole = (pos.x - start.x).abs() <= 1
                && (pos.z - start.z).abs() <= 1
                && (60..=62).contains(&pos.y);
            if hole {
                BlockState::from(BlockKind::Air)
            } else if pos.y >= 60 {
                BlockState::from(BlockKind::Dirt)
            } else {
                BlockState::from(BlockKind::Stone)
            }
        });
        hold(&mut world, ItemKind::WoodenPickaxe);
        let mut runner = dig_down_call("stone");
        let run = drive(
            &mut world,
            &mut runner,
            Call {
                watch: Some(BlockKind::Stone),
                sweep: true,
                ..Call::new(start)
            },
            |_| false,
        );
        assert_eq!(run.outcome, Some(Outcome::Confirmed), "{:?}", run.path);
        let first_dug = *run.dug.first().expect("it dug a stair");
        assert!(
            (first_dug.x - start.x).abs() > 1 || (first_dug.z - start.z).abs() > 1,
            "the first dig is not beside the hole: {first_dug:?} (path {:?})",
            run.path
        );
        assert!(
            run.path.iter().any(|pos| *pos != start && pos.y == 64),
            "the body stepped aside on the surface: {:?}",
            run.path
        );
    }

    /// When no cell within the radius can be dug from, the call still ends with
    /// the refusal's own kind, at once, not after a long search.
    #[test]
    fn with_no_diggable_cell_around_the_refusal_is_the_failure() {
        let start = BlockPos::new(24, 64, 24);
        let mut world = world_with(3, 63, move |pos| {
            // Dirt on top, a hollow under every cell within eight blocks.
            let near = (pos.x - start.x).abs() <= 8 && (pos.z - start.z).abs() <= 8;
            if near && (60..=62).contains(&pos.y) {
                BlockState::from(BlockKind::Air)
            } else if pos.y >= 60 {
                BlockState::from(BlockKind::Dirt)
            } else {
                BlockState::from(BlockKind::Stone)
            }
        });
        hold(&mut world, ItemKind::WoodenPickaxe);
        let mut runner = dig_down_call("stone");
        let run = drive(
            &mut world,
            &mut runner,
            Call::new(start),
            |_| false,
        );
        assert_eq!(run.outcome, Some(Outcome::Failed("unsafe_drop".into())));
        assert!(run.dug.is_empty());
        assert_eq!(run.path, vec![start], "the body never moved");
    }

    /// Guards the real state machine's two phases: over a `World` with real
    /// chunks, `branch_mine(coal_ore)` cuts 8 one-block-down stairs along one
    /// axis to the ore height and then tunnels level at that height.
    #[test]
    fn branch_mine_descends_to_the_ore_height_then_tunnels_level() {
        let start = BlockPos::new(0, 64, 0);
        let mut world = world_with(3, 63, stone_below(63));
        hold(&mut world, ItemKind::WoodenPickaxe);
        let mut runner = branch_mine("coal_ore");
        // No watch: the world has no coal, so nothing can confirm the call.
        let run = drive(
            &mut world,
            &mut runner,
            Call::new(start),
            |path| path.iter().filter(|pos| pos.y == 56).count() >= 6,
        );
        assert_eq!(run.outcome, None, "no ore in the world: nothing confirms");
        assert!(run.path.len() > 9, "the tunnel was cut: {:?}", run.path);
        assert_eq!(run.path[8], BlockPos::new(8, 56, 0), "{:?}", run.path);
        for pair in run.path[..=8].windows(2) {
            assert_eq!(pair[1].y, pair[0].y - 1, "one block down per stair");
            assert_eq!(pair[1].x, pair[0].x + 1, "one column along the axis");
            assert_eq!(pair[1].z, pair[0].z);
        }
        // Every stair broke its tread, once.
        for step in 1..=8 {
            let tread = BlockPos::new(step, 64 - step, 0);
            assert_eq!(
                run.dug.iter().filter(|pos| **pos == tread).count(),
                1,
                "tread {tread:?} in {:?}",
                run.dug
            );
        }
        // One intent per block: nothing anywhere is dug twice.
        let unique: HashSet<BlockPos> = run.dug.iter().copied().collect();
        assert_eq!(
            unique.len(),
            run.dug.len(),
            "a block was dug twice: {:?}",
            run.dug
        );
        // The plain negative the missing watch would otherwise hide: the known
        // map the stop rule reads holds no coal at all.
        assert!(
            run.perception
                .nearest_known(&world, 64, |_| false, |_, state| {
                    BlockKind::from(state) == BlockKind::CoalOre
                })
                .is_none(),
            "the known map holds no coal_ore"
        );
        // From the target height on the tunnel is level.
        assert!(
            run.path[8..].iter().all(|pos| pos.y == 56),
            "{:?}",
            &run.path[8..]
        );
        assert!(
            run.path.iter().filter(|pos| pos.y == 56).count() >= 6,
            "five level steps after the last stair: {:?}",
            run.path
        );
    }

    /// Guards the stop rule: when a dig opens the face of a wanted block inside
    /// [`EXPLORE_NEAR_RADIUS`], the Digger confirms before the next dig can
    /// break the ore itself.
    #[test]
    fn branch_mine_confirms_when_the_tunnel_exposes_the_ore() {
        let ore = BlockPos::new(20, 56, 0);
        let mut world = world_with(3, 63, move |pos| {
            if pos.y > 63 {
                BlockState::from(BlockKind::Air)
            } else if pos == ore {
                BlockState::from(BlockKind::CoalOre)
            } else {
                BlockState::from(BlockKind::Stone)
            }
        });
        hold(&mut world, ItemKind::WoodenPickaxe);
        let mut runner = branch_mine("coal_ore");
        let run = drive(
            &mut world,
            &mut runner,
            Call {
                watch: Some(BlockKind::CoalOre),
                ..Call::new(BlockPos::new(0, 64, 0))
            },
            |_| false,
        );
        assert_eq!(run.outcome, Some(Outcome::Confirmed));
        // The last dig was the feet cell (19, 56); it opened the ore's west
        // face at (20, 56). The next dig in line, the head cell (20, 57), was
        // never requested, and the ore itself was never touched.
        assert_eq!(
            run.dug.last(),
            Some(&BlockPos::new(19, 56, 0)),
            "the exposing dig is the last one: {:?}",
            run.dug
        );
        assert!(
            !run.dug.contains(&BlockPos::new(20, 57, 0)),
            "no dig intent follows the ore exposure: {:?}",
            run.dug
        );
        assert!(
            !run.dug.contains(&ore),
            "the ore was dug instead of confirmed: {:?}",
            run.dug
        );
        assert_eq!(
            world.block_state(ore),
            Some(BlockState::from(BlockKind::CoalOre)),
            "the ore is still in the world"
        );
        // It descended to the ore height and tunneled level from there.
        assert!(
            run.path.iter().any(|pos| pos.y == 56 && pos.x >= 18),
            "{:?}",
            run.path
        );
        assert!(run.path.iter().all(|pos| pos.y >= 56));
    }

    /// Guards the branch dig budget: a wanted block with no known height
    /// (`stone`) gets only [`TUNNEL_RESERVE_DUGS`]; in a world without it the
    /// call digs exactly that many blocks and fails `not_found`, not `stuck`.
    #[test]
    fn branch_mine_fails_not_found_when_the_dig_budget_runs_out() {
        let start = BlockPos::new(0, 48, 0);
        let mut world = world_with(4, 63, move |pos| {
            let pocket = (pos.x, pos.z) == (0, 0) && (48..=49).contains(&pos.y);
            if pos.y > 63 || pocket {
                BlockState::from(BlockKind::Air)
            } else {
                BlockState::from(BlockKind::Dirt)
            }
        });
        hold(&mut world, ItemKind::WoodenPickaxe);
        let mut runner = branch_mine("stone");
        let run = drive(
            &mut world,
            &mut runner,
            Call {
                max_ticks: 3000,
                ..Call::new(start)
            },
            |_| false,
        );
        assert_eq!(run.outcome, Some(Outcome::Failed("not_found".into())));
        // `Runner` owns the `Digger`, so its `dug` counter is not readable
        // here; exactly the reserve count of dig intents, each cell once, is
        // the same invariant (`Digger::dug` only ever counts a settled dig).
        assert_eq!(run.dug.len() as u32, crate::branch_mine::TUNNEL_RESERVE_DUGS);
        let unique: HashSet<BlockPos> = run.dug.iter().copied().collect();
        assert_eq!(
            unique.len(),
            run.dug.len(),
            "a block was dug twice: {:?}",
            run.dug
        );
    }

    /// Guards the open-cave loop: in a pre-carved level tunnel the branch route
    /// only walks, so the dry-step cap must end the call as `stuck` after
    /// [`MAX_DRY_LEVEL_STEPS`] floor cells.
    #[test]
    fn branch_mine_fails_stuck_after_too_many_dry_level_steps() {
        let start = BlockPos::new(0, 48, 0);
        let mut world = world_with(3, 47, stone_below(47));
        hold(&mut world, ItemKind::WoodenPickaxe);
        let mut runner = branch_mine("coal_ore");
        let run = drive(&mut world, &mut runner, Call::new(start), |_| false);
        assert_eq!(run.outcome, Some(Outcome::Failed("stuck".into())));
        assert!(run.dug.is_empty(), "an open tunnel needs no dig: {:?}", run.dug);
        // The start cell plus 63 walks: the cap fires on the 64th accepted
        // walk before that cell is walked, so the feet path ends there.
        assert_eq!(run.path.len() as u32, MAX_DRY_LEVEL_STEPS);
        assert!(run.path.iter().all(|pos| pos.y == 48), "{:?}", run.path);
    }

    /// Guards the time bound of the branch route: past [`BRANCH_MAX_TICKS`]
    /// the digger fails `not_found` between two digs, before it plans another
    /// one; `dig_down` has no tick cap of its own, and a dig in flight is never
    /// cut off mid-receipt.
    #[test]
    fn branch_mine_fails_not_found_past_its_tick_cap() {
        let start = BlockPos::new(0, 56, 0);
        let world = world_with(3, 55, stone_below(55));
        let mut body = Body::new(feet_position(start));
        set_body_look(&mut body, EAST_YAW, 0.0);
        let mut perception = Perception::new(praxion_perception::Config::default());
        perception.begin_tick(&world);

        let mut tick_once = |digger: &mut Digger| {
            let mut ctx = Ctx {
                tick: 0,
                body: &body,
                world: &world,
                perception: &mut perception,
                style: Personality::default(),
            };
            digger.tick(&mut ctx).2
        };

        let mut branch = Digger::branch_mine("coal_ore".to_owned());
        branch.ticks = BRANCH_MAX_TICKS - 1;
        assert_eq!(tick_once(&mut branch), None, "the last tick inside the cap");
        assert_eq!(
            tick_once(&mut branch),
            Some(Outcome::Failed("not_found".into())),
            "the first tick past it"
        );

        // A dig in flight finishes its receipt first: the cap is not read
        // while a target is set.
        let mut digging = Digger::branch_mine("coal_ore".to_owned());
        digging.ticks = BRANCH_MAX_TICKS + 500;
        digging.target = Some(Target {
            cell: BlockPos::new(1, 56, 0),
            started: true,
            settled: false,
            gravity_over: false,
            waited: 0,
        });
        assert_eq!(tick_once(&mut digging), None);

        let mut staircase = Digger::dig_down("stone".to_owned());
        staircase.ticks = BRANCH_MAX_TICKS + 500;
        assert_ne!(
            tick_once(&mut staircase),
            Some(Outcome::Failed("not_found".into())),
            "the staircase has its own dig cap, not the tick cap"
        );
    }

    /// Names a gap in the primitive rather than guarding a feature: once the
    /// block is air and the dig started, `tick_target` has no timeout of its
    /// own for "started, not settled", so a receipt that never settles holds the
    /// call. Production bounds it outside the primitive: `branch_mine`'s flight
    /// deadline is 15,000 ticks (`Primitives::standard()`), after which the
    /// interpreter preempts the call, and the body's receipt poll drops an
    /// `Expired` receipt without ever calling `settle`. No production behaviour
    /// is changed here; the test only makes the gap visible.
    #[test]
    fn an_unsettled_dig_receipt_waits_for_the_flight_deadline() {
        let start = BlockPos::new(0, 64, 0);
        let mut world = world_with(3, 63, stone_below(63));
        hold(&mut world, ItemKind::WoodenPickaxe);
        let mut runner = branch_mine("coal_ore");
        // Far beyond every internal wait (`TARGET_WAIT_TICKS`, `WALK_TICKS`,
        // `AIRBORNE_TICKS` and `SETTLE_TICKS` are all at most 80 ticks) and far
        // below the 15,000-tick flight deadline.
        let run = drive(
            &mut world,
            &mut runner,
            Call {
                receipt: Receipt::Unsettled,
                max_ticks: 600,
                ..Call::new(start)
            },
            |_| false,
        );
        assert_eq!(
            run.outcome, None,
            "an unsettled receipt never finishes the digger"
        );
        assert_eq!(
            run.dug.len(),
            1,
            "the digger waits on its first dig: {:?}",
            run.dug
        );
        assert_eq!(run.path, vec![start], "and never walks");
        assert!(
            runner.is_active(),
            "the call is still in flight, bounded only by the flight deadline"
        );
    }

    /// A world of stone to `top` (a surface grassless world: the heightmap is
    /// the stone's top) with the pocket `feet`/head open, the body in it.
    fn pocket_world(top: i32, feet: BlockPos) -> World {
        use crate::test_world::world_with_surface;
        // Chunk sections are loaded up to `top + 8`, so the air over the ground
        // is real air a body can jump in, not an unloaded section.
        let mut world = world_with_surface(3, top + 8, stone_below(top));
        for dy in 0..2 {
            set_block(
                &mut world,
                BlockPos::new(feet.x, feet.y + dy, feet.z),
                BlockState::from(BlockKind::Air),
            );
        }
        hold(&mut world, ItemKind::WoodenPickaxe);
        world
    }

    fn climb_out() -> Runner {
        let mut runner = Runner::new();
        runner
            .start(&StartAction {
                primitive: "climb_out".to_owned(),
                args: Vec::new(),
            })
            .expect("climb_out starts");
        runner
    }

    /// Guards the whole climb: from a closed pocket in solid stone eight
    /// blocks under the surface, `climb_out` cuts a stair up, one block per
    /// step along one axis, digs every cell once, and confirms as soon as the
    /// body stands under open sky.
    #[test]
    fn climb_out_cuts_a_stair_up_to_the_open_sky() {
        let start = BlockPos::new(2, 56, 2);
        let mut world = pocket_world(63, start);
        let mut runner = climb_out();
        let run = drive(
            &mut world,
            &mut runner,
            Call {
                look: (EAST_YAW, 0.0),
                max_ticks: 2500,
                ..Call::new(start)
            },
            |_| false,
        );
        assert_eq!(run.outcome, Some(Outcome::Confirmed), "{:?}", run.path);
        let top = *run.path.last().expect("a path");
        assert!(top.y >= 63, "the body came up to the ground: {:?}", run.path);
        // One block up and one along per stair, all on one axis.
        for pair in run.path.windows(2) {
            assert_eq!(pair[1].y, pair[0].y + 1, "{:?}", run.path);
            assert_eq!(pair[1].x, pair[0].x + 1, "{:?}", run.path);
            assert_eq!(pair[1].z, pair[0].z);
        }
        // Every cell dug once; the floor beside the start was never dug.
        let unique: HashSet<BlockPos> = run.dug.iter().copied().collect();
        assert_eq!(unique.len(), run.dug.len(), "a block was dug twice: {:?}", run.dug);
        assert!(!run.dug.contains(&BlockPos::new(3, 56, 2)));
        // Three digs per stair, except where the stair opens into the sky.
        assert!(run.dug.len() >= 3 * 6 && run.dug.len() <= 3 * 8, "{}", run.dug.len());
    }

    /// Guards the open stair: a body at the foot of a staircase that is
    /// already cut (its own descent) climbs it without a single dig.
    #[test]
    fn climb_out_walks_an_open_staircase_without_digging() {
        // The descent `branch_mine` leaves: a stair down along +x from the
        // surface at y 64 to y 56, then solid stone on.
        let mut world = pocket_world(63, BlockPos::new(8, 56, 0));
        for step in 0..=8 {
            let feet_y = 64 - step;
            for dy in 0..3 {
                set_block(
                    &mut world,
                    BlockPos::new(step, feet_y + dy, 0),
                    BlockState::from(BlockKind::Air),
                );
            }
        }
        let mut runner = climb_out();
        let run = drive(
            &mut world,
            &mut runner,
            Call {
                look: (EAST_YAW + 180.0, 0.0),
                max_ticks: 600,
                ..Call::new(BlockPos::new(8, 56, 0))
            },
            |_| false,
        );
        assert_eq!(run.outcome, Some(Outcome::Confirmed), "{:?}", run.path);
        assert!(run.dug.is_empty(), "an open stair needs no dig: {:?}", run.dug);
        let top = *run.path.last().expect("a path");
        assert!(top.y >= 63, "{:?}", run.path);
        assert!(top.x < 8, "it walked back up the way it came: {:?}", run.path);
    }

    /// Guards the start state: under open sky the call confirms on its first
    /// tick, without a dig or a step.
    #[test]
    fn climb_out_under_open_sky_confirms_at_once() {
        let start = BlockPos::new(2, 64, 2);
        let mut world = pocket_world(63, BlockPos::new(8, 40, 8));
        let mut runner = climb_out();
        let run = drive(&mut world, &mut runner, Call::new(start), |_| false);
        assert_eq!(run.outcome, Some(Outcome::Confirmed));
        assert!(run.dug.is_empty());
        assert_eq!(run.path, vec![start]);
    }

    /// Guards the typed failure: a body whose floor beside it holds on no side
    /// has no stair to cut, and the call says `no_way_up` without a dig.
    #[test]
    fn climb_out_fails_no_way_up_when_every_side_is_refused() {
        let start = BlockPos::new(2, 56, 2);
        let mut world = pocket_world(63, start);
        // The four floors beside the feet are air: a pillar in a void.
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            set_block(
                &mut world,
                BlockPos::new(start.x + dx, start.y, start.z + dz),
                BlockState::from(BlockKind::Air),
            );
        }
        let mut runner = climb_out();
        let run = drive(&mut world, &mut runner, Call::new(start), |_| false);
        assert_eq!(run.outcome, Some(Outcome::Failed("no_way_up".into())));
        assert!(run.dug.is_empty(), "{:?}", run.dug);
    }

    /// Guards the time bound: past [`CLIMB_MAX_TICKS`] the climb fails
    /// `no_way_up` between two digs, never while a dig is in flight.
    #[test]
    fn climb_out_fails_no_way_up_past_its_tick_cap() {
        let start = BlockPos::new(2, 56, 2);
        let world = pocket_world(63, start);
        let mut body = Body::new(feet_position(start));
        set_body_look(&mut body, EAST_YAW, 0.0);
        let mut perception = Perception::new(praxion_perception::Config::default());
        perception.begin_tick(&world);
        let mut tick_once = |digger: &mut Digger| {
            let mut ctx = Ctx {
                tick: 0,
                body: &body,
                world: &world,
                perception: &mut perception,
                style: Personality::default(),
            };
            digger.tick(&mut ctx).2
        };
        let mut climb = Digger::climb_out();
        climb.ticks = CLIMB_MAX_TICKS - 1;
        assert_eq!(tick_once(&mut climb), None, "the last tick inside the cap");
        assert_eq!(
            tick_once(&mut climb),
            Some(Outcome::Failed("no_way_up".into())),
            "the first tick past it"
        );
    }
}
