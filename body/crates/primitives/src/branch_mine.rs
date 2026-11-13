//! The route of `branch_mine`: where its digger goes next.
//!
//! Underground ore is never visible from the surface (perception is ray-based
//! and only sees exposed faces), so the call has to open the rock itself. It
//! first goes down to the height where the ore is most common, then cuts a
//! level one-wide, two-high tunnel in legs of [`LEG_STEPS`] steps that turn at
//! right angles, alternating clockwise and counter-clockwise (seen from
//! above), so the tunnel zigzags into fresh rock instead of covering its own
//! ground. The step planner and the dig machinery are those of `dig_down`.

use crate::dig_down::{Dir, Slope, axis_order};

/// Level steps of one tunnel leg before the tunnel turns.
pub(crate) const LEG_STEPS: u32 = 16;

/// Digs a `branch_mine` call keeps for its level tunnel, after the descent,
/// refills of falling sand included. A tunnel step digs two cells (head room
/// and feet cell), so this is 80 steps: five legs of [`LEG_STEPS`], enough to
/// cut fresh rock and turn a few times.
pub(crate) const TUNNEL_RESERVE_DUGS: u32 = 160;

// The reserve covers four full legs of two digs per step.
const _: () = assert!(4 * 2 * LEG_STEPS <= TUNNEL_RESERVE_DUGS);

/// Ticks one `branch_mine` call may run, the descent included (two minutes).
/// The dig budget alone does not bound the call in time: a stone dig with a
/// stone pickaxe costs about 35 ticks, so the 202 digs of a surface start
/// took 5,091 ticks in live run D (2026-11-12), the whole goal budget, and the
/// caller never got its typed `not_found` back with time left to do anything
/// else. At 35 ticks a dig, 2,400 ticks are about 68 digs: a descent of a few
/// blocks and a tunnel of some thirty steps. The call fails `not_found`
/// between two digs (a dig in flight settles its receipt first), so it ends a
/// little past the cap, never in the middle of a receipt.
pub(crate) const BRANCH_MAX_TICKS: u32 = 2400;

/// Digs one block of descent costs: the three cells of a stair (clearance,
/// head room, new feet cell). A descent through open air costs fewer; the
/// estimate stays on the safe side.
const DIGS_PER_DESCENT_BLOCK: u32 = 3;

/// The feet height where `branch_mine` starts to tunnel for `ore`, or `None`
/// for a block with no known best height (the tunnel then starts at the
/// current height).
///
/// Source: minecraft.wiki, Java Edition 1.18+ ore generation (articles "Coal
/// Ore", "Iron Ore", "Copper Ore" and "Ore", read again 2026-11-13 from
/// `local/wiki/text/`). The ores peak in a triangle distribution:
/// - coal: second batch y 0..192, peak y 96, so the density at y falls
///   linearly from the peak (half the peak density at y 48); the first batch
///   (y 136..320) lies above any depth a tunnel reaches. The "Ore" table puts
///   the layer with most coal at y 45. y 56 is the compromise: 60% of the
///   peak density, and a descent of about seven blocks from a surface at
///   y 63 instead of fifteen. A stair block costs about 85 ticks and a call
///   has [`BRANCH_MAX_TICKS`], so the tunnel's own share of the call weighs
///   more than the last few percent of density (run D, 2026-11-12: the
///   descent to y 48 took 750 of the 2,400 ticks a call has now).
/// - iron: batch 2 y -24..56, peak y 16; batch 3 is a thin uniform layer over
///   -64..72; batch 1 (y 80..384, peak y 232) is out of reach.
/// - copper: y -16..112, peak y 47..48.
///
/// Ores that live in deepslate (lapis, diamond, redstone) are not listed: a
/// descent to y < 8 needs lava and cave handling this primitive does not have.
pub(crate) fn descent_target(ore: &str) -> Option<i32> {
    match ore {
        "coal_ore" => Some(56),
        "copper_ore" => Some(48),
        "iron_ore" => Some(16),
        _ => None,
    }
}

/// The turn after `heading`: clockwise from above for an even `turns`,
/// counter-clockwise for an odd one.
fn turn(heading: Dir, turns: u32) -> Dir {
    let (dx, dz) = heading;
    if turns.is_multiple_of(2) {
        (-dz, dx)
    } else {
        (dz, -dx)
    }
}

/// The axes to try for the next step, best first. At most the four axes, so
/// it lives on the stack: the planner asks for it on every tick.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Candidates {
    dirs: [Dir; 4],
    len: usize,
}

impl Candidates {
    /// The axes of `order`, minus `skip` (the passage just cut).
    pub(crate) fn from_axes(order: [Dir; 4], skip: Option<Dir>) -> Self {
        let mut dirs = order;
        let mut len = 0;
        for dir in order {
            if Some(dir) != skip {
                dirs[len] = dir;
                len += 1;
            }
        }
        Self { dirs, len }
    }

    /// One axis only.
    pub(crate) fn single(dir: Dir) -> Self {
        Self {
            dirs: [dir; 4],
            len: 1,
        }
    }
}

impl std::ops::Deref for Candidates {
    type Target = [Dir];

    fn deref(&self) -> &Self::Target {
        &self.dirs[..self.len]
    }
}

#[derive(Debug)]
pub(crate) struct Branch {
    /// Feet height to descend to; `None` tunnels from the start height.
    target_y: Option<i32>,
    /// The axis of the last accepted step.
    heading: Option<Dir>,
    /// Level steps walked along `heading`.
    leg: u32,
    /// How many times the heading changed.
    turns: u32,
}

impl Branch {
    pub(crate) fn new(ore: &str) -> Self {
        Self {
            target_y: descent_target(ore),
            heading: None,
            leg: 0,
            turns: 0,
        }
    }

    /// Down while the feet are above the target height, level from there on.
    pub(crate) fn slope(&self, feet_y: i32) -> Slope {
        match self.target_y {
            Some(target) if feet_y > target => Slope::Down,
            _ => Slope::Level,
        }
    }

    /// How many blocks a call that starts at `start_y` may dig: three digs per
    /// block of descent from the start height to [`Branch::target_y`], plus the
    /// fixed [`TUNNEL_RESERVE_DUGS`]. Sizing the cap from the start height
    /// keeps a mountain start from spending its whole budget on the descent
    /// and failing before it tunnels at all.
    pub(crate) fn dig_budget(&self, start_y: i32) -> u32 {
        let descent = self.target_y.map_or(0, |target| {
            start_y.saturating_sub(target).max(0)
        });
        descent as u32 * DIGS_PER_DESCENT_BLOCK + TUNNEL_RESERVE_DUGS
    }

    /// The axes to try for the next step, best first: straight on, or the
    /// turn when the leg is full; never straight back into the passage just
    /// cut. A refused axis falls through to the next one.
    pub(crate) fn candidates(&self, facing: Dir) -> Candidates {
        let Some(heading) = self.heading else {
            return Candidates::from_axes(axis_order(facing), None);
        };
        let first = if self.leg >= LEG_STEPS {
            turn(heading, self.turns)
        } else {
            heading
        };
        let back = (-heading.0, -heading.1);
        Candidates::from_axes(axis_order(first), Some(back))
    }

    /// A step along `dir` was accepted. A new axis starts a new leg.
    pub(crate) fn accept(&mut self, dir: Dir) {
        if self.heading != Some(dir) {
            if self.heading.is_some() {
                self.turns += 1;
            }
            self.heading = Some(dir);
            self.leg = 0;
        }
    }

    /// The body walks onto the new feet cell of a `slope` step.
    pub(crate) fn walked(&mut self, slope: Slope) {
        if slope == Slope::Level {
            self.leg += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use azalea_core::position::BlockPos;
    use azalea_registry::builtin::BlockKind;
    use praxion_world::BlockState;

    use super::*;
    use crate::dig_down::{StairStep, plan_step};

    const EAST: Dir = (1, 0);

    #[test]
    fn each_supported_ore_has_its_height() {
        assert_eq!(descent_target("coal_ore"), Some(56));
        assert_eq!(descent_target("copper_ore"), Some(48));
        assert_eq!(descent_target("iron_ore"), Some(16));
    }

    #[test]
    fn other_blocks_tunnel_where_they_stand() {
        for block in ["diamond_ore", "lapis_ore", "stone", "#logs"] {
            assert_eq!(descent_target(block), None, "{block}");
            assert_eq!(Branch::new(block).slope(70), Slope::Level, "{block}");
        }
    }

    #[test]
    fn the_descent_ends_at_the_target_height() {
        let branch = Branch::new("iron_ore");
        assert_eq!(branch.slope(64), Slope::Down);
        assert_eq!(branch.slope(17), Slope::Down);
        assert_eq!(branch.slope(16), Slope::Level);
        // Already below the target: no descent.
        assert_eq!(branch.slope(5), Slope::Level);
    }

    #[test]
    fn the_first_step_tries_the_facing_axis_first() {
        let branch = Branch::new("coal_ore");
        let order = branch.candidates((0, 1));
        assert_eq!(order[0], (0, 1));
        assert_eq!(order.len(), 4);
    }

    #[test]
    fn the_passage_just_cut_is_never_a_candidate() {
        let mut branch = Branch::new("coal_ore");
        branch.accept(EAST);
        let order = branch.candidates((0, 1));
        assert_eq!(order[0], EAST, "straight on first");
        assert!(!order.contains(&(-1, 0)), "never straight back: {order:?}");
        assert_eq!(order.len(), 3);
    }

    #[test]
    fn a_full_leg_turns_and_the_turns_alternate() {
        let mut branch = Branch::new("coal_ore");
        branch.accept(EAST);
        let mut headings = vec![EAST];
        for _ in 0..3 {
            for _ in 0..LEG_STEPS {
                assert_eq!(branch.candidates(EAST)[0], *headings.last().unwrap());
                branch.walked(Slope::Level);
            }
            let next = branch.candidates(EAST)[0];
            branch.accept(next);
            headings.push(next);
        }
        // East, south, east, south: clockwise then counter-clockwise from
        // above, a zigzag.
        assert_eq!(headings, [EAST, (0, 1), EAST, (0, 1)]);
    }

    #[test]
    fn a_stair_going_down_does_not_fill_the_leg() {
        let mut branch = Branch::new("iron_ore");
        branch.accept(EAST);
        for _ in 0..(2 * LEG_STEPS) {
            branch.walked(Slope::Down);
        }
        assert_eq!(branch.candidates(EAST)[0], EAST);
    }

    #[test]
    fn a_refused_straight_leg_falls_back_to_the_sides() {
        let mut branch = Branch::new("coal_ore");
        branch.accept(EAST);
        let order = branch.candidates(EAST);
        // The caller walks this list until a step is accepted.
        assert_eq!(&order[1..], &[(0, 1), (0, -1)]);
    }

    #[test]
    fn the_dig_budget_scales_with_the_descent() {
        // Three digs per block of descent, plus the fixed tunnel reserve. A
        // surface start at y 64 to the lowest target (y 16) leaves the whole
        // reserve for the tunnel.
        let iron = Branch::new("iron_ore");
        assert_eq!(
            iron.dig_budget(64),
            3 * (64 - 16) + TUNNEL_RESERVE_DUGS
        );
        // Already at or below the target: no descent is charged.
        assert_eq!(iron.dig_budget(16), TUNNEL_RESERVE_DUGS);
        assert_eq!(iron.dig_budget(5), TUNNEL_RESERVE_DUGS);
        // A mountain start charges its own descent instead of eating the
        // tunnel (review, 2026-11-08).
        let coal = Branch::new("coal_ore");
        assert_eq!(
            coal.dig_budget(150),
            3 * (150 - 56) + TUNNEL_RESERVE_DUGS
        );
        // No known height: the whole budget is tunnel.
        assert_eq!(Branch::new("stone").dig_budget(64), TUNNEL_RESERVE_DUGS);
    }

    /// Run the route over solid stone like the digger does, until `steps`
    /// feet cells were walked; return the feet cells in order.
    fn run_through_stone(ore: &str, start: BlockPos, steps: u32) -> Vec<BlockPos> {
        let mut dug: HashSet<BlockPos> = HashSet::new();
        let mut branch = Branch::new(ore);
        let mut feet = start;
        let mut path = vec![feet];
        while path.len() <= steps as usize {
            let lookup = |pos: BlockPos| {
                Some(BlockState::from(if dug.contains(&pos) {
                    BlockKind::Air
                } else {
                    BlockKind::Stone
                }))
            };
            let slope = branch.slope(feet.y);
            let mut chosen = None;
            for dir in branch.candidates(EAST).iter().copied() {
                let step = plan_step(feet, dir, slope, i32::MIN, &lookup);
                if !matches!(step, StairStep::Refuse(_)) {
                    chosen = Some((dir, step));
                    break;
                }
            }
            let (dir, step) = chosen.expect("solid stone always has a way");
            branch.accept(dir);
            match step {
                StairStep::Dig(cell) => {
                    dug.insert(cell);
                }
                StairStep::Walk(tread) => {
                    branch.walked(slope);
                    feet = tread;
                    path.push(feet);
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        path
    }

    #[test]
    fn iron_descends_to_y16_then_tunnels_level_and_turns() {
        let start = BlockPos::new(0, 64, 0);
        let path = run_through_stone("iron_ore", start, 48 + 2 * LEG_STEPS + 1);
        // 48 stairs down, one block each.
        assert_eq!(path[48].y, 16);
        assert!(path[..=48].windows(2).all(|w| w[1].y == w[0].y - 1));
        // Then level: two full legs and the first step of the third.
        let tunnel = &path[48..];
        assert!(tunnel.iter().all(|pos| pos.y == 16));
        let first_leg_end = tunnel[LEG_STEPS as usize];
        assert_eq!(first_leg_end.z, tunnel[0].z, "the first leg is straight");
        let second_leg_end = tunnel[2 * LEG_STEPS as usize];
        assert_ne!(second_leg_end.z, first_leg_end.z, "then it turned");
        assert_eq!(second_leg_end.x, first_leg_end.x);
    }
}
