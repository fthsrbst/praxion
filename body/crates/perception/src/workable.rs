//! The one "workable cell" rule shared by `near`, `explore` and `mine`.
//!
//! A block cell is a target only when breaking it would keep water and lava
//! out: a lake-bed stone that exposes water floods the hole and drowns the
//! body (seed 202 live run 2026-11-08). The rule lives here because it is a
//! world-safety query over a cell, both `praxion-facts` and
//! `praxion-primitives` already depend on this crate, and this crate owns the
//! known-map scan ([`crate::Perception::nearest_known`]) every call site
//! filters; putting it in either caller would force a new crate edge.

use azalea_block::{BlockState, fluid_state::FluidState};
use azalea_core::position::BlockPos;
use azalea_registry::builtin::BlockKind;

/// Whether removing `cell` would let water or lava reach it.
///
/// Every face counts: a fluid above a lake bed falls into the hole and a fluid
/// beside it flows in. A cell `lookup` cannot answer for is not a fluid.
pub fn exposes_fluid(cell: BlockPos, lookup: &impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    const FACES: [(i32, i32, i32); 6] = [
        (1, 0, 0),
        (-1, 0, 0),
        (0, 1, 0),
        (0, -1, 0),
        (0, 0, 1),
        (0, 0, -1),
    ];
    FACES.into_iter().any(|(dx, dy, dz)| {
        let neighbor = BlockPos::new(cell.x + dx, cell.y + dy, cell.z + dz);
        lookup(neighbor).is_some_and(carries_fluid)
    })
}

/// Whether `state` holds a fluid: a water or lava block, or a waterlogged one.
fn carries_fluid(state: BlockState) -> bool {
    !FluidState::from(state).is_empty()
}

/// Whether the cell `pos` holding `state` is a target the body may work on:
/// breaking it would not expose water or lava, or the block is one skills use
/// rather than mine.
///
/// `lookup` is the reader for the neighbouring cells: the raw world for the
/// live body, a map in tests. It must be the same reader at every call site,
/// or `near` says a stone is there while `mine` refuses to dig it.
pub fn workable(
    pos: BlockPos,
    state: BlockState,
    lookup: impl Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    is_workstation(state) || !exposes_fluid(pos, &lookup)
}

/// Blocks skills interact with instead of mining: a crafting table, furnace,
/// chest, smoker, blast furnace or barrel is used where it stands, so water
/// beside it is no reason to pretend it is absent.
///
/// Vanilla has no block tag that groups workstations and containers (the
/// `mineable/*` tags are about tools and `guarded_by_piglins` about loot), so
/// this is a short named list instead of game data.
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
