//! `sky_above()`: whether the column over the body's head is open to the sky.
//!
//! The raw column comes from perception's privileged read
//! ([`Perception::privileged_column`], architecture §5.1) and every read is
//! ledgered here; the judgement is this module's. `explore` judges the columns
//! of its frontier cells with a wider predicate of its own (it also keeps
//! vegetation a body can walk through); this one answers the question about
//! the body's own head, where a stand-in cover (a leaf, a log, water) still
//! leaves the sky in sight and nothing else does.

use azalea_registry::builtin::BlockKind;
use praxion_gamedata as gamedata;
use praxion_perception::{COLUMN_BLOCKS, FactKind, Perception, QueryKind};
use praxion_world::{BlockPos, BlockState, World};

use crate::feet_cell;

/// Blocks above the head a column may hold before the cell counts as covered.
/// A roof farther up than this is a mountain over a cave, not a canopy.
pub const SKY_SCAN_BLOCKS: i32 = 48;

// A privileged read serves at most [`COLUMN_BLOCKS`] states: a larger window
// would silently truncate, a smaller one would waste reads.
const _: () = assert!(SKY_SCAN_BLOCKS as usize <= COLUMN_BLOCKS);

/// Whether a block over the head leaves the sky in sight: air, leaves and logs
/// (a tree canopy is the surface, not a roof) and water (a body swimming under
/// the open sky). Everything else, lava and fire included, covers the column.
fn leaves_sky(state: BlockState) -> bool {
    if state.is_air() {
        return true;
    }
    let kind = BlockKind::from(state);
    if matches!(
        kind,
        BlockKind::Water
            | BlockKind::Kelp
            | BlockKind::KelpPlant
            | BlockKind::Seagrass
            | BlockKind::TallSeagrass
            | BlockKind::BubbleColumn
    ) {
        return true;
    }
    let name = kind.to_str();
    gamedata::block_in_tag(name, "leaves") || gamedata::block_in_tag(name, "logs")
}

/// Whether every block from the head cell (`feet` + 1) up to `surface` leaves
/// the sky in sight. `surface` is the column's `WorldSurface`, the first free
/// y above its highest non-air block; a surface farther than
/// [`SKY_SCAN_BLOCKS`] above the head counts as covered without a scan, and a
/// block the read could not answer covers the column.
fn column_is_open(
    feet: BlockPos,
    surface: i32,
    block: impl Fn(i32) -> Option<BlockState>,
) -> bool {
    let first = feet.y.saturating_add(1);
    if surface.saturating_sub(first) > SKY_SCAN_BLOCKS {
        return false;
    }
    (first..surface).all(|y| block(y).is_some_and(leaves_sky))
}

/// Whether the body stands under open sky. `None` when its position or its
/// column's heightmap is not known: an unknown column is not a covered one, so
/// the interpreter keeps the fact `Unknown`.
///
/// A pit or a ravine with open sky over it answers `true` too: the fact says
/// what is over the head, not how deep the body is.
pub fn sky_above(perception: &mut Perception, world: &World) -> Option<bool> {
    let feet = feet_cell(*perception.own_position(world).value()?);
    let column = perception.privileged_column(
        world,
        feet.x,
        feet.z,
        feet.y.saturating_add(1),
        SKY_SCAN_BLOCKS,
    );
    perception.ledger_privileged(QueryKind::SkyColumn, FactKind::Column, 1);
    let column = column?;
    Some(column_is_open(feet, column.surface, |y| column.state_at(y)))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn at(y: i32) -> BlockPos {
        BlockPos::new(0, y, 0)
    }

    fn open(feet_y: i32, surface: i32, map: &HashMap<i32, BlockKind>) -> bool {
        column_is_open(at(feet_y), surface, |y| {
            Some(BlockState::from(
                map.get(&y).copied().unwrap_or(BlockKind::Air),
            ))
        })
    }

    #[test]
    fn nothing_over_the_head_is_open() {
        // The heightmap surface is the first free y above the highest block:
        // at or below the head cell there is nothing to scan.
        assert!(open(64, 64, &HashMap::new()));
        assert!(open(64, 65, &HashMap::new()));
    }

    #[test]
    fn a_roof_of_stone_dirt_or_gravel_covers_the_column() {
        for kind in [BlockKind::Stone, BlockKind::Dirt, BlockKind::Gravel, BlockKind::Sand] {
            let roof = HashMap::from([(70, kind)]);
            assert!(!open(64, 71, &roof), "{kind:?}");
        }
        // A stone head cell covers it too: the scan starts at the head.
        assert!(!open(64, 66, &HashMap::from([(65, BlockKind::Stone)])));
    }

    #[test]
    fn a_canopy_and_water_leave_the_sky_in_sight() {
        let canopy = HashMap::from([
            (66, BlockKind::OakLeaves),
            (67, BlockKind::OakLog),
            (68, BlockKind::BirchLeaves),
        ]);
        assert!(open(64, 69, &canopy), "a tree is the surface, not a roof");
        for kind in [BlockKind::Water, BlockKind::Kelp, BlockKind::Seagrass] {
            assert!(open(60, 64, &HashMap::from([(62, kind)])), "{kind:?}");
        }
    }

    #[test]
    fn lava_and_fire_cover_the_column() {
        for kind in [BlockKind::Lava, BlockKind::Fire] {
            assert!(!open(64, 67, &HashMap::from([(66, kind)])), "{kind:?}");
        }
    }

    #[test]
    fn a_far_surface_or_an_unread_block_counts_as_covered() {
        assert!(open(64, 65 + SKY_SCAN_BLOCKS, &HashMap::new()), "the window edge");
        assert!(
            !open(64, 66 + SKY_SCAN_BLOCKS, &HashMap::new()),
            "a surface past the window is a mountain over a cave"
        );
        assert!(!column_is_open(at(64), 70, |_| None), "an unread cell");
    }
}
