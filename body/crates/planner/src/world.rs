//! What the planner is allowed to see: known blocks only.
//!
//! The planner never touches the raw store. It asks a [`KnownBlocks`] source
//! one cell at a time; `praxion-perception` is the production implementation
//! ([`PerceptionBlocks`]), and a hand-built [`TestWorld`] is the fixture one.
//! An `Unknown` cell is a value: a move that would enter it stops the plan at
//! the frontier and returns `partial` (contract §5.5).
//!
//! Every answer is cached for the duration of one plan. The A* asks for the
//! same cells many times, and a perception query costs rays; the cache turns
//! that into one ray per cell and also keeps the search deterministic.

use std::collections::{HashMap, HashSet};

use azalea_block::BlockState;
use praxion_perception::{Caller, Perception};
use praxion_world::{BlockPos, Vec3, World};

use crate::blocks;

/// One cell answer. `Known` carries the age of the sighting (ticks since the
/// ray last reached the cell); the planner ignores the age today but the
/// contract asks for it, and a future move may refuse a fact that is too old.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Block {
    Known { state: BlockState, age_ticks: u64 },
    Unknown,
}

/// The only world access the planner gets.
pub trait KnownBlocks {
    fn block(&mut self, pos: BlockPos) -> Block;

    /// The cell an entity stands in, from perception. `None` when the entity
    /// is not known; an `Entity` goal then fails as `GoalUnknown`.
    fn entity_cell(&mut self, id: i32) -> Option<BlockPos> {
        let _ = id;
        None
    }

    /// The lowest y of the world. The default is the overworld's `-64`; a
    /// source that knows its dimension (`PerceptionBlocks`, `DirectBlocks`)
    /// overrides it from the world.
    fn min_y(&self) -> i32 {
        DEFAULT_MIN_Y
    }

    /// The world's height in blocks. The default is the overworld's 384.
    fn height(&self) -> u32 {
        DEFAULT_HEIGHT
    }
}

/// The overworld's vertical extent, used when the source does not know its
/// dimension yet.
pub const DEFAULT_MIN_Y: i32 = -64;
pub const DEFAULT_HEIGHT: u32 = 384;

/// The vanilla world border: no position beyond ±30,000,000 on x or z exists.
/// The planner refuses those before it does any arithmetic on them.
pub const WORLD_BORDER: i32 = 30_000_000;

/// The box one plan may search inside: the world's vertical extent and the
/// world border. Built once per plan from the source, so `plan` can refuse a
/// start or goal that cannot exist before any move arithmetic runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    min_y: i64,
    max_y: i64,
}

impl Limits {
    pub fn of(source: &dyn KnownBlocks) -> Self {
        let min_y = i64::from(source.min_y());
        Self {
            min_y,
            max_y: min_y + i64::from(source.height()) - 1,
        }
    }

    /// Whether a cell is inside the world. Every comparison is widened to
    /// `i64` first: `i32::abs` of `i32::MIN` and `min_y + height` would
    /// otherwise overflow.
    pub fn contains(&self, pos: BlockPos) -> bool {
        let border = i64::from(WORLD_BORDER);
        i64::from(pos.x).abs() <= border
            && i64::from(pos.z).abs() <= border
            && i64::from(pos.y) >= self.min_y
            && i64::from(pos.y) <= self.max_y
    }
}

/// Whether the player can occupy a cell, given the planning constraints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Cell {
    Free,
    Blocked,
    Unknown,
}

/// A plan-scoped view over a [`KnownBlocks`] source.
pub(crate) struct WorldView<'a> {
    source: &'a mut dyn KnownBlocks,
    cache: HashMap<BlockPos, Option<BlockState>>,
    protected: &'a HashSet<BlockPos>,
    blacklist: &'a HashSet<BlockPos>,
}

impl<'a> WorldView<'a> {
    pub fn new(
        source: &'a mut dyn KnownBlocks,
        protected: &'a HashSet<BlockPos>,
        blacklist: &'a HashSet<BlockPos>,
    ) -> Self {
        Self {
            source,
            cache: HashMap::new(),
            protected,
            blacklist,
        }
    }

    /// The block state, or `None` when unknown.
    pub fn state(&mut self, pos: BlockPos) -> Option<BlockState> {
        if let Some(cached) = self.cache.get(&pos) {
            return *cached;
        }
        let state = match self.source.block(pos) {
            Block::Known { state, .. } => Some(state),
            Block::Unknown => None,
        };
        self.cache.insert(pos, state);
        state
    }

    /// `Free` when the player's body can be in this cell; `Blocked` for a
    /// protected or blacklisted cell even if it is air.
    pub fn cell(&mut self, pos: BlockPos) -> Cell {
        if self.protected.contains(&pos) || self.blacklist.contains(&pos) {
            return Cell::Blocked;
        }
        match self.state(pos) {
            Some(state) if blocks::is_passable(state) => Cell::Free,
            Some(_) => Cell::Blocked,
            None => Cell::Unknown,
        }
    }

    /// `Free` when the cell below `pos` is a floor the player can stand on.
    /// Standing on a protected floor is allowed; it is only protected from
    /// being broken or built on.
    pub fn floor(&mut self, pos: BlockPos) -> Cell {
        let below = BlockPos::new(pos.x, pos.y.saturating_sub(1), pos.z);
        match self.state(below) {
            Some(state) if blocks::is_solid(state) => Cell::Free,
            Some(_) => Cell::Blocked,
            None => Cell::Unknown,
        }
    }

    /// Whether the planning state forbids changing or entering this cell.
    pub fn protected_cell(&self, pos: BlockPos) -> bool {
        self.protected.contains(&pos) || self.blacklist.contains(&pos)
    }

    /// A cell a block may be placed into: air, no fluid, not protected.
    pub fn placeable(&mut self, pos: BlockPos) -> Cell {
        if self.protected.contains(&pos) || self.blacklist.contains(&pos) {
            return Cell::Blocked;
        }
        match self.state(pos) {
            Some(state) if blocks::is_replaceable(state) => Cell::Free,
            Some(_) => Cell::Blocked,
            None => Cell::Unknown,
        }
    }

    /// `Free` when the cell holds water. Lava is a hazard, not water.
    pub fn water(&mut self, pos: BlockPos) -> Cell {
        match self.state(pos) {
            Some(state) if blocks::is_water(state) => Cell::Free,
            Some(_) => Cell::Blocked,
            None => Cell::Unknown,
        }
    }

    /// Whether one of the six face neighbours of `pos` is a solid block that
    /// can support a placed block.
    pub fn has_support(&mut self, pos: BlockPos) -> Cell {
        let mut unknown = false;
        for neighbor in neighbors6(pos) {
            match self.state(neighbor) {
                Some(state) if blocks::is_solid(state) => return Cell::Free,
                Some(_) => {}
                None => unknown = true,
            }
        }
        if unknown { Cell::Unknown } else { Cell::Blocked }
    }

}

pub(crate) fn neighbors6(pos: BlockPos) -> [BlockPos; 6] {
    // Saturating, like the moves' own `offset`: a cell at the edge of the
    // coordinate range must never wrap into a different one.
    let step = |dx: i32, dy: i32, dz: i32| {
        BlockPos::new(
            pos.x.saturating_add(dx),
            pos.y.saturating_add(dy),
            pos.z.saturating_add(dz),
        )
    };
    [
        step(0, 1, 0),
        step(0, -1, 0),
        step(0, 0, -1),
        step(0, 0, 1),
        step(1, 0, 0),
        step(-1, 0, 0),
    ]
}

/// The production [`KnownBlocks`]: the perception API, with the player's own
/// world passed per query. `begin_tick` is the driver's job, not the
/// planner's, so a plan spends the tick's remaining ray budget and no more.
pub struct PerceptionBlocks<'a> {
    pub perception: &'a mut Perception,
    pub world: &'a World,
}

impl KnownBlocks for PerceptionBlocks<'_> {
    fn block(&mut self, pos: BlockPos) -> Block {
        match self.perception.known(pos) {
            Some(remembered) => Block::Known {
                state: remembered.state,
                age_ticks: self.perception.tick().saturating_sub(remembered.seen_tick),
            },
            None => Block::Unknown,
        }
    }

    fn entity_cell(&mut self, id: i32) -> Option<BlockPos> {
        self.perception
            .entity(Caller::Other, self.world, id)
            .value()
            .map(|fact| fact.position.to_block_pos_floor())
    }

    fn min_y(&self) -> i32 {
        self.world.min_y().unwrap_or(DEFAULT_MIN_Y)
    }

    fn height(&self) -> u32 {
        self.world.height().unwrap_or(DEFAULT_HEIGHT)
    }
}

/// A hand-built world for tests and fixtures. Everything not named is air;
/// [`TestWorld::set_unknown`] names cells the planner must not know.
#[derive(Clone, Debug)]
pub struct TestWorld {
    blocks: HashMap<BlockPos, BlockState>,
    unknown: HashSet<BlockPos>,
    entities: HashMap<i32, BlockPos>,
}

impl TestWorld {
    pub fn new() -> Self {
        Self {
            blocks: HashMap::new(),
            unknown: HashSet::new(),
            entities: HashMap::new(),
        }
    }

    /// A stone floor at `y` for every x and z within `radius`.
    pub fn flat(y: i32, radius: i32) -> Self {
        let mut world = Self::new();
        for x in -radius..=radius {
            for z in -radius..=radius {
                world.set(x, y, z, BlockState::from(azalea_registry::builtin::BlockKind::Stone));
            }
        }
        world
    }

    pub fn set(&mut self, x: i32, y: i32, z: i32, state: BlockState) {
        self.blocks.insert(BlockPos::new(x, y, z), state);
    }

    pub fn set_pos(&mut self, pos: BlockPos, state: BlockState) {
        self.blocks.insert(pos, state);
    }

    pub fn remove(&mut self, x: i32, y: i32, z: i32) {
        self.blocks.remove(&BlockPos::new(x, y, z));
    }

    pub fn set_unknown(&mut self, x: i32, y: i32, z: i32) {
        self.unknown.insert(BlockPos::new(x, y, z));
    }

    pub fn add_entity(&mut self, id: i32, pos: BlockPos) {
        self.entities.insert(id, pos);
    }

    /// The state at `pos` as the fixture knows it, ignoring `unknown`.
    pub fn known_state(&self, pos: BlockPos) -> BlockState {
        self.blocks.get(&pos).copied().unwrap_or(BlockState::AIR)
    }

    pub fn is_unknown(&self, pos: BlockPos) -> bool {
        self.unknown.contains(&pos)
    }
}

impl Default for TestWorld {
    fn default() -> Self {
        Self::new()
    }
}

impl KnownBlocks for TestWorld {
    fn block(&mut self, pos: BlockPos) -> Block {
        if self.unknown.contains(&pos) {
            return Block::Unknown;
        }
        Block::Known {
            state: self.known_state(pos),
            age_ticks: 0,
        }
    }

    fn entity_cell(&mut self, id: i32) -> Option<BlockPos> {
        self.entities.get(&id).copied()
    }
}

/// A `KnownBlocks` view over the real `World`, for fixtures that exercise the
/// planner without perception (the follower test). Production code uses
/// [`PerceptionBlocks`].
pub struct DirectBlocks<'a> {
    pub world: &'a World,
}

impl KnownBlocks for DirectBlocks<'_> {
    fn block(&mut self, pos: BlockPos) -> Block {
        match self.world.block_state(pos) {
            Some(state) => Block::Known {
                state,
                age_ticks: 0,
            },
            None => Block::Unknown,
        }
    }

    fn min_y(&self) -> i32 {
        self.world.min_y().unwrap_or(DEFAULT_MIN_Y)
    }

    fn height(&self) -> u32 {
        self.world.height().unwrap_or(DEFAULT_HEIGHT)
    }
}

/// The centre of a cell, as a `Vec3`.
pub fn center(pos: BlockPos) -> Vec3 {
    Vec3::new(f64::from(pos.x) + 0.5, f64::from(pos.y), f64::from(pos.z) + 0.5)
}
