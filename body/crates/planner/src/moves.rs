//! Our moves over the block graph. Only the A* core and the cost constants are
//! Azalea's; these successors are written for Praxion.
//!
//! Cost model, all in expected ticks:
//! - walk and diagonal: Azalea's sprint cost per block;
//! - step up: `max(walk, jump) + JUMP_PENALTY`;
//! - descend: `WALK_OFF + max(FALL_N[d], CENTER_AFTER_FALL)`;
//! - dig: sprint cost plus `hardness * 100 / best-tool-speed` per broken block
//!   plus `BLOCK_BREAK_ADDITIONAL_PENALTY`;
//! - bridge and pillar: walking plus a fixed place cost.
//!
//! Hard limits (session 6, contract §5.5): a protected or blacklisted cell is
//! never broken, placed on, or occupied; no move digs straight down; a dig is
//! refused when the block or one of its six neighbours is unknown, is a fluid,
//! or when a face neighbour is a fluid (removing it would let the fluid flow
//! into the path).
//!
//! Every dig and place move carries the primitive call it will run, built from
//! the matching [`PrimitiveRecord`] (name and version), with the position in
//! [`PrimitiveCall::target`] and the record's arguments in `args`. Effects,
//! cancellation class and evidence therefore exist once, in the rules
//! registry.

use std::collections::HashMap;

use praxion_rules::{ArgValue, Effects, PrimitiveRecord, Primitives};
use praxion_world::BlockPos;

use crate::astar::{Edge, Movement};
use crate::blocks;
use crate::costs::{
    BLOCK_BREAK_ADDITIONAL_PENALTY, CENTER_AFTER_FALL_COST, ENTER_WATER_PENALTY,
    FALL_N_BLOCKS_COST, JUMP_ONE_BLOCK_COST, JUMP_PENALTY, SPRINT_ONE_BLOCK_COST,
    WALK_OFF_BLOCK_COST, WALK_ONE_BLOCK_COST, WALK_ONE_IN_WATER_COST,
};
use crate::state::{Inventory, Placements};
use crate::world::{Cell, WorldView, neighbors6, offset};

/// A node: the feet cell, whether the player is standing on a block this path
/// just placed (so a second pillar step has support), and how many blocks the
/// plan may still place from each placeable hotbar slot. The budget is part of
/// the key, so a route that used more blocks is a different state and can never
/// place more than the hotbar holds.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Node {
    pub pos: BlockPos,
    pub standing_on_placed: bool,
    pub placements: Placements,
}

pub type PlanEdge = Edge<Node, MoveData>;

/// One primitive invocation a move will run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PrimitiveCall {
    pub name: &'static str,
    pub version: u32,
    /// The cell the primitive acts on.
    pub target: BlockPos,
    pub args: Vec<ArgValue>,
}

impl PrimitiveCall {
    fn mine(primitives: &Primitives, target: BlockPos, state: azalea_block::BlockState) -> Self {
        let record = primitives
            .get("mine")
            .expect("the rules registry always has `mine`");
        Self::from_record(record, target, vec![ArgValue::Block(blocks::name(state).to_owned())])
    }

    fn place(primitives: &Primitives, target: BlockPos, item: &str) -> Self {
        let record = primitives
            .get("place")
            .expect("the rules registry always has `place`");
        Self::from_record(record, target, vec![ArgValue::Block(item.to_owned())])
    }

    fn from_record(record: &PrimitiveRecord, target: BlockPos, args: Vec<ArgValue>) -> Self {
        Self {
            name: record.name,
            version: record.version,
            target,
            args,
        }
    }

    /// The registry record this call names.
    pub fn record<'a>(&self, primitives: &'a Primitives) -> Option<&'a PrimitiveRecord> {
        primitives.get(self.name)
    }

    /// The declared effects for this call, from the registry.
    pub fn effects<'a>(&self, primitives: &'a Primitives) -> Option<&'a Effects> {
        self.record(primitives)
            .map(|record| record.effects_for(&self.args))
    }
}

/// What kind of movement an edge is. The follower switches on this; the
/// primitive layer runs `primitives` before the movement for the world-changing
/// kinds.
///
/// Two kinds carry a distance in their geometry rather than a field, because
/// the independent reviewer oracle reads `from`/`to` and classifies every
/// water step by kind: `Swim` is one cell in water, sideways when
/// `to.y == from.y` and down one when `to.y == from.y - 1`, and `EnterWater`
/// is a step into water, level when `to.y == from.y` and a fall of
/// `from.y - to.y` blocks otherwise.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MoveKind {
    Walk,
    Diagonal,
    StepUp,
    Descend { drop: i32 },
    /// Break the listed cells (feet and/or head of the target) and step in.
    Dig { broken: Vec<BlockPos> },
    /// Place a block under the gap cell and cross from one side to the other.
    Bridge { placed: BlockPos, over: BlockPos },
    /// Jump, place a block under the feet, and stand on it.
    Pillar { placed: BlockPos },
    /// Step or drop from land into a water cell; the drop is in the geometry.
    EnterWater,
    /// Swim one cell under water: sideways, or down one from the surface.
    Swim,
    /// Rise one cell in a water column (hold jump).
    SwimUp,
    /// Leave the water onto standable land; `rise` is one block up (also hold
    /// jump).
    ExitWater { rise: bool },
}

impl MoveKind {
    pub fn name(&self) -> &'static str {
        match self {
            MoveKind::Walk => "walk",
            MoveKind::Diagonal => "diagonal",
            MoveKind::StepUp => "step_up",
            MoveKind::Descend { .. } => "descend",
            MoveKind::Dig { .. } => "dig",
            MoveKind::Bridge { .. } => "bridge",
            MoveKind::Pillar { .. } => "pillar",
            MoveKind::EnterWater => "enter_water",
            MoveKind::Swim => "swim",
            MoveKind::SwimUp => "swim_up",
            MoveKind::ExitWater { .. } => "exit_water",
        }
    }

    pub fn changes_world(&self) -> bool {
        matches!(
            self,
            MoveKind::Dig { .. } | MoveKind::Bridge { .. } | MoveKind::Pillar { .. }
        )
    }
}

/// The payload of one A* movement.
#[derive(Clone, Debug)]
pub struct MoveData {
    pub kind: MoveKind,
    pub primitives: Vec<PrimitiveCall>,
    pub cost: f32,
}

/// The planning context the successors need.
///
/// Two lifetimes so the borrow of the [`WorldView`] can be shorter than the
/// borrow of the block source it wraps; otherwise a second search could not
/// reuse the view.
pub(crate) struct MoveCtx<'w, 'a> {
    pub world: &'w mut WorldView<'a>,
    pub inventory: &'a Inventory,
    pub primitives: &'a Primitives,
    pub max_drop: i32,
    pub allow_world_changes: bool,
    pub frontier: &'w mut HashMap<BlockPos, Node>,
}

const DIRS: [(i32, i32); 4] = [(0, 1), (0, -1), (1, 0), (-1, 0)];

/// The four diagonal offsets. A diagonal step needs two open side cells, so it
/// cannot reuse [`DIRS`]: Azalea builds it from a direction plus its right
/// neighbour (`basic.rs:411-415`), which is the same set.
const DIAGONALS: [(i32, i32); 4] = [(1, 1), (1, -1), (-1, 1), (-1, -1)];

/// A block placement's fixed cost, in ticks (the `place` record's
/// `default_max_ticks` is 20; the action itself is a few ticks).
pub const PLACE_BLOCK_COST: f32 = 4.;

pub(crate) fn successors(ctx: &mut MoveCtx, node: Node) -> Vec<PlanEdge> {
    let mut edges = Vec::new();
    if ctx.world.water(node.pos) == Cell::Free {
        // A swimming body has only water moves: walking, digging and building
        // need a footing the water does not give.
        water_moves(ctx, node, &mut edges);
        return edges;
    }
    walk(ctx, node, &mut edges);
    diagonal(ctx, node, &mut edges);
    step_up(ctx, node, &mut edges);
    descend(ctx, node, &mut edges);
    dig_ahead(ctx, node, &mut edges);
    bridge(ctx, node, &mut edges);
    pillar_up(ctx, node, &mut edges);
    enter_water(ctx, node, &mut edges);
    edges
}

/// Azalea's base cost of one block moved from a cell (`basic.rs:24-34`,
/// `:397-409`): the sprint cost on open ground, the in-water walk cost when the
/// feet cell is water. Praxion does not use Azalea's `BARITONE_COMPAT` branch
/// (that one uses `WALK_ONE_BLOCK_COST` in water); `costs.rs` carries the
/// non-Baritone `WALK_ONE_IN_WATER_COST`, which is what [`MoveKind::Swim`]
/// charges too.
fn step_cost(ctx: &mut MoveCtx, pos: BlockPos) -> f32 {
    if ctx.world.water(pos) == Cell::Free {
        WALK_ONE_IN_WATER_COST
    } else {
        SPRINT_ONE_BLOCK_COST
    }
}

fn note_unknown(ctx: &mut MoveCtx, cell: BlockPos, parent: Node) {
    ctx.frontier.entry(cell).or_insert(parent);
}

/// Whether both body cells at `pos` are free. `Err(cell)` names an unknown
/// cell that stopped the check.
fn body_free(ctx: &mut MoveCtx, pos: BlockPos) -> Result<bool, BlockPos> {
    ctx.world.body_free(pos)
}

/// Whether `pos` has a solid floor. `Err(cell)` names an unknown cell.
fn floor_free(ctx: &mut MoveCtx, pos: BlockPos) -> Result<bool, BlockPos> {
    ctx.world.floor_free(pos)
}

fn push(
    edges: &mut Vec<PlanEdge>,
    target: BlockPos,
    standing_on_placed: bool,
    placements: Placements,
    kind: MoveKind,
    cost: f32,
    primitives: Vec<PrimitiveCall>,
) {
    edges.push(Edge {
        movement: Movement {
            target: Node {
                pos: target,
                standing_on_placed,
                placements,
            },
            data: MoveData {
                kind,
                primitives,
                cost,
            },
        },
        cost,
    });
}

fn walk(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    // Azalea `basic.rs:24-34`: the sprint cost on open ground, the in-water
    // walk cost when the feet cell is water. `successors` sends a water node to
    // `water_moves`, so in practice this is the sprint branch; keeping the
    // water branch here puts Azalea's selection in the one place that costs a
    // step.
    let base_cost = step_cost(ctx, pos);
    for (dx, dz) in DIRS {
        let target = offset(pos, dx, 0, dz);
        match body_free(ctx, target) {
            Err(unknown) => note_unknown(ctx, unknown, node),
            Ok(false) => {}
            Ok(true) => match floor_free(ctx, target) {
                Ok(true) => push(
                    edges,
                    target,
                    false,
                    node.placements,
                    MoveKind::Walk,
                    base_cost,
                    Vec::new(),
                ),
                Ok(false) => {}
                Err(unknown) => note_unknown(ctx, unknown, node),
            },
        }
    }
}

fn diagonal(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    let base_cost = step_cost(ctx, pos);
    // Azalea `basic.rs:408-409`: +0.001 as a tie-breaker so a straight step is
    // preferred when the two cost the same.
    let straight_diagonal = base_cost.mul_add(std::f32::consts::SQRT_2, 0.001);
    // Azalea `basic.rs:453-460`: a diagonal that has only one open side hugs
    // the wall and pays half a walk block for it.
    let wall_hug = straight_diagonal + WALK_ONE_BLOCK_COST / 2.;
    for (dx, dz) in DIAGONALS {
        let side_x = offset(pos, dx, 0, 0);
        let side_z = offset(pos, 0, 0, dz);
        let target = offset(pos, dx, 0, dz);

        let side_x_free = match body_free(ctx, side_x) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(free) => free,
        };
        let side_z_free = match body_free(ctx, side_z) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(free) => free,
        };
        // Azalea `basic.rs:437-439`: both sides closed means the diagonal is
        // taken by a normal walk around a corner, never here.
        if !side_x_free && !side_z_free {
            continue;
        }
        let cost = if side_x_free && side_z_free {
            straight_diagonal
        } else {
            wall_hug
        };
        match body_free(ctx, target) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(false) => continue,
            Ok(true) => {}
        }
        match floor_free(ctx, target) {
            Ok(true) => push(
                edges,
                target,
                false,
                node.placements,
                MoveKind::Diagonal,
                cost,
                Vec::new(),
            ),
            Ok(false) => {}
            Err(unknown) => note_unknown(ctx, unknown, node),
        }
    }
}

fn step_up(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    // The jump needs the current head cell and the cell above it.
    for cell in [offset(pos, 0, 1, 0), offset(pos, 0, 2, 0)] {
        match ctx.world.cell(cell) {
            Cell::Unknown => {
                note_unknown(ctx, cell, node);
                return;
            }
            Cell::Blocked => return,
            Cell::Free => {}
        }
    }

    let cost = WALK_ONE_BLOCK_COST.max(*JUMP_ONE_BLOCK_COST) + JUMP_PENALTY;
    for (dx, dz) in DIRS {
        let target = offset(pos, dx, 1, dz);
        match body_free(ctx, target) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(false) => continue,
            Ok(true) => {}
        }
        match floor_free(ctx, target) {
            Ok(true) => push(
                edges,
                target,
                false,
                node.placements,
                MoveKind::StepUp,
                cost,
                Vec::new(),
            ),
            Ok(false) => {}
            Err(unknown) => note_unknown(ctx, unknown, node),
        }
    }
}

fn descend(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    for (dx, dz) in DIRS {
        let horizontal = offset(pos, dx, 0, dz);
        match body_free(ctx, horizontal) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(false) => continue,
            Ok(true) => {}
        }

        for drop in 1..=ctx.max_drop {
            let target = offset(horizontal, 0, -drop, 0);

            let mut clear = true;
            // The cells the body falls through, above the landing cell.
            for k in 0..drop {
                match body_free(ctx, offset(horizontal, 0, -k, 0)) {
                    Err(unknown) => {
                        note_unknown(ctx, unknown, node);
                        clear = false;
                        break;
                    }
                    Ok(false) => {
                        clear = false;
                        break;
                    }
                    Ok(true) => {}
                }
            }
            if !clear {
                continue;
            }

            match body_free(ctx, target) {
                Err(unknown) => {
                    note_unknown(ctx, unknown, node);
                    continue;
                }
                Ok(false) => continue,
                Ok(true) => {}
            }
            match floor_free(ctx, target) {
                Ok(true) => {
                    let fall = FALL_N_BLOCKS_COST
                        .get(drop as usize)
                        .copied()
                        .unwrap_or(f32::INFINITY);
                    let cost = WALK_OFF_BLOCK_COST + fall.max(CENTER_AFTER_FALL_COST);
                    push(
                        edges,
                        target,
                        false,
                        node.placements,
                        MoveKind::Descend { drop },
                        cost,
                        Vec::new(),
                    );
                }
                Ok(false) => {}
                Err(unknown) => note_unknown(ctx, unknown, node),
            }
        }
    }
}

fn dig_ahead(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    if !ctx.allow_world_changes {
        return;
    }
    for (dx, dz) in DIRS {
        let target = offset(pos, dx, 0, dz);
        let mut broken = Vec::new();
        let mut refused = false;

        for cell in [target, offset(target, 0, 1, 0)] {
            let Some(state) = ctx.world.state(cell) else {
                note_unknown(ctx, cell, node);
                refused = true;
                break;
            };
            if ctx.world.protected_cell(cell) {
                refused = true;
                break;
            }
            if blocks::is_passable(state) {
                continue;
            }
            match safe_to_dig(ctx, cell, state) {
                DigCheck::Ok => broken.push((cell, state)),
                DigCheck::Unknown(unknown) => {
                    note_unknown(ctx, unknown, node);
                    refused = true;
                    break;
                }
                DigCheck::Refused => {
                    refused = true;
                    break;
                }
            }
        }
        if refused || broken.is_empty() {
            continue;
        }

        match floor_free(ctx, target) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
        }

        let mut cost = SPRINT_ONE_BLOCK_COST + BLOCK_BREAK_ADDITIONAL_PENALTY;
        let mut primitives = Vec::new();
        for (cell, state) in &broken {
            cost += blocks::dig_cost_ticks(*state, ctx.inventory);
            primitives.push(PrimitiveCall::mine(ctx.primitives, *cell, *state));
        }

        push(
            edges,
            target,
            false,
            node.placements,
            MoveKind::Dig {
                broken: broken.iter().map(|(cell, _)| *cell).collect(),
            },
            cost,
            primitives,
        );
    }
}

enum DigCheck {
    Ok,
    Unknown(BlockPos),
    Refused,
}

/// Digging `cell` must not let a fluid reach the path and must not touch an
/// unknown cell.
fn safe_to_dig(ctx: &mut MoveCtx, cell: BlockPos, state: azalea_block::BlockState) -> DigCheck {
    if !blocks::is_breakable(state) || blocks::fluid(state).is_some() {
        return DigCheck::Refused;
    }
    for neighbor in neighbors6(cell) {
        let Some(neighbor_state) = ctx.world.state(neighbor) else {
            return DigCheck::Unknown(neighbor);
        };
        if blocks::fluid(neighbor_state).is_some() {
            return DigCheck::Refused;
        }
    }
    DigCheck::Ok
}

fn bridge(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    if !ctx.allow_world_changes {
        return;
    }
    // The next stack that still has blocks; when it runs out the next node's
    // budget names the following stack, so a long bridge of two item kinds
    // switches stacks instead of stopping.
    let Some(slot) = node.placements.next_slot() else {
        return;
    };
    let Some(item) = ctx.inventory.slot_item(slot).map(str::to_owned) else {
        return;
    };
    let Some(remaining) = node.placements.take(slot) else {
        return;
    };
    for (dx, dz) in DIRS {
        let gap = offset(pos, dx, 0, dz);
        let far = offset(pos, dx * 2, 0, dz * 2);

        match body_free(ctx, gap) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(false) => continue,
            Ok(true) => {}
        }
        match floor_free(ctx, gap) {
            Ok(true) => continue, // not a gap
            Ok(false) => {}
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
        }
        match body_free(ctx, far) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(false) => continue,
            Ok(true) => {}
        }
        match floor_free(ctx, far) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
        }

        let placed = offset(gap, 0, -1, 0);
        match ctx.world.placeable(placed) {
            Cell::Unknown => {
                note_unknown(ctx, placed, node);
                continue;
            }
            Cell::Blocked => continue,
            Cell::Free => {}
        }
        match ctx.world.has_support(placed) {
            Cell::Unknown => {
                // An unknown support could be solid, so the caller should look.
                note_unknown(ctx, placed, node);
                continue;
            }
            Cell::Blocked => continue,
            Cell::Free => {}
        }

        let primitives = vec![PrimitiveCall::place(ctx.primitives, placed, &item)];
        let cost = WALK_ONE_BLOCK_COST * 1.5 + PLACE_BLOCK_COST;
        push(
            edges,
            far,
            false,
            remaining,
            MoveKind::Bridge { placed, over: gap },
            cost,
            primitives,
        );
    }
}

fn pillar_up(ctx: &mut MoveCtx, node @ Node { pos, standing_on_placed, .. }: Node, edges: &mut Vec<PlanEdge>) {
    if !ctx.allow_world_changes {
        return;
    }
    let Some(slot) = node.placements.next_slot() else {
        return;
    };
    let Some(item) = ctx.inventory.slot_item(slot).map(str::to_owned) else {
        return;
    };
    let Some(remaining) = node.placements.take(slot) else {
        return;
    };

    // The block goes where the feet are now.
    match ctx.world.placeable(pos) {
        Cell::Unknown => {
            note_unknown(ctx, pos, node);
            return;
        }
        Cell::Blocked => return,
        Cell::Free => {}
    }
    // Support is the real floor, or the block we placed during the previous
    // pillar step (the `standing_on_placed` flag).
    if !standing_on_placed {
        match ctx.world.floor(pos) {
            Cell::Unknown => {
                note_unknown(ctx, offset(pos, 0, -1, 0), node);
                return;
            }
            Cell::Blocked => return,
            Cell::Free => {}
        }
    }

    let target = offset(pos, 0, 1, 0);
    match body_free(ctx, target) {
        Err(unknown) => {
            note_unknown(ctx, unknown, node);
            return;
        }
        Ok(false) => return,
        Ok(true) => {}
    }

    let primitives = vec![PrimitiveCall::place(ctx.primitives, pos, &item)];
    let cost = WALK_ONE_BLOCK_COST.max(*JUMP_ONE_BLOCK_COST) + JUMP_PENALTY + PLACE_BLOCK_COST;
    push(
        edges,
        target,
        true,
        remaining,
        MoveKind::Pillar { placed: pos },
        cost,
        primitives,
    );
}

/// Whether both body cells of `pos` are water or the air above the surface.
/// `Err(cell)` names an unknown cell; `Ok(false)` means the body does not fit.
fn water_body(ctx: &mut MoveCtx, pos: BlockPos) -> Result<bool, BlockPos> {
    let head = offset(pos, 0, 1, 0);
    match ctx.world.state(pos) {
        None => Err(pos),
        Some(state) if blocks::is_water(state) => match ctx.world.state(head) {
            None => Err(head),
            Some(head) if blocks::is_water(head) || blocks::is_passable(head) => Ok(true),
            Some(_) => Ok(false),
        },
        Some(_) => Ok(false),
    }
}

/// `Ok(true)` when `pos` is a cell a body falling into water passes through:
/// water with room for the head, or open air. A protected or blacklisted cell
/// stops it. `Err(cell)` names an unknown cell.
fn fall_through(ctx: &mut MoveCtx, pos: BlockPos) -> Result<bool, BlockPos> {
    let head = offset(pos, 0, 1, 0);
    if ctx.world.protected_cell(pos) || ctx.world.protected_cell(head) {
        return Ok(false);
    }
    match water_body(ctx, pos) {
        Ok(true) => Ok(true),
        Err(unknown) => Err(unknown),
        Ok(false) => body_free(ctx, pos),
    }
}

/// `Ok(true)` when `pos` is a land cell the body can stand in: passable feet
/// and head above a solid floor. Water never counts as land.
fn standable_land(ctx: &mut MoveCtx, pos: BlockPos) -> Result<bool, BlockPos> {
    match body_free(ctx, pos) {
        Err(unknown) => Err(unknown),
        Ok(false) => Ok(false),
        Ok(true) => floor_free(ctx, pos),
    }
}

fn enter_water(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    for (dx, dz) in DIRS {
        let horizontal = offset(pos, dx, 0, dz);
        // Drop 0 steps in at the same level; 1..=`max_drop` falls in. Azalea
        // reaches the second case through `descend_move` (`basic.rs:259-311`):
        // a zero fall distance that ends on water becomes a one-block fall and
        // pays `ENTER_WATER_PENALTY`. A landing cell that is water removes the
        // fall damage, so the drop is bounded by the planner's `max_drop`, not
        // by a damage limit.
        for drop in 0..=ctx.max_drop {
            let target = offset(horizontal, 0, -drop, 0);
            // The cells the body passes before the landing cell.
            let mut clear = true;
            for k in 0..drop {
                match fall_through(ctx, offset(horizontal, 0, -k, 0)) {
                    Ok(true) => {}
                    Ok(false) => {
                        clear = false;
                        break;
                    }
                    Err(unknown) => {
                        note_unknown(ctx, unknown, node);
                        clear = false;
                        break;
                    }
                }
            }
            if !clear {
                continue;
            }
            match water_body(ctx, target) {
                Err(unknown) => note_unknown(ctx, unknown, node),
                Ok(false) => {}
                Ok(true) => {
                    let head = offset(target, 0, 1, 0);
                    if ctx.world.protected_cell(target) || ctx.world.protected_cell(head) {
                        continue;
                    }
                    let cost = if drop == 0 {
                        WALK_ONE_BLOCK_COST + ENTER_WATER_PENALTY
                    } else {
                        let fall = FALL_N_BLOCKS_COST
                            .get(drop as usize)
                            .copied()
                            .unwrap_or(f32::INFINITY);
                        WALK_OFF_BLOCK_COST + fall.max(CENTER_AFTER_FALL_COST) + ENTER_WATER_PENALTY
                    };
                    push(
                        edges,
                        target,
                        false,
                        node.placements,
                        MoveKind::EnterWater,
                        cost,
                        Vec::new(),
                    );
                }
            }
        }
    }
}

fn water_moves(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    for (dx, dz) in DIRS {
        let target = offset(pos, dx, 0, dz);
        match water_body(ctx, target) {
            Err(unknown) => note_unknown(ctx, unknown, node),
            Ok(true) => {
                let head = offset(target, 0, 1, 0);
                if ctx.world.protected_cell(target) || ctx.world.protected_cell(head) {
                    continue;
                }
                push(
                    edges,
                    target,
                    false,
                    node.placements,
                    MoveKind::Swim,
                    WALK_ONE_IN_WATER_COST,
                    Vec::new(),
                );
            }
            Ok(false) => {
                // Step out at the same level.
                let head = offset(target, 0, 1, 0);
                if !ctx.world.protected_cell(target) && !ctx.world.protected_cell(head) {
                    match standable_land(ctx, target) {
                        Ok(true) => push(
                            edges,
                            target,
                            false,
                            node.placements,
                            MoveKind::ExitWater { rise: false },
                            WALK_ONE_IN_WATER_COST,
                            Vec::new(),
                        ),
                        Ok(false) => {}
                        Err(unknown) => note_unknown(ctx, unknown, node),
                    }
                }
                // Climb out one block up: the cell above the land beside us
                // must be standable. A pool dug flush into the floor has a
                // solid block at feet level beside the water, so requiring
                // water there (`water(target) == Free`) made every flush pool
                // a dead end; the land top is exactly `target + 1`.
                let raised = offset(target, 0, 1, 0);
                let raised_head = offset(raised, 0, 1, 0);
                if !ctx.world.protected_cell(raised) && !ctx.world.protected_cell(raised_head) {
                    match standable_land(ctx, raised) {
                        Ok(true) => {
                            let cost = WALK_ONE_IN_WATER_COST + *JUMP_ONE_BLOCK_COST;
                            push(
                                edges,
                                raised,
                                false,
                                node.placements,
                                MoveKind::ExitWater { rise: true },
                                cost,
                                Vec::new(),
                            );
                        }
                        Ok(false) => {}
                        Err(unknown) => note_unknown(ctx, unknown, node),
                    }
                }
            }
        }
    }

    // Swim down one cell inside the water column. The geometry says `Swim` is
    // vertical, so the follower needs no new input: in water the body sinks
    // unless jump is held.
    let below = offset(pos, 0, -1, 0);
    match water_body(ctx, below) {
        Err(unknown) => note_unknown(ctx, unknown, node),
        Ok(true) => {
            let head = offset(below, 0, 1, 0);
            if !ctx.world.protected_cell(below) && !ctx.world.protected_cell(head) {
                push(
                    edges,
                    below,
                    false,
                    node.placements,
                    MoveKind::Swim,
                    WALK_ONE_IN_WATER_COST,
                    Vec::new(),
                );
            }
        }
        Ok(false) => {}
    }

    // Rise one cell inside the water column.
    let above = offset(pos, 0, 1, 0);
    match water_body(ctx, above) {
        Err(unknown) => note_unknown(ctx, unknown, node),
        Ok(true) => {
            let head = offset(above, 0, 1, 0);
            if !ctx.world.protected_cell(above) && !ctx.world.protected_cell(head) {
                let cost = WALK_ONE_IN_WATER_COST.max(*JUMP_ONE_BLOCK_COST) + JUMP_PENALTY;
                push(
                    edges,
                    above,
                    false,
                    node.placements,
                    MoveKind::SwimUp,
                    cost,
                    Vec::new(),
                );
            }
        }
        Ok(false) => {}
    }
}

#[cfg(test)]
mod tests {
    use praxion_rules::Primitives;

    use super::*;

    #[test]
    fn every_world_changing_move_names_a_registry_record() {
        let primitives = Primitives::standard();
        let mine = PrimitiveCall::mine(
            &primitives,
            BlockPos::new(0, 64, 0),
            azalea_block::BlockState::from(azalea_registry::builtin::BlockKind::Stone),
        );
        assert_eq!(mine.name, "mine");
        assert_eq!(mine.version, 1);
        assert_eq!(mine.args, vec![ArgValue::Block("stone".into())]);
        assert!(mine.record(&primitives).is_some());
        assert!(mine.effects(&primitives).is_some());

        let place = PrimitiveCall::place(&primitives, BlockPos::new(0, 63, 0), "cobblestone");
        assert_eq!(place.name, "place");
        assert_eq!(place.args, vec![ArgValue::Block("cobblestone".into())]);
    }
}
