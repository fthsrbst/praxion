//! `dig_down`: dig a walkable one-wide staircase toward a ground block.
//!
//! Stone lies three to six blocks under the dirt of flat terrain and no ray
//! ever reaches it from the surface, so `explore` cannot find it and, by
//! decision, may not dig. This primitive does what a person does: it cuts a
//! diagonal staircase down along one axis until the wanted block is known
//! within mining reach, then returns `confirmed` so the caller's next rule
//! (`mine`) takes over.
//!
//! One step goes from the feet cell `F = (x, y, z)` to the next column
//! `(x + dx, ., z + dz)`:
//!
//! ```text
//!   y+1   .  [A]     A: clearance, so the body fits while it walks in
//!   y     F  [B]     B: head room of the new stair
//!   y-1  [S] [T]     T: the new feet cell, one lower
//!   y-2  [S] [floor] floor: must be solid, so the fall is one block
//! ```
//!
//! The cells A, B, T are dug top-down (a gravity block above A then drains
//! into the open cells one dig at a time, never onto a cell still closed
//! below it), then the body walks onto T. The block under the body is never
//! dug. Every dug block gets exactly one dig intent and receipt, like `mine`.

use azalea_core::position::BlockPos;
use praxion_headless::{Click, ControlFrame};
use praxion_interpreter::Outcome;
use praxion_perception::exposes_fluid;
use praxion_planner::{blocks, center};
use praxion_world::BlockState;

use crate::common::{HandSelect, ToolStep, aim_precise, hold_tool, idle};
use crate::frontier::nearest_known_block;
use crate::mine::{block_gone, pickup_frame};
use crate::runner::{Ctx, IntentSpec};

/// How deep the staircase may go below where the call started.
pub(crate) const MAX_DEPTH: i32 = 12;

/// How many blocks one call may dig, refills of falling sand included.
pub(crate) const MAX_DUGS: u32 = 40;

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

/// The next move from the feet cell `feet` along `dir`. Pure over a block
/// lookup, so the safety rules are tested without a world. `start_y` is the
/// feet height where the call began.
///
/// Refusals are decided before the first dig of a step, so a step that cannot
/// be finished is never started.
pub(crate) fn plan_step(
    feet: BlockPos,
    dir: Dir,
    start_y: i32,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> StairStep {
    let (dx, dz) = dir;
    // Top-down: clearance, head room, the new feet cell.
    let cells = [
        offset(feet, dx, 1, dz),
        offset(feet, dx, 0, dz),
        offset(feet, dx, -1, dz),
    ];
    let tread = cells[2];
    let floor = offset(feet, dx, -2, dz);
    if tread.y < start_y - MAX_DEPTH {
        return StairStep::Refuse(Refusal::TooDeep);
    }

    let mut states = [None; 3];
    for (state, cell) in states.iter_mut().zip(cells) {
        *state = lookup(cell);
    }
    let Some(floor_state) = lookup(floor) else {
        return StairStep::Refuse(Refusal::UnsafeDrop);
    };
    if blocks::fluid(floor_state).is_some() {
        return StairStep::Refuse(Refusal::Fluid);
    }
    // The fall onto the new stair is one block: its floor must hold.
    if !blocks::is_solid(floor_state) || blocks::is_hazard(floor_state) {
        return StairStep::Refuse(Refusal::UnsafeDrop);
    }

    let mut first_closed = None;
    for (state, cell) in states.into_iter().zip(cells) {
        let Some(state) = state else {
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
fn axis_order(first: Dir) -> [Dir; 4] {
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

#[derive(Debug)]
pub(crate) struct DigDown {
    /// The wanted block, as in `mine(block)`.
    wanted: String,
    /// Feet height at the first tick; the depth cap counts from it.
    start_y: Option<i32>,
    /// The axis, once a step has been accepted.
    dir: Option<Dir>,
    dug: u32,
    target: Option<Target>,
    /// The feet cell the body is walking onto, and the ticks spent.
    walking: Option<(BlockPos, u32)>,
    hand: HandSelect,
    tool_waited: u32,
    settle_waited: u32,
    cooldown: u32,
    airborne: u32,
}

impl DigDown {
    pub(crate) fn new(wanted: String) -> Self {
        Self {
            wanted,
            start_y: None,
            dir: None,
            dug: 0,
            target: None,
            walking: None,
            hand: HandSelect::default(),
            tool_waited: 0,
            settle_waited: 0,
            cooldown: 0,
            airborne: 0,
        }
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
        if self.target.is_some() {
            return self.tick_target(ctx);
        }
        let frame = idle(ctx);
        // The stop condition is checked between digs only: a dig in flight
        // always settles its receipt first.
        if self.walking.is_none()
            && nearest_known_block(
                ctx.perception,
                ctx.world,
                &self.wanted,
                praxion_rules::registry::EXPLORE_NEAR_RADIUS,
                &[],
            )
            .is_some()
        {
            return (frame, None, Some(Outcome::Confirmed));
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
            return (frame, None, None);
        }
        self.airborne = 0;
        let feet = ctx.body.position().to_block_pos_floor();
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
        let world = ctx.world;
        let lookup = |pos: BlockPos| world.block_state(pos);
        let all = axis_order(facing(ctx.body.look().y_rot()));
        let dirs: &[Dir] = match &self.dir {
            Some(dir) => std::slice::from_ref(dir),
            None => &all,
        };
        let mut first_refusal = None;
        for &dir in dirs {
            let step = plan_step(feet, dir, start_y, &lookup);
            if let StairStep::Refuse(refusal) = step {
                first_refusal.get_or_insert(refusal);
                continue;
            }
            self.dir = Some(dir);
            match step {
                StairStep::Dig(cell) => {
                    if self.dug >= MAX_DUGS {
                        return (frame, None, Some(Outcome::Failed("no_block".into())));
                    }
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
                    self.walking = Some((tread, 0));
                }
                StairStep::Refuse(_) => unreachable!("refusals continue above"),
            }
            return (frame, None, None);
        }
        let kind = first_refusal.map_or("no_block", Refusal::kind);
        (frame, None, Some(Outcome::Failed(kind.into())))
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
            let step = plan_step(feet, dir, start_y, &map.lookup());
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
        let step = plan_step(feet, EAST, 64, &map.lookup());
        // Surface terrain: the clearance (y=65) and head (y=64) cells are air,
        // so the first dig is the dirt at the new feet cell.
        assert_eq!(step, StairStep::Dig(BlockPos::new(1, 63, 0)));
    }

    #[test]
    fn water_next_to_a_cell_refuses_the_step() {
        let mut map = Map::flat(63, 59);
        map.set(BlockPos::new(1, 63, 1), BlockKind::Water);
        let step = plan_step(BlockPos::new(0, 64, 0), EAST, 64, &map.lookup());
        assert_eq!(step, StairStep::Refuse(Refusal::Fluid));
    }

    #[test]
    fn water_above_the_stair_refuses_the_step() {
        let mut map = Map::flat(63, 59);
        map.set(BlockPos::new(1, 64, 0), BlockKind::Water);
        let step = plan_step(BlockPos::new(0, 64, 0), EAST, 64, &map.lookup());
        assert_eq!(step, StairStep::Refuse(Refusal::Fluid), "a water cell");
    }

    #[test]
    fn lava_below_the_new_stair_refuses_the_step() {
        let mut map = Map::flat(63, 59);
        map.set(BlockPos::new(1, 62, 0), BlockKind::Lava);
        let step = plan_step(BlockPos::new(0, 64, 0), EAST, 64, &map.lookup());
        assert_eq!(step, StairStep::Refuse(Refusal::Fluid), "lava is the floor");
    }

    #[test]
    fn a_hole_under_the_new_stair_is_an_unsafe_drop() {
        let mut map = Map::flat(63, 59);
        // The new feet cell is open and nothing under it for two blocks.
        for y in 61..=63 {
            map.set(BlockPos::new(1, y, 0), BlockKind::Air);
        }
        let step = plan_step(BlockPos::new(0, 64, 0), EAST, 64, &map.lookup());
        assert_eq!(step, StairStep::Refuse(Refusal::UnsafeDrop));
    }

    #[test]
    fn an_unloaded_cell_is_an_unsafe_drop() {
        let map = Map::flat(63, 59);
        let lookup = |pos: BlockPos| (pos.x != 1).then(|| BlockState::from(map.kind(pos)));
        let step = plan_step(BlockPos::new(0, 64, 0), EAST, 64, &lookup);
        assert_eq!(step, StairStep::Refuse(Refusal::UnsafeDrop));
    }

    #[test]
    fn bedrock_cannot_be_dug() {
        let mut map = Map::flat(63, 59);
        map.set(BlockPos::new(1, 63, 0), BlockKind::Bedrock);
        let step = plan_step(BlockPos::new(0, 64, 0), EAST, 64, &map.lookup());
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
            plan_step(feet, EAST, 64, &map.lookup()),
            StairStep::Dig(BlockPos::new(1, 65, 0))
        );
        // After the sand at y=65 is dug, the one at y=66 drops into it: the
        // cell is closed again and is dug again (a new receipt).
        map.set(BlockPos::new(1, 65, 0), BlockKind::Air);
        map.set(BlockPos::new(1, 63, 0), BlockKind::Air);
        map.set(BlockPos::new(1, 64, 0), BlockKind::Air);
        // Everything is open but the sand still hangs over the column.
        assert_eq!(plan_step(feet, EAST, 64, &map.lookup()), StairStep::Settle);
        map.set(BlockPos::new(1, 66, 0), BlockKind::Air);
        map.set(BlockPos::new(1, 65, 0), BlockKind::Sand);
        assert_eq!(
            plan_step(feet, EAST, 64, &map.lookup()),
            StairStep::Dig(BlockPos::new(1, 65, 0)),
            "the refill is dug again"
        );
        map.set(BlockPos::new(1, 65, 0), BlockKind::Air);
        assert_eq!(
            plan_step(feet, EAST, 64, &map.lookup()),
            StairStep::Walk(BlockPos::new(1, 63, 0))
        );
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
