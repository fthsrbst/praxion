//! The four goal types of the planner contract (§5.5): a block, a region, an
//! entity, and a radius around a position.
//!
//! The heuristic is Azalea's (`azalea/src/pathfinder/goals.rs`, MIT, commit
//! 8041a14): the diagonal-plus-straight x/z distance in ticks plus Azalea's
//! vertical term. It is an estimate, not an admissible bound for every move
//! (Azalea documents the same for its own jumps and falls); the A* therefore
//! keeps Baritone-style best-path bookkeeping for budget stops.

use std::f32::consts::SQRT_2;

use praxion_world::BlockPos;

use crate::costs::{
    COST_HEURISTIC, FALL_N_BLOCKS_COST, JUMP_ONE_BLOCK_COST, JUMP_PENALTY, WALK_ONE_BLOCK_COST,
};

/// What the planner is asked to reach.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Goal {
    /// Exactly this cell.
    Block(BlockPos),
    /// Any cell in the inclusive box.
    Region { min: BlockPos, max: BlockPos },
    /// The entity's current cell, resolved from perception before the search.
    Entity(i32),
    /// A cell whose centre is within `radius` blocks of `pos`'s centre.
    Near { pos: BlockPos, radius: f32 },
}

impl Goal {
    /// The estimated remaining cost from `n`, in ticks.
    pub fn heuristic(&self, n: BlockPos) -> f32 {
        match *self {
            Goal::Block(target) => block_heuristic(target, n),
            Goal::Region { min, max } => {
                let dx = outside(n.x, min.x, max.x) as f32;
                let dz = outside(n.z, min.z, max.z) as f32;
                let dy = outside(n.y, min.y, max.y) as f32;
                xz_heuristic(dx, dz) + y_heuristic(dy)
            }
            Goal::Near { pos, radius } => {
                let dx = delta(pos.x, n.x) as f32;
                let dz = delta(pos.z, n.z) as f32;
                let dy = delta(pos.y, n.y) as f32;
                let gap = radius.max(0.);
                let horizontal = (dx * dx + dz * dz).sqrt() - gap;
                let vertical = dy.abs() - gap;
                xz_heuristic(horizontal.max(0.), 0.)
                    + y_heuristic(if vertical > 0. { dy.signum() * vertical } else { 0. })
            }
            // Resolved to a `Block` before the search; never asked directly.
            Goal::Entity(_) => f32::INFINITY,
        }
    }

    /// Whether `n` satisfies the goal.
    pub fn success(&self, n: BlockPos) -> bool {
        match *self {
            Goal::Block(target) => n == target,
            Goal::Region { min, max } => {
                (min.x..=max.x).contains(&n.x)
                    && (min.y..=max.y).contains(&n.y)
                    && (min.z..=max.z).contains(&n.z)
            }
            Goal::Near { pos, radius } => {
                let dx = delta(pos.x, n.x) as f64;
                let dy = delta(pos.y, n.y) as f64;
                let dz = delta(pos.z, n.z) as f64;
                (dx * dx + dy * dy + dz * dz).sqrt() <= f64::from(radius)
            }
            Goal::Entity(_) => false,
        }
    }
}

/// `a - b` widened to `i64`: `i32::MIN - i32::MAX` fits, while the same
/// subtraction in `i32` panics in debug builds and wraps in release. Every
/// difference in a goal or a heuristic goes through here or [`outside`].
fn delta(a: i32, b: i32) -> i64 {
    i64::from(a) - i64::from(b)
}

fn outside(value: i32, min: i32, max: i32) -> i64 {
    let value = i64::from(value);
    let min = i64::from(min);
    let max = i64::from(max);
    if value < min {
        min - value
    } else if value > max {
        value - max
    } else {
        0
    }
}

fn block_heuristic(target: BlockPos, n: BlockPos) -> f32 {
    let dx = delta(target.x, n.x) as f32;
    let dy = delta(target.y, n.y) as f32;
    let dz = delta(target.z, n.z) as f32;
    xz_heuristic(dx, dz) + y_heuristic(dy)
}

fn xz_heuristic(dx: f32, dz: f32) -> f32 {
    let x = dx.abs();
    let z = dz.abs();

    let diagonal;
    let straight;

    if x < z {
        straight = z - x;
        diagonal = x;
    } else {
        straight = x - z;
        diagonal = z;
    }

    (diagonal * SQRT_2 + straight) * COST_HEURISTIC
}

/// Azalea's vertical term (`goals.rs:98-117`), with the same caveat that the
/// upward part is an overestimate.
fn y_heuristic(dy: f32) -> f32 {
    if dy > 0. {
        (f32::max(*JUMP_ONE_BLOCK_COST, WALK_ONE_BLOCK_COST) + JUMP_PENALTY
            - crate::costs::SPRINT_ONE_BLOCK_COST)
            * dy
    } else if dy < 0. {
        (FALL_N_BLOCKS_COST[2] / 2.) * -dy
    } else {
        0.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn block_goal_reaches_only_its_cell() {
        let goal = Goal::Block(pos(3, 64, 5));
        assert!(goal.success(pos(3, 64, 5)));
        assert!(!goal.success(pos(3, 64, 6)));
        assert!(goal.heuristic(pos(3, 64, 5)) < 1e-3);
        assert!(goal.heuristic(pos(0, 64, 5)) > 0.);
    }

    #[test]
    fn region_goal_accepts_its_box() {
        let goal = Goal::Region {
            min: pos(0, 64, 0),
            max: pos(2, 66, 2),
        };
        assert!(goal.success(pos(0, 64, 0)));
        assert!(goal.success(pos(2, 66, 2)));
        assert!(!goal.success(pos(3, 66, 2)));
        assert!(!goal.success(pos(1, 63, 1)));
        assert!(goal.heuristic(pos(3, 66, 3)) > 0.);
        assert!(goal.heuristic(pos(1, 65, 1)) < 1e-3);
    }

    #[test]
    fn near_goal_uses_the_centre_distance() {
        let goal = Goal::Near {
            pos: pos(0, 64, 0),
            radius: 2.,
        };
        assert!(goal.success(pos(2, 64, 0)));
        assert!(!goal.success(pos(3, 64, 0)));
        assert!(!goal.success(pos(0, 67, 0)));
        assert_eq!(goal.heuristic(pos(2, 64, 0)), 0.);
    }

    #[test]
    fn an_unresolved_entity_goal_is_never_reached() {
        let goal = Goal::Entity(7);
        assert!(!goal.success(pos(0, 64, 0)));
        assert!(goal.heuristic(pos(0, 64, 0)).is_infinite());
    }

    /// FINDING 1: `i32::MAX - i32::MIN` overflows. Every goal must stay a
    /// finite estimate at the ends of the coordinate range, in debug and in
    /// release.
    #[test]
    fn the_heuristic_survives_the_coordinate_extremes() {
        let far = pos(i32::MAX, i32::MAX, i32::MIN);
        let near = pos(i32::MIN, i32::MIN, i32::MAX);
        let goals = [
            Goal::Block(far),
            Goal::Region {
                min: far,
                max: near,
            },
            Goal::Near {
                pos: far,
                radius: 2.,
            },
        ];
        for goal in goals {
            for at in [far, near, pos(0, 64, 0)] {
                let h = goal.heuristic(at);
                assert!(!h.is_nan(), "{goal:?} at {at:?}");
                assert!(h.is_finite(), "{goal:?} at {at:?} gave {h}");
                // The success predicate is called for the same cells; it must
                // not overflow either.
                let _ = goal.success(at);
            }
        }
        assert!(Goal::Block(pos(0, 64, 0)).heuristic(far).is_finite());
        assert!(Goal::Near {
            pos: far,
            radius: f32::MAX,
        }
        .heuristic(near)
        .is_finite());
    }
}
