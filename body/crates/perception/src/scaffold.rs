//! The cells the body filled with a block to climb (`pillar`), remembered so
//! no target search takes the body's own scaffolding for a resource (decision
//! 2026-11-09 "Five next areas"; critique finding 2: a pillar of logs is real
//! `#logs`, so `mine` would break it, drop it and cycle, and `near(#logs, 5)`
//! would keep a skill chopping the block it just put under its feet).
//!
//! It lives here, not in a primitive, because both sides of the perception
//! boundary need the same answer (review finding 1): the facts (`near`) answer
//! decision code that may not read the raw store, and the primitives (`mine`'s
//! target search, `explore`'s known-map search) ask it directly. With the set
//! in one primitive crate the exclusion existed on the primitive side only, so
//! `near(#logs, 5)` and `mine(#logs)` disagreed about the same block.
//!
//! One set per thread, like the caches: the driver ticks every primitive and
//! every fact on one thread, and parallel tests each get their own. It lives
//! for the process: the column stays after the climb, as a player's does.

use std::cell::RefCell;
use std::collections::HashSet;

use praxion_world::BlockPos;

thread_local! {
    static SCAFFOLD: RefCell<HashSet<BlockPos>> = RefCell::new(HashSet::new());
}

/// Remember `pos` as a cell the body filled to stand on. Called only once the
/// place receipt settled: a click the server refused must not hide a real
/// block from every later target search (review finding 3).
pub fn note(pos: BlockPos) {
    SCAFFOLD.with(|set| {
        set.borrow_mut().insert(pos);
    });
}

/// Whether the body placed `pos` as scaffolding.
pub fn is_scaffold(pos: BlockPos) -> bool {
    SCAFFOLD.with(|set| set.borrow().contains(&pos))
}

/// Forget `pos`: the cell it was placed in is empty again (the body dug its own
/// column down, or something broke it), so it is not scaffolding any more and a
/// real block that later fills the cell is a target again.
pub fn forget(pos: BlockPos) {
    SCAFFOLD.with(|set| {
        set.borrow_mut().remove(&pos);
    });
}

/// How many scaffold blocks stand stacked, one on the other, ending at `top`
/// and going down.
pub fn column_height(top: BlockPos) -> u32 {
    let mut height = 0;
    let mut cell = top;
    while is_scaffold(cell) {
        height += 1;
        cell = BlockPos::new(cell.x, cell.y - 1, cell.z);
    }
    height
}

/// Forget every remembered cell. Tests only: the driver never needs it, and a
/// column stays after its climb.
pub fn reset() {
    SCAFFOLD.with(|set| set.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_counts_only_the_stacked_scaffold_cells_ending_at_the_top() {
        reset();
        let base = BlockPos::new(3, 64, 3);
        note(base);
        note(BlockPos::new(3, 65, 3));
        note(BlockPos::new(8, 64, 8));
        assert_eq!(column_height(BlockPos::new(3, 65, 3)), 2);
        assert_eq!(
            column_height(BlockPos::new(3, 66, 3)),
            0,
            "nothing stands there"
        );
        assert!(is_scaffold(base));
        assert!(!is_scaffold(BlockPos::new(3, 70, 3)));
        reset();
    }
}
