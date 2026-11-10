//! `leave_shelter()`: open the shelter in the morning and climb out (decision
//! 2026-11-10 "Shelter shape and exit": a body left sealed in at dawn fails
//! every ladder goal). It works for both shapes `shelter()` builds, and for any
//! pit or box like them, because it plans from the cells around the body each
//! time it stands on the ground, never from a remembered shape:
//!
//! 1. **Out?** The call is done when nothing is over the head and the body can
//!    walk away on the surface: a side cell it can stand in (two open cells
//!    over a solid floor) with a second such cell beyond it, level or one
//!    lower ([`is_out`]). A cell perception cannot answer is not open, so the
//!    call never claims "out" on a guess.
//! 2. **The cover.** A block over the head is mined first, but only once the
//!    way out is certain: a side door ([`door_cell`]), or a climb whose every
//!    known condition holds ([`climb_check`]: blocks enough, carried or the
//!    cover's own drop, headroom, a solid support, no fluid, the column limit).
//!    A refused climb fails with the cover still in place, so the shelter stays
//!    sealed and the skill tries again later (run 9 review, item 1).
//! 3. **A side door.** Where a wall can be opened onto the surface (the
//!    wall-in box), its head-level block and then its feet-level block are
//!    mined, and the body is out.
//! 4. **Climb.** Otherwise (the dug hole: its sides are ground with more
//!    ground above) the body stacks one block under its feet with `pillar`'s
//!    own state machine and plans again from the new height. The pillar's
//!    health rule is off here ([`Pillar::climbing_out`]): the hole walls the
//!    column, so the climb cannot end in a fall, and the body went in hurt.
//!
//! ```text
//!  dug hole (three pillar steps)      wall-in box (a door in one side)
//!   y0  .  .  .     out: y0 .  P  .    y1  #  [C] .     C: cover, mined
//!   y-1 # [C] #          #  P  #       y0  # head D     D: door, two digs
//!   y-2 # head#          #  P  #       y-1 # feet D
//!   y-3 # feet#          #     #
//! ```
//!
//! Every block it mines drops next to the body, which picks it up on the way
//! (the cover falls into the hole onto it); at the end it walks to any drop
//! still lying within reach for a few ticks before it confirms. The pillar
//! blocks stay, like any pillar column.
//!
//! Typed failures: `no_exit` (no block to climb with, or no way out the
//! pillar rules allow: the column limit, no headroom, a cover that drops
//! nothing with any carried tool), `unsafe` (fluid at the cover, or the pillar's
//! own fire and fluid rules), `stuck` (bounds).

use azalea_core::position::BlockPos;
use azalea_registry::builtin::EntityKind;
use praxion_facts::feet_cell;
use praxion_interpreter::Outcome;
use praxion_perception::{Answer, Caller, EntityFilter, exposes_fluid, scaffold};
use praxion_planner::blocks;
use praxion_world::BlockState;

use super::{
    AIRBORNE_TICKS, ITEM_TICKS, Job, JobKind, SHELTER_BLOCKS, Shelter, Step, cell_state,
    choose_block, drops_shelter_block, failed, is_gravity, is_workstation,
};
use crate::common::{ToolChoice, choose_tool, count_item, idle, pickup_frame};
use crate::pillar::{MAX_COLUMN, Pillar};
use crate::place::place_support;
use crate::runner::Ctx;

/// The whole call's bound: the cover (up to 80 ticks of tool and aim, then
/// the break), three pillar steps of up to 165 ticks each (`pillar`'s own
/// bound), or two door digs, and the drop collection. The registry budget is
/// above this, so the call always ends with its own typed kind first.
pub(crate) const LEAVE_TICKS: u32 = 720;
/// Ticks spent walking to drops near the body once it is out.
const COLLECT_TICKS: u32 = 30;
/// How far from the eye a drop may lie and still be collected.
const COLLECT_RADIUS: u32 = 3;
/// How far above or below the feet a drop may lie and still be collected (one
/// left at the bottom of the hole is not walked back to).
const COLLECT_DY: f64 = 1.5;

const SIDES: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

fn at(cell: BlockPos, dx: i32, dy: i32, dz: i32) -> BlockPos {
    BlockPos::new(cell.x + dx, cell.y + dy, cell.z + dz)
}

impl Shelter {
    pub(super) fn tick_leave(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let frame = idle(ctx);
        self.ticks += 1;
        if self.ticks > LEAVE_TICKS {
            return failed(frame, "stuck");
        }
        if let Some(pillar) = &mut self.pillar {
            let (frame, pending, outcome) = pillar.tick(ctx, self.pillar_started);
            return match outcome {
                Some(Outcome::Failed(kind)) => {
                    log::debug!("leave_shelter: the pillar step failed {kind}");
                    failed(frame, pillar_failure(&kind))
                }
                // The pillar never confirms by itself: its receipt does,
                // through `note_settled`.
                other => (frame, pending, other),
            };
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
        let feet = feet_cell(ctx.body.position());
        if !self.out && is_out(ctx, feet) {
            log::debug!("leave_shelter: out at {feet:?}");
            self.out = true;
        }
        if self.out {
            return self.collect(ctx);
        }
        // The cover.
        let over = at(feet, 0, 2, 0);
        let Some(state) = cell_state(ctx, over) else {
            return self.wait_for_answer(frame);
        };
        if !blocks::is_passable(state) {
            let carried = carried_blocks(ctx);
            if door_cell(ctx, feet, &[]).is_none()
                && let Err(kind) = climb_check(ctx, feet, state, &[], carried)
            {
                log::debug!("leave_shelter: no climb out of {feet:?} ({kind}); the cover stays");
                return failed(frame, kind);
            }
            return match cover_dig(ctx, over, state) {
                Ok(()) => {
                    self.start(Job::new(over, JobKind::Dig));
                    (frame, None, None)
                }
                Err(kind) => failed(frame, kind),
            };
        }
        // A door in a wall.
        if let Some(cell) = door_cell(ctx, feet, &[]) {
            self.start(Job::new(cell, JobKind::Dig));
            return (frame, None, None);
        }
        // Climb one block.
        let Some(block) = choose_block(ctx) else {
            // The cover's drop shows up a few ticks after the dig.
            self.item_ticks += 1;
            if self.item_ticks > ITEM_TICKS {
                return failed(frame, "no_exit");
            }
            return (frame, None, None);
        };
        self.item_ticks = 0;
        log::debug!("leave_shelter: pillar {block} at {feet:?}");
        self.pillar = Some(Pillar::climbing_out(block));
        self.pillar_started = false;
        (frame, None, None)
    }

    /// Walk to a drop near the body for a few ticks, then confirm.
    fn collect(&mut self, ctx: &mut Ctx<'_>) -> Step {
        let frame = idle(ctx);
        self.collect_ticks += 1;
        if self.collect_ticks > COLLECT_TICKS {
            return (frame, None, Some(Outcome::Confirmed));
        }
        let items = match ctx.perception.entities_near(
            Caller::Other,
            ctx.world,
            COLLECT_RADIUS,
            EntityFilter::Kind(EntityKind::Item),
        ) {
            Answer::Known { value, .. } => value,
            // Unanswerable this tick is not "nothing dropped": look again.
            Answer::Unknown(_) => return (frame, None, None),
        };
        let y = ctx.body.position().y;
        let nearest = items
            .iter()
            .filter(|item| (item.position.y - y).abs() <= COLLECT_DY)
            .min_by(|a, b| a.distance.total_cmp(&b.distance));
        match nearest {
            Some(item) => (pickup_frame(ctx, item.position), None, None),
            None => (frame, None, Some(Outcome::Confirmed)),
        }
    }
}

/// A pillar step's failure, as `leave_shelter`'s own kind.
fn pillar_failure(kind: &str) -> &'static str {
    match kind {
        "no_blocks" | "too_high" | "no_headroom" => "no_exit",
        "unsafe" => "unsafe",
        _ => "stuck",
    }
}

/// Whether the body is out: nothing over its head, and a side cell it can
/// stand in with a second one beyond it, level or one lower (the surface, not
/// a notch in a pit wall). Unknown cells are not open.
pub(crate) fn is_out(ctx: &mut Ctx<'_>, feet: BlockPos) -> bool {
    if !known(ctx, &[], at(feet, 0, 2, 0)).is_some_and(blocks::is_passable) {
        return false;
    }
    SIDES.into_iter().any(|(dx, dz)| {
        standable(ctx, &[], at(feet, dx, 0, dz)) && onward(ctx, &[], at(feet, 2 * dx, 0, 2 * dz))
    })
}

/// Whether a body standing at `feet`, sealed under the cover at `feet + 2`,
/// can get out: the checks `leave_shelter` makes before it mines the cover (a
/// door, else the climb; then the cover itself). `shelter()` asks it before it
/// digs or seals anything, over the cells as they will be once it is done
/// (`future`) and the blocks it will still carry (`carried`, the cover aside),
/// so it never builds a shelter it cannot leave (run 9, third review).
pub(super) fn exit_check(
    ctx: &mut Ctx<'_>,
    feet: BlockPos,
    future: &[(BlockPos, BlockState)],
    carried: u32,
) -> Result<(), &'static str> {
    let over = at(feet, 0, 2, 0);
    let Some(cover) = known(ctx, future, over) else {
        return Err("no_exit");
    };
    if door_cell(ctx, feet, future).is_none() {
        climb_check(ctx, feet, cover, future, carried)?;
    }
    cover_dig(ctx, over, cover)
}

/// The blocks the body carries that a climb may stand on.
pub(super) fn carried_blocks(ctx: &Ctx<'_>) -> u32 {
    SHELTER_BLOCKS.iter().map(|block| count_item(ctx.world, block)).sum()
}

/// A cell the body can stand in: it and the cell over it open, a solid floor.
fn standable(ctx: &mut Ctx<'_>, future: &[(BlockPos, BlockState)], cell: BlockPos) -> bool {
    open(ctx, future, cell)
        && open(ctx, future, at(cell, 0, 1, 0))
        && solid(ctx, future, at(cell, 0, -1, 0))
}

/// The cell beyond the first: standable level, or one step down.
fn onward(ctx: &mut Ctx<'_>, future: &[(BlockPos, BlockState)], cell: BlockPos) -> bool {
    standable(ctx, future, cell)
        || (open(ctx, future, at(cell, 0, 1, 0)) && standable(ctx, future, at(cell, 0, -1, 0)))
}

fn open(ctx: &mut Ctx<'_>, future: &[(BlockPos, BlockState)], cell: BlockPos) -> bool {
    known(ctx, future, cell)
        .is_some_and(|state| blocks::is_passable(state) && blocks::fluid(state).is_none())
}

fn solid(ctx: &mut Ctx<'_>, future: &[(BlockPos, BlockState)], cell: BlockPos) -> bool {
    known(ctx, future, cell).is_some_and(|state| blocks::is_solid(state) && !blocks::is_hazard(state))
}

/// A cell as it will be (`future`, the cells `shelter()` is about to dig or
/// close), else as perception answers it now.
fn known(ctx: &mut Ctx<'_>, future: &[(BlockPos, BlockState)], cell: BlockPos) -> Option<BlockState> {
    match future.iter().find(|(pos, _)| *pos == cell) {
        Some((_, state)) => Some(*state),
        None => cell_state(ctx, cell),
    }
}

/// Before the cover of a hole is mined: whether the climb out can be done,
/// judged by `pillar`'s own rules over the whole column at once, so a refused
/// climb leaves the shelter sealed (run 9 review, item 1). The climb is the
/// lowest height with a way out onto the surface; a way out perception cannot
/// see from inside the hole counts as the column limit, the most blocks a
/// climb may take. A cell perception cannot answer is not a refusal: from
/// inside a sealed hole the cells over the cover may be out of sight and
/// memory, and the pillar asks again on every step once the cover is open.
/// `no_exit`: too few blocks (`carried`, plus the cover's own drop), no
/// headroom, or the column limit; `unsafe`: fluid in or beside the column, or
/// a support the pillar refuses. Cells in `future` are read as they will be
/// ([`exit_check`]); `leave_shelter` passes none.
fn climb_check(
    ctx: &mut Ctx<'_>,
    feet: BlockPos,
    cover: BlockState,
    future: &[(BlockPos, BlockState)],
    carried: u32,
) -> Result<(), &'static str> {
    let need = (1..=MAX_COLUMN)
        .find(|&k| {
            let level = at(feet, 0, k as i32, 0);
            SIDES.into_iter().any(|(dx, dz)| {
                standable(ctx, future, at(level, dx, 0, dz))
                    && onward(ctx, future, at(level, 2 * dx, 0, 2 * dz))
            })
        })
        .unwrap_or(MAX_COLUMN);
    let support = at(feet, 0, -1, 0);
    if scaffold::column_height(support) + need > MAX_COLUMN {
        return Err("no_exit");
    }
    let drop = u32::from(drops_shelter_block(cover));
    if carried + drop < need {
        log::debug!("leave_shelter: {carried} blocks and {drop} from the cover, {need} to climb");
        return Err("no_exit");
    }
    if let Some(state) = known(ctx, future, support)
        && (blocks::fluid(state).is_some() || !blocks::is_solid(state) || !place_support(state))
    {
        return Err("unsafe");
    }
    // The cells the body passes through, the cover's own cell aside: each
    // step needs its feet cell free for the block and three free cells over
    // it, as `pillar` checks.
    let need = need as i32;
    for up in (0..need + 3).filter(|&up| up != 2) {
        let Some(state) = known(ctx, future, at(feet, 0, up, 0)) else {
            continue;
        };
        if blocks::fluid(state).is_some() {
            return Err("unsafe");
        }
        if (!state.is_air() && !blocks::is_passable(state)) || (up < need && !blocks::is_replaceable(state)) {
            return Err("no_exit");
        }
    }
    // No fluid beside the column at any step.
    for dy in -1..=need {
        for dx in -1..=1 {
            for dz in -1..=1 {
                if dx == 0 && dz == 0 {
                    continue;
                }
                let near = known(ctx, future, at(feet, dx, dy, dz));
                if near.is_some_and(|state| blocks::fluid(state).is_some()) {
                    return Err("unsafe");
                }
            }
        }
    }
    Ok(())
}

/// Whether the cover may be mined: no fluid in it or known over it, breakable,
/// and some carried item (or the hand) gets its drop back.
fn cover_dig(ctx: &mut Ctx<'_>, cover: BlockPos, state: BlockState) -> Result<(), &'static str> {
    let above = ctx.perception.known(at(cover, 0, 1, 0)).map(|seen| seen.state);
    if blocks::fluid(state).is_some() || above.is_some_and(|state| blocks::fluid(state).is_some()) {
        return Err("unsafe");
    }
    if !diggable(ctx, cover, state) {
        log::debug!("leave_shelter: cannot mine the cover {:?} at {cover:?}", blocks::name(state));
        return Err("no_exit");
    }
    Ok(())
}

/// A wall block the call may mine: breakable, no fluid in it or known beside
/// it, never scaffolding or a workstation, and its drop comes back with what
/// the body carries.
fn diggable(ctx: &mut Ctx<'_>, cell: BlockPos, state: BlockState) -> bool {
    let held = praxion_headless::held_item(ctx.world);
    let memory = |pos: BlockPos| ctx.perception.known(pos).map(|seen| seen.state);
    blocks::fluid(state).is_none()
        && !blocks::is_hazard(state)
        && blocks::is_breakable(state)
        && !scaffold::is_scaffold(cell)
        && !is_workstation(state)
        && !exposes_fluid(cell, &memory)
        && choose_tool(state, ctx.world.inventory(), &held) != ToolChoice::Missing
}

/// The next block to mine for a door: on the side whose door leads onto the
/// surface (a solid floor under it, the surface beyond it, no gravity block
/// over it) with the fewest blocks to mine, its head-level block first.
fn door_cell(ctx: &mut Ctx<'_>, feet: BlockPos, future: &[(BlockPos, BlockState)]) -> Option<BlockPos> {
    let mut best: Option<(usize, BlockPos)> = None;
    for (dx, dz) in SIDES {
        let low = at(feet, dx, 0, dz);
        let high = at(low, 0, 1, 0);
        if !solid(ctx, future, at(low, 0, -1, 0)) || !onward(ctx, future, at(feet, 2 * dx, 0, 2 * dz)) {
            continue;
        }
        if known(ctx, future, at(high, 0, 1, 0)).is_none_or(is_gravity) {
            continue;
        }
        let mut digs = Vec::new();
        let mut ok = true;
        for cell in [high, low] {
            match known(ctx, future, cell) {
                Some(state) if blocks::is_passable(state) && blocks::fluid(state).is_none() => {}
                Some(state) if diggable(ctx, cell, state) => digs.push(cell),
                _ => ok = false,
            }
        }
        // `digs` is never empty here: with both cells open the body is out.
        if ok
            && let Some(first) = digs.first()
            && best.is_none_or(|(count, _)| digs.len() < count)
        {
            best = Some((digs.len(), *first));
        }
    }
    best.map(|(_, cell)| cell)
}
