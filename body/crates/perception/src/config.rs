//! Tunable perception settings. The defaults are the decided values; a stricter
//! profile for an ablation is a different `Config`.

use crate::Caller;

/// Sight range in blocks (decided: about 64, tunable).
pub const DEFAULT_RANGE: u32 = 64;

/// Player eye height standing, in blocks. Minecraft's player eye height is
/// 1.62 (Minecraft Wiki, "Player"); the body sends a sneaking pose separately
/// because the raw store does not track the own entity's pose yet.
pub const DEFAULT_EYE_HEIGHT: f64 = 1.62;

/// Player eye height while sneaking: 1.27 blocks (Minecraft Wiki, "Sneaking").
pub const DEFAULT_SNEAK_EYE_HEIGHT: f64 = 1.27;

/// Hearing radius in blocks (decided: about 16, tunable).
pub const DEFAULT_HEARING_RADIUS: u32 = 16;

/// Rays one tick may cast, all callers together.
pub const DEFAULT_RAY_BUDGET: u32 = 4_000;

/// Of [`DEFAULT_RAY_BUDGET`], the part only `Caller::Reflex` may spend.
pub const DEFAULT_REFLEX_RESERVE: u32 = 1_000;

/// Distinct tracked entities whose visibility may be computed per tick.
pub const DEFAULT_ENTITY_CAP: u32 = 64;

/// Remembered block positions kept, least recently used dropped first.
pub const DEFAULT_MEMORY_CAPACITY: usize = 16_384;

/// Largest block volume a `blocks_in` request may name.
pub const DEFAULT_MAX_REGION_BLOCKS: u64 = 1_000_000;

/// Largest result list a `blocks_in` request may produce.
pub const DEFAULT_MAX_LIST_RESULTS: usize = 4_096;

/// Perception settings.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub range: u32,
    pub eye_height: f64,
    pub sneak_eye_height: f64,
    pub hearing_radius: u32,
    pub ray_budget: u32,
    pub reflex_reserve: u32,
    pub entity_cap: u32,
    pub memory_capacity: usize,
    pub max_region_blocks: u64,
    pub max_list_results: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            range: DEFAULT_RANGE,
            eye_height: DEFAULT_EYE_HEIGHT,
            sneak_eye_height: DEFAULT_SNEAK_EYE_HEIGHT,
            hearing_radius: DEFAULT_HEARING_RADIUS,
            ray_budget: DEFAULT_RAY_BUDGET,
            reflex_reserve: DEFAULT_REFLEX_RESERVE,
            entity_cap: DEFAULT_ENTITY_CAP,
            memory_capacity: DEFAULT_MEMORY_CAPACITY,
            max_region_blocks: DEFAULT_MAX_REGION_BLOCKS,
            max_list_results: DEFAULT_MAX_LIST_RESULTS,
        }
    }
}

impl Config {
    /// Rays a caller may still spend this tick, given what was spent.
    pub fn remaining(&self, caller: Caller, used: u32) -> u32 {
        match caller {
            Caller::Reflex => self.ray_budget.saturating_sub(used),
            Caller::Other => self
                .ray_budget
                .saturating_sub(self.reflex_reserve)
                .saturating_sub(used),
        }
    }
}
