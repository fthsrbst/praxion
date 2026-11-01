//! Typed failures and the bounded stuck handling of session 6 / contract §5.5.
//!
//! The planner itself only reports `NoPath`, `Partial`, `Timeout` or
//! `Success`. The caller keeps one [`StuckGuard`] per goal: it records the
//! cells of every replan, blacklists a cell after `threshold` replans that
//! revisit it, and clears blacklist entries when their reset condition holds.
//! A blacklisted cell is never entered, broken or built on (the planner reads
//! it through [`PlanningState::blacklist`]).

use std::collections::{HashMap, HashSet};

use praxion_world::BlockPos;

use crate::Plan;

/// A typed reason a plan cannot finish.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unreachable {
    /// The known graph has no route to the goal.
    NoRoute,
    /// An `Entity` goal could not be resolved from perception.
    GoalUnknown,
    /// The time or node budget stopped the search.
    Timeout,
    /// The best route ends at an unknown cell (a `partial` result).
    Frontier { cell: BlockPos },
    /// The same cells came back in `replans` replans; the cell is blacklisted.
    Loop { cell: BlockPos, replans: u32 },
}

/// When a blacklist entry is cleared.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ResetCondition {
    /// The player is at most this many blocks (squared distance) from the
    /// cell: getting closer means a new attempt can work.
    CloserThan(f64),
    /// A tool of at least this tier is in the hotbar (0 hand, 1 wood, 2 stone,
    /// 3 iron, 4 diamond, 5 netherite).
    BetterTool(u8),
}

#[derive(Clone, Copy, Debug)]
pub struct BlacklistEntry {
    pub replans: u32,
    pub reset: ResetCondition,
}

/// The stuck handler the caller keeps per goal.
#[derive(Clone, Debug)]
pub struct StuckGuard {
    threshold: u32,
    replans: HashMap<BlockPos, u32>,
    blacklist: HashMap<BlockPos, BlacklistEntry>,
    reset: ResetCondition,
}

impl StuckGuard {
    /// Blacklist a cell after `threshold` replans that visit it. The default
    /// reset is getting within 8 blocks of the cell.
    pub fn new(threshold: u32) -> Self {
        Self {
            threshold: threshold.max(1),
            replans: HashMap::new(),
            blacklist: HashMap::new(),
            reset: ResetCondition::CloserThan(64.),
        }
    }

    pub fn with_reset(mut self, reset: ResetCondition) -> Self {
        self.reset = reset;
        self
    }

    /// Record one replan's path. Returns the first cell that crossed the
    /// threshold in this call, with its count.
    pub fn record_replan(&mut self, plan: &Plan) -> Option<Unreachable> {
        let mut seen = HashSet::new();
        let mut first = None;
        // The start cell is revisited by every replan by definition; counting
        // it would blacklist the start after N attempts and stop all movement.
        for cell in plan.path_cells().into_iter().skip(1) {
            if !seen.insert(cell) {
                continue;
            }
            let count = self.replans.entry(cell).or_insert(0);
            *count += 1;
            if *count >= self.threshold && !self.blacklist.contains_key(&cell) {
                self.blacklist.insert(
                    cell,
                    BlacklistEntry {
                        replans: *count,
                        reset: self.reset,
                    },
                );
                first.get_or_insert(Unreachable::Loop {
                    cell,
                    replans: *count,
                });
            }
        }
        first
    }

    pub fn is_blacklisted(&self, pos: BlockPos) -> bool {
        self.blacklist.contains_key(&pos)
    }

    pub fn blacklist(&self) -> HashSet<BlockPos> {
        self.blacklist.keys().copied().collect()
    }

    /// Clear entries whose reset condition the current state satisfies.
    pub fn reset(&mut self, player: BlockPos, tool_tier: u8) {
        self.blacklist.retain(|cell, entry| match entry.reset {
            ResetCondition::CloserThan(distance) => {
                distance_squared(player, *cell) > distance
            }
            ResetCondition::BetterTool(tier) => tool_tier <= tier,
        });
    }

    /// For tests and reports.
    pub fn blacklisted(&self) -> impl Iterator<Item = (&BlockPos, &BlacklistEntry)> {
        self.blacklist.iter()
    }
}

fn distance_squared(a: BlockPos, b: BlockPos) -> f64 {
    let dx = f64::from(a.x - b.x);
    let dy = f64::from(a.y - b.y);
    let dz = f64::from(a.z - b.z);
    dx * dx + dy * dy + dz * dz
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{PlanStatus, Step, moves::MoveKind};

    fn plan(cells: &[(i32, i32, i32)]) -> Plan {
        let steps = cells
            .windows(2)
            .map(|pair| {
                let from = BlockPos::new(pair[0].0, pair[0].1, pair[0].2);
                let to = BlockPos::new(pair[1].0, pair[1].1, pair[1].2);
                Step {
                    from,
                    to,
                    kind: MoveKind::Walk,
                    primitives: Vec::new(),
                    cost: 1.,
                }
            })
            .collect();
        Plan {
            status: PlanStatus::NoPath {
                reason: Unreachable::NoRoute,
            },
            steps,
            cost: 0.,
            nodes_expanded: 0,
            elapsed: Duration::ZERO,
        }
    }

    #[test]
    fn the_same_cells_in_n_replans_are_blacklisted() {
        let mut guard = StuckGuard::new(3);
        let plan = plan(&[(0, 64, 0), (0, 64, 1), (0, 64, 2)]);
        assert_eq!(guard.record_replan(&plan), None);
        assert_eq!(guard.record_replan(&plan), None);
        let hit = guard.record_replan(&plan).expect("third replan trips");
        assert_eq!(
            hit,
            Unreachable::Loop {
                cell: BlockPos::new(0, 64, 1),
                replans: 3
            }
        );
        assert!(guard.is_blacklisted(BlockPos::new(0, 64, 1)));
        assert!(!guard.is_blacklisted(BlockPos::new(0, 64, 0)), "the start is not blacklisted");
        assert_eq!(guard.blacklist().len(), 2);
    }

    #[test]
    fn getting_closer_or_a_better_tool_clears_the_blacklist() {
        let mut guard = StuckGuard::new(1).with_reset(ResetCondition::CloserThan(4.));
        let plan = plan(&[(0, 64, 0), (0, 64, 1)]);
        guard.record_replan(&plan);
        assert!(guard.is_blacklisted(BlockPos::new(0, 64, 1)));
        guard.reset(BlockPos::new(5, 64, 1), 0);
        assert!(guard.is_blacklisted(BlockPos::new(0, 64, 1)));
        guard.reset(BlockPos::new(1, 64, 1), 0);
        assert!(!guard.is_blacklisted(BlockPos::new(0, 64, 1)));

        let mut guard = StuckGuard::new(1).with_reset(ResetCondition::BetterTool(2));
        guard.record_replan(&plan);
        guard.reset(BlockPos::new(0, 64, 0), 2);
        assert!(guard.is_blacklisted(BlockPos::new(0, 64, 1)));
        guard.reset(BlockPos::new(0, 64, 0), 3);
        assert!(!guard.is_blacklisted(BlockPos::new(0, 64, 1)));
    }
}
