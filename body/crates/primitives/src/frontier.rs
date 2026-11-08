//! The `near` fact over the known map and the frontier ranking used by
//! `explore`.

use std::collections::{HashSet, VecDeque};

use azalea_core::position::BlockPos;
use azalea_registry::builtin::BlockKind;
use praxion_gamedata as gamedata;
use praxion_perception::{Perception, workable};
use praxion_world::{BlockState, World};

use crate::heading::{Dir, SECTORS, sector_of};

/// The `near` fact over the perception known map: a target is near when a
/// workable cell this perception has seen inside `radius` holds it. No rays are
/// spent, and the answer is `false` when the map knows of none, so
/// `not near(T, R)` can start an exploration. `None` only when the eye itself
/// is not known yet.
pub(crate) fn near_block(
    perception: &Perception,
    world: &World,
    wanted: &str,
    radius: u32,
) -> Option<bool> {
    perception.eye_cell(world)?;
    Some(nearest_known_block(perception, world, wanted, radius, &[]).is_some())
}

/// The nearest cell in the perception known map that holds `wanted` and is
/// workable (breaking it would not expose water or lava), with its distance from
/// the eye. `None` when nothing is known inside `radius`. `exclude` skips cells
/// a failed plan already rejected.
///
/// This is the one known-map target search behind `explore`'s approach, `mine`'s
/// walk toward a remembered cell, `craft`'s table and `dig_down`'s stop check.
/// The [`workable`] rule keeps all of them from choosing a lake-bed cell that
/// `mine` would refuse, so a search cannot end on a stone the body cannot dig.
pub(crate) fn nearest_known_block(
    perception: &Perception,
    world: &World,
    wanted: &str,
    radius: u32,
    exclude: &[BlockPos],
) -> Option<(BlockPos, f64)> {
    let tag = wanted.strip_prefix('#');
    let wanted_kind = if tag.is_none() {
        Some(wanted.parse::<BlockKind>().ok()?)
    } else {
        None
    };
    let excluded: HashSet<BlockPos> = exclude.iter().copied().collect();
    perception.nearest_known(
        world,
        radius,
        |pos| excluded.contains(&pos),
        |pos, state| {
            let kind = BlockKind::from(state);
            let matches = match (tag, wanted_kind) {
                (Some(tag), _) => gamedata::block_in_tag(kind.to_str(), tag),
                (None, Some(wanted_kind)) => kind == wanted_kind,
                (None, None) => false,
            };
            matches && workable(pos, state, |cell| world.block_state(cell))
        },
    )
}

/// A frontier the explorer could walk to, with the ordering key it was scored
/// by. The caller may cache a ranked list instead of rescanning the map every
/// tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrontierChoice {
    pub cell: BlockPos,
    /// Squared distance from the start cell. Combined with the avoid and
    /// off-level flags this is the ordering used below.
    pub score: i64,
    /// How many known walkable frontier cells were acceptable.
    pub candidates: u32,
    /// The quality flags this candidate was ordered by: avoided, off level,
    /// wet, uphill. A caller that re-orders the list keeps the best tier
    /// together.
    pub tier: (bool, bool, bool, bool),
}

/// The best frontier, for callers that do not need the score.
pub fn choose_frontier(
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Option<BlockPos> {
    rank_frontiers(
        start,
        horizontal,
        vertical,
        explored,
        rejected,
        known,
        allow_water,
    )
    .first()
    .map(|choice| choice.cell)
}

/// Ranks the known walkable frontier cells once, nearest first within the best
/// quality tier. The returned list is sorted best first and capped; the cost is
/// the reachability flood plus one pass over its cells, never the full
/// `(2r+1)^3` cube and never a linear scan of the explored list for every cell
/// in it.
pub fn rank_frontiers(
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Vec<FrontierChoice> {
    scan_frontiers(
        start,
        horizontal,
        vertical,
        explored,
        rejected,
        known,
        allow_water,
        |_| true,
    )
    .rank(None)
}

/// More candidates than an explore budget can try; bounds the allocation.
const MAX_CHOICES: usize = 64;

/// Ranking key: the candidate's tier flags, how far below the on-level band it
/// lies (zero within it), then its forward progress (zero without a heading),
/// distance and position.
type RankKey = (bool, bool, bool, bool, i64, i64, i64, i32, i32, i32);

/// Lateral drift costs half as much as forward progress gains, so a cell
/// straight ahead beats a diagonal one at the same distance.
const LATERAL_WEIGHT: f64 = 0.5;

/// Blocks below the start a frontier may lie before it counts as off level.
const OFF_LEVEL_BELOW: i64 = 8;

/// Blocks below the start a frontier may lie and still rank with the on-level
/// ones: anything deeper is a ravine edge and waits until nothing on level
/// lies ahead.
const ON_LEVEL_BELOW: i64 = 2;

/// A frontier cell of a scan with its quality tier (avoided, off level, wet,
/// uphill) and its squared distance from the start.
type ScanCell = (BlockPos, (bool, bool, bool, bool), i64);

/// The frontier cells of one scan, before they are ordered. Scanning is the
/// expensive part (the reachability flood); ordering is cheap, so a caller can
/// pick a heading from the scan and then rank the same scan toward it.
pub(crate) struct FrontierScan {
    start: BlockPos,
    cells: Vec<ScanCell>,
}

/// Scans the known walkable frontier cells reachable from `start`. `keep`
/// drops cells before they are counted (explore passes the open-to-sky test,
/// so the cap on the ranked list never fills with cave cells).
// The frontier query parameters are the caller's configuration; grouping them
// would add a one-use type.
#[allow(clippy::too_many_arguments)]
pub(crate) fn scan_frontiers(
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
    keep: impl Fn(BlockPos) -> bool,
) -> FrontierScan {
    // Eight blocks: enough that a second pass over the same edge is avoided,
    // small enough that a genuinely new direction still wins.
    const AVOID_RADIUS_SQ: i64 = 8 * 8;
    let horizontal_sq_limit = i64::from(horizontal) * i64::from(horizontal);
    let in_box = |pos: BlockPos| {
        let dx = i64::from(pos.x) - i64::from(start.x);
        let dy = i64::from(pos.y) - i64::from(start.y);
        let dz = i64::from(pos.z) - i64::from(start.z);
        dx.abs() <= i64::from(horizontal)
            && dy.abs() <= i64::from(vertical)
            && dz.abs() <= i64::from(horizontal)
            && dx * dx + dz * dz <= horizontal_sq_limit
    };

    // Only frontier cells reachable through known walkable cells count. An
    // unseen wall is an unknown cell, so it stops this flood and a cell behind
    // it is never chosen.
    let mut reachable: HashSet<BlockPos> = HashSet::new();
    let mut queue: VecDeque<BlockPos> = VecDeque::new();
    reachable.insert(start);
    queue.push_back(start);
    while let Some(cell) = queue.pop_front() {
        for neighbor in walk_neighbors(cell) {
            if !in_box(neighbor) || reachable.contains(&neighbor) {
                continue;
            }
            let Some(state) = known(neighbor) else {
                continue;
            };
            if !standable(neighbor, state, &known, allow_water) {
                continue;
            }
            reachable.insert(neighbor);
            queue.push_back(neighbor);
        }
    }

    let explored_set: HashSet<BlockPos> = explored.iter().copied().collect();
    let rejected_set: HashSet<BlockPos> = rejected.iter().copied().collect();
    let mut cells = Vec::new();
    for pos in &reachable {
        let pos = *pos;
        // The cell the body stands in is never a frontier to walk to. Its head
        // or a side cell can stay unknown (the view scan skips the eye cell),
        // so it does touch the unknown map, and distance zero plus the best
        // tier made it the first candidate on every scan. `explore` then got a
        // successful empty plan, marked the cell explored and never moved
        // (seed 101 live run 2026-11-08: 500 scans, 24,000 ticks, no step).
        if pos == start {
            continue;
        }
        if explored_set.contains(&pos) || rejected_set.contains(&pos) {
            continue;
        }
        let Some(state) = known(pos) else {
            continue;
        };
        if !standable(pos, state, &known, allow_water)
            || !touches_unknown(pos, &known)
            || !keep(pos)
        {
            continue;
        }
        let dx = i64::from(pos.x) - i64::from(start.x);
        let dy = i64::from(pos.y) - i64::from(start.y);
        let dz = i64::from(pos.z) - i64::from(start.z);
        let avoid = explored.iter().any(|explored| {
            let ax = i64::from(explored.x) - i64::from(pos.x);
            let az = i64::from(explored.z) - i64::from(pos.z);
            ax * ax + az * az <= AVOID_RADIUS_SQ
        });
        // Stay near the player's level: a frontier far below (a ravine) or
        // more than four above scores worse than any level one. Caves are
        // `explore`'s open-to-sky filter; this bound is wide enough to walk
        // down a mountain, where two blocks per leg took nine legs to descend
        // fifteen (seed 303 peak, 2026-11-08). The `-8` bound only admits a
        // slope: the rank key still puts every cell more than two below behind
        // the on-level ones, so a ravine edge never beats flat ground by being
        // farther forward.
        let off_level = !(-OFF_LEVEL_BELOW..=4).contains(&dy);
        // Water is walkable, but a land edge is a better place to look for
        // trees: crossing an ocean with a 1200-tick budget is how a doomed
        // search spends it.
        let wet = praxion_planner::blocks::is_water(state);
        // Do not climb a mountain looking for wood: plains and valleys hold
        // the trees, and an uphill frontier is how a search ended at y=117
        // with the forest still 160 blocks away.
        let uphill = dy > 0;
        cells.push((
            pos,
            (avoid, off_level, wet, uphill),
            dx * dx + dy * dy + dz * dz,
        ));
    }
    FrontierScan { start, cells }
}

impl FrontierScan {
    /// Orders the cells best first and caps the list. Without a heading the
    /// nearest cell of the best tier wins. With one, the cell farthest forward
    /// along it wins (lateral drift is discounted): a search that always takes
    /// the nearest edge circles in place, one that keeps a direction crosses
    /// the plains (seed 606: six calls, never 90 blocks from the spawn).
    pub(crate) fn rank(&self, heading: Option<Dir>) -> Vec<FrontierChoice> {
        let candidates = u32::try_from(self.cells.len()).unwrap_or(u32::MAX);
        let mut ranked: Vec<(RankKey, FrontierChoice)> = self
            .cells
            .iter()
            .map(|&(pos, tier, distance_sq)| {
                let forward = heading.map_or(0, |dir| self.forward_key(pos, dir));
                // Zero on level, positive below it: a ravine edge orders
                // after every cell within two below to four above, however
                // far forward it lies.
                let below = (-(i64::from(pos.y) - i64::from(self.start.y)) - ON_LEVEL_BELOW).max(0);
                (
                    (
                        tier.0,
                        tier.1,
                        tier.2,
                        tier.3,
                        below,
                        -forward,
                        distance_sq,
                        pos.x,
                        pos.z,
                        pos.y,
                    ),
                    FrontierChoice {
                        cell: pos,
                        score: distance_sq,
                        candidates,
                        tier,
                    },
                )
            })
            .collect();
        ranked.sort_by_key(|(key, _)| *key);
        ranked.truncate(MAX_CHOICES);
        ranked.into_iter().map(|(_, choice)| choice).collect()
    }

    /// Forward progress of `pos` along `dir` less the lateral drift, in half
    /// blocks.
    fn forward_key(&self, pos: BlockPos, (ux, uz): Dir) -> i64 {
        let dx = f64::from(pos.x - self.start.x);
        let dz = f64::from(pos.z - self.start.z);
        let along = dx * ux + dz * uz;
        let across = (dx * uz - dz * ux).abs();
        ((along - LATERAL_WEIGHT * across) * 2.0).round() as i64
    }

    /// Unseen boundary per compass sector: the number of best-tier frontier
    /// cells in it. A sector behind water or a cliff has none. Counting cells
    /// rather than summing distances keeps the explored trail, whose far edge
    /// lies farther than a fresh one, from pulling the search back.
    pub(crate) fn sector_weights(&self) -> [u32; SECTORS] {
        let mut weights = [0; SECTORS];
        let Some(best) = self.cells.iter().map(|&(_, tier, _)| tier).min() else {
            return weights;
        };
        for &(pos, tier, _) in &self.cells {
            if tier == best {
                let dx = i64::from(pos.x) - i64::from(self.start.x);
                let dz = i64::from(pos.z) - i64::from(self.start.z);
                weights[sector_of(dx, dz)] += 1;
            }
        }
        weights
    }
}

fn standable(
    pos: BlockPos,
    state: BlockState,
    known: &impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> bool {
    if praxion_planner::blocks::is_water(state) {
        return allow_water;
    }
    if !praxion_planner::blocks::is_passable(state) {
        return false;
    }
    known(BlockPos::new(pos.x, pos.y.saturating_sub(1), pos.z))
        .is_some_and(praxion_planner::blocks::is_solid)
}

fn touches_unknown(pos: BlockPos, known: &impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    [
        BlockPos::new(pos.x.saturating_add(1), pos.y, pos.z),
        BlockPos::new(pos.x.saturating_sub(1), pos.y, pos.z),
        BlockPos::new(pos.x, pos.y, pos.z.saturating_add(1)),
        BlockPos::new(pos.x, pos.y, pos.z.saturating_sub(1)),
        BlockPos::new(pos.x, pos.y.saturating_add(1), pos.z),
        BlockPos::new(pos.x, pos.y.saturating_sub(1), pos.z),
    ]
    .into_iter()
    .any(|neighbor| known(neighbor).is_none())
}

/// The cells a walking player can reach from `pos` in one move, for the
/// frontier reachability flood: eight directions, one block up, three down.
fn walk_neighbors(pos: BlockPos) -> Vec<BlockPos> {
    let mut neighbors = Vec::with_capacity(40);
    for dx in -1..=1 {
        for dz in -1..=1 {
            if dx == 0 && dz == 0 {
                continue;
            }
            for dy in -3..=1 {
                neighbors.push(BlockPos::new(
                    pos.x.saturating_add(dx),
                    pos.y.saturating_add(dy),
                    pos.z.saturating_add(dz),
                ));
            }
        }
    }
    neighbors
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn air() -> BlockState {
        BlockState::from(BlockKind::Air)
    }

    fn stone() -> BlockState {
        BlockState::from(BlockKind::Stone)
    }

    fn water() -> BlockState {
        BlockState::from(BlockKind::Water)
    }

    fn lava() -> BlockState {
        BlockState::from(BlockKind::Lava)
    }

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    /// A known platform: air at `y` and `y + 1`, stone floor below and a stone
    /// ceiling above, over the given box. Nothing outside the box is known.
    fn platform(
        map: &mut HashMap<BlockPos, BlockState>,
        x0: i32,
        x1: i32,
        z0: i32,
        z1: i32,
        y: i32,
    ) {
        for x in x0..=x1 {
            for z in z0..=z1 {
                map.insert(at(x, y - 1, z), stone());
                map.insert(at(x, y, z), air());
                map.insert(at(x, y + 1, z), air());
                map.insert(at(x, y + 2, z), stone());
            }
        }
    }

    fn known(map: &HashMap<BlockPos, BlockState>) -> impl Fn(BlockPos) -> Option<BlockState> + '_ {
        move |pos| map.get(&pos).copied()
    }

    fn scan(map: &HashMap<BlockPos, BlockState>, start: BlockPos, horizontal: i32) -> FrontierScan {
        scan_frontiers(start, horizontal, 12, &[], &[], known(map), true, |_| true)
    }

    #[test]
    fn a_heading_prefers_the_far_edge_ahead_over_the_nearest_ring_cell() {
        // Seed 202 is plains with its forest 160 blocks away. Ranking by
        // distance from the start walks the sight-radius ring; ranking along a
        // heading goes straight to the far edge.
        let mut map = HashMap::new();
        platform(&mut map, -30, 30, -2, 2, 64);
        let scan = scan(&map, at(0, 64, 0), 32);
        let west = scan.rank(Some((-1.0, 0.0)));
        assert_eq!(west[0].cell.x, -30, "the far west edge wins: {:?}", west[0]);
        let east = scan.rank(Some((1.0, 0.0)));
        assert_eq!(east[0].cell.x, 30, "the heading decides: {:?}", east[0]);
        let nearest = scan.rank(None);
        assert!(nearest[0].cell.x.abs() <= 2, "no heading is nearest first");
    }

    #[test]
    fn sector_weights_count_the_unseen_boundary_per_sector() {
        let mut map = HashMap::new();
        // A corridor 5 wide, 30 long west of the start and 12 long east: the
        // sides touch unknown cells too, so the longer arm has more boundary.
        platform(&mut map, -30, 12, -2, 2, 64);
        let weights = scan(&map, at(0, 64, 0), 32).sector_weights();
        assert!(weights[0] > 0, "{weights:?}");
        assert!(weights[4] > weights[0], "{weights:?}");
    }

    #[test]
    fn a_frontier_down_a_slope_is_on_level_until_it_is_a_ravine() {
        // A staircase down two blocks per column: a mountain is walked down,
        // so eight blocks below the start is still on level and ten is not.
        let mut map = HashMap::new();
        for x in 0..=5 {
            platform(&mut map, x, x, -1, 1, 64 - 2 * x);
        }
        let scan = scan(&map, at(0, 64, 0), 8);
        let off_level = |y| {
            scan.cells
                .iter()
                .find(|cell| cell.0.y == y)
                .map(|cell| cell.1.1)
        };
        assert_eq!(off_level(56), Some(false), "eight below is on level");
        assert_eq!(off_level(54), Some(true), "ten below is a ravine");
    }

    #[test]
    fn a_ravine_edge_ranks_after_flat_ground_within_the_tier() {
        // Heading east: flat ground ends at x = 30, then a stair drops two
        // blocks per step to a ravine floor eight below. The ravine edge lies
        // farther forward, so forward progress alone would take it; the -8
        // admit bound keeps it in the tier, but the ranking must put it after
        // every on-level cell.
        let mut map = HashMap::new();
        platform(&mut map, 0, 30, 0, 0, 64);
        for x in 31..=34 {
            platform(&mut map, x, x, 0, 0, 64 - 2 * (x - 30));
        }
        platform(&mut map, 34, 45, 0, 0, 56);
        let ranked = scan(&map, at(0, 64, 0), 48).rank(Some((1.0, 0.0)));
        let position = |cell: BlockPos| ranked.iter().position(|choice| choice.cell == cell);
        let flat = position(at(30, 64, 0)).expect("the flat edge is a frontier");
        let ravine = position(at(45, 56, 0)).expect("the ravine edge is a frontier");
        assert!(
            flat < ravine,
            "flat ground beats the deeper, farther edge: {ranked:?}"
        );
        let deepest_on_level = ranked
            .iter()
            .rposition(|choice| choice.cell.y >= 62)
            .expect("an on-level cell");
        let first_below = ranked
            .iter()
            .position(|choice| choice.cell.y < 62)
            .expect("a cell more than two below");
        assert!(
            deepest_on_level < first_below,
            "every cell within -2 ranks before every deeper one: {ranked:?}"
        );
    }

    #[test]
    fn the_nearest_reachable_frontier_is_chosen() {
        let mut map = HashMap::new();
        platform(&mut map, -1, 1, -1, 1, 64);
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true)
            .expect("a frontier on the platform edge");
        // All four edge cells are one block away; the deterministic tie-break
        // picks the smallest x, then z.
        assert_eq!(chosen, at(-1, 64, 0));
        assert!(map.contains_key(&chosen), "a frontier cell is a known cell");
        assert!(touches_unknown(chosen, &known(&map)));
    }

    #[test]
    fn a_closed_known_room_has_no_frontier() {
        let mut map = HashMap::new();
        platform(&mut map, -1, 1, -1, 1, 64);
        // Seal the sides; the floor and ceiling are already known stone.
        for y in 64..=66 {
            for x in -2..=2 {
                map.insert(at(x, y, -2), stone());
                map.insert(at(x, y, 2), stone());
            }
            for z in -2..=2 {
                map.insert(at(-2, y, z), stone());
                map.insert(at(2, y, z), stone());
            }
        }
        assert_eq!(
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true),
            None,
            "a sealed room offers nothing to explore"
        );
    }

    #[test]
    fn a_cell_behind_an_unseen_wall_is_never_chosen() {
        let mut map = HashMap::new();
        // Known corridor from x = 0 to 1, with a remembered island at x = 3.
        for x in 0..=1 {
            map.insert(at(x, 63, 0), stone());
            map.insert(at(x, 64, 0), air());
            map.insert(at(x, 65, 0), stone());
            map.insert(at(x, 64, -1), stone());
            map.insert(at(x, 64, 1), stone());
        }
        map.insert(at(-1, 64, 0), stone());
        for x in 3..=4 {
            map.insert(at(x, 63, 0), stone());
            map.insert(at(x, 64, 0), air());
            map.insert(at(x, 65, 0), stone());
            map.insert(at(x, 64, -1), stone());
            map.insert(at(x, 64, 1), stone());
        }
        // x = 2 is unknown: an unseen wall. The near frontier is x = 1.
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true);
        assert_eq!(chosen, Some(at(1, 64, 0)));
        // Once that one is explored, the island behind the unseen wall must
        // stay unreachable rather than become the next target.
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[at(1, 64, 0)], &[], known(&map), true);
        assert_eq!(chosen, None, "the unseen wall blocks the island");
    }

    #[test]
    fn water_is_walkable_and_lava_is_not() {
        let mut map = HashMap::new();
        // A one-wide corridor whose far cell is unknown.
        for x in 0..=1 {
            map.insert(at(x, 63, 0), stone());
            map.insert(at(x, 64, 0), if x == 0 { air() } else { water() });
            map.insert(at(x, 65, 0), stone());
            map.insert(at(x, 64, -1), stone());
            map.insert(at(x, 64, 1), stone());
        }
        map.insert(at(-1, 64, 0), stone());
        assert_eq!(
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true),
            Some(at(1, 64, 0)),
            "a swimmable water cell is a frontier"
        );

        map.insert(at(1, 64, 0), lava());
        assert_eq!(
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true),
            None,
            "lava is a hazard, not walkable"
        );
    }

    #[test]
    fn explored_frontiers_are_scored_away() {
        let mut map = HashMap::new();
        platform(&mut map, 0, 30, -2, 2, 64);
        let chosen = choose_frontier(
            at(15, 64, 0),
            32,
            8,
            &[at(15, 64, 2)],
            &[],
            known(&map),
            true,
        )
        .expect("a frontier far from the explored one");
        assert_ne!(chosen, at(15, 64, -2), "the nearest side is avoided");
        assert!(chosen.x < 15, "a new direction wins: {chosen:?}");
    }

    #[test]
    fn ranked_frontiers_carry_their_score_and_candidate_count() {
        let mut map = HashMap::new();
        platform(&mut map, -1, 1, -1, 1, 64);
        let ranked = rank_frontiers(at(0, 64, 0), 8, 8, &[], &[], known(&map), true);
        assert!(!ranked.is_empty(), "an open platform edge is a frontier");
        let best = ranked[0];
        assert_eq!(best.cell, at(-1, 64, 0));
        assert_eq!(best.score, 1, "the score is the squared distance");
        assert!(
            best.candidates >= ranked.len() as u32,
            "the count covers the ranked candidates"
        );
        assert!(ranked.len() <= 64, "the list is bounded");
    }

    #[test]
    fn the_cell_the_body_stands_in_is_never_a_frontier() {
        // The body's own head or side cell may stay unknown (the view scan
        // skips the eye cell), so the start touches the unknown map and used
        // to be ranked first with the best tier. Walking to where you already
        // stand is a successful empty plan, so `explore` marked it explored
        // again on every scan and never moved (seed 101 live run 2026-11-08).
        let mut map = HashMap::new();
        platform(&mut map, -1, 1, -1, 1, 64);
        map.remove(&at(1, 64, 0)); // an unknown cell beside the start
        for ranked in [
            rank_frontiers(at(0, 64, 0), 8, 8, &[], &[], known(&map), true),
            scan(&map, at(0, 64, 0), 8).rank(Some((1.0, 0.0))),
        ] {
            assert!(
                ranked.iter().all(|choice| choice.cell != at(0, 64, 0)),
                "the start is not a walk target: {ranked:?}"
            );
            assert!(
                !ranked.is_empty(),
                "the edge cells beside the start remain frontiers"
            );
        }
    }

    #[test]
    fn rank_frontiers_does_not_grow_with_the_explored_list() {
        // The old scan tested every explored cell for every cell of a 65^3
        // cube. Now the explored set is a hash set and the candidates come
        // from the reachability flood, so a long rejected list cannot turn
        // the scan into quadratic work. This pins the result, not a timer.
        let mut map = HashMap::new();
        platform(&mut map, -8, 8, -8, 8, 64);
        let rejected: Vec<BlockPos> = (0..500).map(|i| at(i % 16 - 8, 64, i / 16 - 8)).collect();
        let ranked = rank_frontiers(at(0, 64, 0), 8, 8, &[], &rejected, known(&map), true);
        assert!(ranked.iter().all(|choice| !rejected.contains(&choice.cell)));
    }

    /// Blocks a simulated body sees around itself on flat ground.
    const SIGHT: i32 = 40;

    /// Walks a synthetic body the way `explore` does: scan, take the first
    /// ranked frontier of the best tier, see everything within [`SIGHT`] of
    /// the new cell. `land(x, z)` says whether a column is dry. Returns the
    /// cells reached and the heading sector each leg used. `force` names the
    /// first heading, for tests that need one before the weights speak.
    fn simulate(
        land: impl Fn(i32, i32) -> bool,
        start: (i32, i32),
        force: Option<usize>,
        legs: usize,
    ) -> (Vec<(i32, i32)>, Vec<usize>) {
        use crate::heading::SearchHeading;
        let mut seen: std::collections::HashSet<(i32, i32)> = std::collections::HashSet::new();
        let see = |at: (i32, i32), seen: &mut std::collections::HashSet<(i32, i32)>| {
            for x in at.0 - SIGHT..=at.0 + SIGHT {
                for z in at.1 - SIGHT..=at.1 + SIGHT {
                    if (x - at.0).pow(2) + (z - at.1).pow(2) <= SIGHT * SIGHT {
                        seen.insert((x, z));
                    }
                }
            }
        };
        see(start, &mut seen);
        let mut search = SearchHeading::default();
        if let Some(sector) = force {
            let mut weights = [0; crate::heading::SECTORS];
            weights[sector] = 1;
            search.direction(at(start.0, 64, start.1), &weights);
        }
        let mut here = start;
        let mut explored: Vec<BlockPos> = Vec::new();
        let mut path = vec![start];
        let mut sectors = Vec::new();
        for _ in 0..legs {
            let origin = at(here.0, 64, here.1);
            let known = |pos: BlockPos| {
                if !seen.contains(&(pos.x, pos.z)) || !(62..=66).contains(&pos.y) {
                    return None;
                }
                Some(match pos.y {
                    ..=63 => stone(),
                    64 if !land(pos.x, pos.z) => water(),
                    _ => air(),
                })
            };
            let scan = scan_frontiers(origin, 64, 8, &explored, &[], known, false, |_| true);
            let heading = search.direction(origin, &scan.sector_weights());
            let mut ranked = scan.rank(heading);
            if let Some(tier) = ranked.first().map(|choice| choice.tier) {
                ranked.retain(|choice| choice.tier == tier);
            }
            let Some(goal) = ranked.first().map(|choice| choice.cell) else {
                search.give_up();
                continue;
            };
            sectors.push(search.sector().expect("a heading was used"));
            here = (goal.x, goal.z);
            see(here, &mut seen);
            explored.push(goal);
            if explored.len() > 64 {
                explored.remove(0);
            }
            search.arrived(goal);
            path.push(here);
        }
        (path, sectors)
    }

    #[test]
    fn successive_legs_cross_plains_toward_a_forest_beyond_sight() {
        // A dry strip 41 wide with water on both sides and to the west; the
        // forest starts at x = 200, far beyond the 40-block sight. The nearest
        // edge always lies within sight, so only a kept direction makes
        // monotonic progress east.
        let (path, _) = simulate(|x, z| x >= -20 && z.abs() <= 20, (0, 0), None, 6);
        assert!(
            path.windows(2).all(|pair| pair[1].0 > pair[0].0),
            "every leg moves east: {path:?}"
        );
        let reached = path.last().expect("a path").0;
        assert!(
            reached >= 160,
            "six legs bring the forest into sight (x >= 160): {path:?}"
        );
    }

    #[test]
    fn a_heading_blocked_by_water_turns_and_does_not_swing_back() {
        // A square island (water beyond 70 blocks) and a search forced east at
        // the start. It reaches the shore, gains nothing, gives east up and
        // moves on, shore after shore, without returning to a sector it gave
        // up or oscillating between two headings.
        let (_, sectors) = simulate(|x, z| x.abs() <= 70 && z.abs() <= 70, (20, 0), Some(0), 40);
        assert_eq!(sectors[0], 0, "the forced heading is used first");
        let mut turns = sectors.clone();
        turns.dedup();
        assert!(turns.len() >= 3, "the search keeps turning: {sectors:?}");
        let mut seen = std::collections::HashSet::new();
        for sector in &turns {
            assert!(
                seen.insert(*sector),
                "heading {sector} was taken twice: {sectors:?}"
            );
        }
    }

    #[test]
    fn a_kept_heading_beats_the_nearest_edge_on_open_plains() {
        // Open plains in every direction, no forest. Nearest-first circles the
        // start; a heading leaves it. Eight legs must end farther from the
        // start than the sight range.
        let (path, sectors) = simulate(|_, _| true, (0, 0), None, 8);
        let last = path.last().expect("a path");
        let far = f64::from(last.0).hypot(f64::from(last.1));
        assert!(
            far > f64::from(2 * SIGHT),
            "the body left the start: {path:?}"
        );
        let mut turns = sectors.clone();
        turns.dedup();
        assert_eq!(turns.len(), 1, "one heading held all the way: {sectors:?}");
    }
}
