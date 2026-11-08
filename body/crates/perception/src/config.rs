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

/// Cells the known map may hold at once; `0` keeps no memory at all. This is a
/// safety cap beside the section radius, sized well above a 32-block sweep.
pub const DEFAULT_MEMORY_CAPACITY: usize = 1_000_000;

/// Blocks around the player whose seen sections the known map keeps. Sections
/// are evicted farthest first when the bound is exceeded.
pub const DEFAULT_KNOWN_RADIUS: u32 = 96;

/// Blocks around the player the sweep probes. Near enough that a few seconds
/// of leftover ray budget cover the space the planner is likely to use.
pub const DEFAULT_SWEEP_RADIUS: u32 = 32;

/// A known cell older than this is probed again.
pub const DEFAULT_SWEEP_STALE_TICKS: u64 = 100;

/// A cell the rays did not reach is not probed again for this many ticks, so
/// hidden cells cannot starve the sweep.
pub const DEFAULT_SWEEP_RETRY_TICKS: u64 = 40;

/// Ticks between view scans. Every scan casts the whole fan.
pub const DEFAULT_VIEW_SCAN_PERIOD: u64 = 10;

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
    /// Cells the known map may hold at once; `0` disables memory. The section
    /// radius is the main bound; this is a second, exact cap.
    pub memory_capacity: usize,
    /// Radius whose chunk sections the known map keeps.
    pub known_radius: u32,
    /// Radius the per-tick sweep probes.
    pub sweep_radius: u32,
    /// Age after which a known cell is probed again.
    pub sweep_stale_ticks: u64,
    /// Delay before a cell no ray reached is probed again.
    pub sweep_retry_ticks: u64,
    /// Ticks between view scans.
    pub view_scan_period: u64,
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
            known_radius: DEFAULT_KNOWN_RADIUS,
            sweep_radius: DEFAULT_SWEEP_RADIUS,
            sweep_stale_ticks: DEFAULT_SWEEP_STALE_TICKS,
            sweep_retry_ticks: DEFAULT_SWEEP_RETRY_TICKS,
            view_scan_period: DEFAULT_VIEW_SCAN_PERIOD,
            max_region_blocks: DEFAULT_MAX_REGION_BLOCKS,
            max_list_results: DEFAULT_MAX_LIST_RESULTS,
        }
    }
}

impl Config {
    /// Rays `caller` may still spend this tick, given what each caller spent.
    ///
    /// `Caller::Other` always keeps the unreserved share
    /// `ray_budget - reflex_reserve`, less its own spending: rays Reflex spent
    /// from its reserve do not shrink it. `Caller::Reflex` may spend whatever
    /// is left of the whole budget, its reserve plus the unreserved share
    /// Other has not spent yet.
    ///
    /// Other is also capped by the whole budget, so Reflex using the unused
    /// unreserved share can never let the tick pass `ray_budget`.
    pub fn remaining(&self, caller: Caller, other_used: u32, reflex_used: u32) -> u32 {
        let whole_left = self.ray_budget.saturating_sub(other_used.saturating_add(reflex_used));
        match caller {
            Caller::Reflex => whole_left,
            Caller::Other => self
                .ray_budget
                .saturating_sub(self.reflex_reserve)
                .saturating_sub(other_used)
                .min(whole_left),
        }
    }
}
