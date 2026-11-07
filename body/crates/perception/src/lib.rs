//! The perception API: deny-by-default world queries under the line-of-sight
//! rule (architecture §5.1).
//!
//! Only this crate reads the raw store. Every query answers [`Answer::Known`]
//! with a provenance and an age, or [`Answer::Unknown`] with a reason. A query
//! never guesses and never returns a stale value where the contract says
//! `Unknown(budget)`.
//!
//! [`Perception::begin_tick`] starts a tick and resets the budget and caches;
//! every query then takes the world it was told about. The world is passed per
//! query instead of being stored so the driver keeps ownership of it and can
//! apply packets between ticks.
//!
//! See `docs/decisions.md` 2026-11-01 ("See-through blocks") and
//! `docs/sessions/03-perception-world-model.md`.

mod answer;
mod cache;
mod config;
mod hash;
mod known;
mod ledger;
mod see_through;
mod sight;
mod sweep;

#[cfg(test)]
mod tests;

use std::collections::HashSet;

use azalea_core::position::ChunkSectionPos;
use azalea_entity::dimensions::EntityDimensions;
use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_world::{BlockPos, BlockState, Entity, Health, ItemStack, Vec3, World};

pub use answer::{Answer, Caller, Provenance, Reason};
use cache::{BlockCacheKey, EntityCacheKey, VisibilityCache};
pub use config::{
    Config, DEFAULT_ENTITY_CAP, DEFAULT_EYE_HEIGHT, DEFAULT_HEARING_RADIUS, DEFAULT_KNOWN_RADIUS,
    DEFAULT_MAX_LIST_RESULTS, DEFAULT_MAX_REGION_BLOCKS, DEFAULT_MEMORY_CAPACITY, DEFAULT_RANGE,
    DEFAULT_RAY_BUDGET, DEFAULT_REFLEX_RESERVE, DEFAULT_SNEAK_EYE_HEIGHT, DEFAULT_SWEEP_RADIUS,
    DEFAULT_SWEEP_RETRY_TICKS, DEFAULT_SWEEP_STALE_TICKS,
};
pub use known::Remembered;
pub use ledger::{FactKind, Ledger, LedgerEntry, QueryKind};
pub use sweep::Sweep;

use sight::{CellReader, CrossedChunks, SightTable};

/// A block fact returned by a query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlockFact {
    pub pos: BlockPos,
    pub state: BlockState,
    pub kind: BlockKind,
    pub distance: f64,
}

/// An entity fact returned by a query.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityFact {
    pub id: i32,
    pub kind: EntityKind,
    pub position: Vec3,
    pub distance: f64,
}

/// A heard sound, with its own age.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundFact {
    pub id: String,
    pub position: Vec3,
    pub tick: u64,
    pub age_ticks: u64,
    pub entity: Option<i32>,
}

/// Which blocks a query accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockFilter {
    Any,
    Kind(BlockKind),
}

impl BlockFilter {
    fn matches(self, kind: BlockKind) -> bool {
        match self {
            BlockFilter::Any => true,
            BlockFilter::Kind(wanted) => kind == wanted,
        }
    }
}

/// Which entities a query accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntityFilter {
    Any,
    Kind(EntityKind),
}

impl EntityFilter {
    fn matches(self, kind: EntityKind) -> bool {
        match self {
            EntityFilter::Any => true,
            EntityFilter::Kind(wanted) => kind == wanted,
        }
    }
}

/// An inclusive block box, e.g. a region for [`Perception::blocks_in`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockRegion {
    pub min: BlockPos,
    pub max: BlockPos,
}

impl BlockRegion {
    pub fn new(a: BlockPos, b: BlockPos) -> Self {
        Self {
            min: BlockPos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)),
            max: BlockPos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)),
        }
    }

    /// A cube of `radius` blocks around `center`.
    pub fn around(center: BlockPos, radius: u32) -> Self {
        let radius = i32::try_from(radius).unwrap_or(i32::MAX);
        Self::new(
            BlockPos::new(
                center.x.saturating_sub(radius),
                center.y.saturating_sub(radius),
                center.z.saturating_sub(radius),
            ),
            BlockPos::new(
                center.x.saturating_add(radius),
                center.y.saturating_add(radius),
                center.z.saturating_add(radius),
            ),
        )
    }

    pub fn volume(&self) -> u64 {
        let x = i64::from(self.max.x) - i64::from(self.min.x) + 1;
        let y = i64::from(self.max.y) - i64::from(self.min.y) + 1;
        let z = i64::from(self.max.z) - i64::from(self.min.z) + 1;
        x.max(0) as u64 * y.max(0) as u64 * z.max(0) as u64
    }
}

/// The perception API for one world.
pub struct Perception {
    config: Config,
    world_valid: bool,
    last_followed: Option<u32>,
    last_dimension: Option<String>,
    tick: u64,
    sneaking: bool,
    /// Rays each caller spent this tick. Kept apart so one caller's spending
    /// never shrinks the other's guaranteed share.
    rays_other: u32,
    rays_reflex: u32,
    entity_checked: HashSet<i32>,
    entity_overflow: u64,
    table: SightTable,
    block_cache: VisibilityCache<BlockCacheKey>,
    entity_cache: VisibilityCache<EntityCacheKey>,
    last_eye_cell: Option<BlockPos>,
    known: known::KnownMap,
    ledger: Ledger,
}

impl Perception {
    pub fn new(config: Config) -> Self {
        Self {
            known: known::KnownMap::new(
                config.known_radius,
                config.memory_capacity,
                config.sweep_stale_ticks,
                config.sweep_retry_ticks,
            ),
            config,
            world_valid: false,
            last_followed: None,
            last_dimension: None,
            tick: 0,
            sneaking: false,
            rays_other: 0,
            rays_reflex: 0,
            entity_checked: HashSet::new(),
            entity_overflow: 0,
            table: SightTable::new(),
            block_cache: VisibilityCache::new(),
            entity_cache: VisibilityCache::new(),
            last_eye_cell: None,
            ledger: Ledger::default(),
        }
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Set by the body from the client's pose. The raw store does not track the
    /// own entity's pose yet, so the eye height is chosen here.
    pub fn set_sneaking(&mut self, sneaking: bool) {
        self.sneaking = sneaking;
    }

    pub fn sneaking(&self) -> bool {
        self.sneaking
    }

    /// Start a tick: reset the ray budget and invalidate what the contract
    /// names.
    ///
    /// The caches live across ticks. They are invalidated by the world's own
    /// revisions (a block change, a chunk load or unload) and by the eye
    /// moving to another cell, exactly as architecture §5.1 says; a tick with
    /// no change keeps them, so a repeated query costs no ray at all.
    pub fn begin_tick(&mut self, world: &World) {
        self.world_valid = world.status().is_valid();
        let followed = world.followed();
        let dimension = world.dimension().map(|dimension| dimension.dimension().to_owned());
        // Memory and caches must not cross a connection, a dimension or a
        // desync: revisions are per world instance and mean nothing after a
        // reset.
        if !self.world_valid
            || self.last_followed.is_some_and(|last| Some(last) != followed)
            || self.last_dimension.as_deref().is_some_and(|last| Some(last) != dimension.as_deref())
        {
            self.known.clear();
            self.block_cache.clear();
            self.entity_cache.clear();
            self.last_eye_cell = None;
        }
        self.last_followed = followed;
        self.last_dimension = dimension;
        self.tick = self.tick.saturating_add(1);
        self.rays_other = 0;
        self.rays_reflex = 0;
        self.entity_checked.clear();
        self.entity_overflow = 0;
        self.forget_other_eye_cells(world);
    }

    /// Drop the cache entries of the eye cell the eye left. Every key carries
    /// the eye cell, so those entries could never be used again.
    fn forget_other_eye_cells(&mut self, world: &World) {
        let eye_cell = self.valid_eye(world).map(|eye| eye.to_block_pos_floor());
        if eye_cell == self.last_eye_cell {
            return;
        }
        match eye_cell {
            Some(cell) => {
                self.block_cache.retain_eye_cell(cell);
                self.entity_cache.retain_eye_cell(cell);
            }
            None => {
                self.block_cache.clear();
                self.entity_cache.clear();
            }
        }
        self.last_eye_cell = eye_cell;
    }

    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn rays_used_this_tick(&self) -> u32 {
        self.rays_other.saturating_add(self.rays_reflex)
    }

    pub fn rays_remaining(&self, caller: Caller) -> u32 {
        self.remaining_rays(caller)
    }

    /// Rays `caller` may still spend this tick (see [`Config::remaining`]).
    fn remaining_rays(&self, caller: Caller) -> u32 {
        self.config.remaining(caller, self.rays_other, self.rays_reflex)
    }

    /// Write a cell the body just changed (a confirmed place or dig) to the
    /// known map from the world, without a ray. Before, the map learnt the
    /// change only when the sweep next probed the cell, so on the tick a
    /// placed table was confirmed `near(crafting_table, 4)` was still false
    /// and the skill chose to go and gather logs for another one.
    pub fn note_changed(&mut self, world: &World, pos: BlockPos) {
        let (Some(state), Some(eye)) = (world.block_state(pos), self.eye(world)) else {
            return;
        };
        self.record_seen(world, pos, state, eye);
    }

    /// Write a cell a ray reached to the known map, with the chunk revision it
    /// was checked against.
    fn record_seen(&mut self, world: &World, pos: BlockPos, state: BlockState, eye: Vec3) {
        let focus = section_key_of(eye.to_block_pos_floor());
        let revision = world
            .chunk_revision(praxion_world::ChunkPos::from(pos))
            .unwrap_or(0);
        self.known
            .record_seen(pos, state, self.tick, revision, focus);
    }

    /// Account one cast ray to `caller`.
    fn spend_ray(&mut self, caller: Caller) {
        match caller {
            Caller::Reflex => self.rays_reflex = self.rays_reflex.saturating_add(1),
            Caller::Other => self.rays_other = self.rays_other.saturating_add(1),
        }
    }

    /// Entities over [`Config::entity_cap`] this tick. Each one answered
    /// `Unknown(budget)`.
    pub fn entity_overflow(&self) -> u64 {
        self.entity_overflow
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    /// Cells a ray has reached and the known map keeps.
    pub fn memory_len(&self) -> usize {
        self.known.seen_len()
    }

    /// Sections the known map currently holds (its bound is the number of
    /// sections within [`Config::known_radius`]).
    pub fn known_sections(&self) -> usize {
        self.known.sections_len()
    }

    /// The known map's section bound, derived from [`Config::known_radius`].
    pub fn known_section_capacity(&self) -> usize {
        known::section_capacity(self.config.known_radius)
    }

    /// A block state from the known map, with the tick it was last seen. Spends
    /// no ray: this is what the planner reads.
    pub fn known(&self, pos: BlockPos) -> Option<Remembered> {
        self.known.get(pos)
    }

    /// The nearest cell of the known map within `radius` of the eye whose
    /// remembered state `accept` returns true for, with the eye distance.
    ///
    /// This is the one query behind both `near(block, r)` and `explore`: they
    /// must agree, or a skill would think nothing is near while `explore`
    /// returns instantly, or the reverse. It spends no rays, so its cost is a
    /// bounded cube scan, and `exclude` lets a caller skip cells a failed plan
    /// already rejected.
    pub fn nearest_known(
        &self,
        world: &World,
        radius: u32,
        exclude: impl Fn(BlockPos) -> bool,
        mut accept: impl FnMut(BlockState) -> bool,
    ) -> Option<(BlockPos, f64)> {
        let eye = self.eye(world)?;
        let eye_cell = eye.to_block_pos_floor();
        let r = i32::try_from(radius).unwrap_or(i32::MAX);
        let r2 = i64::from(r) * i64::from(r);
        // Visit Chebyshev shells from the eye outwards and return the first
        // match. A tree at eight blocks costs 17^3 cells, not the whole 65^3
        // cube; only a genuinely empty radius pays the full scan.
        for shell in 0..=r {
            for dy in -shell..=shell {
                for dz in -shell..=shell {
                    for dx in -shell..=shell {
                        if dx.abs().max(dy.abs()).max(dz.abs()) != shell {
                            continue;
                        }
                        let distance_sq = i64::from(dx) * i64::from(dx)
                            + i64::from(dy) * i64::from(dy)
                            + i64::from(dz) * i64::from(dz);
                        if distance_sq > r2 {
                            continue;
                        }
                        let pos = BlockPos::new(
                            eye_cell.x.saturating_add(dx),
                            eye_cell.y.saturating_add(dy),
                            eye_cell.z.saturating_add(dz),
                        );
                        if exclude(pos) {
                            continue;
                        }
                        let Some(remembered) = self.known.get(pos) else {
                            continue;
                        };
                        if accept(remembered.state) {
                            return Some((pos, eye.distance_to(pos.center())));
                        }
                    }
                }
            }
        }
        None
    }

    /// The world's eye position: player position plus the current eye height.
    pub fn eye(&self, world: &World) -> Option<Vec3> {
        let pos = world.player_position()?;
        let height = if self.sneaking {
            self.config.sneak_eye_height
        } else {
            self.config.eye_height
        };
        Some(Vec3::new(pos.x, pos.y + height, pos.z))
    }

    /// The block cell the eye is in.
    pub fn eye_cell(&self, world: &World) -> Option<BlockPos> {
        self.eye(world).map(|eye| eye.to_block_pos_floor())
    }

    /// Deny-by-default: a query on an invalid world answers
    /// `Unknown(never_seen)` and spends nothing. Every query starts here
    /// (architecture §5.1).
    fn valid_world<'w>(&self, world: &'w World) -> Option<&'w World> {
        self.world_valid.then_some(world)
    }

    /// [`valid_world`](Self::valid_world) plus the eye. Every ray query needs
    /// both, and both failures answer the same `Unknown(never_seen)`.
    fn valid_eye(&self, world: &World) -> Option<Vec3> {
        self.valid_world(world)?;
        self.eye(world)
    }

    /// A block at `pos`. Answers memory (with its age) when the block is no
    /// longer in sight; answers `Unknown(budget)`, never memory, when the ray
    /// budget is spent.
    pub fn block(&mut self, caller: Caller, world: &World, pos: BlockPos) -> Answer<BlockFact> {
        let Some(eye) = self.valid_eye(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        let Some(state) = world.block_state(pos) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        let mut reader = CellReader::new();
        match self.visible_block(world, caller, eye, pos, state, &mut reader) {
            Ok(()) => {
                self.record_seen(world, pos, state, eye);
                let kind = sight::kind(state);
                Answer::known(
                    BlockFact { pos, state, kind, distance: eye.distance_to(pos.center()) },
                    Provenance::Sight,
                    0,
                )
            }
            Err(Reason::Budget) => Answer::Unknown(Reason::Budget),
            Err(reason) => {
                if reason == Reason::OutOfSight {
                    self.ledger.record(QueryKind::Block, FactKind::BlockState, self.tick);
                }
                match self.known.get(pos) {
                    Some(remembered) => {
                        let kind = sight::kind(remembered.state);
                        Answer::known(
                            BlockFact {
                                pos,
                                state: remembered.state,
                                kind,
                                distance: eye.distance_to(pos.center()),
                            },
                            Provenance::Memory,
                            self.tick.saturating_sub(remembered.seen_tick),
                        )
                    }
                    None => Answer::Unknown(reason),
                }
            }
        }
    }

    /// Every visible block of `filter` inside `region`, bounded by
    /// [`Config::max_region_blocks`] and [`Config::max_list_results`].
    ///
    /// The region is walked section by section, nearest section first, and a
    /// section whose palette cannot hold a wanted state is skipped without
    /// touching its 4096 cells.
    pub fn blocks_in(
        &mut self,
        caller: Caller,
        world: &World,
        region: BlockRegion,
        filter: BlockFilter,
    ) -> Answer<Vec<BlockFact>> {
        let Some(eye) = self.valid_eye(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        if region.volume() > self.config.max_region_blocks {
            self.ledger.record(QueryKind::BlocksIn, FactKind::RegionSize, self.tick);
            return Answer::Unknown(Reason::Denied);
        }
        let wanted = match filter {
            BlockFilter::Kind(kind) => Some(kind),
            BlockFilter::Any => None,
        };
        let range = f64::from(self.config.range);
        let mut found = Vec::new();
        let mut reader = CellReader::new();
        for (_, section) in sections_near(eye, region) {
            let Some(view) = world.section(section) else {
                continue;
            };
            if wanted.is_some_and(|kind| !view.may_contain(|state| sight::kind(state) == kind)) {
                continue;
            }
            let cells = SectionCells::of(section, region);
            for y in cells.min.y..=cells.max.y {
                for z in cells.min.z..=cells.max.z {
                    for x in cells.min.x..=cells.max.x {
                        let pos = BlockPos::new(x, y, z);
                        let center = pos.center();
                        let distance = eye.distance_to(center);
                        if distance > range {
                            continue;
                        }
                        let state = view.state(
                            (x - cells.base.x) as usize,
                            (y - cells.base.y) as usize,
                            (z - cells.base.z) as usize,
                        );
                        let kind = sight::kind(state);
                        if !filter.matches(kind) {
                            continue;
                        }
                        match self.visible_block(world, caller, eye, pos, state, &mut reader) {
                            Ok(()) => {
                                // A block a region query sees by sight is seen
                                // like one `block` answers, so the known map
                                // keeps it.
                                self.record_seen(world, pos, state, eye);
                                found.push(BlockFact { pos, state, kind, distance });
                                if found.len() >= self.config.max_list_results {
                                    self.ledger.record(
                                        QueryKind::BlocksIn,
                                        FactKind::ResultLimit,
                                        self.tick,
                                    );
                                    return Answer::Unknown(Reason::Denied);
                                }
                            }
                            Err(Reason::Budget) => return Answer::Unknown(Reason::Budget),
                            Err(_) => {}
                        }
                    }
                }
            }
        }
        Answer::known(found, Provenance::Sight, 0)
    }

    /// The nearest visible block of `kind` within `radius` blocks.
    ///
    /// Sections are visited nearest first and skipped by their palette, so the
    /// scan costs about the sections that can hold `kind`, not the whole
    /// sphere. The walk stops once the nearest candidates are provably closer
    /// than any cell of the sections left.
    pub fn nearest_block(
        &mut self,
        caller: Caller,
        world: &World,
        kind: BlockKind,
        radius: u32,
    ) -> Answer<BlockFact> {
        let Some(eye) = self.valid_eye(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        let eye_cell = eye.to_block_pos_floor();
        let limit = self.config.max_list_results;
        if limit == 0 {
            return Answer::Unknown(Reason::NeverSeen);
        }
        let radius = f64::from(radius).min(f64::from(self.config.range));
        let radius_sq = radius * radius;
        let r = radius as i32;
        let region = BlockRegion::new(
            BlockPos::new(
                eye_cell.x.saturating_sub(r),
                eye_cell.y.saturating_sub(r),
                eye_cell.z.saturating_sub(r),
            ),
            BlockPos::new(
                eye_cell.x.saturating_add(r),
                eye_cell.y.saturating_add(r),
                eye_cell.z.saturating_add(r),
            ),
        );
        let mut nearest: std::collections::BinaryHeap<ByDistance> =
            std::collections::BinaryHeap::new();
        for (lower_bound_sq, section) in sections_near(eye, region) {
            // Every cell left is at least this far away, so a full candidate
            // set closer than that already holds the answer.
            if nearest.len() >= limit
                && nearest.peek().is_some_and(|farthest| farthest.0 <= lower_bound_sq)
            {
                break;
            }
            let Some(view) = world.section(section) else {
                continue;
            };
            if !view.may_contain(|state| sight::kind(state) == kind) {
                continue;
            }
            let cells = SectionCells::of(section, region);
            for y in cells.min.y..=cells.max.y {
                for z in cells.min.z..=cells.max.z {
                    for x in cells.min.x..=cells.max.x {
                        let dx = eye.x - (f64::from(x) + 0.5);
                        let dy = eye.y - (f64::from(y) + 0.5);
                        let dz = eye.z - (f64::from(z) + 0.5);
                        let distance_sq = dx * dx + dy * dy + dz * dz;
                        if distance_sq > radius_sq {
                            continue;
                        }
                        let state = view.state(
                            (x - cells.base.x) as usize,
                            (y - cells.base.y) as usize,
                            (z - cells.base.z) as usize,
                        );
                        if sight::kind(state) != kind {
                            continue;
                        }
                        push_nearest(
                            &mut nearest,
                            limit,
                            distance_sq,
                            BlockPos::new(x, y, z),
                        );
                    }
                }
            }
        }
        let mut reader = CellReader::new();
        for entry in nearest.into_sorted_vec() {
            let pos = entry.1;
            let Some(state) = world.block_state(pos) else {
                continue;
            };
            match self.visible_block(world, caller, eye, pos, state, &mut reader) {
                Ok(()) => {
                    self.record_seen(world, pos, state, eye);
                    return Answer::known(
                        BlockFact {
                            pos,
                            state,
                            kind,
                            distance: eye.distance_to(pos.center()),
                        },
                        Provenance::Sight,
                        0,
                    );
                }
                Err(Reason::Budget) => return Answer::Unknown(Reason::Budget),
                Err(_) => {}
            }
        }
        Answer::Unknown(Reason::NeverSeen)
    }

    /// One tracked entity by id.
    pub fn entity(&mut self, caller: Caller, world: &World, id: i32) -> Answer<EntityFact> {
        let Some(eye) = self.valid_eye(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        let Some(entity) = world.entity(id) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        let distance = eye.distance_to(entity.position());
        if distance > f64::from(self.config.range) {
            return Answer::Unknown(Reason::OutOfRange);
        }
        match self.visible_entity(world, caller, entity) {
            Ok(()) => Answer::known(fact(entity, distance), Provenance::Sight, 0),
            Err(Reason::OutOfSight) => {
                self.ledger.record(QueryKind::Entity, FactKind::EntityPosition, self.tick);
                Answer::Unknown(Reason::OutOfSight)
            }
            Err(reason) => Answer::Unknown(reason),
        }
    }

    /// Every visible entity of `filter` within `radius` blocks.
    pub fn entities_near(
        &mut self,
        caller: Caller,
        world: &World,
        radius: u32,
        filter: EntityFilter,
    ) -> Answer<Vec<EntityFact>> {
        let Some(eye) = self.valid_eye(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        let radius = f64::from(radius).min(f64::from(self.config.range));
        let mut found = Vec::new();
        let entities: Vec<Entity> = world.entities().cloned().collect();
        for entity in &entities {
            if !filter.matches(entity.kind()) {
                continue;
            }
            let distance = eye.distance_to(entity.position());
            if distance > radius {
                continue;
            }
            match self.visible_entity(world, caller, entity) {
                Ok(()) => {
                    found.push(fact(entity, distance));
                    if found.len() >= self.config.max_list_results {
                        self.ledger.record(
                            QueryKind::EntitiesNear,
                            FactKind::ResultLimit,
                            self.tick,
                        );
                        return Answer::Unknown(Reason::Denied);
                    }
                }
                Err(Reason::Budget) => return Answer::Unknown(Reason::Budget),
                Err(_) => {}
            }
        }
        Answer::known(found, Provenance::Sight, 0)
    }

    /// The nearest visible entity of `kind` within `radius` blocks.
    pub fn nearest_entity(
        &mut self,
        caller: Caller,
        world: &World,
        kind: EntityKind,
        radius: u32,
    ) -> Answer<EntityFact> {
        let Some(eye) = self.valid_eye(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        let radius = f64::from(radius).min(f64::from(self.config.range));
        let mut candidates: Vec<(f64, Entity)> = world
            .entities()
            .filter(|entity| entity.kind() == kind)
            .map(|entity| (eye.distance_to(entity.position()), entity.clone()))
            .filter(|(distance, _)| *distance <= radius)
            .collect();
        candidates.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (distance, entity) in candidates {
            match self.visible_entity(world, caller, &entity) {
                Ok(()) => return Answer::known(fact(&entity, distance), Provenance::Sight, 0),
                Err(Reason::Budget) => return Answer::Unknown(Reason::Budget),
                Err(_) => {}
            }
        }
        Answer::Unknown(Reason::NeverSeen)
    }

    /// Sounds within `radius` blocks, capped by [`Config::hearing_radius`].
    /// Each fact carries its own age; the answer's age is the newest one.
    pub fn sounds_near(&mut self, world: &World, radius: u32) -> Answer<Vec<SoundFact>> {
        let Some(eye) = self.valid_eye(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        let radius = radius.min(self.config.hearing_radius);
        let mut found = Vec::new();
        let mut newest: Option<u64> = None;
        for sound in world.sounds() {
            let distance = eye.distance_to(sound.position());
            if distance > f64::from(radius) {
                continue;
            }
            let age = self.tick.saturating_sub(sound.tick());
            newest = Some(newest.map_or(age, |old| old.min(age)));
            found.push(SoundFact {
                id: sound.id().to_owned(),
                position: sound.position(),
                tick: sound.tick(),
                age_ticks: age,
                entity: sound.entity(),
            });
        }
        if found.is_empty() {
            return Answer::Unknown(Reason::NeverSeen);
        }
        Answer::known(found, Provenance::Hearing, newest.unwrap_or(0))
    }

    /// Own position: an F3 fact, allowed without a ray.
    pub fn own_position(&self, world: &World) -> Answer<Vec3> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        match world.player_position() {
            Some(pos) => Answer::known(pos, Provenance::Sight, 0),
            None => Answer::Unknown(Reason::NeverSeen),
        }
    }

    /// Own rotation `(yaw, pitch)`: an F3 fact.
    pub fn own_rotation(&self, world: &World) -> Answer<(f32, f32)> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        Answer::known(world.player_rotation(), Provenance::Sight, 0)
    }

    /// Own dimension: an F3 fact.
    pub fn dimension(&self, world: &World) -> Answer<praxion_world::Dimension> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        match world.dimension().cloned() {
            Some(dimension) => Answer::known(dimension, Provenance::Sight, 0),
            None => Answer::Unknown(Reason::NeverSeen),
        }
    }

    /// Biome at the own position. The raw store does not carry biome data in
    /// this step, so the answer is `Unknown(never_seen)`.
    ///
    /// TODO(backlog "Biome and light in the raw store (F3 facts)"): read the
    /// biome from `world` once the raw store keeps it. The `&World` parameter
    /// is here already so callers need no change when the data arrives.
    pub fn biome(&self, _world: &World) -> Answer<String> {
        Answer::Unknown(Reason::NeverSeen)
    }

    /// Light level at the own position. The raw store does not carry light
    /// data in this step, so the answer is `Unknown(never_seen)`.
    ///
    /// TODO(backlog "Biome and light in the raw store (F3 facts)"): read the
    /// light level from `world` once the raw store keeps it. The `&World`
    /// parameter is here already so callers need no change when it arrives.
    pub fn light_level(&self, _world: &World) -> Answer<u8> {
        Answer::Unknown(Reason::NeverSeen)
    }

    /// Day time: an F3 fact.
    pub fn day_time(&self, world: &World) -> Answer<u64> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        Answer::known(world.day_time(), Provenance::Sight, 0)
    }

    /// Own health: an F3 fact.
    pub fn own_health(&self, world: &World) -> Answer<Health> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        match world.health() {
            Some(health) => Answer::known(health, Provenance::Sight, 0),
            None => Answer::Unknown(Reason::NeverSeen),
        }
    }

    /// Own food level: an F3 fact.
    pub fn own_food(&self, world: &World) -> Answer<u32> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        match world.health() {
            Some(health) => Answer::known(health.food(), Provenance::Sight, 0),
            None => Answer::Unknown(Reason::NeverSeen),
        }
    }

    /// Own air supply in ticks, from the player's own entity data: an F3 fact,
    /// allowed without a ray. `Unknown(never_seen)` until the server sends it,
    /// which is not the same as full air.
    pub fn air(&self, world: &World) -> Answer<u32> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        match world.own_air() {
            Some(air) => Answer::known(air, Provenance::Sight, 0),
            None => Answer::Unknown(Reason::NeverSeen),
        }
    }

    /// Whether the server has the player on fire, from its shared flags byte:
    /// an F3 fact, allowed without a ray. `Unknown` until the server sends it.
    pub fn burning(&self, world: &World) -> Answer<bool> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        match world.own_burning() {
            Some(burning) => Answer::known(burning, Provenance::Sight, 0),
            None => Answer::Unknown(Reason::NeverSeen),
        }
    }

    /// Own inventory: an F3 fact.
    pub fn inventory(&self, world: &World) -> Answer<Vec<ItemStack>> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        Answer::known(world.inventory().to_vec(), Provenance::Sight, 0)
    }

    /// Selected hotbar slot: an F3 fact.
    pub fn selected_slot(&self, world: &World) -> Answer<u32> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        match world.selected_hotbar_slot() {
            Some(slot) => Answer::known(slot, Provenance::Sight, 0),
            None => Answer::Unknown(Reason::NeverSeen),
        }
    }

    /// Game mode: an F3 fact.
    pub fn game_mode(&self, world: &World) -> Answer<azalea_core::game_type::GameMode> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        match world.game_mode() {
            Some(mode) => Answer::known(mode, Provenance::Sight, 0),
            None => Answer::Unknown(Reason::NeverSeen),
        }
    }

    /// Weather: an F3 fact.
    pub fn weather(&self, world: &World) -> Answer<praxion_world::Weather> {
        let Some(world) = self.valid_world(world) else {
            return Answer::Unknown(Reason::NeverSeen);
        };
        Answer::known(world.weather(), Provenance::Sight, 0)
    }

    /// A mob's health is denied by policy. The attempt is ledgered.
    pub fn entity_health(&mut self, _caller: Caller, _id: i32) -> Answer<f32> {
        self.ledger.record(QueryKind::EntityHealth, FactKind::EntityHealth, self.tick);
        Answer::Unknown(Reason::Denied)
    }

    /// A remembered block by position, without a ray and without a ledger
    /// entry. `None` when nothing was ever seen there.
    pub fn remembered(&self, pos: BlockPos) -> Option<(Remembered, u64)> {
        self.known.get(pos).map(|remembered| {
            let age = self.tick.saturating_sub(remembered.seen_tick);
            (remembered, age)
        })
    }

    /// `Ok(())` when the block is visible now; otherwise the reason it is not.
    ///
    /// The answer is cached across ticks and stays valid while the block state
    /// is the same, the eye is in the same cell, and no chunk the rays crossed
    /// changed (architecture §5.1).
    ///
    /// `reader` is the caller's cell reader; it should already hold the section
    /// of `pos`, so the face neighbours cost a state read each.
    fn visible_block<'w>(
        &mut self,
        world: &'w World,
        caller: Caller,
        eye: Vec3,
        pos: BlockPos,
        state: BlockState,
        reader: &mut CellReader<'w>,
    ) -> Result<(), Reason> {
        let eye_cell = eye.to_block_pos_floor();
        let revision = world.revision();
        let key = (eye_cell, pos);
        if let Some(visible) = self.block_cache.cached(world, key, Some(state), revision) {
            return if visible { Ok(()) } else { Err(Reason::OutOfSight) };
        }
        let distance = eye.distance_to(pos.center());
        if distance > f64::from(self.config.range) {
            return Err(Reason::OutOfRange);
        }
        if self.remaining_rays(caller) == 0 {
            return Err(Reason::Budget);
        }
        let mut points = [Vec3::ZERO; 7];
        reader.prime(world, pos);
        let count = sight::target_points(world, pos, &mut self.table, &mut points, reader);
        let mut crossed = CrossedChunks::default();
        // A ray that reaches the target never reads the target's last cell, so
        // record its chunk here: a block change in it must invalidate the entry
        // even when it happens beside the ray.
        crossed.record_target(world, pos);
        let mut visible = false;
        for point in &points[..count] {
            if self.remaining_rays(caller) == 0 {
                return Err(Reason::Budget);
            }
            self.spend_ray(caller);
            if sight::line_of_sight(world, eye, *point, pos, &mut self.table, &mut crossed) {
                visible = true;
                break;
            }
        }
        if crossed.is_complete() {
            self.block_cache
                .remember(key, Some(state), visible, revision, crossed);
        }
        if visible { Ok(()) } else { Err(Reason::OutOfSight) }
    }

    /// `Ok(())` when a ray set reaches the entity. The entity is checked at
    /// most once per tick, and at most [`Config::entity_cap`] entities are.
    fn visible_entity(
        &mut self,
        world: &World,
        caller: Caller,
        entity: &Entity,
    ) -> Result<(), Reason> {
        let Some(eye) = self.eye(world) else {
            return Err(Reason::NeverSeen);
        };
        let eye_cell = eye.to_block_pos_floor();
        let cell = entity.position().to_block_pos_floor();
        let key = (eye_cell, entity.id(), cell);
        let revision = world.revision();
        if let Some(visible) = self.entity_cache.cached(world, key, None, revision) {
            return if visible { Ok(()) } else { Err(Reason::OutOfSight) };
        }
        if !self.entity_checked.contains(&entity.id())
            && self.entity_checked.len() as u32 >= self.config.entity_cap
        {
            self.entity_overflow = self.entity_overflow.saturating_add(1);
            return Err(Reason::Budget);
        }
        if self.remaining_rays(caller) == 0 {
            return Err(Reason::Budget);
        }
        self.entity_checked.insert(entity.id());

        let dimensions = EntityDimensions::from(entity.kind());
        let feet = entity.position();
        let eye_point = Vec3::new(feet.x, feet.y + f64::from(dimensions.eye_height), feet.z);
        let center = Vec3::new(
            feet.x,
            feet.y + f64::from(dimensions.height) / 2.0,
            feet.z,
        );
        let mut points = [Vec3::ZERO; 3];
        let mut count = 0;
        for point in [eye_point, center, feet] {
            if points[..count]
                .iter()
                .any(|seen| seen.distance_squared_to(point) < 1.0e-12)
            {
                continue;
            }
            points[count] = point;
            count += 1;
        }

        let mut crossed = CrossedChunks::default();
        crossed.record_target(world, cell);
        let mut visible = false;
        for point in &points[..count] {
            if self.remaining_rays(caller) == 0 {
                return Err(Reason::Budget);
            }
            self.spend_ray(caller);
            let point_cell = point.to_block_pos_floor();
            if sight::line_of_sight(world, eye, *point, point_cell, &mut self.table, &mut crossed) {
                visible = true;
                break;
            }
        }
        if crossed.is_complete() {
            self.entity_cache.remember(key, None, visible, revision, crossed);
        }
        if visible { Ok(()) } else { Err(Reason::OutOfSight) }
    }
}

fn fact(entity: &Entity, distance: f64) -> EntityFact {
    EntityFact {
        id: entity.id(),
        kind: entity.kind(),
        position: entity.position(),
        distance,
    }
}

/// One block position ordered by distance; the heap keeps the farthest on top
/// so the nearest `limit` candidates fit in bounded memory.
///
/// The distance is squared: it orders candidates exactly like the distance and
/// costs no square root.
#[derive(Clone, Copy, Debug)]
struct ByDistance(f64, BlockPos);

impl PartialEq for ByDistance {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == std::cmp::Ordering::Equal
    }
}

impl Eq for ByDistance {}

impl PartialOrd for ByDistance {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ByDistance {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

/// Keep the `limit` nearest candidates without holding the whole search in
/// memory.
fn push_nearest(
    candidates: &mut std::collections::BinaryHeap<ByDistance>,
    limit: usize,
    distance_sq: f64,
    pos: BlockPos,
) {
    if limit == 0 {
        return;
    }
    candidates.push(ByDistance(distance_sq, pos));
    if candidates.len() > limit {
        candidates.pop();
    }
}

/// The cells of one section that also lie inside `region`.
struct SectionCells {
    /// The section's first cell, for turning a world coordinate into the
    /// section-local index [`praxion_world::SectionView::state`] takes.
    base: BlockPos,
    min: BlockPos,
    max: BlockPos,
}

impl SectionCells {
    fn of(section: ChunkSectionPos, region: BlockRegion) -> Self {
        // `section.x * 16` is a multiple of 16 inside `i32` for any section
        // coordinate a block position can produce, so `+ 15` cannot overflow.
        let base = BlockPos::new(section.x * 16, section.y * 16, section.z * 16);
        let end = BlockPos::new(base.x + 15, base.y + 15, base.z + 15);
        Self {
            base,
            min: BlockPos::new(
                base.x.max(region.min.x),
                base.y.max(region.min.y),
                base.z.max(region.min.z),
            ),
            max: BlockPos::new(
                end.x.min(region.max.x),
                end.y.min(region.max.y),
                end.z.min(region.max.z),
            ),
        }
    }
}

/// The sections that overlap `region`, nearest first: the key is the squared
/// distance from `eye` to the closest cell center the section holds. That is
/// also a lower bound on the distance of any candidate in the section, which
/// is what lets a search stop early.
fn sections_near(eye: Vec3, region: BlockRegion) -> Vec<(f64, ChunkSectionPos)> {
    let first = ChunkSectionPos {
        x: region.min.x >> 4,
        y: region.min.y >> 4,
        z: region.min.z >> 4,
    };
    let last = ChunkSectionPos {
        x: region.max.x >> 4,
        y: region.max.y >> 4,
        z: region.max.z >> 4,
    };
    let mut sections = Vec::with_capacity(
        (i64::from(last.x) - i64::from(first.x) + 1).max(1) as usize
            * (i64::from(last.y) - i64::from(first.y) + 1).max(1) as usize
            * (i64::from(last.z) - i64::from(first.z) + 1).max(1) as usize,
    );
    for x in first.x..=last.x {
        for y in first.y..=last.y {
            for z in first.z..=last.z {
                let section = ChunkSectionPos { x, y, z };
                sections.push((lower_bound_sq(eye, SectionCells::of(section, region)), section));
            }
        }
    }
    sections.sort_by(|a, b| a.0.total_cmp(&b.0));
    sections
}

/// The section key `(x, y, z)` of a block position.
fn section_key_of(pos: BlockPos) -> (i32, i32, i32) {
    (pos.x >> 4, pos.y >> 4, pos.z >> 4)
}

/// The squared distance from `eye` to the nearest cell center in `cells`.
fn lower_bound_sq(eye: Vec3, cells: SectionCells) -> f64 {
    let nearest = |value: f64, low: i32, high: i32| {
        let low = f64::from(low) + 0.5;
        let high = f64::from(high) + 0.5;
        if value < low {
            low
        } else if value > high {
            high
        } else {
            value
        }
    };
    let dx = eye.x - nearest(eye.x, cells.min.x, cells.max.x);
    let dy = eye.y - nearest(eye.y, cells.min.y, cells.max.y);
    let dz = eye.z - nearest(eye.z, cells.min.z, cells.max.z);
    dx * dx + dy * dy + dz * dz
}

/// Tests for the section order the two block searches rely on.
#[cfg(test)]
mod region_tests {
    use super::*;

    #[test]
    fn sections_are_ordered_by_distance_and_bound_the_cells() {
        let eye = Vec3::new(0.5, 65.6, 0.5);
        let region = BlockRegion::around(BlockPos::new(0, 65, 0), 20);
        let sections = sections_near(eye, region);
        // x and z: -20..=20 is sections -2..=1, y: 45..=85 is sections 2..=5.
        assert_eq!(sections.len(), 4 * 4 * 4, "{sections:?}");
        for window in sections.windows(2) {
            assert!(window[0].0 <= window[1].0, "{sections:?}");
        }
        assert_eq!(sections[0].1, ChunkSectionPos { x: 0, y: 4, z: 0 });
        for (lower_bound_sq, section) in &sections {
            let cells = SectionCells::of(*section, region);
            for x in cells.min.x..=cells.max.x {
                for y in cells.min.y..=cells.max.y {
                    for z in cells.min.z..=cells.max.z {
                        let center = BlockPos::new(x, y, z).center();
                        let distance_sq = eye.distance_squared_to(center);
                        assert!(
                            distance_sq >= *lower_bound_sq - 1.0e-9,
                            "{section:?} bound {lower_bound_sq} > cell {distance_sq}"
                        );
                    }
                }
            }
        }
    }
}
