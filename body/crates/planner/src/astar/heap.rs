// Lifted from Azalea (MIT), commit 8041a14:
// `azalea/src/pathfinder/astar/heap.rs`.
// Copyright (c) 2022 mat. The full MIT notice is in `AZALEA-LICENSE.md`.
//
// Modifications: the `radix_heap` crate is not a dependency of this workspace,
// so `PathfinderHeap` is Azalea's own `BinaryHeap` fallback path only. Its
// ordering is identical to the radix-heap path; the radix heap was only a
// constant-factor optimisation. The tests below are lifted as they are.

use std::{
    cmp,
    collections::BinaryHeap,
};

/// The open set of the A*, ordered by `f_score` (and then `g_score`).
#[derive(Default)]
pub struct PathfinderHeap {
    binary_heap: BinaryHeap<WeightedNode>,
}

impl PathfinderHeap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: WeightedNode) {
        self.binary_heap.push(item);
    }

    pub fn pop(&mut self) -> Option<WeightedNode> {
        self.binary_heap.pop()
    }
}

#[derive(PartialEq, Debug)]
#[repr(C)]
pub struct WeightedNode {
    /// Sum of the g_score and heuristic
    pub f_score: f32,
    /// The actual cost to get to this node
    pub g_score: f32,
    pub index: u32,
}

impl Ord for WeightedNode {
    #[inline]
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        // intentionally inverted to make the BinaryHeap a min-heap
        match other.f_score.total_cmp(&self.f_score) {
            cmp::Ordering::Equal => self.g_score.total_cmp(&other.g_score),
            s => s,
        }
    }
}
impl Eq for WeightedNode {}
impl PartialOrd for WeightedNode {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weighted_node(f: f32, g: f32) -> WeightedNode {
        WeightedNode {
            f_score: f,
            g_score: g,
            index: 0,
        }
    }

    #[test]
    fn test_weighted_node_eq() {
        let a = weighted_node(0., 0.);
        let b = weighted_node(0., 0.);
        assert!(a == b);
    }
    #[test]
    fn test_weighted_node_le() {
        let a = weighted_node(1., 0.);
        let b = weighted_node(0., 0.);
        assert_eq!(a.cmp(&b), cmp::Ordering::Less);
        assert!(a.le(&b));
    }
    #[test]
    fn test_weighted_node_le_g() {
        let a = weighted_node(0., 1.);
        let b = weighted_node(0., 0.);
        assert_eq!(a.cmp(&b), cmp::Ordering::Greater);
        assert!(!a.le(&b));
    }
}
