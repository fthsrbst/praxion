//! The known map: block states this perception instance saw by sight, kept in
//! chunk sections and bounded by the configured radius.
//!
//! Two kinds of entry share the map, because both keep the sweep from spending
//! rays on the same cell every tick:
//!
//! - `Seen`: a ray reached the cell and this is the state it saw, with the tick
//!   it was last seen and the chunk revision it was last checked against.
//! - `Miss`: the rays were blocked or too short, so no block fact exists. A
//!   `Miss` never answers a query; it only postpones the next probe.
//!
//! Only `Seen` entries are facts. A read therefore answers `Known(state, age)`
//! exactly when a ray reached the cell; nothing unseen is ever known.
//!
//! The map is bounded per chunk section. When a new section pushes the count
//! over the capacity, the section farthest from the player's section is
//! evicted, so the map keeps the space around the player and forgets the
//! edges first.

use std::collections::HashMap;

use praxion_world::{BlockPos, BlockState};

use crate::hash::FxBuildHasher;

/// A block state this instance saw and the tick it was last seen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Remembered {
    pub state: BlockState,
    pub seen_tick: u64,
}

/// What the map remembers about one cell.
#[derive(Clone, Copy, Debug)]
enum Cell {
    Seen {
        state: BlockState,
        /// Tick of the last ray that reached the cell.
        seen_tick: u64,
        /// Tick of the last visibility test, visible or not. It decides when a
        /// stale cell is due again.
        checked_tick: u64,
        /// The chunk revision at that test. A block change makes every cell of
        /// the chunk a candidate again.
        checked_revision: u64,
    },
    /// A ray test that did not reach the cell; no block fact is stored.
    Miss {
        checked_tick: u64,
        checked_revision: u64,
    },
}

impl Cell {
    fn checked_tick(&self) -> u64 {
        match *self {
            Cell::Seen { checked_tick, .. } | Cell::Miss { checked_tick, .. } => checked_tick,
        }
    }
}

/// A section key `(x, y, z)` in section coordinates.
type SectionKey = (i32, i32, i32);

#[derive(Debug, Default)]
struct Section {
    cells: HashMap<u16, Cell, FxBuildHasher>,
}

/// How many sections fit within `radius` blocks of the player: a cube of
/// `2 * floor(radius / 16) + 1` sections on a side.
pub(crate) fn section_capacity(radius: u32) -> usize {
    let side = (radius / 16) as usize * 2 + 1;
    side * side * side
}

fn section_key(pos: BlockPos) -> SectionKey {
    (pos.x >> 4, pos.y >> 4, pos.z >> 4)
}

#[cfg(test)]
fn section_base(key: SectionKey) -> BlockPos {
    BlockPos::new(key.0 * 16, key.1 * 16, key.2 * 16)
}

/// The section-local index of `pos`, `0..4096`.
fn local_index(pos: BlockPos) -> u16 {
    ((pos.x & 15) as u16) | (((pos.z & 15) as u16) << 4) | (((pos.y & 15) as u16) << 8)
}

fn section_center_sq(a: SectionKey, b: SectionKey) -> i64 {
    let dx = i64::from(a.0) - i64::from(b.0);
    let dy = i64::from(a.1) - i64::from(b.1);
    let dz = i64::from(a.2) - i64::from(b.2);
    dx * dx + dy * dy + dz * dz
}

/// Bounded, section-keyed store of seen block states and probe results.
#[derive(Debug)]
pub(crate) struct KnownMap {
    sections: HashMap<SectionKey, Section, FxBuildHasher>,
    capacity: usize,
    cells: usize,
    cell_capacity: usize,
    stale_ticks: u64,
    retry_ticks: u64,
}

impl KnownMap {
    pub(crate) fn new(
        radius: u32,
        cell_capacity: usize,
        stale_ticks: u64,
        retry_ticks: u64,
    ) -> Self {
        Self {
            sections: HashMap::default(),
            capacity: section_capacity(radius),
            cells: 0,
            cell_capacity,
            stale_ticks,
            retry_ticks,
        }
    }

    fn section_mut(&mut self, pos: BlockPos, focus: SectionKey) -> &mut Section {
        let key = section_key(pos);
        if !self.sections.contains_key(&key) {
            if self.sections.len() >= self.capacity {
                self.evict_farthest(focus, Some(key));
            }
            self.sections.insert(key, Section::default());
        }
        self.sections.get_mut(&key).expect("just inserted")
    }

    /// Drop sections until `incoming` fits; the farthest from `focus` goes
    /// first.
    fn evict_farthest(&mut self, focus: SectionKey, incoming: Option<SectionKey>) {
        while self.sections.len() >= self.capacity {
            let farthest = self
                .sections
                .keys()
                .copied()
                .max_by(|a, b| {
                    section_center_sq(*a, focus)
                        .cmp(&section_center_sq(*b, focus))
                        .then_with(|| a.cmp(b))
                });
            let Some(farthest) = farthest else {
                break;
            };
            if Some(farthest) == incoming {
                break;
            }
            if let Some(section) = self.sections.remove(&farthest) {
                self.cells = self.cells.saturating_sub(section.cells.len());
            }
        }
    }

    /// Keep the exact cell cap: drop the cell that has waited longest since
    /// its last visibility test. The section radius is the normal bound; this
    /// cap is what a caller that sets `Config::memory_capacity` asks for.
    fn enforce_cell_bound(&mut self) {
        while self.cells > self.cell_capacity {
            let mut oldest: Option<(u64, SectionKey, u16)> = None;
            for (key, section) in &self.sections {
                for (index, cell) in &section.cells {
                    let tick = cell.checked_tick();
                    if oldest.is_none_or(|(oldest_tick, _, _)| tick < oldest_tick) {
                        oldest = Some((tick, *key, *index));
                    }
                }
            }
            let Some((_, key, index)) = oldest else {
                break;
            };
            let removed = self
                .sections
                .get_mut(&key)
                .and_then(|section| section.cells.remove(&index))
                .is_some();
            if removed {
                self.cells = self.cells.saturating_sub(1);
            } else {
                break;
            }
            if self
                .sections
                .get(&key)
                .is_some_and(|section| section.cells.is_empty())
            {
                self.sections.remove(&key);
            }
        }
    }

    /// Record a cell a ray reached. It replaces a `Miss` and refreshes a
    /// `Seen` entry.
    pub(crate) fn record_seen(
        &mut self,
        pos: BlockPos,
        state: BlockState,
        tick: u64,
        revision: u64,
        focus: SectionKey,
    ) {
        if self.capacity == 0 || self.cell_capacity == 0 {
            return;
        }
        let cell = Cell::Seen {
            state,
            seen_tick: tick,
            checked_tick: tick,
            checked_revision: revision,
        };
        let index = local_index(pos);
        let section = self.section_mut(pos, focus);
        let existed = section.cells.insert(index, cell).is_some();
        if !existed {
            self.cells = self.cells.saturating_add(1);
            self.enforce_cell_bound();
        }
    }

    /// Record that a ray test did not reach the cell. A cell that was seen
    /// before keeps its state and its age: an unseen change is never known.
    pub(crate) fn record_miss(
        &mut self,
        pos: BlockPos,
        tick: u64,
        revision: u64,
        focus: SectionKey,
    ) {
        if self.capacity == 0 || self.cell_capacity == 0 {
            return;
        }
        let index = local_index(pos);
        let section = self.section_mut(pos, focus);
        if let Some(Cell::Seen { checked_tick, checked_revision, .. }) =
            section.cells.get_mut(&index)
        {
            *checked_tick = tick;
            *checked_revision = revision;
        } else if section
            .cells
            .insert(
                index,
                Cell::Miss { checked_tick: tick, checked_revision: revision },
            )
            .is_none()
        {
            self.cells = self.cells.saturating_add(1);
            self.enforce_cell_bound();
        }
    }

    /// The block fact at `pos`, when a ray has reached it.
    pub(crate) fn get(&self, pos: BlockPos) -> Option<Remembered> {
        match self.sections.get(&section_key(pos))?.cells.get(&local_index(pos))? {
            Cell::Seen { state, seen_tick, .. } => Some(Remembered {
                state: *state,
                seen_tick: *seen_tick,
            }),
            Cell::Miss { .. } => None,
        }
    }

    /// Whether the sweep should spend a ray on this cell now: nothing is
    /// known, the entry is stale, or its chunk changed since the last test.
    pub(crate) fn needs_probe(&self, pos: BlockPos, tick: u64, revision: u64) -> bool {
        let Some(cell) = self
            .sections
            .get(&section_key(pos))
            .and_then(|section| section.cells.get(&local_index(pos)))
        else {
            return true;
        };
        match *cell {
            Cell::Seen { checked_tick, checked_revision, .. } => {
                checked_revision != revision
                    || tick.saturating_sub(checked_tick) >= self.stale_ticks
            }
            Cell::Miss { checked_tick, checked_revision } => {
                checked_revision != revision
                    || tick.saturating_sub(checked_tick) >= self.retry_ticks
            }
        }
    }

    /// Number of cells a ray has reached.
    pub(crate) fn seen_len(&self) -> usize {
        self.sections
            .values()
            .map(|section| {
                section
                    .cells
                    .values()
                    .filter(|cell| matches!(cell, Cell::Seen { .. }))
                    .count()
            })
            .sum()
    }

    pub(crate) fn sections_len(&self) -> usize {
        self.sections.len()
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    #[cfg(test)]
    pub(crate) fn seen_entries(&self) -> Vec<(BlockPos, Remembered)> {
        let mut entries = Vec::new();
        for (key, section) in &self.sections {
            let base = section_base(*key);
            for (index, cell) in &section.cells {
                if let Cell::Seen { state, seen_tick, .. } = *cell {
                    let pos = BlockPos::new(
                        base.x + i32::from(index & 15),
                        base.y + i32::from((index >> 8) & 15),
                        base.z + i32::from((index >> 4) & 15),
                    );
                    entries.push((pos, Remembered { state, seen_tick }));
                }
            }
        }
        entries
    }

    pub(crate) fn clear(&mut self) {
        self.sections.clear();
        self.cells = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: u16) -> BlockState {
        BlockState::try_from(u32::from(id)).expect("valid block state")
    }

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn a_seen_cell_keeps_its_state_and_age() {
        let mut map = KnownMap::new(96, 1_000_000, 100, 40);
        let cell = pos(4, 65, 0);
        map.record_seen(cell, state(1), 10, 7, (0, 4, 0));
        let remembered = map.get(cell).expect("seen");
        assert_eq!(remembered.state, state(1));
        assert_eq!(remembered.seen_tick, 10);
        assert_eq!(map.get(pos(4, 65, 1)), None);
    }

    #[test]
    fn a_miss_is_not_a_block_fact_and_is_not_probed_again_too_soon() {
        let mut map = KnownMap::new(96, 1_000_000, 100, 40);
        let cell = pos(4, 65, 0);
        map.record_miss(cell, 10, 7, (0, 4, 0));
        assert_eq!(map.get(cell), None);
        assert!(!map.needs_probe(cell, 20, 7), "retry interval not reached");
        assert!(map.needs_probe(cell, 50, 7), "retry interval reached");
        assert!(map.needs_probe(cell, 11, 8), "a chunk change forces a probe");
    }

    #[test]
    fn a_seen_cell_is_stale_after_the_interval_or_a_chunk_change() {
        let mut map = KnownMap::new(96, 1_000_000, 100, 40);
        let cell = pos(4, 65, 0);
        map.record_seen(cell, state(1), 10, 7, (0, 4, 0));
        assert!(!map.needs_probe(cell, 50, 7));
        assert!(map.needs_probe(cell, 110, 7), "stale after 100 ticks");
        assert!(map.needs_probe(cell, 50, 8), "the chunk changed");
    }

    #[test]
    fn the_cell_cap_is_exact_and_zero_keeps_nothing() {
        let mut map = KnownMap::new(96, 2, 100, 40);
        for i in 0..5 {
            map.record_seen(pos(i, 65, 0), state(1), 1 + i as u64, 1, (0, 4, 0));
        }
        assert!(map.seen_len() <= 2, "{} cells over a cap of 2", map.seen_len());

        let mut none = KnownMap::new(96, 0, 100, 40);
        none.record_seen(pos(0, 65, 0), state(1), 1, 1, (0, 4, 0));
        none.record_miss(pos(1, 65, 0), 1, 1, (0, 4, 0));
        assert_eq!(none.seen_len(), 0);
        assert_eq!(none.get(pos(0, 65, 0)), None);
    }

    #[test]
    fn the_farthest_section_is_evicted_first() {
        // Capacity for radius 16 is 3 * 3 * 3 = 27 sections.
        assert_eq!(section_capacity(16), 27);
        assert_eq!(section_capacity(96), 13 * 13 * 13);
        let mut map = KnownMap::new(16, 1_000_000, 100, 40);
        assert_eq!(map.capacity(), 27);

        let focus = (0, 4, 0);
        // Fill every slot of the 3 x 3 x 3 cube around the focus.
        for x in -1..=1 {
            for y in 3..=5 {
                for z in -1..=1 {
                    let cell = pos(x * 16 + 1, y * 16 + 1, z * 16 + 1);
                    map.record_seen(cell, state(1), 1, 1, focus);
                }
            }
        }
        assert_eq!(map.sections_len(), 27);
        // A new section must evict one of the eight corner sections, which are
        // all equally farthest; the new, closer section is kept.
        let corners: Vec<BlockPos> = [-1, 1]
            .into_iter()
            .flat_map(|x| {
                [3, 5].into_iter().flat_map(move |y| {
                    [-1, 1]
                        .into_iter()
                        .map(move |z| pos(x * 16 + 1, y * 16 + 1, z * 16 + 1))
                })
            })
            .collect();
        let new = pos(2 * 16 + 1, 4 * 16 + 1, 0);
        map.record_seen(new, state(2), 1, 1, focus);
        assert_eq!(map.sections_len(), 27, "the map never exceeds its bound");
        let remaining = corners.iter().filter(|cell| map.get(**cell).is_some()).count();
        assert_eq!(remaining, 7, "one corner section went first");
        assert!(map.get(new).is_some(), "the new cell is kept");
    }
}
