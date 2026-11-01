//! Line of sight: a 3D DDA voxel walk (Amanatides and Woo, "A Fast Voxel
//! Traversal Algorithm for Ray Tracing", 1987) with ray-vs-AABB tests inside
//! each cell.
//!
//! The walk visits the cells the segment passes through. Each visited cell
//! before the target is tested against the block's collision boxes. The target
//! cell is never tested: the question is whether the ray reaches the block, not
//! whether the block hides itself.
//!
//! Two things keep a ray inside the tick budget:
//!
//! - [`SightTable`] classifies a block state once (pass, opaque, or needs a box
//!   test) in a table indexed by state id. The walk then does no collision-shape
//!   fetch and no hash lookup per cell: it is one array read.
//! - The section the walk is in is kept across cells, so the chunk map is read
//!   once per section instead of once per cell. Each section entered records
//!   its chunk revision in a fixed array, which is what makes a cached result
//!   invalidate exactly when a block the ray depended on changed.

use azalea_block::BlockState;
use azalea_core::aabb::Aabb;
use azalea_core::position::{BlockPos, ChunkPos, ChunkSectionPos, Vec3};
use azalea_physics::collision::BlockWithShape;
use azalea_registry::builtin::BlockKind;
use praxion_world::{SectionView, World};

use crate::see_through::see_through_override;

/// A little more than zero, for boundary comparisons.
const EPS: f64 = 1.0e-9;

/// How many chunks the visibility of one block may depend on and still be
/// cached: the chunks its rays crossed plus its own. A 64-block ray crosses at
/// most 12 chunks (grid traversal), so only long diagonal rays end up uncached;
/// their result is still correct, just not remembered.
pub(crate) const MAX_CROSSED_CHUNKS: usize = 6;

/// How a sight ray meets the cell that holds a block state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sight {
    /// Not described yet; [`SightTable`] fills it on first use.
    Unfilled,
    /// The ray always passes through the cell.
    Pass,
    /// The block fills its cell, so the ray always hits it.
    Opaque,
    /// The ray needs a box test against this entry of [`SightTable::boxes`].
    Boxes(u32),
}

/// The sight class of every block state, built on first use and kept for the
/// life of the [`Perception`](crate::Perception) (architecture §5.1: the table
/// of see-through blocks decided at first code).
pub(crate) struct SightTable {
    /// One entry per state id; `entries[state.id()]`.
    entries: Vec<Sight>,
    /// The collision boxes of every state that is neither pass nor opaque.
    boxes: Vec<Box<[Aabb]>>,
}

impl SightTable {
    pub(crate) fn new() -> Self {
        Self {
            entries: vec![Sight::Unfilled; usize::from(BlockState::MAX_STATE) + 1],
            boxes: Vec::new(),
        }
    }

    /// The sight class of `state`, describing it the first time it is asked
    /// for.
    #[inline]
    pub(crate) fn sight(&mut self, state: BlockState) -> Sight {
        let entry = self.entries[usize::from(state.id())];
        if entry == Sight::Unfilled {
            self.describe(state)
        } else {
            entry
        }
    }

    /// The collision boxes of a `Sight::Boxes` entry.
    #[inline]
    fn boxes(&self, index: u32) -> &[Aabb] {
        &self.boxes[index as usize]
    }

    /// Classify a state and remember it.
    fn describe(&mut self, state: BlockState) -> Sight {
        let index = usize::from(state.id());
        let name = state
            .as_block_kind()
            .to_str()
            .trim_start_matches("minecraft:");
        let sight = match see_through_override(name) {
            // An override decides sight; the collision shape does not matter.
            Some(true) => Sight::Pass,
            // A no-collision override fills its cell so the ray cannot slip
            // through the block geometrically.
            Some(false) => Sight::Opaque,
            None => {
                let shape = state.base_collision_shape();
                if shape.is_empty() {
                    Sight::Pass
                } else {
                    let boxes = shape.to_aabbs();
                    if is_unit_cube(&boxes) {
                        Sight::Opaque
                    } else {
                        let slot = u32::try_from(self.boxes.len())
                            .expect("more boxed states than a u32 can index");
                        self.boxes.push(boxes.into_boxed_slice());
                        Sight::Boxes(slot)
                    }
                }
            }
        };
        self.entries[index] = sight;
        sight
    }
}

/// Whether these boxes are one box that fills the whole cell.
///
/// A box that covers the cell always stops a ray that passes through it, which
/// is what the sight table means by [`Sight::Opaque`].
fn is_unit_cube(boxes: &[Aabb]) -> bool {
    match boxes {
        [only] => {
            only.min.x <= 0.0
                && only.min.y <= 0.0
                && only.min.z <= 0.0
                && only.max.x >= 1.0
                && only.max.y >= 1.0
                && only.max.z >= 1.0
        }
        _ => false,
    }
}

/// The chunks the visibility of one block depends on: the chunks its rays
/// crossed plus the target's own chunk, with the revision each had at cast time.
///
/// A fixed array keeps the walk allocation-free and makes a cache entry small.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CrossedChunks {
    entries: [(ChunkPos, u64); MAX_CROSSED_CHUNKS],
    len: u8,
    /// `false` when a crossed chunk could not be recorded (too many of them,
    /// or a chunk without a revision): the result must not be cached.
    complete: bool,
}

impl Default for CrossedChunks {
    fn default() -> Self {
        Self {
            entries: [(ChunkPos::new(0, 0), 0); MAX_CROSSED_CHUNKS],
            len: 0,
            complete: true,
        }
    }
}

impl CrossedChunks {
    /// Whether the result may be cached at all.
    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    /// Whether every chunk this ray crossed still has the revision it had.
    pub(crate) fn is_current(&self, world: &World) -> bool {
        self.complete
            && self.entries[..usize::from(self.len)]
                .iter()
                .all(|(pos, revision)| world.chunk_revision(*pos) == Some(*revision))
    }

    /// Record the chunk the walk just entered, once per chunk.
    #[inline]
    fn record(&mut self, world: &World, pos: ChunkPos) {
        if !self.complete {
            return;
        }
        let seen = &self.entries[..usize::from(self.len)];
        if seen.iter().any(|(chunk, _)| *chunk == pos) {
            return;
        }
        match world.chunk_revision(pos) {
            Some(revision) if usize::from(self.len) < MAX_CROSSED_CHUNKS => {
                self.entries[usize::from(self.len)] = (pos, revision);
                self.len += 1;
            }
            _ => self.complete = false,
        }
    }

    /// Record a chunk the rays depend on that they may not have entered, such
    /// as the target block's own chunk (architecture §5.1: a block change in
    /// the target's chunk invalidates the entry).
    pub(crate) fn record_target(&mut self, world: &World, pos: BlockPos) {
        self.record(world, ChunkPos::from(pos));
    }
}

/// Reads block states while keeping the current section, so a walk over many
/// cells goes through the chunk map once per section instead of once per cell.
pub(crate) struct CellReader<'w> {
    section_pos: Option<ChunkSectionPos>,
    section: Option<SectionView<'w>>,
}

impl<'w> CellReader<'w> {
    pub(crate) fn new() -> Self {
        Self {
            section_pos: None,
            section: None,
        }
    }

    /// Whether the reader already holds the section `pos` is in.
    #[inline]
    fn holds(&self, pos: BlockPos) -> bool {
        self.section_pos == Some(ChunkSectionPos::from(pos))
    }

    /// Point the reader at `section`, loading it once.
    #[inline]
    fn enter(&mut self, world: &'w World, section: ChunkSectionPos) {
        self.section_pos = Some(section);
        self.section = world.section(section);
    }

    /// The state at `pos`, fetching its section when the reader is elsewhere.
    #[inline]
    pub(crate) fn state(&mut self, world: &'w World, pos: BlockPos) -> Option<BlockState> {
        if !self.holds(pos) {
            self.enter(world, ChunkSectionPos::from(pos));
        }
        self.held_state(pos)
    }

    /// Point the reader at the section of `pos` before reading its neighbours.
    #[inline]
    pub(crate) fn prime(&mut self, world: &'w World, pos: BlockPos) {
        if !self.holds(pos) {
            self.enter(world, ChunkSectionPos::from(pos));
        }
    }

    /// The state at `pos` in the section the reader holds.
    #[inline]
    fn held_state(&self, pos: BlockPos) -> Option<BlockState> {
        let view = self.section?;
        Some(view.state(
            (pos.x & 15) as usize,
            (pos.y & 15) as usize,
            (pos.z & 15) as usize,
        ))
    }
}

/// The block's center, plus the center of every face that borders a
/// see-through cell (at most seven points). Returns how many `points` are set.
///
/// `reader` should already hold the section of `pos` (see
/// [`CellReader::prime`]): the six neighbours then cost a state read each
/// instead of a chunk-map lookup each.
pub(crate) fn target_points<'w>(
    world: &'w World,
    pos: BlockPos,
    table: &mut SightTable,
    points: &mut [Vec3; 7],
    reader: &mut CellReader<'w>,
) -> usize {
    const FACES: [([i32; 3], [f64; 3]); 6] = [
        ([1, 0, 0], [1.0, 0.5, 0.5]),
        ([-1, 0, 0], [0.0, 0.5, 0.5]),
        ([0, 1, 0], [0.5, 1.0, 0.5]),
        ([0, -1, 0], [0.5, 0.0, 0.5]),
        ([0, 0, 1], [0.5, 0.5, 1.0]),
        ([0, 0, -1], [0.5, 0.5, 0.0]),
    ];
    points[0] = pos.center();
    let mut count = 1;
    for (offset, center) in FACES {
        let neighbor = BlockPos::new(pos.x + offset[0], pos.y + offset[1], pos.z + offset[2]);
        let Some(state) = reader.state(world, neighbor) else {
            continue;
        };
        if table.sight(state) == Sight::Pass {
            points[count] = Vec3::new(
                f64::from(pos.x) + center[0],
                f64::from(pos.y) + center[1],
                f64::from(pos.z) + center[2],
            );
            count += 1;
        }
    }
    count
}

/// Whether the segment `from`..`to` reaches `target` without being blocked.
///
/// Unloaded or out-of-world cells before the target block the ray: the walk
/// cannot prove the path is clear. Every chunk entered is appended to
/// `crossed`, so the caller can cache the answer until one of them changes.
pub(crate) fn line_of_sight(
    world: &World,
    from: Vec3,
    to: Vec3,
    target: BlockPos,
    table: &mut SightTable,
    crossed: &mut CrossedChunks,
) -> bool {
    let delta = to - from;
    let from_cell = from.to_block_pos_floor();
    if from_cell == target {
        return true;
    }
    if delta.length_squared() < EPS {
        return false;
    }

    let mut cell = from_cell;
    let mut step = [0i32; 3];
    let mut t_max = [f64::INFINITY; 3];
    let mut t_delta = [f64::INFINITY; 3];
    let origin = [from.x, from.y, from.z];
    let direction = [delta.x, delta.y, delta.z];
    let cell_axis = [cell.x, cell.y, cell.z];
    for axis in 0..3 {
        if direction[axis] > EPS {
            step[axis] = 1;
            let boundary = f64::from(cell_axis[axis]) + 1.0;
            t_max[axis] = (boundary - origin[axis]) / direction[axis];
            t_delta[axis] = 1.0 / direction[axis];
        } else if direction[axis] < -EPS {
            step[axis] = -1;
            let boundary = f64::from(cell_axis[axis]);
            t_max[axis] = (boundary - origin[axis]) / direction[axis];
            t_delta[axis] = -1.0 / direction[axis];
        }
    }

    // The section the walk is in, fetched once per section instead of once per
    // cell.
    let mut reader = CellReader::new();
    let mut section_pos = ChunkSectionPos::from(from_cell);
    reader.enter(world, section_pos);
    crossed.record(world, ChunkPos::new(section_pos.x, section_pos.z));

    // No more cell boundaries than this; a guard against a numerical stall.
    let max_steps = (delta.x.abs() + delta.y.abs() + delta.z.abs()).ceil() as usize + 8;
    for _ in 0..max_steps.min(1024) {
        if cell == target {
            return true;
        }
        if !reader.holds(cell) {
            section_pos = ChunkSectionPos::from(cell);
            reader.enter(world, section_pos);
            crossed.record(world, ChunkPos::new(section_pos.x, section_pos.z));
        }
        match reader.held_state(cell) {
            Some(state) => {
                let sight = table.sight(state);
                match sight {
                    Sight::Pass => {}
                    Sight::Opaque => return false,
                    Sight::Boxes(index) => {
                        let origin = Vec3::new(
                            f64::from(cell.x),
                            f64::from(cell.y),
                            f64::from(cell.z),
                        );
                        for bx in table.boxes(index) {
                            let min = Vec3::new(
                                origin.x + bx.min.x,
                                origin.y + bx.min.y,
                                origin.z + bx.min.z,
                            );
                            let max = Vec3::new(
                                origin.x + bx.max.x,
                                origin.y + bx.max.y,
                                origin.z + bx.max.z,
                            );
                            if ray_hits_aabb(from, delta, min, max) {
                                return false;
                            }
                        }
                    }
                    Sight::Unfilled => unreachable!("sight() describes a state on first use"),
                }
            }
            None => {
                // The eye cell itself is always ours; any other gap in the
                // world stops the proof.
                if cell != from_cell {
                    return false;
                }
            }
        }

        let axis = if t_max[0] <= t_max[1] && t_max[0] <= t_max[2] {
            0
        } else if t_max[1] <= t_max[2] {
            1
        } else {
            2
        };
        if !t_max[axis].is_finite() {
            return true;
        }
        let t = t_max[axis];
        if t > 1.0 + EPS {
            return true;
        }
        match axis {
            0 => cell.x += step[0],
            1 => cell.y += step[1],
            _ => cell.z += step[2],
        }
        t_max[axis] += t_delta[axis];
    }
    false
}

/// Segment-versus-AABB in the unit interval `t ∈ [0, 1]`. Bounds are
/// inclusive: a ray exactly on a face touches the box and counts as a hit.
pub(crate) fn ray_hits_aabb(from: Vec3, delta: Vec3, min: Vec3, max: Vec3) -> bool {
    let mut t_min = 0.0f64;
    let mut t_max = 1.0f64;
    let origin = [from.x, from.y, from.z];
    let direction = [delta.x, delta.y, delta.z];
    let low = [min.x, min.y, min.z];
    let high = [max.x, max.y, max.z];
    for axis in 0..3 {
        if direction[axis].abs() < EPS {
            if origin[axis] < low[axis] || origin[axis] > high[axis] {
                return false;
            }
            continue;
        }
        let inv = 1.0 / direction[axis];
        let mut t1 = (low[axis] - origin[axis]) * inv;
        let mut t2 = (high[axis] - origin[axis]) * inv;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        t_min = t_min.max(t1);
        t_max = t_max.min(t2);
        if t_min > t_max {
            return false;
        }
    }
    true
}

/// Whether the cell holding `state` blocks the segment, used by the exhaustive
/// table test. The walk calls the same three cases inline.
#[cfg(test)]
pub(crate) fn blocks_cell(
    table: &mut SightTable,
    state: BlockState,
    from: Vec3,
    delta: Vec3,
    cell: BlockPos,
) -> bool {
    match table.sight(state) {
        Sight::Pass => false,
        Sight::Opaque => true,
        Sight::Boxes(index) => {
            let (cx, cy, cz) = (f64::from(cell.x), f64::from(cell.y), f64::from(cell.z));
            table.boxes(index).iter().any(|bx| {
                ray_hits_aabb(
                    from,
                    delta,
                    Vec3::new(cx + bx.min.x, cy + bx.min.y, cz + bx.min.z),
                    Vec3::new(cx + bx.max.x, cy + bx.max.y, cz + bx.max.z),
                )
            })
        }
        Sight::Unfilled => unreachable!("sight() describes a state on first use"),
    }
}

/// The block kind of a state, without boxing a `dyn BlockTrait`.
#[inline]
pub(crate) fn kind(state: BlockState) -> BlockKind {
    state.as_block_kind()
}
