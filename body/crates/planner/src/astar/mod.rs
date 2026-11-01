// Lifted from Azalea (MIT), commit 8041a14:
// `azalea/src/pathfinder/astar/mod.rs` and `heap.rs` / `nodemap.rs`.
// Copyright (c) 2022 mat. The full MIT notice is in `AZALEA-LICENSE.md`.
//
// Modifications:
// - the two `PathfinderTimeout`s are one [`Budget`] with both a wall-clock and
//   a node limit, because the planner contract (§5.5) asks for both;
// - the result is [`Search`] with an explicit [`SearchStatus`] instead of
//   `is_partial`, so the caller can tell a budget stop from an exhausted graph;
// - the `num_format`/`tracing` performance log is replaced by counters in the
//   result (no logging dependency in this crate);
// - `reconstruct_path` is the same code, kept in the module.

pub mod heap;
pub mod nodemap;

use std::{
    fmt::{self, Debug},
    hash::Hash,
    time::{Duration, Instant},
};

use heap::{PathfinderHeap, WeightedNode};
use nodemap::NodeMap;

/// The cost of one path, in expected ticks.
pub struct Search<P: Hash + Copy, M> {
    pub movements: Vec<Movement<P, M>>,
    pub status: SearchStatus,
    /// The A* cost for executing the path.
    pub cost: f32,
    pub nodes_expanded: usize,
    pub elapsed: Duration,
}

/// How the search ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchStatus {
    /// A node satisfied `success`.
    Success,
    /// The time or node budget ran out first.
    Budget,
    /// The open set emptied without satisfying `success`.
    Exhausted,
}

/// A wall-clock and node limit for one search (contract §5.5: "a time budget
/// per segment"; the node limit makes tests deterministic).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Budget {
    pub time: Duration,
    pub nodes: usize,
}

impl Budget {
    pub const fn new(time: Duration, nodes: usize) -> Self {
        Self { time, nodes }
    }

    /// Only a node limit; the wall clock is effectively unbounded.
    pub const fn nodes(nodes: usize) -> Self {
        Self {
            time: Duration::from_secs(86_400),
            nodes,
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::new(Duration::from_millis(100), 200_000)
    }
}

// used for better results when timing out
// based on Baritone's implementation (with tweaks), see
// https://github.com/cabaletta/baritone/blob/1.19.4/src/main/java/baritone/pathing/calc/AbstractNodeCostSearch.java#L68
// for more info
const COEFFICIENTS: [f32; 7] = [1.5, 2., 2.5, 3., 4., 5., 15.];

const MIN_IMPROVEMENT: f32 = 0.01;

/// How often the wall clock is read. The node limit is checked every node.
/// Thirty-two nodes is a few hundred microseconds in a debug build, so a
/// 100 ms segment budget is not overrun by a whole check interval.
const TIME_CHECK_INTERVAL: usize = 32;

// Sources:
// - https://en.wikipedia.org/wiki/A*_search_algorithm
// - https://github.com/evenfurther/pathfinding/blob/main/src/directed/astar.rs
// - https://github.com/cabaletta/baritone/blob/1.19.4/src/main/java/baritone/pathing/calc/AbstractNodeCostSearch.java
pub fn a_star<P, M, HeuristicFn, SuccessorsFn, SuccessFn>(
    start: P,
    heuristic: HeuristicFn,
    mut successors: SuccessorsFn,
    success: SuccessFn,
    budget: Budget,
) -> Search<P, M>
where
    P: Eq + Hash + Copy + Debug,
    HeuristicFn: Fn(P) -> f32,
    SuccessorsFn: FnMut(P) -> Vec<Edge<P, M>>,
    SuccessFn: Fn(P) -> bool,
{
    let start_time = Instant::now();

    let mut open_set = PathfinderHeap::new();
    open_set.push(WeightedNode {
        g_score: 0.,
        f_score: 0.,
        index: 0,
    });
    let mut nodes: NodeMap<P> = NodeMap::default();
    nodes.insert(
        start,
        Node {
            came_from: u32::MAX,
            g_score: 0.,
        },
    );

    let mut best_paths: [u32; 7] = [0; 7];
    let mut best_path_scores: [f32; 7] = [heuristic(start); 7];

    let mut num_nodes = 0_usize;
    let mut status = SearchStatus::Exhausted;

    while let Some(WeightedNode {
        index, g_score, ..
    }) = open_set.pop()
    {
        let (&node, node_data) = nodes.get_index(index).unwrap();
        if g_score > node_data.g_score {
            continue;
        }

        num_nodes += 1;

        if success(node) {
            let best_path = index;
            return Search {
                movements: reconstruct_path(nodes, best_path, successors),
                status: SearchStatus::Success,
                cost: g_score,
                nodes_expanded: num_nodes,
                elapsed: start_time.elapsed(),
            };
        }

        for neighbor in successors(node) {
            let tentative_g_score = g_score + neighbor.cost;
            let neighbor_heuristic;
            let neighbor_index;

            match nodes.entry(neighbor.movement.target) {
                indexmap::map::Entry::Occupied(mut e) => {
                    if e.get().g_score > tentative_g_score {
                        neighbor_heuristic = heuristic(*e.key());
                        neighbor_index = e.index() as u32;
                        e.insert(Node {
                            came_from: index,
                            g_score: tentative_g_score,
                        });
                    } else {
                        continue;
                    }
                }
                indexmap::map::Entry::Vacant(e) => {
                    neighbor_heuristic = heuristic(*e.key());
                    neighbor_index = e.index() as u32;
                    e.insert(Node {
                        came_from: index,
                        g_score: tentative_g_score,
                    });
                }
            }

            // we don't update the existing node, which means that the same node might be
            // present in the open_set multiple times. this is fine because at the start of
            // the loop we check `g_score > node_data.g_score`.
            open_set.push(WeightedNode {
                index: neighbor_index,
                g_score: tentative_g_score,
                f_score: tentative_g_score + neighbor_heuristic,
            });

            for (coefficient_i, &coefficient) in COEFFICIENTS.iter().enumerate() {
                let node_score = neighbor_heuristic + tentative_g_score / coefficient;
                if best_path_scores[coefficient_i] - node_score > MIN_IMPROVEMENT {
                    best_paths[coefficient_i] = neighbor_index;
                    best_path_scores[coefficient_i] = node_score;
                }
            }
        }

        // The node budget is cheap to check every iteration, so a tiny limit
        // stops exactly at that limit. The clock is read less often.
        let out_of_nodes = num_nodes >= budget.nodes;
        let out_of_time = num_nodes.is_multiple_of(TIME_CHECK_INTERVAL)
            && start_time.elapsed() >= budget.time;
        if out_of_nodes || out_of_time {
            status = SearchStatus::Budget;
            break;
        }
    }

    let best_path_idx = determine_best_path_idx(best_paths, 0);
    Search {
        movements: reconstruct_path(nodes, best_paths[best_path_idx], successors),
        status,
        cost: best_path_scores[best_path_idx],
        nodes_expanded: num_nodes,
        elapsed: start_time.elapsed(),
    }
}

fn determine_best_path_idx(best_paths: [u32; 7], start: u32) -> usize {
    // this basically makes sure we don't create a path that's really short

    for (i, &node) in best_paths.iter().enumerate() {
        if node != start {
            return i;
        }
    }
    0
}

fn reconstruct_path<P, M, SuccessorsFn>(
    nodes: NodeMap<P>,
    mut current_index: u32,
    mut successors: SuccessorsFn,
) -> Vec<Movement<P, M>>
where
    P: Eq + Hash + Copy + Debug,
    SuccessorsFn: FnMut(P) -> Vec<Edge<P, M>>,
{
    let mut path = Vec::new();
    while let Some((&node_position, node)) = nodes.get_index(current_index) {
        if node.came_from == u32::MAX {
            break;
        }
        let came_from_position = *nodes.get_index(node.came_from).unwrap().0;

        // find the movement data for this successor, we have to do this again because
        // we don't include the movement data in the Node (as an optimization)
        let mut best_successor = None;
        let mut best_successor_cost = f32::INFINITY;
        for successor in successors(came_from_position) {
            if successor.movement.target == node_position && successor.cost < best_successor_cost {
                best_successor_cost = successor.cost;
                best_successor = Some(successor);
            }
        }
        let Some(found_successor) = best_successor else {
            // A successor stopped being possible while reconstructing the path.
            // Return what was rebuilt so far; the caller sees a short path.
            return vec![];
        };

        path.push(Movement {
            target: node_position,
            data: found_successor.movement.data,
        });

        current_index = node.came_from;
    }
    path.reverse();
    path
}

pub struct Node {
    pub came_from: u32,
    pub g_score: f32,
}

#[derive(Clone, Debug)]
pub struct Edge<P: Hash + Copy, M> {
    pub movement: Movement<P, M>,
    pub cost: f32,
}

pub struct Movement<P: Hash + Copy, M> {
    pub target: P,
    pub data: M,
}

impl<P: Hash + Copy + Debug, M: Debug> Debug for Movement<P, M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Movement")
            .field("target", &self.target)
            .field("data", &self.data)
            .finish()
    }
}
impl<P: Hash + Copy + Clone, M: Clone> Clone for Movement<P, M> {
    fn clone(&self) -> Self {
        Self {
            target: self.target,
            data: self.data.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A one-dimensional line of cells: `0 -> 1 -> ... -> n`.
    fn line_successors(pos: i32) -> Vec<Edge<i32, ()>> {
        let mut edges = Vec::new();
        for step in [-1, 1] {
            let target = pos + step;
            if (0..=20).contains(&target) {
                edges.push(Edge {
                    movement: Movement { target, data: () },
                    cost: 1.,
                });
            }
        }
        edges
    }

    #[test]
    fn finds_the_shortest_path_on_a_line() {
        let search = a_star(
            0,
            |pos: i32| (10 - pos).max(0) as f32,
            line_successors,
            |pos: i32| pos == 10,
            Budget::nodes(1_000),
        );
        assert_eq!(search.status, SearchStatus::Success);
        let targets: Vec<i32> = search.movements.iter().map(|m| m.target).collect();
        assert_eq!(targets, (1..=10).collect::<Vec<_>>());
        assert!((search.cost - 10.).abs() < 1e-6);
    }

    #[test]
    fn an_exhausted_graph_is_not_a_budget_stop() {
        let search = a_star(
            0,
            |pos: i32| (10 - pos).max(0) as f32,
            line_successors,
            |pos: i32| pos == 99,
            Budget::nodes(1_000),
        );
        assert_eq!(search.status, SearchStatus::Exhausted);
        assert!(search.movements.len() <= 20);
    }

    #[test]
    fn a_node_budget_stops_the_search() {
        let search = a_star(
            0,
            |pos: i32| (100 - pos).max(0) as f32,
            line_successors,
            |pos: i32| pos == 20,
            Budget::new(Duration::from_secs(60), 3),
        );
        assert_eq!(search.status, SearchStatus::Budget);
        assert!(search.nodes_expanded <= 3, "{}", search.nodes_expanded);
    }
}
