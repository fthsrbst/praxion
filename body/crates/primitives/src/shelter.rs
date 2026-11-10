//! `shelter()`: seal the body in for the night (decision 2026-11-10 "A sealed
//! shelter now"). Where the ground under the feet can be dug safely, the body
//! digs straight down and closes the cell over its head; elsewhere it walls
//! itself in where it stands. The call is done when `sheltered()` is true: the
//! fact and the primitive share one rule ([`praxion_facts::closes_shelter`]),
//! so the skill's `sheltered() -> wait` and this call can never disagree.
//!
//! # Why three blocks down, not two
//!
//! The decision says two. Two is not enough to close the hole: in a two-deep
//! hole the head is level with the ground, so the cell over the head sits
//! above the ground with air on every side, and a block can only be placed
//! against the face of another block. From inside the hole no face borders
//! that cell (the rim's top faces are above the eye and cannot be seen from
//! below). Three deep, the cell over the head is the first dug cell, which has
//! ground on all four sides: its walls are the faces the cover goes against.
//!
//! ```text
//!  two deep (cannot close)        three deep (closes)
//!   y0   .  [?]  .   no face       y0   .   .   .
//!   y-1  #  head #                 y-1  #  [C]  #   C: cover, against the walls
//!   y-2  #  feet #                 y-2  #  head #
//!   y-3  #   #   #                 y-3  #  feet #
//!                                  y-4  #   #   #
//! ```
//!
//! # Dig safety
//!
//! The three cells and the floor under them are judged before the first dig,
//! with `dig_down`'s rules: no fluid in or next to a dug cell, nothing that
//! cannot be broken or is a hazard, a solid floor (the falls are one block
//! each), never the body's own scaffolding or a placed workstation, and a
//! tool that makes the dig worth it. Those cells are under the ground, out of
//! sight, so they are read through perception's privileged column read, which
//! ledgers every read (architecture §5.1). Anything unknown refuses the dig.
//!
//! # Walling in
//!
//! Where the dig is refused, the body closes the nine cells [`shelter_cells`]
//! names where it stands: feet level first, then head level (each new block is
//! the support of the one above it), then the cell over the head. On open
//! ground the cell over the head borders no block, so the body first jumps and
//! places a helper block on top of one of its head-level walls (the top face is
//! only in view from above it, at the top of a jump, as in `pillar`) and then
//! closes the cover against the helper's side.
//!
//! Every cell the body reads is next to its own feet or head cell, so it is in
//! view: perception answers from sight.
//!
//! # A cover is guaranteed before the first dig
//!
//! A dig-down starts only when the cover is certain: the body carries one of
//! [`SHELTER_BLOCKS`], or the first cell it digs drops one (dirt, grass, stone
//! with a pickaxe). A column with a gravity block in it (sand, gravel, concrete
//! powder) is never dug: its drop cannot be the cover, and a cover of it would
//! fall on the head. Otherwise the call walls in, or fails `no_blocks` before
//! it digs anything.
//!
//! # The way out is checked before the first dig or block
//!
//! A shelter the body cannot leave keeps it shut in all day (run 9, third
//! review). So before it digs or places anything the call runs
//! `leave_shelter`'s own exit checks ([`leave::exit_check`]) over the cells as
//! they will be once it is done: the hole's cells dug and its cover placed, or
//! the walls and cover placed around the body. A dig-down whose climb out
//! would be refused (leaves or another block over the mouth, too few blocks to
//! climb with counting the cover's drop, a cover nothing carried can mine back,
//! fluid) walls in instead, whose exit is one mined wall; a wall-in that could
//! not be left either fails `no_exit` before the first block, and one with too
//! few blocks for its walls fails `no_blocks`.
//!
//! The call builds only with a block it can mine back with its drop
//! ([`build_block`]): cobblestone without a pickaxe is never a cover or a wall.
//!
//! # Leaving in the morning
//!
//! `leave_shelter()` runs on the same machine in its own mode
//! ([`leave`]): it mines the cover and climbs out (decision 2026-11-10
//! "Shelter shape and exit").

use azalea_core::direction::Direction;
use azalea_core::position::{BlockPos, Vec3};
use azalea_registry::builtin::BlockKind;
use praxion_facts::{closes_shelter, feet_cell, shelter_cells, sheltered};
use praxion_headless::{Click, ControlFrame, Look};
use praxion_interpreter::Outcome;
use praxion_perception::{COLUMN_BLOCKS, Caller, FactKind, QueryKind, exposes_fluid, scaffold};
use praxion_planner::{blocks, center};
use praxion_world::BlockState;

use crate::common::{
    HandSelect, Selecting, ToolChoice, ToolStep, aim_precise, cell_centre, choose_aim,
    choose_tool, count_item, hold_tool, idle, item_name, pickup_frame,
};
use crate::mine::block_gone;
use crate::pillar::Pillar;
use crate::place::{overlaps_body, place_support};
use crate::runner::{Ctx, IntentSpec};

mod leave;

/// How many cells the body digs straight down (see the module docs for why
/// three).
pub(crate) const HOLE_DEPTH: i32 = 3;

/// The whole call's bound. The worst case is a wall-in on open ground: ten
/// placements (eight walls, the helper and the cover) of about twenty ticks
/// each (select, aim, receipt), two jumps, and the waits for answers. A dig
/// down is three digs of dirt by hand (15 ticks each plus up to 40 of tool
/// selection) and one placement. The registry budget is above this, so the
/// call always ends with its own typed kind first.
pub(crate) const CALL_TICKS: u32 = 600;

/// The blocks `shelter` builds with (and `leave_shelter` climbs with), best
/// first: plain ground and stone blocks, then planks, then logs (worth more,
/// but better than the night). Gravity blocks (sand, gravel) are left out: a
/// cover of them falls on the head.
const SHELTER_BLOCKS: [&str; 13] = [
    "dirt",
    "cobblestone",
    "cobbled_deepslate",
    "coarse_dirt",
    "stone",
    "deepslate",
    "andesite",
    "diorite",
    "granite",
    "tuff",
    "netherrack",
    "#planks",
    "#logs",
];

/// The window of each dig receipt, as in `dig_down`.
const DIG_WINDOW: u64 = 160;
/// The window of each place receipt, as in `place`.
const PLACE_WINDOW: u64 = 40;
/// Ticks a dig may wait for its intent to register (tool, aim).
const DIG_WAIT_TICKS: u32 = 80;
/// Ticks a placement may take from the plan to the click (hand, aim).
const PLACE_AIM_TICKS: u32 = 40;
/// Ticks to wait for a place receipt after the click before planning again
/// (the cell then either holds the block or is placed again).
const RECEIPT_TICKS: u32 = 45;
/// Ticks the body may stay in the air (a fall into the hole) at a time.
const AIRBORNE_TICKS: u32 = 60;
/// Ticks of the whole call spent stepping to the middle of the feet cell: off
/// a ledge into the hole, or out of the way of a cell to fill.
const STEP_TICKS: u32 = 60;
/// Ticks of the whole call spent waiting for a perception answer.
const ANSWER_TICKS: u32 = 40;
/// Ticks of the whole call spent waiting for a placeable block to show in the
/// inventory (the dug dirt is picked up a few ticks after it drops).
const ITEM_TICKS: u32 = 40;
/// The pitch error, in degrees, accepted before the helper jump.
const JUMP_AIM_TOLERANCE: f32 = 1.0;
/// Ticks of one jump to wait for the click window.
const AIR_TICKS: u32 = 14;
/// Jumps one helper placement may make.
const MAX_JUMPS: u32 = 2;
/// How far above the standing feet the eye is at the top of a jump: the 1.25
/// block jump apex measured in `pillar` plus the 1.62 eye height.
const JUMP_EYE_RISE: f64 = 1.25 + 1.62;

/// Why the ground under the feet may not be dug.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DigRefusal {
    /// A cell cannot be read: unloaded, or above the column read's window.
    Unknown,
    /// Water or lava is in or next to a cell, or under the floor.
    Fluid,
    /// A cell is a hazard, open (a deeper fall), or the floor does not hold.
    Drop,
    /// A cell cannot be broken (bedrock).
    Unbreakable,
    /// A cell is the body's own scaffolding or a placed workstation.
    Support,
    /// A cell is a gravity block (sand, gravel, concrete powder).
    Gravity,
    /// Nothing carried closes the hole and the first dug cell drops nothing
    /// that does.
    NoCover,
    /// The finished hole could not be left ([`leave::exit_check`]).
    NoExit,
}

/// Whether the [`HOLE_DEPTH`] cells under `feet` may be dug, from a block
/// lookup. Pure, so the rules are tested without a world.
pub(crate) fn plan_dig_down(
    feet: BlockPos,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> Result<(), DigRefusal> {
    for depth in 1..=HOLE_DEPTH {
        let cell = BlockPos::new(feet.x, feet.y - depth, feet.z);
        let state = lookup(cell).ok_or(DigRefusal::Unknown)?;
        if blocks::fluid(state).is_some() {
            return Err(DigRefusal::Fluid);
        }
        if blocks::is_hazard(state) || !blocks::is_solid(state) {
            return Err(DigRefusal::Drop);
        }
        if !blocks::is_breakable(state) {
            return Err(DigRefusal::Unbreakable);
        }
        if is_gravity(state) {
            return Err(DigRefusal::Gravity);
        }
        if scaffold::is_scaffold(cell) || is_workstation(state) {
            return Err(DigRefusal::Support);
        }
        if exposes_fluid(cell, lookup) {
            return Err(DigRefusal::Fluid);
        }
    }
    let floor = BlockPos::new(feet.x, feet.y - HOLE_DEPTH - 1, feet.z);
    let state = lookup(floor).ok_or(DigRefusal::Unknown)?;
    if blocks::fluid(state).is_some() {
        return Err(DigRefusal::Fluid);
    }
    if blocks::is_hazard(state) || !blocks::is_solid(state) {
        return Err(DigRefusal::Drop);
    }
    Ok(())
}

/// Whether a dig-down may start: the cover is guaranteed when the body carries
/// a block of [`SHELTER_BLOCKS`] (`carries_block`) or the first dug cell,
/// `first`, drops one. Pure, so the rule is tested without a world.
pub(crate) fn cover_guaranteed(carries_block: bool, first: BlockState) -> bool {
    carries_block || drops_shelter_block(first)
}

/// Whether breaking `state` drops a block of [`SHELTER_BLOCKS`] (see
/// [`shelter_drop`]).
fn drops_shelter_block(state: BlockState) -> bool {
    shelter_drop(state).is_some()
}

/// The block of [`SHELTER_BLOCKS`] breaking `state` drops: the loot table where
/// `praxion-gamedata` has one (grass drops dirt, stone cobblestone), else the
/// few ground blocks that drop something else, else the block itself. The tool
/// is checked apart ([`choose_tool`]): this is what drops with it.
fn shelter_drop(state: BlockState) -> Option<&'static str> {
    let name = blocks::name(state);
    if let Some(drops) = praxion_gamedata::block_drops(name) {
        return drops.iter().map(|drop| drop.item).find(|item| is_shelter_block(item));
    }
    let item = match name {
        "deepslate" => "cobbled_deepslate",
        "podzol" | "mycelium" | "dirt_path" | "farmland" => "dirt",
        other => other,
    };
    is_shelter_block(item).then_some(item)
}

/// Whether `item` is one of [`SHELTER_BLOCKS`] (a name or a member of a tag).
fn is_shelter_block(item: &str) -> bool {
    SHELTER_BLOCKS.iter().any(|block| entry_matches(block, item))
}

/// Whether `item` is the [`SHELTER_BLOCKS`] entry `entry` (a name or a tag).
fn entry_matches(entry: &str, item: &str) -> bool {
    match entry.strip_prefix('#') {
        Some(tag) => praxion_gamedata::item_in_tag(item, tag),
        None => entry == item,
    }
}

/// A block that falls when the cell under it opens: sand, red sand, gravel,
/// their suspicious kinds and every concrete powder.
fn is_gravity(state: BlockState) -> bool {
    let name = blocks::name(state);
    matches!(
        name,
        "sand" | "red_sand" | "gravel" | "suspicious_sand" | "suspicious_gravel"
    ) || name.ends_with("_concrete_powder")
}

/// A block the body placed to use where it stands: never dug out from under it.
fn is_workstation(state: BlockState) -> bool {
    matches!(
        BlockKind::from(state),
        BlockKind::CraftingTable
            | BlockKind::Furnace
            | BlockKind::Chest
            | BlockKind::Smoker
            | BlockKind::BlastFurnace
            | BlockKind::Barrel
    )
}

/// The block face pointing from a support toward the cell beside it.
fn face_toward(support: BlockPos, cell: BlockPos) -> Option<Direction> {
    match (cell.x - support.x, cell.y - support.y, cell.z - support.z) {
        (1, 0, 0) => Some(Direction::East),
        (-1, 0, 0) => Some(Direction::West),
        (0, 1, 0) => Some(Direction::Up),
        (0, -1, 0) => Some(Direction::Down),
        (0, 0, 1) => Some(Direction::South),
        (0, 0, -1) => Some(Direction::North),
        _ => None,
    }
}

/// How the call closes the shelter, decided once on the first ground tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Plan {
    /// Dig [`HOLE_DEPTH`] cells straight down from `origin`, then close.
    DigDown { origin: BlockPos },
    /// Close the cells around the body where it stands.
    WallIn,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum JumpStage {
    /// Hand and look: everything before the jump.
    Prepare,
    /// In the air: click when the crosshair is on the support's top.
    Air,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum JobKind {
    Dig,
    /// Place against `face` of `support`, aiming at `point`.
    Place {
        support: BlockPos,
        face: Direction,
        point: Vec3,
    },
    /// Place on top of a support from the top of a jump.
    JumpPlace(Jump),
}

/// The helper placement's jump.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Jump {
    /// The head-level wall whose top face takes the click.
    support: BlockPos,
    /// The look that puts the crosshair on that face at the top of the jump.
    yaw: f32,
    pitch: f32,
    stage: JumpStage,
    jumps: u32,
    stage_ticks: u32,
}

/// One block to break or place, with its own receipt.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Job {
    cell: BlockPos,
    kind: JobKind,
    /// The use click went out (a placement).
    clicked: bool,
    /// The runner registered the receipt.
    started: bool,
    /// The receipt settled `confirmed`.
    settled: bool,
    /// Ticks spent on this job before its click or registration.
    waited: u32,
}

impl Job {
    fn new(cell: BlockPos, kind: JobKind) -> Self {
        Self {
            cell,
            kind,
            clicked: false,
            started: false,
            settled: false,
            waited: 0,
        }
    }
}

type Step = (ControlFrame, Option<IntentSpec>, Option<Outcome>);

fn failed(frame: ControlFrame, kind: &str) -> Step {
    (frame, None, Some(Outcome::Failed(kind.into())))
}

/// What the call does: seal the body in (`shelter()`), or open the shelter
/// and climb out (`leave_shelter()`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Seal,
    Leave,
}

#[derive(Debug, Default)]
pub(crate) struct Shelter {
    mode: Mode,
    ticks: u32,
    plan: Option<Plan>,
    job: Option<Job>,
    hand: HandSelect,
    tool_waited: u32,
    /// The block chosen for the current placement, kept for the whole job.
    block: Option<String>,
    airborne: u32,
    step_ticks: u32,
    answer_ticks: u32,
    item_ticks: u32,
    /// `leave_shelter`: the pillar step under way (one block per step).
    pillar: Option<Pillar>,
    /// The runner registered the pillar step's place receipt.
    pillar_started: bool,
    /// `leave_shelter`: the body is out; it collects the drops near it.
    out: bool,
    collect_ticks: u32,
}

impl Shelter {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// `leave_shelter()`: open the shelter and climb out.
    pub(crate) fn leave() -> Self {
        Self {
            mode: Mode::Leave,
            ..Self::default()
        }
    }

    /// The runner registered the current job's receipt.
    pub(crate) fn note_registered(&mut self) {
        if self.pillar.is_some() {
            self.pillar_started = true;
        } else if let Some(job) = &mut self.job {
            job.started = true;
        }
    }

    /// The current job's receipt settled `confirmed`. A pillar step ends here:
    /// the block stands, the cell is the body's scaffolding, and the next tick
    /// plans again from the new height.
    pub(crate) fn note_settled(&mut self) {
        if let Some(mut pillar) = self.pillar.take() {
            pillar.note_settled();
            self.pillar_started = false;
        } else if let Some(job) = &mut self.job {
            job.settled = true;
        }
    }

    pub(crate) fn tick(&mut self, ctx: &mut Ctx<'_>) -> Step {
        if self.mode == Mode::Leave {
            return self.tick_leave(ctx);
        }
        let frame = idle(ctx);
        self.ticks += 1;
        if self.ticks > CALL_TICKS {
            return failed(frame, "stuck");
        }
        if self.job.is_some() {
            return self.tick_job(ctx);
        }
        if !ctx.body.on_ground() {
            self.airborne += 1;
            if self.airborne > AIRBORNE_TICKS {
                return failed(frame, "stuck");
            }
            return (frame, None, None);
        }
        self.airborne = 0;
        if sheltered(ctx.perception, ctx.world) == Some(true) {
            return (frame, None, Some(Outcome::Confirmed));
        }
        let feet = feet_cell(ctx.body.position());
        let plan = match self.plan {
            Some(plan) => plan,
            None => {
                match self.choose_plan(ctx, feet) {
                    Ok(plan) => {
                        log::debug!("shelter: {plan:?} at {feet:?}");
                        self.plan = Some(plan);
                        plan
                    }
                    Err(Some(kind)) => return failed(frame, kind),
                    // A perception answer is missing this tick.
                    Err(None) => return self.wait_for_answer(frame),
                }
            }
        };
        if let Plan::DigDown { origin } = plan
            && feet.y > origin.y - HOLE_DEPTH
        {
            return self.dig_down(ctx, origin, feet, frame);
        }
        self.close(ctx, feet, frame)
    }

    /// Fluid around the body fails the call; otherwise dig down when the
    /// ground allows it, else wall in. `Err(None)` waits for an answer.
    fn choose_plan(&mut self, ctx: &mut Ctx<'_>, feet: BlockPos) -> Result<Plan, Option<&'static str>> {
        let head = BlockPos::new(feet.x, feet.y + 1, feet.z);
        for cell in shelter_cells(feet).into_iter().chain([feet, head]) {
            let state = cell_state(ctx, cell).ok_or(None)?;
            if blocks::fluid(state).is_some() || blocks::is_hazard(state) {
                log::debug!("shelter: {:?} at {cell:?} next to the body", BlockKind::from(state));
                return Err(Some("unsafe"));
            }
        }
        let dig = self.can_dig_down(ctx, feet).and_then(|dug| {
            if !cover_guaranteed(build_block(ctx, &[]).is_some(), dug[0]) {
                return Err(DigRefusal::NoCover);
            }
            dig_down_exit(ctx, feet, dug).map_err(|kind| {
                log::debug!("shelter: the hole at {feet:?} could not be left ({kind})");
                DigRefusal::NoExit
            })
        });
        match dig {
            Ok(()) => Ok(Plan::DigDown { origin: feet }),
            Err(refusal) => {
                log::debug!("shelter: no dig down at {feet:?}: {refusal:?}");
                wall_in_check(ctx, feet).map(|()| Plan::WallIn)
            }
        }
    }

    /// The dig-down rules over the privileged column reads of the body's
    /// column and its four neighbours, plus a tool worth digging with. `Ok`
    /// carries the cells to dig, top first: their drops are the blocks the
    /// cover and the climb out may use.
    fn can_dig_down(
        &self,
        ctx: &mut Ctx<'_>,
        feet: BlockPos,
    ) -> Result<[BlockState; HOLE_DEPTH as usize], DigRefusal> {
        let from_y = feet.y - HOLE_DEPTH - 1;
        let columns: Vec<_> = [(0, 0), (1, 0), (-1, 0), (0, 1), (0, -1)]
            .into_iter()
            .map(|(dx, dz)| {
                let (x, z) = (feet.x + dx, feet.z + dz);
                let column =
                    ctx.perception
                        .privileged_column(ctx.world, x, z, from_y, COLUMN_BLOCKS as i32);
                ((x, z), column)
            })
            .collect();
        ctx.perception
            .ledger_privileged(QueryKind::SkyColumn, FactKind::Column, columns.len() as u32);
        let lookup = |pos: BlockPos| {
            let (_, column) = columns.iter().find(|((x, z), _)| *x == pos.x && *z == pos.z)?;
            let column = column.as_ref()?;
            // Above the heightmap surface every cell is air by definition.
            if pos.y >= column.surface {
                return Some(BlockState::AIR);
            }
            column.state_at(pos.y)
        };
        plan_dig_down(feet, &lookup)?;
        // A cell no carried item drops (stone by hand) takes seconds per block
        // at night: wall in instead.
        let held = praxion_headless::held_item(ctx.world);
        let mut dug = [BlockState::AIR; HOLE_DEPTH as usize];
        for (index, depth) in (1..=HOLE_DEPTH).enumerate() {
            let state = lookup(BlockPos::new(feet.x, feet.y - depth, feet.z))
                .ok_or(DigRefusal::Unknown)?;
            if choose_tool(state, ctx.world.inventory(), &held) == ToolChoice::Missing {
                return Err(DigRefusal::Unbreakable);
            }
            dug[index] = state;
        }
        Ok(dug)
    }

    /// One step of the descent: dig the cell under the feet, or, when it is
    /// open already, step to the middle of the column so the body drops in.
    fn dig_down(
        &mut self,
        ctx: &mut Ctx<'_>,
        origin: BlockPos,
        feet: BlockPos,
        frame: ControlFrame,
    ) -> Step {
        let column = BlockPos::new(origin.x, feet.y, origin.z);
        let below = BlockPos::new(origin.x, feet.y - 1, origin.z);
        let Some(state) = cell_state(ctx, below) else {
            return self.wait_for_answer(frame);
        };
        if feet != column || blocks::is_passable(state) {
            // Off to the side of the hole, or on its rim after the dig: walk
            // to the middle and fall in.
            return self.step_to(ctx, column);
        }
        self.start(Job::new(below, JobKind::Dig));
        (frame, None, None)
    }

    /// Close the first open cell around the body, in [`shelter_cells`] order.
    fn close(&mut self, ctx: &mut Ctx<'_>, feet: BlockPos, frame: ControlFrame) -> Step {
        let cells = shelter_cells(feet);
        for (index, cell) in cells.into_iter().enumerate() {
            let Some(state) = cell_state(ctx, cell) else {
                return self.wait_for_answer(frame);
            };
            if closes_shelter(state) {
                continue;
            }
            if blocks::fluid(state).is_some() || blocks::is_hazard(state) {
                return failed(frame, "unsafe");
            }
            if !blocks::is_replaceable(state) {
                // A flower or a torch takes the click: break it first. A slab,
                // a fence or a chest cannot be closed by this call.
                if blocks::is_passable(state) && blocks::is_breakable(state) {
                    self.start(Job::new(cell, JobKind::Dig));
                    return (frame, None, None);
                }
                log::debug!("shelter: cannot close {:?} at {cell:?}", BlockKind::from(state));
                return failed(frame, "stuck");
            }
            let Some((block, _)) = build_block(ctx, &[]) else {
                // The dug dirt shows up a few ticks after the dig.
                self.item_ticks += 1;
                if self.item_ticks > ITEM_TICKS {
                    return failed(frame, "no_blocks");
                }
                return (frame, None, None);
            };
            if overlaps_body(&ctx.body.bounding_box(), cell) {
                return self.step_to(ctx, feet);
            }
            if let Some((support, face, point)) = place_aim(ctx, cell, feet) {
                self.block = Some(block);
                self.start(Job::new(cell, JobKind::Place { support, face, point }));
                return (frame, None, None);
            }
            // Only the cover can lack a support on every side (open ground):
            // put a helper on a head-level wall from the top of a jump.
            if index == cells.len() - 1
                && let Some(job) = helper_job(ctx, feet, cell)
            {
                self.block = Some(block);
                self.start(job);
                return (frame, None, None);
            }
            log::debug!("shelter: no face to place {cell:?} against");
            return failed(frame, "stuck");
        }
        // Every cell closes but the fact did not say so this tick (an answer
        // it could not get): ask again next tick.
        self.wait_for_answer(frame)
    }

    fn start(&mut self, job: Job) {
        self.tool_waited = 0;
        self.job = Some(job);
    }

    fn wait_for_answer(&mut self, frame: ControlFrame) -> Step {
        self.answer_ticks += 1;
        if self.answer_ticks > ANSWER_TICKS {
            return failed(frame, "stuck");
        }
        (frame, None, None)
    }

    /// Walk to the middle of the feet cell `cell`.
    fn step_to(&mut self, ctx: &Ctx<'_>, cell: BlockPos) -> Step {
        self.step_ticks += 1;
        if self.step_ticks > STEP_TICKS {
            return failed(idle(ctx), "stuck");
        }
        (pickup_frame(ctx, center(cell)), None, None)
    }

    fn tick_job(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let job = self.job.expect("checked by the caller");
        match job.kind {
            JobKind::Dig => self.tick_dig(ctx, job),
            JobKind::Place { support, face, point } => self.tick_place(ctx, job, support, face, point),
            JobKind::JumpPlace(_) => self.tick_jump(ctx),
        }
    }

    /// Break the job's cell: tool, aim, click until its receipt settles.
    fn tick_dig(&mut self, ctx: &mut Ctx<'_>, mut job: Job) -> Step {
        let mut frame = idle(ctx);
        let state = cell_state(ctx, job.cell);
        match (block_gone(state), job.started, job.settled) {
            // Gone before a receipt was registered: plan again.
            (true, false, _) => {
                self.job = None;
                return (frame, None, None);
            }
            (true, true, true) => {
                self.job = None;
                return (frame, None, None);
            }
            // Broken; stop clicking until the receipt settles.
            (true, true, false) => return (frame, None, None),
            // Settled, but a block is back (falling sand): a new receipt.
            (false, true, true) => {
                job.started = false;
                job.settled = false;
                job.waited = 0;
                self.tool_waited = 0;
            }
            (false, _, _) => {}
        }
        if !job.started {
            job.waited += 1;
            if job.waited > DIG_WAIT_TICKS {
                return failed(frame, "stuck");
            }
            if let Some(state) = state {
                match hold_tool(ctx, state, &mut self.hand, &mut self.tool_waited, &mut frame) {
                    ToolStep::Ready => {}
                    ToolStep::Pending => {
                        self.job = Some(job);
                        return (frame, None, None);
                    }
                    ToolStep::Missing => return failed(frame, "stuck"),
                }
            }
        }
        let point = choose_aim(ctx, job.cell)
            .point
            .unwrap_or_else(|| cell_centre(job.cell));
        let (look, aligned) = aim_precise(ctx, point);
        frame.look = look;
        self.job = Some(job);
        if !aligned {
            return (frame, None, None);
        }
        frame.clicks.push(Click::Attack);
        let pending = (!job.started).then_some(IntentSpec::Dig {
            pos: job.cell,
            window: DIG_WINDOW,
        });
        (frame, pending, None)
    }

    /// Place against a support face in view: hand, aim, one use click, then
    /// the receipt.
    fn tick_place(
        &mut self,
        ctx: &mut Ctx<'_>,
        mut job: Job,
        support: BlockPos,
        face: Direction,
        point: Vec3,
    ) -> Step {
        let mut frame = idle(ctx);
        job.waited += 1;
        if job.clicked {
            if job.started && job.settled {
                self.job = None;
            } else if job.waited > RECEIPT_TICKS {
                // No receipt registered or settled in time: plan again; the
                // cell either holds the block now or is placed again.
                self.job = None;
            } else {
                self.job = Some(job);
            }
            return (frame, None, None);
        }
        if job.waited > PLACE_AIM_TICKS {
            self.job = None;
            return (frame, None, None);
        }
        self.job = Some(job);
        let Some(block) = self.block.clone() else {
            self.job = None;
            return (frame, None, None);
        };
        match self.hand.tick(ctx.world, &block, &mut frame) {
            Selecting::Held => {}
            Selecting::Pending => return (frame, None, None),
            Selecting::Unavailable => return failed(frame, "no_blocks"),
        }
        let (look, aligned) = aim_precise(ctx, point);
        frame.look = look;
        if !aligned {
            return (frame, None, None);
        }
        let pick = praxion_headless::pick(ctx.world, ctx.body);
        let on_face = pick
            .block
            .as_ref()
            .is_some_and(|hit| !hit.miss && hit.block_pos == support && hit.direction == face);
        if !on_face || pick.entity_closer {
            // The view changed since the plan: plan again.
            self.job = None;
            return (frame, None, None);
        }
        frame.clicks.push(Click::Use);
        job.clicked = true;
        job.waited = 0;
        self.job = Some(job);
        let pending = IntentSpec::Place {
            pos: job.cell,
            block: held_block(ctx).unwrap_or(block),
            window: PLACE_WINDOW,
        };
        (frame, Some(pending), None)
    }

    /// The helper block on top of a head-level wall, clicked from the top of a
    /// jump: look first, jump, click while the crosshair is on the top face.
    fn tick_jump(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let mut frame = idle(ctx);
        let block = self.block.clone();
        let Self { job: slot, hand, .. } = self;
        let Some(job) = slot.as_mut() else {
            unreachable!("tick_jump runs the current job");
        };
        let JobKind::JumpPlace(jump) = &mut job.kind else {
            unreachable!("tick_jump runs a jump job");
        };
        jump.stage_ticks += 1;
        if job.clicked {
            job.waited += 1;
            if (job.started && job.settled) || job.waited > RECEIPT_TICKS {
                *slot = None;
            }
            return (frame, None, None);
        }
        let Some(block) = block else {
            *slot = None;
            return (frame, None, None);
        };
        frame.look = Look::Aim {
            yaw: jump.yaw,
            pitch: jump.pitch,
        };
        match jump.stage {
            JumpStage::Prepare => {
                if jump.stage_ticks > PLACE_AIM_TICKS {
                    return failed(frame, "stuck");
                }
                match hand.tick(ctx.world, &block, &mut frame) {
                    Selecting::Held => {}
                    Selecting::Pending => return (frame, None, None),
                    Selecting::Unavailable => return failed(frame, "no_blocks"),
                }
                let look = ctx.body.look();
                let aligned = praxion_planner::wrap_degrees(jump.yaw - look.y_rot()).abs()
                    <= JUMP_AIM_TOLERANCE
                    && (jump.pitch - look.x_rot()).abs() <= JUMP_AIM_TOLERANCE;
                if aligned && ctx.body.on_ground() {
                    frame.held.jump = true;
                    jump.jumps += 1;
                    jump.stage = JumpStage::Air;
                    jump.stage_ticks = 0;
                }
                (frame, None, None)
            }
            JumpStage::Air => {
                let pick = praxion_headless::pick(ctx.world, ctx.body);
                let on_top = pick.block.as_ref().is_some_and(|hit| {
                    !hit.miss && hit.block_pos == jump.support && hit.direction == Direction::Up
                });
                if on_top && !pick.entity_closer {
                    frame.clicks.push(Click::Use);
                    job.clicked = true;
                    job.waited = 0;
                    let pending = IntentSpec::Place {
                        pos: job.cell,
                        block: held_block(ctx).unwrap_or(block),
                        window: PLACE_WINDOW,
                    };
                    return (frame, Some(pending), None);
                }
                if jump.stage_ticks > AIR_TICKS && ctx.body.on_ground() {
                    if jump.jumps >= MAX_JUMPS {
                        return failed(frame, "stuck");
                    }
                    jump.stage = JumpStage::Prepare;
                    jump.stage_ticks = 0;
                }
                (frame, None, None)
            }
        }
    }
}

/// The state of a cell as perception answers it (sight, else memory).
fn cell_state(ctx: &mut Ctx<'_>, pos: BlockPos) -> Option<BlockState> {
    ctx.perception
        .block(Caller::Other, ctx.world, pos)
        .value()
        .map(|fact| fact.state)
}

/// The first block of [`SHELTER_BLOCKS`] the body carries.
fn choose_block(ctx: &Ctx<'_>) -> Option<String> {
    SHELTER_BLOCKS
        .iter()
        .find(|block| count_item(ctx.world, block) > 0)
        .map(|block| (*block).to_owned())
}

/// The block the call builds with: the first of [`SHELTER_BLOCKS`] the body
/// carries, or will carry once it has picked up `extra` (the drops of the cells
/// it is about to dig), whose placed block it can mine back with its drop:
/// the way out mines the cover and maybe a wall ([`leave::exit_check`]), so a
/// block nothing carried gets back (cobblestone without a pickaxe) is never
/// built with. The entry to select, and the placed block's state.
fn build_block(ctx: &Ctx<'_>, extra: &[&str]) -> Option<(String, BlockState)> {
    SHELTER_BLOCKS.iter().find_map(|entry| {
        let state = match carried_block(ctx, entry) {
            Some(state) => state,
            None => block_of(extra.iter().find(|item| entry_matches(entry, item))?)?,
        };
        recoverable(ctx, state).then(|| ((*entry).to_owned(), state))
    })
}

/// The placed block of the first stack the body carries for the
/// [`SHELTER_BLOCKS`] entry `entry`. Slot 0, the grid's result, is a preview,
/// not an owned item.
fn carried_block(ctx: &Ctx<'_>, entry: &str) -> Option<BlockState> {
    let stack = ctx
        .world
        .inventory()
        .iter()
        .skip(1)
        .find(|stack| stack.count() > 0 && crate::common::item_matches(stack, entry))?;
    block_of(praxion_gamedata::normalize(item_name(stack)))
}

/// The block an item places: every [`SHELTER_BLOCKS`] item shares its name.
fn block_of(item: &str) -> Option<BlockState> {
    item.parse::<BlockKind>().ok().map(BlockState::from)
}

/// Whether the body could mine `state` back and get a block of
/// [`SHELTER_BLOCKS`] for it, with what it carries.
fn recoverable(ctx: &Ctx<'_>, state: BlockState) -> bool {
    let held = praxion_headless::held_item(ctx.world);
    drops_shelter_block(state) && choose_tool(state, ctx.world.inventory(), &held) != ToolChoice::Missing
}

/// Whether the hole dug from `origin` (`dug`: its cells, top first) could be
/// left in the morning: the cover is the block [`build_block`] will choose once
/// the dug drops are picked up, and every block but the cover is left for the
/// climb.
fn dig_down_exit(
    ctx: &mut Ctx<'_>,
    origin: BlockPos,
    dug: [BlockState; HOLE_DEPTH as usize],
) -> Result<(), &'static str> {
    let drops: Vec<&str> = dug.into_iter().filter_map(shelter_drop).collect();
    let Some((_, cover)) = build_block(ctx, &drops) else {
        return Err("no_exit");
    };
    let carried = (leave::carried_blocks(ctx) + drops.len() as u32).saturating_sub(1);
    let cell = |depth: i32| BlockPos::new(origin.x, origin.y - depth, origin.z);
    let future = [(cell(1), cover), (cell(2), BlockState::AIR), (cell(3), BlockState::AIR)];
    leave::exit_check(ctx, cell(HOLE_DEPTH), &future, carried)
}

/// Whether the body may wall itself in at `feet`: blocks enough for every open
/// cell of [`shelter_cells`] (and the helper the cover needs on open ground),
/// and a way out of the finished box. `Err(None)` waits for an answer.
fn wall_in_check(ctx: &mut Ctx<'_>, feet: BlockPos) -> Result<(), Option<&'static str>> {
    let Some((block, state)) = build_block(ctx, &[]) else {
        // Nothing to build with, or only blocks the body could not mine back.
        return Err(Some(if choose_block(ctx).is_some() { "no_exit" } else { "no_blocks" }));
    };
    let mut future = Vec::new();
    for cell in shelter_cells(feet) {
        if !closes_shelter(cell_state(ctx, cell).ok_or(None)?) {
            future.push((cell, state));
        }
    }
    let cover = BlockPos::new(feet.x, feet.y + 2, feet.z);
    let helper = future.iter().any(|(cell, _)| *cell == cover)
        && ![(1, 0, 0), (-1, 0, 0), (0, 0, 1), (0, 0, -1), (0, 1, 0)]
            .into_iter()
            .any(|(dx, dy, dz)| {
                cell_state(ctx, BlockPos::new(cover.x + dx, cover.y + dy, cover.z + dz))
                    .is_some_and(|state| blocks::is_solid(state) && place_support(state))
            });
    let placements = future.len() as u32 + u32::from(helper);
    // Only blocks the body can mine back go into the walls.
    let usable: u32 = SHELTER_BLOCKS
        .iter()
        .filter(|entry| carried_block(ctx, entry).is_some_and(|state| recoverable(ctx, state)))
        .map(|entry| count_item(ctx.world, entry))
        .sum();
    if usable < placements {
        log::debug!("shelter: {usable} usable blocks for {placements} placements ({block} first)");
        return Err(Some("no_blocks"));
    }
    let carried = leave::carried_blocks(ctx).saturating_sub(placements);
    leave::exit_check(ctx, feet, &future, carried).map_err(Some)
}

/// The held item's own name, which the place receipt expects (a tag such as
/// `#planks` names the stack that was chosen).
fn held_block(ctx: &Ctx<'_>) -> Option<String> {
    let held = praxion_headless::held_item(ctx.world);
    (!held.is_empty()).then(|| praxion_gamedata::normalize(item_name(&held)).to_owned())
}

/// A support face to place `cell` against: a neighbour of `cell` (not the
/// body's own feet or head cell) whose face toward `cell` is where the
/// crosshair lands. The nearest such face wins.
fn place_aim(
    ctx: &mut Ctx<'_>,
    cell: BlockPos,
    feet: BlockPos,
) -> Option<(BlockPos, Direction, Vec3)> {
    const NEIGHBOURS: [(i32, i32, i32); 6] = [
        (0, -1, 0),
        (1, 0, 0),
        (-1, 0, 0),
        (0, 0, 1),
        (0, 0, -1),
        (0, 1, 0),
    ];
    let head = BlockPos::new(feet.x, feet.y + 1, feet.z);
    let eye = ctx.body.eye_position();
    let mut best: Option<(f64, (BlockPos, Direction, Vec3))> = None;
    for (dx, dy, dz) in NEIGHBOURS {
        let support = BlockPos::new(cell.x + dx, cell.y + dy, cell.z + dz);
        if support == feet || support == head {
            continue;
        }
        let Some(state) = cell_state(ctx, support) else {
            continue;
        };
        if !blocks::is_solid(state) || !place_support(state) {
            continue;
        }
        let face = face_toward(support, cell).expect("a face neighbour");
        // The face's centre, inset into the support so the ray enters it.
        let point = cell_centre(support)
            + Vec3::new(f64::from(-dx), f64::from(-dy), f64::from(-dz)) * 0.45;
        let (yaw, pitch) = praxion_planner::aim(eye, point);
        let hit = praxion_headless::pick_block_looking(ctx.world, ctx.body, yaw, pitch);
        let lands = hit.is_some_and(|hit| !hit.miss && hit.block_pos == support && hit.direction == face);
        if !lands {
            continue;
        }
        let distance = eye.distance_to(point);
        if best.as_ref().is_none_or(|(nearest, _)| distance < *nearest) {
            best = Some((distance, (support, face, point)));
        }
    }
    best.map(|(_, aim)| aim)
}

/// The helper for a cover with no support: an open cell beside the cover whose
/// cell below (a head-level wall) closes the shelter. Its top face is aimed at
/// from where the eye will be at the top of a jump.
fn helper_job(ctx: &mut Ctx<'_>, feet: BlockPos, cover: BlockPos) -> Option<Job> {
    let eye = ctx.body.eye_position();
    let apex = Vec3::new(eye.x, ctx.body.position().y + JUMP_EYE_RISE, eye.z);
    for (dx, dz) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
        let helper = BlockPos::new(cover.x + dx, cover.y, cover.z + dz);
        let wall = BlockPos::new(helper.x, helper.y - 1, helper.z);
        debug_assert_eq!(wall.y, feet.y + 1, "the wall is at head level");
        let open = cell_state(ctx, helper).is_some_and(blocks::is_replaceable);
        let holds = cell_state(ctx, wall).is_some_and(|state| {
            closes_shelter(state) && blocks::is_solid(state) && place_support(state)
        });
        if !open || !holds {
            continue;
        }
        let top = Vec3::new(f64::from(wall.x) + 0.5, f64::from(wall.y) + 1.0, f64::from(wall.z) + 0.5);
        let (yaw, pitch) = praxion_planner::aim(apex, top);
        return Some(Job::new(
            helper,
            JobKind::JumpPlace(Jump {
                support: wall,
                yaw,
                pitch,
                stage: JumpStage::Prepare,
                jumps: 0,
                stage_ticks: 0,
            }),
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use azalea_registry::builtin::ItemKind;
    use praxion_headless::Body;
    use praxion_interpreter::StartAction;
    use praxion_perception::Perception;
    use praxion_planner::Personality;
    use praxion_world::World;

    use super::*;
    use crate::runner::Runner;
    use crate::test_world::{
        give, move_body, move_player, set_block, set_health, swept, world_with_surface,
    };

    fn state(kind: BlockKind) -> BlockState {
        BlockState::from(kind)
    }

    /// A tiny column world for the pure dig rules: dirt from y 63 down,
    /// overrides on top.
    fn lookup(overrides: &HashMap<BlockPos, BlockKind>) -> impl Fn(BlockPos) -> Option<BlockState> + '_ {
        move |pos| {
            let kind = overrides
                .get(&pos)
                .copied()
                .unwrap_or(if pos.y <= 63 { BlockKind::Dirt } else { BlockKind::Air });
            (kind != BlockKind::Barrier).then(|| state(kind))
        }
    }

    const FEET: BlockPos = BlockPos { x: 8, y: 64, z: 8 };

    #[test]
    fn flat_dirt_may_be_dug_three_deep() {
        assert_eq!(plan_dig_down(FEET, &lookup(&HashMap::new())), Ok(()));
    }

    #[test]
    fn the_dig_rules_refuse_fluid_drops_bedrock_supports_and_the_unknown() {
        // A barrier stands for a cell the lookup cannot answer.
        for (cell, kind, refusal) in [
            (BlockPos::new(9, 62, 8), BlockKind::Water, DigRefusal::Fluid),
            (BlockPos::new(8, 60, 8), BlockKind::Lava, DigRefusal::Fluid),
            (BlockPos::new(8, 62, 8), BlockKind::Air, DigRefusal::Drop),
            (BlockPos::new(8, 60, 8), BlockKind::Air, DigRefusal::Drop),
            (BlockPos::new(8, 61, 8), BlockKind::MagmaBlock, DigRefusal::Drop),
            (BlockPos::new(8, 61, 8), BlockKind::Bedrock, DigRefusal::Unbreakable),
            (BlockPos::new(8, 63, 8), BlockKind::CraftingTable, DigRefusal::Support),
            (BlockPos::new(8, 62, 8), BlockKind::Barrier, DigRefusal::Unknown),
        ] {
            let overrides = HashMap::from([(cell, kind)]);
            assert_eq!(
                plan_dig_down(FEET, &lookup(&overrides)),
                Err(refusal),
                "{kind:?} at {cell:?}"
            );
        }
        // The body's own pillar is never dug out from under it.
        scaffold::reset();
        scaffold::note(BlockPos::new(8, 63, 8));
        assert_eq!(
            plan_dig_down(FEET, &lookup(&HashMap::new())),
            Err(DigRefusal::Support)
        );
        scaffold::reset();
    }

    /// A body, its world and a runner running `shelter()`, with the receipts
    /// played by the test: a dig turns the cell to air and the dug dirt lands
    /// in the hotbar; a placement puts the held block in the cell.
    struct Rig {
        world: World,
        perception: Perception,
        body: Body,
        runner: Runner,
        tick: u64,
        dirt: i32,
        dug: Vec<BlockPos>,
        placed: Vec<BlockPos>,
        jumped: bool,
        /// A dug block's drop never reaches the inventory.
        lose_drops: bool,
    }

    impl Rig {
        /// The body stands at [`FEET`] on ground whose top is y 63, `fill`
        /// below it, with `dirt` dirt in the first hotbar slot.
        fn new(fill: impl Fn(BlockPos) -> BlockState, dirt: i32) -> Self {
            let mut world = world_with_surface(1, 63, fill);
            move_player(&mut world, FEET);
            if dirt > 0 {
                give(&mut world, 36, ItemKind::Dirt, dirt);
            }
            let mut body = Body::new(center(FEET));
            for _ in 0..5 {
                body.step(&praxion_headless::Held::NONE, &world);
            }
            assert!(body.on_ground());
            let mut runner = Runner::new();
            runner
                .start(&StartAction {
                    primitive: "shelter".into(),
                    args: Vec::new(),
                })
                .expect("shelter starts");
            Self {
                perception: swept(&world),
                world,
                body,
                runner,
                tick: 0,
                dirt,
                dug: Vec::new(),
                placed: Vec::new(),
                jumped: false,
                lose_drops: false,
            }
        }

        /// Start `primitive` on a fresh runner, the world and body as they are.
        fn start(&mut self, primitive: &str) {
            self.runner = Runner::new();
            self.runner
                .start(&StartAction {
                    primitive: primitive.into(),
                    args: Vec::new(),
                })
                .expect("the primitive starts");
            self.dug.clear();
            self.placed.clear();
        }

        fn set_dirt(&mut self, count: i32) {
            self.dirt = count;
            give(&mut self.world, 36, ItemKind::Dirt, count);
        }

        /// Run until the call ends; the outcome.
        fn run(&mut self, max: u64) -> Option<Outcome> {
            for _ in 0..max {
                self.perception.begin_tick(&self.world);
                let result = {
                    let mut ctx = Ctx {
                        tick: self.tick,
                        body: &self.body,
                        world: &self.world,
                        perception: &mut self.perception,
                        style: Personality::default(),
                    };
                    self.runner.tick(&mut ctx)
                };
                result
                    .frame
                    .apply_with_menu(&mut self.body, self.tick)
                    .expect("frame applies");
                self.jumped |= result.frame.held.jump;
                self.body.step(&result.frame.held, &self.world);
                move_body(&mut self.world, self.body.position());
                self.tick += 1;
                match result.pending {
                    Some(IntentSpec::Dig { pos, .. }) => {
                        self.runner.registered(self.tick);
                        set_block(&mut self.world, pos, BlockState::AIR);
                        self.dug.push(pos);
                        if !self.lose_drops {
                            self.set_dirt(self.dirt + 1);
                        }
                        self.runner.settle(Outcome::Confirmed);
                    }
                    Some(IntentSpec::Place { pos, block, .. }) => {
                        assert_eq!(block, "dirt");
                        self.runner.registered(self.tick);
                        set_block(&mut self.world, pos, state(BlockKind::Dirt));
                        self.placed.push(pos);
                        self.set_dirt(self.dirt - 1);
                        self.runner.settle(Outcome::Confirmed);
                    }
                    Some(other) => panic!("unexpected intent {other:?}"),
                    None => {}
                }
                if let Some(outcome) = result.outcome {
                    return Some(outcome);
                }
            }
            None
        }

        fn feet(&self) -> BlockPos {
            feet_cell(self.body.position())
        }

        fn kind(&self, pos: BlockPos) -> Option<BlockKind> {
            // The test's own view of the world, as the referee would check it.
            self.world.block_state(pos).map(BlockKind::from)
        }

        fn sheltered(&mut self) -> Option<bool> {
            self.perception.begin_tick(&self.world);
            sheltered(&mut self.perception, &self.world)
        }
    }

    fn dirt_ground(pos: BlockPos) -> BlockState {
        state(if pos.y >= 58 { BlockKind::Dirt } else { BlockKind::Stone })
    }

    #[test]
    fn on_diggable_ground_the_body_digs_three_down_and_closes_the_cell_over_its_head() {
        scaffold::reset();
        // No block carried: the dug dirt is what closes the hole.
        let mut rig = Rig::new(dirt_ground, 0);
        let outcome = rig.run(CALL_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Confirmed), "dug {:?}, placed {:?}", rig.dug, rig.placed);
        assert_eq!(
            rig.dug,
            [63, 62, 61].map(|y| BlockPos::new(8, y, 8)),
            "three cells straight down, one receipt each"
        );
        assert_eq!(rig.feet(), BlockPos::new(8, 61, 8), "the body stands at the bottom");
        assert_eq!(rig.placed, [BlockPos::new(8, 63, 8)], "one block over the head");
        assert_eq!(rig.kind(BlockPos::new(8, 63, 8)), Some(BlockKind::Dirt));
        assert_eq!(rig.sheltered(), Some(true), "the fact agrees");
        assert!(!rig.jumped, "no jump in a hole");
    }

    #[test]
    fn where_the_ground_may_not_be_dug_the_body_walls_itself_in() {
        scaffold::reset();
        // Bedrock underfoot: no dig. Ten blocks: eight walls, the helper on a
        // wall and the cover against it.
        let mut rig = Rig::new(|_| state(BlockKind::Bedrock), 10);
        let outcome = rig.run(CALL_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Confirmed), "placed {:?}", rig.placed);
        assert!(rig.dug.is_empty(), "nothing dug");
        assert_eq!(rig.feet(), FEET, "the body stays where it stood");
        for cell in shelter_cells(FEET) {
            assert_eq!(rig.kind(cell), Some(BlockKind::Dirt), "{cell:?} is closed");
        }
        assert_eq!(rig.placed.len(), 10, "{:?}", rig.placed);
        assert_eq!(rig.placed.last(), Some(&BlockPos::new(8, 66, 8)), "the cover comes last");
        assert!(rig.jumped, "the helper went on from the top of a jump");
        assert_eq!(rig.sheltered(), Some(true));
    }

    #[test]
    fn with_nothing_to_build_with_and_no_dig_the_call_fails_no_blocks() {
        scaffold::reset();
        let mut rig = Rig::new(|_| state(BlockKind::Bedrock), 0);
        assert_eq!(rig.run(20), Some(Outcome::Failed("no_blocks".into())));
        assert!(rig.placed.is_empty() && rig.dug.is_empty());
    }

    #[test]
    fn water_beside_the_body_fails_unsafe() {
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 4);
        set_block(&mut rig.world, BlockPos::new(9, 64, 8), state(BlockKind::Water));
        assert_eq!(rig.run(20), Some(Outcome::Failed("unsafe".into())));
        assert!(rig.placed.is_empty() && rig.dug.is_empty());
    }

    #[test]
    fn a_body_already_sealed_in_is_done_at_once() {
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 4);
        for cell in shelter_cells(FEET) {
            set_block(&mut rig.world, cell, state(BlockKind::Stone));
        }
        assert_eq!(rig.run(5), Some(Outcome::Confirmed));
        assert!(rig.placed.is_empty() && rig.dug.is_empty());
    }

    #[test]
    fn a_column_with_a_gravity_block_is_never_dug() {
        for (cell, kind) in [
            (BlockPos::new(8, 63, 8), BlockKind::Sand),
            (BlockPos::new(8, 62, 8), BlockKind::RedSand),
            (BlockPos::new(8, 61, 8), BlockKind::Gravel),
            (BlockPos::new(8, 62, 8), BlockKind::WhiteConcretePowder),
        ] {
            let overrides = HashMap::from([(cell, kind)]);
            assert_eq!(
                plan_dig_down(FEET, &lookup(&overrides)),
                Err(DigRefusal::Gravity),
                "{kind:?} at {cell:?}"
            );
        }
    }

    #[test]
    fn the_cover_is_guaranteed_by_a_carried_block_or_the_first_dug_cell() {
        // Dirt, grass and stone (with the pickaxe `can_dig_down` asks for)
        // drop a block that closes the hole; sand, a leaf or a flower do not.
        for kind in [BlockKind::Dirt, BlockKind::GrassBlock, BlockKind::Stone, BlockKind::Deepslate, BlockKind::OakLog] {
            assert!(cover_guaranteed(false, state(kind)), "{kind:?} drops a cover");
        }
        for kind in [BlockKind::Sand, BlockKind::Gravel, BlockKind::Clay, BlockKind::OakLeaves] {
            assert!(!cover_guaranteed(false, state(kind)), "{kind:?} drops no cover");
            assert!(cover_guaranteed(true, state(kind)), "a carried block is the cover");
        }
    }

    #[test]
    fn on_sand_with_nothing_to_build_with_the_call_fails_before_it_digs() {
        // Item 2 of the run 9 review: a dug sand cell drops sand, which never
        // closes the hole, so the body used to dig in and then fail
        // `no_blocks` at the bottom of an open pit.
        scaffold::reset();
        let mut rig = Rig::new(|_| state(BlockKind::Sand), 0);
        assert_eq!(rig.run(20), Some(Outcome::Failed("no_blocks".into())));
        assert!(rig.dug.is_empty(), "nothing dug: {:?}", rig.dug);
    }

    #[test]
    fn a_shelter_on_a_dirt_path_closes_the_cells_around_the_feet_not_its_own_head() {
        // Item 3: on a 15/16 dirt path the feet stand at y 63.9375, so
        // `floor(y)` named the path itself the feet cell and the call tried
        // to fill the body's own head cell.
        scaffold::reset();
        let mut rig = Rig::new(|_| state(BlockKind::Bedrock), 10);
        set_block(&mut rig.world, BlockPos::new(8, 63, 8), state(BlockKind::DirtPath));
        let mut body = Body::new(center(FEET));
        for _ in 0..10 {
            body.step(&praxion_headless::Held::NONE, &rig.world);
        }
        assert!(body.on_ground() && body.position().y < 64.0, "{:?}", body.position());
        rig.body = body;
        move_body(&mut rig.world, rig.body.position());
        rig.perception = swept(&rig.world);
        let outcome = rig.run(CALL_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Confirmed), "placed {:?}", rig.placed);
        for cell in shelter_cells(FEET) {
            assert_eq!(rig.kind(cell), Some(BlockKind::Dirt), "{cell:?} is closed");
        }
        assert_eq!(rig.sheltered(), Some(true));
    }

    #[test]
    fn in_the_morning_the_body_mines_its_cover_and_pillars_out_of_the_hole() {
        // Decision 2026-11-10 "Shelter shape and exit".
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 0);
        assert_eq!(rig.run(CALL_TICKS.into()), Some(Outcome::Confirmed));
        assert_eq!(rig.feet(), BlockPos::new(8, 61, 8));
        rig.start("leave_shelter");
        let outcome = rig.run(leave::LEAVE_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Confirmed), "dug {:?}, placed {:?}", rig.dug, rig.placed);
        assert_eq!(rig.dug, [BlockPos::new(8, 63, 8)], "the cover, picked back up");
        assert_eq!(
            rig.placed,
            [61, 62, 63].map(|y| BlockPos::new(8, y, 8)),
            "one pillar block per step, from the hole's floor up"
        );
        assert_eq!(rig.feet(), FEET, "the body stands on the surface");
        assert_eq!(rig.sheltered(), Some(false));
        assert_eq!(rig.dirt, 0, "every dug block went into the climb");
    }

    #[test]
    fn in_the_morning_a_walled_in_body_mines_its_cover_and_a_door() {
        scaffold::reset();
        let mut rig = Rig::new(|_| state(BlockKind::Bedrock), 10);
        assert_eq!(rig.run(CALL_TICKS.into()), Some(Outcome::Confirmed));
        rig.start("leave_shelter");
        let outcome = rig.run(leave::LEAVE_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Confirmed), "dug {:?}, placed {:?}", rig.dug, rig.placed);
        assert_eq!(rig.dug.first(), Some(&BlockPos::new(8, 66, 8)), "the cover first");
        assert_eq!(rig.dug.len(), 3, "the cover and a two-block door: {:?}", rig.dug);
        let (door_high, door_low) = (rig.dug[1], rig.dug[2]);
        assert_eq!((door_high.y, door_low.y), (65, 64), "head level, then feet level");
        assert_eq!((door_high.x, door_high.z), (door_low.x, door_low.z), "one side");
        assert!(rig.placed.is_empty(), "no climb: {:?}", rig.placed);
        assert_eq!(rig.feet(), FEET);
        assert_eq!(rig.dirt, 3, "the three mined blocks came back");
        assert_eq!(rig.sheltered(), Some(false));
    }

    #[test]
    fn a_body_in_the_open_is_out_at_once() {
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 0);
        rig.start("leave_shelter");
        assert_eq!(rig.run(5), Some(Outcome::Confirmed));
        assert!(rig.dug.is_empty() && rig.placed.is_empty());
    }

    #[test]
    fn a_hole_with_nothing_to_climb_with_fails_no_exit_and_stays_sealed() {
        // Run 9 review item 1b: the cover used to be mined first, so a climb
        // that then failed left the body in an open pit at dawn.
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 0);
        assert_eq!(rig.run(CALL_TICKS.into()), Some(Outcome::Confirmed));
        // The cover's own dirt is one block of the three the climb needs.
        rig.set_dirt(1);
        rig.start("leave_shelter");
        let outcome = rig.run(leave::LEAVE_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Failed("no_exit".into())), "dug {:?}", rig.dug);
        assert!(rig.dug.is_empty(), "the cover stays: {:?}", rig.dug);
        assert_eq!(rig.sheltered(), Some(true), "still sealed in");
    }

    #[test]
    fn a_hurt_body_still_climbs_out_of_its_hole() {
        // Run 9 review item 1a: the body takes shelter at ten health or less,
        // and the pillar refused to climb below ten, so it never came out.
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 0);
        assert_eq!(rig.run(CALL_TICKS.into()), Some(Outcome::Confirmed));
        set_health(&mut rig.world, 6.0);
        rig.start("leave_shelter");
        let outcome = rig.run(leave::LEAVE_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Confirmed), "dug {:?}, placed {:?}", rig.dug, rig.placed);
        assert_eq!(rig.feet(), FEET, "the body stands on the surface");
        assert_eq!(rig.sheltered(), Some(false));
    }

    #[test]
    fn water_over_the_climb_fails_unsafe_before_the_cover_is_mined() {
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 4);
        assert_eq!(rig.run(CALL_TICKS.into()), Some(Outcome::Confirmed));
        // Water beside the column at the last step's feet level.
        set_block(&mut rig.world, BlockPos::new(9, 64, 8), state(BlockKind::Water));
        // Seen from the surface (the body looked around before it dug in).
        move_body(&mut rig.world, center(FEET));
        for _ in 0..10 {
            rig.perception.begin_tick(&rig.world);
            rig.perception.sweep(&rig.world);
        }
        move_body(&mut rig.world, rig.body.position());
        rig.start("leave_shelter");
        let outcome = rig.run(leave::LEAVE_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Failed("unsafe".into())), "dug {:?}", rig.dug);
        assert!(rig.dug.is_empty(), "the cover stays: {:?}", rig.dug);
    }

    #[test]
    fn leaves_over_the_mouth_wall_the_body_in_instead_of_digging_a_hole_it_cannot_leave() {
        // Run 9, third review: the climb out of the hole needs the cells over
        // its mouth free (`pillar`'s headroom), so a hole under a leaf was a
        // shelter the body could not leave. The way out is checked first.
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 10);
        set_block(&mut rig.world, BlockPos::new(8, 66, 8), state(BlockKind::OakLeaves));
        let outcome = rig.run(CALL_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Confirmed), "dug {:?}, placed {:?}", rig.dug, rig.placed);
        assert!(rig.dug.is_empty(), "no hole under the leaves: {:?}", rig.dug);
        assert_eq!(rig.feet(), FEET, "walled in where it stood");
        assert_eq!(rig.placed.len(), 8, "eight walls, the leaves are the cover: {:?}", rig.placed);
        assert_eq!(rig.sheltered(), Some(true));
        rig.start("leave_shelter");
        let outcome = rig.run(leave::LEAVE_TICKS.into());
        assert_eq!(outcome, Some(Outcome::Confirmed), "dug {:?}, placed {:?}", rig.dug, rig.placed);
        assert_eq!(rig.sheltered(), Some(false), "and it gets out in the morning");
    }

    #[test]
    fn too_few_blocks_to_climb_out_or_wall_in_fail_before_anything_is_dug() {
        // Clay drops no building block: one carried dirt would be the cover,
        // and the climb out of three cells would have only the cover's own
        // drop. Walling in needs ten. Nothing is dug or placed.
        scaffold::reset();
        let mut rig = Rig::new(|_| state(BlockKind::Clay), 1);
        rig.lose_drops = true;
        assert_eq!(rig.run(20), Some(Outcome::Failed("no_blocks".into())));
        assert!(rig.dug.is_empty() && rig.placed.is_empty(), "dug {:?}, placed {:?}", rig.dug, rig.placed);
        assert_eq!(rig.sheltered(), Some(false));
    }

    #[test]
    fn a_cobblestone_cover_without_a_pickaxe_is_never_built() {
        // Cobblestone mined by hand drops nothing, so a cover (or a wall) of
        // it could not be opened in the morning: the call fails `no_exit`
        // before it digs or places anything.
        scaffold::reset();
        let mut rig = Rig::new(|_| state(BlockKind::Clay), 0);
        rig.lose_drops = true;
        give(&mut rig.world, 37, ItemKind::Cobblestone, 16);
        assert_eq!(rig.run(20), Some(Outcome::Failed("no_exit".into())));
        assert!(rig.dug.is_empty() && rig.placed.is_empty(), "dug {:?}, placed {:?}", rig.dug, rig.placed);
    }

    #[test]
    fn the_dig_plan_ledgers_its_privileged_column_reads() {
        scaffold::reset();
        let mut rig = Rig::new(dirt_ground, 0);
        rig.run(3);
        let entry = rig
            .perception
            .ledger()
            .get(QueryKind::SkyColumn, FactKind::Column)
            .expect("the hidden cells under the feet are a ledgered privileged read");
        assert_eq!(entry.count(), 5, "the body's column and its four neighbours");
    }
}
