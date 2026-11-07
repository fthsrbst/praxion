//! A string-pulled route for the follower.
//!
//! The planner's steps are block cells. Following every cell centre produces
//! the stop-turn-go walk the movement style replaces (§4.4). A [`Route`] keeps
//! consecutive flat `Walk` steps only where the straight segment between the
//! last kept waypoint and the next one is not walkable through the same
//! predicates A* uses for `Walk`. Every other move (including `Diagonal`)
//! keeps its exact block-centre waypoint, because a jump, drop, bridge, pillar
//! or swim needs alignment.
//!
//! `line` is the kept route as a polyline (the start included, because the
//! follower's projection needs it) with the horizontal arc length of each
//! point, and `step_arcs` maps every plan step's cell onto that line so the
//! follower can still count progress per plan step.

use praxion_world::{BlockPos, Vec3};

use crate::Step;
use crate::moves::MoveKind;
use crate::state::PlanningState;
use crate::world::{KnownBlocks, WorldView, center};

/// The spacing of the walkability samples along a candidate segment, in
/// blocks.
const SAMPLE_BLOCKS: f64 = 0.25;
/// The body's half width: a segment also has to be clear this far beside its
/// centre line.
const HALF_WIDTH: f64 = 0.3;
/// A walking segment counts as open (the loose forward gate) when this much
/// space on both sides is known and walkable. A route squeezed beside an
/// unknown cell or a drop keeps the tight gate.
const OPEN_HALF_WIDTH: f64 = 0.9;

/// One waypoint the follower steers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RoutePoint {
    pub pos: BlockPos,
    /// The plan step that ends at this waypoint.
    pub step: usize,
    /// The segment to this waypoint has open, known ground on both sides, so
    /// the loose forward gate is safe here.
    pub open: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Route {
    points: Vec<RoutePoint>,
    /// The start and every kept waypoint, as cell centres.
    line: Vec<Vec3>,
    /// The horizontal arc length of every point on `line`; `arcs[0]` is 0.
    arcs: Vec<f64>,
    /// For each plan step, its cell projected onto `line` as an arc length.
    step_arcs: Vec<f64>,
    /// The horizontal length of `line`, in blocks.
    total: f64,
}

impl Route {
    /// String-pull `steps` through `world`, treating the planning state's
    /// protected and blacklisted cells as blocked.
    pub(crate) fn from_steps(
        steps: &[Step],
        world: &mut dyn KnownBlocks,
        state: &PlanningState,
    ) -> Self {
        let mut view = WorldView::new(world, &state.protected, &state.blacklist);

        let start = steps.first().map(|step| step.from);
        let mut points = Vec::new();
        let mut anchor = start;
        for (i, step) in steps.iter().enumerate() {
            let walk = flat_walk(step);
            let drop = walk
                && anchor.is_some_and(|from| {
                    steps.get(i + 1).is_some_and(|next| {
                        flat_walk(next)
                            && next.from.y == step.to.y
                            && straight_walkable(
                                &mut view,
                                from,
                                next.to,
                                step.to.y,
                                HALF_WIDTH,
                            )
                    })
                });
            if drop {
                continue;
            }
            let open = walk
                && anchor.is_some_and(|from| {
                    straight_walkable(&mut view, from, step.to, step.to.y, OPEN_HALF_WIDTH)
                });
            points.push(RoutePoint {
                pos: step.to,
                step: i,
                open,
            });
            anchor = Some(step.to);
        }

        let mut line = Vec::with_capacity(points.len() + usize::from(start.is_some()));
        if let Some(start) = start {
            line.push(center(start));
        }
        line.extend(points.iter().map(|point| center(point.pos)));
        let mut arcs = Vec::with_capacity(line.len());
        if !line.is_empty() {
            arcs.push(0.0);
        }
        for pair in line.windows(2) {
            let dx = pair[1].x - pair[0].x;
            let dz = pair[1].z - pair[0].z;
            let total = arcs.last().copied().unwrap_or(0.0);
            arcs.push(total + (dx * dx + dz * dz).sqrt());
        }
        let step_arcs = step_arcs(&line, &arcs, steps.iter().map(|step| center(step.to)));
        let total = arcs.last().copied().unwrap_or(0.0);
        Self {
            points,
            line,
            arcs,
            step_arcs,
            total,
        }
    }

    pub(crate) fn points(&self) -> &[RoutePoint] {
        &self.points
    }

    /// The point `look_ahead` blocks along the line ahead of `position`'s
    /// projection, clamped to the route's end.
    pub(crate) fn carrot(&self, position: Vec3, look_ahead: f32) -> Vec3 {
        let (arc, _) = self.project(position);
        self.at_arc((arc + f64::from(look_ahead)).min(self.total))
    }

    /// The horizontal distance still to walk from `position`'s projection.
    pub(crate) fn remaining(&self, position: Vec3) -> f64 {
        (self.total - self.project(position).0).max(0.0)
    }

    /// How many plan steps the body has passed, by arc length.
    pub(crate) fn steps_passed(&self, position: Vec3) -> usize {
        let arc = self.project(position).0;
        self.step_arcs
            .iter()
            .take_while(|step_arc| **step_arc <= arc + 1e-6)
            .count()
    }

    /// The projection of `position` onto the line: `(arc, point)`.
    fn project(&self, position: Vec3) -> (f64, Vec3) {
        project_point(&self.line, &self.arcs, position)
    }

    /// The plan steps of the route waypoints from `next` whose horizontal
    /// distance ahead of `position` along the line is under `reach`.
    pub(crate) fn steps_within(
        &self,
        next: usize,
        position: Vec3,
        reach: f64,
    ) -> impl Iterator<Item = usize> + '_ {
        let arc = self.project(position).0;
        self.points
            .get(next..)
            .unwrap_or(&[])
            .iter()
            .take_while(move |point| self.step_arcs[point.step] - arc < reach)
            .map(|point| point.step)
    }

    /// The point at horizontal arc length `arc` along the line.
    fn at_arc(&self, arc: f64) -> Vec3 {
        let Some(first) = self.line.first().copied() else {
            return Vec3::ZERO;
        };
        for (i, pair) in self.line.windows(2).enumerate() {
            let (a, b) = (pair[0], pair[1]);
            if arc <= self.arcs[i + 1] {
                let length = self.arcs[i + 1] - self.arcs[i];
                let t = ((arc - self.arcs[i]) / length).clamp(0.0, 1.0);
                return Vec3::new(
                    a.x + (b.x - a.x) * t,
                    a.y + (b.y - a.y) * t,
                    a.z + (b.z - a.z) * t,
                );
            }
        }
        self.line.last().copied().unwrap_or(first)
    }

    #[cfg(test)]
    fn total(&self) -> f64 {
        self.total
    }
}

/// A flat walking step: it keeps its height and moves like a person. Only
/// `Walk` qualifies: a `Diagonal` is its own move kind and keeps its exact
/// waypoint, as the design requires.
fn flat_walk(step: &Step) -> bool {
    if !matches!(step.kind, MoveKind::Walk) {
        return false;
    }
    debug_assert!(step.from.y == step.to.y, "an A* Walk never changes height");
    true
}

/// Whether the straight segment between two cell centres is walkable at `y`:
/// every 0.25 block, with the body's half width on both sides, the feet and
/// head are free and the floor is solid.
fn straight_walkable(
    view: &mut WorldView<'_>,
    from: BlockPos,
    to: BlockPos,
    y: i32,
    half_width: f64,
) -> bool {
    let a = center(from);
    let b = center(to);
    let dx = b.x - a.x;
    let dz = b.z - a.z;
    let distance = (dx * dx + dz * dz).sqrt();
    if distance < 1e-9 {
        return true;
    }
    let samples = ((distance / SAMPLE_BLOCKS).ceil() as usize).max(1);
    // Unit perpendicular, scaled by the requested half width.
    let side_x = -dz / distance * half_width;
    let side_z = dx / distance * half_width;
    for sample in 0..=samples {
        let t = sample as f64 / samples as f64;
        for side in [-1.0, 0.0, 1.0] {
            let x = a.x + dx * t + side_x * side;
            let z = a.z + dz * t + side_z * side;
            let cell = BlockPos::new(x.floor() as i32, y, z.floor() as i32);
            if !matches!(view.body_free(cell), Ok(true)) {
                return false;
            }
            if !matches!(view.floor_free(cell), Ok(true)) {
                return false;
            }
        }
    }
    true
}

/// Project each `target` onto `line` and return its horizontal arc length.
/// The result is made monotonic: the plan's cells are in walking order, and a
/// projection at a corner may otherwise land a hair behind the previous one.
fn step_arcs(line: &[Vec3], arcs: &[f64], targets: impl Iterator<Item = Vec3>) -> Vec<f64> {
    let mut step_arcs = Vec::new();
    let mut previous = 0.0;
    for target in targets {
        let (arc, _) = project_point(line, arcs, target);
        let arc = arc.max(previous);
        previous = arc;
        step_arcs.push(arc);
    }
    step_arcs
}

fn project_point(line: &[Vec3], arcs: &[f64], position: Vec3) -> (f64, Vec3) {
    let Some(first) = line.first().copied() else {
        return (0.0, Vec3::ZERO);
    };
    let mut best = (0.0, first, f64::INFINITY);
    for (i, pair) in line.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        let dx = b.x - a.x;
        let dz = b.z - a.z;
        let length = arcs[i + 1] - arcs[i];
        let t = (((position.x - a.x) * dx + (position.z - a.z) * dz) / (length * length))
            .clamp(0.0, 1.0);
        let x = a.x + dx * t;
        let z = a.z + dz * t;
        let distance_sq = (position.x - x).powi(2) + (position.z - z).powi(2);
        if distance_sq < best.2 {
            best = (arcs[i] + length * t, Vec3::new(x, a.y, z), distance_sq);
        }
    }
    (best.0, best.1)
}

#[cfg(test)]
mod tests {
    use azalea_registry::builtin::BlockKind;
    use praxion_world::BlockPos;

    use super::*;
    use crate::world::TestWorld;

    fn step(from: BlockPos, to: BlockPos, kind: MoveKind) -> Step {
        Step {
            from,
            to,
            kind,
            primitives: Vec::new(),
            cost: 1.0,
        }
    }

    fn stone() -> azalea_block::BlockState {
        azalea_block::BlockState::from(BlockKind::Stone)
    }

    fn l_steps() -> Vec<Step> {
        vec![
            step(
                BlockPos::new(0, 64, 0),
                BlockPos::new(1, 64, 0),
                MoveKind::Walk,
            ),
            step(
                BlockPos::new(1, 64, 0),
                BlockPos::new(1, 64, 1),
                MoveKind::Walk,
            ),
        ]
    }

    #[test]
    fn a_straight_flat_path_keeps_only_its_ends() {
        let mut world = TestWorld::flat(63, 12);
        let steps: Vec<Step> = (0..4)
            .map(|x| {
                step(
                    BlockPos::new(x, 64, 0),
                    BlockPos::new(x + 1, 64, 0),
                    MoveKind::Walk,
                )
            })
            .collect();
        let route = Route::from_steps(&steps, &mut world, &PlanningState::default());
        let points: Vec<BlockPos> = route.points().iter().map(|point| point.pos).collect();
        assert_eq!(points, vec![BlockPos::new(4, 64, 0)]);
    }

    #[test]
    fn an_l_corner_is_kept_only_when_the_diagonal_is_blocked() {
        let steps = l_steps();

        // Open space: the diagonal replaces both steps, so only the end stays.
        let mut open = TestWorld::flat(63, 8);
        let route = Route::from_steps(&steps, &mut open, &PlanningState::default());
        let points: Vec<BlockPos> = route.points().iter().map(|point| point.pos).collect();
        assert_eq!(points, vec![BlockPos::new(1, 64, 1)]);

        // A wall at the inside of the corner blocks the diagonal but not the
        // planned L, so the corner waypoint must survive.
        let mut blocked = TestWorld::flat(63, 8);
        blocked.set(0, 64, 1, stone());
        let route = Route::from_steps(&steps, &mut blocked, &PlanningState::default());
        let points: Vec<BlockPos> = route.points().iter().map(|point| point.pos).collect();
        assert_eq!(
            points,
            vec![BlockPos::new(1, 64, 0), BlockPos::new(1, 64, 1)]
        );
    }

    #[test]
    fn a_blacklisted_diagonal_cell_keeps_the_corner() {
        // The world is open, but the planner may not occupy the cell on the
        // diagonal, so the smoother must not cut the corner through it.
        let mut world = TestWorld::flat(63, 8);
        let mut state = PlanningState::default();
        state.blacklist.insert(BlockPos::new(0, 64, 1));
        let route = Route::from_steps(&l_steps(), &mut world, &state);
        let points: Vec<BlockPos> = route.points().iter().map(|point| point.pos).collect();
        assert_eq!(
            points,
            vec![BlockPos::new(1, 64, 0), BlockPos::new(1, 64, 1)]
        );
    }

    #[test]
    fn a_step_up_keeps_its_waypoint() {
        let mut world = TestWorld::flat(63, 8);
        // The raised cell needs its own floor.
        world.set(2, 63, 0, stone());
        world.set(2, 64, 0, stone());
        let steps = vec![
            step(
                BlockPos::new(0, 64, 0),
                BlockPos::new(1, 64, 0),
                MoveKind::Walk,
            ),
            step(
                BlockPos::new(1, 64, 0),
                BlockPos::new(1, 65, 0),
                MoveKind::StepUp,
            ),
        ];
        let route = Route::from_steps(&steps, &mut world, &PlanningState::default());
        let points: Vec<&RoutePoint> = route.points().iter().collect();
        assert_eq!(points.len(), 2);
        assert_eq!(points[1].pos, BlockPos::new(1, 65, 0));
        assert_ne!(steps[1].kind, MoveKind::Walk);
    }

    #[test]
    fn the_carrot_sits_look_ahead_and_clamps_to_the_end() {
        let mut world = TestWorld::flat(63, 16);
        let steps: Vec<Step> = (0..10)
            .map(|x| {
                step(
                    BlockPos::new(x, 64, 0),
                    BlockPos::new(x + 1, 64, 0),
                    MoveKind::Walk,
                )
            })
            .collect();
        let route = Route::from_steps(&steps, &mut world, &PlanningState::default());
        let start = Vec3::new(0.5, 64.0, 0.5);

        let carrot = route.carrot(start, 2.5);
        assert!((carrot.x - 3.0).abs() < 1e-6, "{carrot:?}");
        assert!((carrot.z - 0.5).abs() < 1e-6, "{carrot:?}");

        let near_end = Vec3::new(9.9, 64.0, 0.5);
        let carrot = route.carrot(near_end, 2.5);
        assert!(
            (carrot.x - 10.5).abs() < 1e-6,
            "the carrot is the end: {carrot:?}"
        );
        assert!((route.remaining(start) - 10.0).abs() < 1e-6);
        assert!(route.total() > 9.9);
    }
}
