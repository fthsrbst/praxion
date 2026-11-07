//! The `near` fact over the known map and the frontier ranking used by
//! `explore`.

use std::collections::{HashSet, VecDeque};

use azalea_core::position::BlockPos;
use azalea_registry::builtin::BlockKind;
use praxion_gamedata as gamedata;
use praxion_perception::Perception;
use praxion_world::{BlockState, World};

/// The `near` fact over the perception known map: a target is near when a cell
/// this perception has seen inside `radius` holds it. No rays are spent, and
/// the answer is `false` when the map knows of none, so `not near(T, R)` can
/// start an exploration. `None` only when the eye itself is not known yet.
pub(crate) fn near_block(
    perception: &Perception,
    world: &World,
    wanted: &str,
    radius: u32,
) -> Option<bool> {
    perception.eye_cell(world)?;
    Some(nearest_known_block(perception, world, wanted, radius, &[]).is_some())
}

/// The nearest cell in the perception known map that holds `wanted`, with its
/// distance from the eye. `None` when nothing is known inside `radius`.
/// `exclude` skips cells a failed plan already rejected.
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
        |state| {
            let kind = BlockKind::from(state);
            match (tag, wanted_kind) {
                (Some(tag), _) => gamedata::block_in_tag(kind.to_str(), tag),
                (None, Some(wanted_kind)) => kind == wanted_kind,
                (None, None) => false,
            }
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

/// Ranks the known walkable frontier cells once. The returned list is sorted
/// best first and capped; the cost is the reachability flood plus one pass over
/// its cells, never the full `(2r+1)^3` cube and never a linear scan of the
/// explored list for every cell in it.
pub fn rank_frontiers(
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Vec<FrontierChoice> {
    rank_frontiers_inner(
        None,
        start,
        horizontal,
        vertical,
        explored,
        rejected,
        known,
        allow_water,
    )
}

/// Ranking key: the candidate's tier flags, then its distance and position.
type RankKey = (bool, bool, bool, bool, i64, i64, i32, i32, i32);

/// Like [`rank_frontiers`], but orders by distance from `origin` (the first
/// cell this run explored) so a call walks outward instead of circling the
/// nearest ring of the known map. Seed 202's spawn is plains and the nearest
/// forest is 160 blocks away; nearest-first walked a ring at the sight radius
/// for 4,000 ticks and never reached it.
// The frontier query parameters are the caller's configuration; grouping them
// would add a one-use type.
#[allow(clippy::too_many_arguments)]
pub(crate) fn rank_frontiers_from(
    origin: BlockPos,
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Vec<FrontierChoice> {
    rank_frontiers_inner(
        Some(origin),
        start,
        horizontal,
        vertical,
        explored,
        rejected,
        known,
        allow_water,
    )
}

// The inner form differs only by the optional origin; the rest is the query.
#[allow(clippy::too_many_arguments)]
fn rank_frontiers_inner(
    origin: Option<BlockPos>,
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Vec<FrontierChoice> {
    // Eight blocks: enough that a second pass over the same edge is avoided,
    // small enough that a genuinely new direction still wins.
    const AVOID_RADIUS_SQ: i64 = 8 * 8;
    /// More candidates than an explore budget can try; bounds the allocation.
    const MAX_CHOICES: usize = 64;
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
    let mut ranked: Vec<(RankKey, FrontierChoice)> = Vec::new();
    let mut candidates = 0u32;
    for pos in &reachable {
        let pos = *pos;
        if explored_set.contains(&pos) || rejected_set.contains(&pos) {
            continue;
        }
        let Some(state) = known(pos) else {
            continue;
        };
        if !standable(pos, state, &known, allow_water) || !touches_unknown(pos, &known) {
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
        // Stay on the player's level: a frontier more than two blocks below (a
        // cave) or four above scores worse than any level one, so exploring
        // does not dive into a ravine by accident.
        let off_level = !(-2..=4).contains(&dy);
        // Water is walkable, but a land edge is a better place to look for
        // trees: crossing an ocean with a 1200-tick budget is how a doomed
        // search spends it.
        let wet = praxion_planner::blocks::is_water(state);
        // Do not climb a mountain looking for wood: plains and valleys hold
        // the trees, and an uphill frontier is how a search ended at y=117
        // with the forest still 160 blocks away.
        let uphill = dy > 0;
        let distance_sq = dx * dx + dy * dy + dz * dz;
        // Walk outward from the origin, not around the nearest ring of the
        // known map: a farther frontier wins over a nearer one at the same
        // quality. Without this, seed 202 circled at the sight edge for 4,000
        // ticks while its forest sat 160 blocks away. The plain ranking keeps
        // nearest-first, for callers that want the closest edge.
        let away = origin
            .map(|origin| {
                let ox = i64::from(pos.x) - i64::from(origin.x);
                let oz = i64::from(pos.z) - i64::from(origin.z);
                ox * ox + oz * oz
            })
            .unwrap_or(0);
        candidates = candidates.saturating_add(1);
        let tier = (avoid, off_level, wet, uphill);
        ranked.push((
            (
                avoid,
                off_level,
                wet,
                uphill,
                -away,
                distance_sq,
                pos.x,
                pos.z,
                pos.y,
            ),
            FrontierChoice {
                cell: pos,
                score: distance_sq,
                candidates: 0,
                tier,
            },
        ));
    }
    ranked.sort_by_key(|(key, _)| *key);
    ranked.truncate(MAX_CHOICES);
    ranked
        .into_iter()
        .map(|(_, mut choice)| {
            choice.candidates = candidates;
            choice
        })
        .collect()
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

    #[test]
    fn a_far_frontier_outranks_the_nearest_ring_cell() {
        // Seed 202 is plains with its forest 160 blocks away. Ranking by
        // distance from the start walks the sight-radius ring; ranking away
        // from the origin heads outward instead.
        let mut map = HashMap::new();
        platform(&mut map, -30, 30, -2, 2, 64);
        let ranked = rank_frontiers_from(
            at(0, 64, 0),
            at(0, 64, 0),
            32,
            8,
            &[],
            &[],
            known(&map),
            true,
        );
        let best = ranked[0];
        assert_eq!(best.cell.x, -30, "the far edge wins: {best:?}");
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
}
