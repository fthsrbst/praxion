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
use crate::state::Inventory;
use crate::world::{Cell, WorldView, neighbors6};

/// A node: the feet cell, whether the player is standing on a block this path
/// just placed (so a second pillar step has support), and how many blocks the
/// plan may still place. The count is part of the key, so a route that used
/// more blocks is a different state and can never exceed the hotbar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Node {
    pub pos: BlockPos,
    pub standing_on_placed: bool,
    pub places_left: u16,
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
    /// Step from land into a water cell.
    EnterWater,
    /// Swim one cell horizontally under water (or at the surface).
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

fn offset(pos: BlockPos, dx: i32, dy: i32, dz: i32) -> BlockPos {
    BlockPos::new(pos.x + dx, pos.y + dy, pos.z + dz)
}

fn note_unknown(ctx: &mut MoveCtx, cell: BlockPos, parent: Node) {
    ctx.frontier.entry(cell).or_insert(parent);
}

/// Whether both body cells at `pos` are free. `Err(cell)` names an unknown
/// cell that stopped the check.
fn body_free(ctx: &mut MoveCtx, pos: BlockPos) -> Result<bool, BlockPos> {
    match ctx.world.cell(pos) {
        Cell::Unknown => Err(pos),
        Cell::Blocked => Ok(false),
        Cell::Free => match ctx.world.cell(pos.up(1)) {
            Cell::Unknown => Err(pos.up(1)),
            Cell::Blocked => Ok(false),
            Cell::Free => Ok(true),
        },
    }
}

/// Whether `pos` has a solid floor. `Err(cell)` names an unknown cell.
fn floor_free(ctx: &mut MoveCtx, pos: BlockPos) -> Result<bool, BlockPos> {
    match ctx.world.floor(pos) {
        Cell::Unknown => Err(pos.down(1)),
        Cell::Blocked => Ok(false),
        Cell::Free => Ok(true),
    }
}

fn push(
    edges: &mut Vec<PlanEdge>,
    target: BlockPos,
    standing_on_placed: bool,
    places_left: u16,
    kind: MoveKind,
    cost: f32,
    primitives: Vec<PrimitiveCall>,
) {
    edges.push(Edge {
        movement: Movement {
            target: Node {
                pos: target,
                standing_on_placed,
                places_left,
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
                    node.places_left,
                    MoveKind::Walk,
                    SPRINT_ONE_BLOCK_COST,
                    Vec::new(),
                ),
                Ok(false) => {}
                Err(unknown) => note_unknown(ctx, unknown, node),
            },
        }
    }
}

fn diagonal(ctx: &mut MoveCtx, node @ Node { pos, .. }: Node, edges: &mut Vec<PlanEdge>) {
    let cost = SPRINT_ONE_BLOCK_COST.mul_add(std::f32::consts::SQRT_2, 0.001);
    for (dx, dz) in DIRS {
        let side_x = offset(pos, dx, 0, 0);
        let side_z = offset(pos, 0, 0, dz);
        let target = offset(pos, dx, 0, dz);

        match body_free(ctx, side_x) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(false) => continue,
            Ok(true) => {}
        }
        match body_free(ctx, side_z) {
            Err(unknown) => {
                note_unknown(ctx, unknown, node);
                continue;
            }
            Ok(false) => continue,
            Ok(true) => {}
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
            Ok(true) => push(
                edges,
                target,
                false,
                node.places_left,
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
    for cell in [pos.up(1), pos.up(2)] {
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
                node.places_left,
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
            let target = horizontal.down(drop);

            let mut clear = true;
            // The cells the body falls through, above the landing cell.
            for k in 0..drop {
                match body_free(ctx, horizontal.down(k)) {
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
                        node.places_left,
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

        for cell in [target, target.up(1)] {
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
            node.places_left,
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
    if !ctx.allow_world_changes || node.places_left == 0 {
        return;
    }
    let Some(item) = ctx.inventory.place_item().map(str::to_owned) else {
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

        let placed = gap.down(1);
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
            node.places_left - 1,
            MoveKind::Bridge { placed, over: gap },
            cost,
            primitives,
        );
    }
}

fn pillar_up(ctx: &mut MoveCtx, node @ Node { pos, standing_on_placed, .. }: Node, edges: &mut Vec<PlanEdge>) {
    if !ctx.allow_world_changes || node.places_left == 0 {
        return;
    }
    let Some(item) = ctx.inventory.place_item().map(str::to_owned) else {
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
                note_unknown(ctx, pos.down(1), node);
                return;
            }
            Cell::Blocked => return,
            Cell::Free => {}
        }
    }

    let target = pos.up(1);
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
        node.places_left - 1,
        MoveKind::Pillar { placed: pos },
        cost,
        primitives,
    );
}

/// Whether both body cells of `pos` are water or the air above the surface.
/// `Err(cell)` names an unknown cell; `Ok(false)` means the body does not fit.
fn water_body(ctx: &mut MoveCtx, pos: BlockPos) -> Result<bool, BlockPos> {
    match ctx.world.state(pos) {
        None => Err(pos),
        Some(state) if blocks::is_water(state) => match ctx.world.state(pos.up(1)) {
            None => Err(pos.up(1)),
            Some(head) if blocks::is_water(head) || blocks::is_passable(head) => Ok(true),
            Some(_) => Ok(false),
        },
        Some(_) => Ok(false),
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
        let target = offset(pos, dx, 0, dz);
        match water_body(ctx, target) {
            Err(unknown) => note_unknown(ctx, unknown, node),
            Ok(false) => {}
            Ok(true) => {
                if ctx.world.protected_cell(target) || ctx.world.protected_cell(target.up(1)) {
                    continue;
                }
                push(
                    edges,
                    target,
                    false,
                    node.places_left,
                    MoveKind::EnterWater,
                    WALK_ONE_BLOCK_COST + ENTER_WATER_PENALTY,
                    Vec::new(),
                );
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
                if ctx.world.protected_cell(target) || ctx.world.protected_cell(target.up(1)) {
                    continue;
                }
                push(
                    edges,
                    target,
                    false,
                    node.places_left,
                    MoveKind::Swim,
                    WALK_ONE_IN_WATER_COST,
                    Vec::new(),
                );
            }
            Ok(false) => {
                // Step out at the same level.
                if !ctx.world.protected_cell(target) && !ctx.world.protected_cell(target.up(1)) {
                    match standable_land(ctx, target) {
                        Ok(true) => push(
                            edges,
                            target,
                            false,
                            node.places_left,
                            MoveKind::ExitWater { rise: false },
                            WALK_ONE_IN_WATER_COST,
                            Vec::new(),
                        ),
                        Ok(false) => {}
                        Err(unknown) => note_unknown(ctx, unknown, node),
                    }
                }
                // Climb out one block up: the water must continue beside us
                // and the cell above it must be standable land.
                if ctx.world.water(target) == Cell::Free {
                    let raised = target.up(1);
                    if !ctx.world.protected_cell(raised)
                        && !ctx.world.protected_cell(raised.up(1))
                    {
                        match standable_land(ctx, raised) {
                            Ok(true) => {
                                let cost = WALK_ONE_IN_WATER_COST + *JUMP_ONE_BLOCK_COST;
                                push(
                                    edges,
                                    raised,
                                    false,
                                    node.places_left,
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
    }

    // Rise one cell inside the water column.
    let above = pos.up(1);
    match water_body(ctx, above) {
        Err(unknown) => note_unknown(ctx, unknown, node),
        Ok(true) => {
            if !ctx.world.protected_cell(above) && !ctx.world.protected_cell(above.up(1)) {
                let cost = WALK_ONE_IN_WATER_COST.max(*JUMP_ONE_BLOCK_COST) + JUMP_PENALTY;
                push(
                    edges,
                    above,
                    false,
                    node.places_left,
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
