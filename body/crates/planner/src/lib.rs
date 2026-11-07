//! The planner of Demo-0: A* over block cells, our own moves, and the contract
//! of architecture §5.5.
//!
//! ```text
//! PlanningState + KnownBlocks + Goal
//!         │
//!         ├─ astar/  Azalea's lifted core (MIT), a time and node budget
//!         ├─ moves   walk, diagonal, step up, descend, dig, bridge, pillar
//!         └─ world   known blocks only; unknown = frontier
//!         ▼
//! Plan { Success | Partial(cell) | Timeout | NoPath(reason), steps }
//!         ▼
//! Follower ── ControlFrame per tick ──> actuator
//! ```
//!
//! Only the A* core, its heap/node map and the cost constants are lifted from
//! Azalea (commit `8041a14`, MIT; see `AZALEA-LICENSE.md`). The moves, goals,
//! world view, stuck handler and follower are Praxion's.
//!
//! **Single source.** Every dig or place step carries a [`PrimitiveCall`] built
//! from the matching record in [`praxion_rules::Primitives`]: the name and
//! version come from the record, the position is in `target`, and the record's
//! arguments are in `args`. The caller can ask
//! [`PrimitiveCall::effects`] for the declared effects, so a path never
//! restates them.
//!
//! **Frontier.** When a move would enter a cell the world source does not
//! know, the planner cannot decide it. It notes the cell and its last known
//! parent cell, finishes the search to the goal, and if the goal was not
//! reached it plans again to the best frontier parent. The result is
//! `Partial { cell }`: the caller looks at `cell` and replans. A `partial`
//! result transfers no world state.

#[cfg(test)]
mod tests;

mod astar;
pub mod blocks;
mod costs;
mod follow;
mod goal;
mod look;
mod moves;
mod route;
mod state;
mod stuck;
mod world;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use praxion_rules::Primitives;
use praxion_world::BlockPos;

pub use astar::{Budget, SearchStatus};
pub use follow::{Follower, Progress};
pub use goal::Goal;
pub use look::{
    Cue, Gaze, GazeMode, Personality, STEP, WALK_TOLERANCE_DEG, aim, look_towards, wrap_degrees,
};
pub use moves::{MoveData, MoveKind, PLACE_BLOCK_COST, PrimitiveCall};
pub use state::{HotbarItem, Inventory, Placements, PlanningState, place_block_kind};
pub use stuck::{BlacklistEntry, ResetCondition, StuckGuard, Unreachable};
pub use world::{
    Block, DirectBlocks, KnownBlocks, Limits, PerceptionBlocks, TestWorld, center,
};

use crate::astar::a_star;
use crate::moves::{MoveCtx, Node};
use crate::world::WorldView;

/// One step of a path. `from` is the feet cell before the move and `to` after
/// it; `primitives` must finish before the movement starts.
#[derive(Clone, Debug)]
pub struct Step {
    pub from: BlockPos,
    pub to: BlockPos,
    pub kind: MoveKind,
    pub primitives: Vec<PrimitiveCall>,
    pub cost: f32,
}

/// The four statuses of the contract (§5.5), each with the path so far.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlanStatus {
    Success,
    /// The best route ends at an unknown cell. The caller looks, then replans.
    Partial { cell: BlockPos },
    /// The time or node budget ran out.
    Timeout,
    NoPath { reason: Unreachable },
}

/// The planner's answer.
#[derive(Clone, Debug)]
pub struct Plan {
    pub status: PlanStatus,
    pub steps: Vec<Step>,
    /// The A* cost of the returned path, in expected ticks.
    pub cost: f32,
    pub nodes_expanded: usize,
    pub elapsed: Duration,
}

impl Plan {
    /// Every cell the path touches, the start included. Used by the stuck
    /// handler's replan loop detection.
    pub fn path_cells(&self) -> Vec<BlockPos> {
        let Some(first) = self.steps.first() else {
            return Vec::new();
        };
        let mut cells = Vec::with_capacity(self.steps.len() + 1);
        cells.push(first.from);
        cells.extend(self.steps.iter().map(|step| step.to));
        cells
    }

    /// The typed reason the plan did not succeed.
    pub fn unreachable(&self) -> Option<Unreachable> {
        match self.status {
            PlanStatus::Success => None,
            PlanStatus::Partial { cell } => Some(Unreachable::Frontier { cell }),
            PlanStatus::Timeout => Some(Unreachable::Timeout),
            PlanStatus::NoPath { reason } => Some(reason),
        }
    }
}

/// The planner. One value is reused for every segment; the world and the
/// planning state are passed per call.
#[derive(Clone, Debug)]
pub struct Planner {
    primitives: Primitives,
    budget: Budget,
    max_drop: i32,
    allow_world_changes: bool,
}

impl Default for Planner {
    fn default() -> Self {
        Self::new()
    }
}

impl Planner {
    pub fn new() -> Self {
        Self {
            primitives: Primitives::standard(),
            budget: Budget::default(),
            max_drop: 3,
            allow_world_changes: true,
        }
    }

    pub fn with_budget(budget: Budget) -> Self {
        Self {
            budget,
            ..Self::new()
        }
    }

    /// The maximum drop a descend move may take. Default 3 (`spikes`/session
    /// 6); clamped to at least 1 and at most the size of the cost table.
    pub fn set_max_drop(&mut self, drop: i32) -> &mut Self {
        self.max_drop = drop.clamp(1, 4_096);
        self
    }

    pub fn max_drop(&self) -> i32 {
        self.max_drop
    }

    /// Allow dig, bridge and pillar. Turned off for path-only fixtures.
    pub fn set_allow_world_changes(&mut self, allow: bool) -> &mut Self {
        self.allow_world_changes = allow;
        self
    }

    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// The primitive registry the path's calls come from.
    pub fn primitives(&self) -> &Primitives {
        &self.primitives
    }

    pub fn plan(
        &self,
        source: &mut dyn KnownBlocks,
        state: &PlanningState,
        start: BlockPos,
        goal: Goal,
    ) -> Plan {
        self.plan_with_budget(source, state, start, goal, self.budget)
    }

    pub fn plan_with_budget(
        &self,
        source: &mut dyn KnownBlocks,
        state: &PlanningState,
        start: BlockPos,
        goal: Goal,
        budget: Budget,
    ) -> Plan {
        let started = Instant::now();

        // An entity goal becomes the cell perception reports for it.
        let goal = match goal {
            Goal::Entity(id) => match source.entity_cell(id) {
                Some(cell) => Goal::Block(cell),
                None => {
                    return empty_plan(
                        PlanStatus::NoPath {
                            reason: Unreachable::GoalUnknown,
                        },
                        started,
                    );
                }
            },
            other => other,
        };

        // Refuse a start or a goal the world cannot contain before any goal
        // arithmetic or move offset runs on it (FINDING 1). The cell is the
        // typed reason the caller sees.
        let limits = Limits::of(source);
        if let Some(pos) = first_outside(limits, start, &goal) {
            return empty_plan(
                PlanStatus::NoPath {
                    reason: Unreachable::OutsideWorld { pos },
                },
                started,
            );
        }

        let mut frontier: HashMap<BlockPos, Node> = HashMap::new();
        let mut view = WorldView::new(source, &state.protected, &state.blacklist);

        let start_node = Node {
            pos: start,
            standing_on_placed: false,
            placements: state.inventory.place_stacks(),
        };
        // Leave a fifth of the segment budget for the frontier search, so a
        // `partial` result still fits inside the caller's budget.
        let first_budget = Budget {
            time: budget.time.mul_f32(0.8),
            nodes: budget.nodes,
        };
        let first = {
            let mut ctx = MoveCtx {
                world: &mut view,
                inventory: &state.inventory,
                primitives: &self.primitives,
                max_drop: self.max_drop,
                allow_world_changes: self.allow_world_changes,
                frontier: &mut frontier,
            };
            a_star(
                start_node,
                |node: Node| goal.heuristic(node.pos),
                |node: Node| moves::successors(&mut ctx, node),
                |node: Node| goal.success(node.pos),
                first_budget,
            )
        };

        if first.status == SearchStatus::Success {
            return build_plan(first, PlanStatus::Success, start, started);
        }

        // Plan to the best frontier parent: the same heuristic, a success
        // predicate that accepts the parents of unknown cells.
        if !frontier.is_empty() {
            let mut parent_cell: HashMap<Node, BlockPos> = HashMap::new();
            for (cell, parent) in &frontier {
                let better = match parent_cell.get(parent) {
                    Some(current) => goal.heuristic(*cell) < goal.heuristic(*current),
                    None => true,
                };
                if better {
                    parent_cell.insert(*parent, *cell);
                }
            }
            let parents: HashSet<Node> = parent_cell.keys().copied().collect();

            let remaining = budget
                .time
                .saturating_sub(first.elapsed)
                .max(Duration::from_millis(1));
            let second = {
                let mut ctx = MoveCtx {
                    world: &mut view,
                    inventory: &state.inventory,
                    primitives: &self.primitives,
                    max_drop: self.max_drop,
                    allow_world_changes: self.allow_world_changes,
                    frontier: &mut frontier,
                };
                a_star(
                    start_node,
                    |node: Node| goal.heuristic(node.pos),
                    |node: Node| moves::successors(&mut ctx, node),
                    |node: Node| parents.contains(&node),
                    Budget {
                        time: remaining,
                        nodes: budget.nodes,
                    },
                )
            };
            if second.status == SearchStatus::Success {
                let end = second
                    .movements
                    .last()
                    .map(|movement| movement.target)
                    .unwrap_or(start_node);
                if let Some(cell) = parent_cell.get(&end).copied() {
                    return build_plan(second, PlanStatus::Partial { cell }, start, started);
                }
            }
        }

        let status = match first.status {
            SearchStatus::Success => PlanStatus::Success,
            SearchStatus::Budget => PlanStatus::Timeout,
            SearchStatus::Exhausted => PlanStatus::NoPath {
                reason: Unreachable::NoRoute,
            },
        };
        build_plan(first, status, start, started)
    }
}

/// The first cell of the start and the goal that the world cannot contain:
/// outside the world's height, or beyond the vanilla world border.
fn first_outside(limits: Limits, start: BlockPos, goal: &Goal) -> Option<BlockPos> {
    let (low, high) = match *goal {
        Goal::Block(pos) => (pos, pos),
        Goal::Region { min, max } => (min, max),
        Goal::Near { pos, .. } => (pos, pos),
        // An `Entity` goal is already resolved to a `Block` above.
        Goal::Entity(_) => (start, start),
    };
    [start, low, high]
        .into_iter()
        .find(|pos| !limits.contains(*pos))
}

fn build_plan(
    search: astar::Search<Node, MoveData>,
    status: PlanStatus,
    start: BlockPos,
    started: Instant,
) -> Plan {
    let mut from = start;
    let mut steps = Vec::with_capacity(search.movements.len());
    for movement in search.movements {
        let to = movement.target.pos;
        steps.push(Step {
            from,
            to,
            kind: movement.data.kind,
            primitives: movement.data.primitives,
            cost: movement.data.cost,
        });
        from = to;
    }
    Plan {
        status,
        steps,
        cost: search.cost,
        nodes_expanded: search.nodes_expanded,
        elapsed: started.elapsed(),
    }
}

fn empty_plan(status: PlanStatus, started: Instant) -> Plan {
    Plan {
        status,
        steps: Vec::new(),
        cost: 0.,
        nodes_expanded: 0,
        elapsed: started.elapsed(),
    }
}
