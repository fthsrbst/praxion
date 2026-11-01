//! One chunk section, seen from the outside.
//!
//! A [`SectionView`] is the read seam for code that walks cells in bulk (the
//! sight ray walk, the block searches): ask for a section once, then read
//! states without going through the chunk map for every block. It also answers
//! the palette question that lets a caller skip a whole section: "can any of
//! these states exist in here?".
//!
//! Nothing here is mutable: the view borrows the section and every field of
//! [`World`](crate::World) stays private.

use azalea_block::BlockState;
use azalea_core::position::ChunkSectionBlockPos;
use azalea_world::palette::Palette;

/// A borrowed 16×16×16 section of a loaded chunk.
#[derive(Clone, Copy)]
pub struct SectionView<'w> {
    states: &'w azalea_world::palette::PalettedContainer<BlockState>,
}

impl<'w> SectionView<'w> {
    pub(crate) fn new(section: &'w azalea_world::Section) -> Self {
        Self { states: &section.states }
    }

    /// The block state at section-local `x`, `y`, `z` (each below 16).
    #[inline]
    pub fn state(&self, x: usize, y: usize, z: usize) -> BlockState {
        // A single-valued section answers the same state for every cell.
        if let Palette::SingleValue(state) = &self.states.palette {
            return *state;
        }
        self.states.get(ChunkSectionBlockPos::new(x as u8, y as u8, z as u8))
    }

    /// Whether any state this section can hold satisfies `wanted`.
    ///
    /// `false` is a proof that the section holds none of them, so a search can
    /// skip all 4096 cells. `true` means the palette cannot rule it out: a
    /// global palette (more than 256 distinct states) always answers `true`,
    /// and a multi-valued palette is a superset of what the section holds, so
    /// the answer is never a "no" that hides a block.
    pub fn may_contain(&self, wanted: impl Fn(BlockState) -> bool) -> bool {
        match &self.states.palette {
            Palette::SingleValue(state) => wanted(*state),
            Palette::Linear(states) | Palette::Hashmap(states) => {
                states.iter().any(|state| wanted(*state))
            }
            Palette::Global => true,
        }
    }
}
