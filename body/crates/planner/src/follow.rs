//! The path follower: a route becomes one [`ControlFrame`] per tick.
//!
//! The order is the contract's "camera then move" (§5.2), but the camera now
//! looks at a carrot on a human route instead of at each block centre: the
//! follower holds no movement key until the yaw heading error is inside the
//! gate, then walks while it keeps turning. The look request is an absolute
//! target; the driver's [`crate::Gaze`] turns it into the human gesture. A
//! step-up or pillar also holds jump while grounded; a bridge holds sneak.
//!
//! The follower reports progress and deviation, and marks itself `stalled`
//! after `max_ticks_per_step` ticks on one step so the caller can replan. It
//! never digs or places: those primitives belong to the layer above, which
//! runs them before the movement segment.

use praxion_headless::{ControlFrame, Held};
use praxion_world::{BlockPos, Vec3};

use crate::Step;
use crate::look::{Personality, aim, wrap_degrees};
use crate::moves::MoveKind;
use crate::route::{Route, RoutePoint};
use crate::state::PlanningState;
use crate::world::{KnownBlocks, center};

/// The forward gate on open, flat walking, standing: a yaw error this large
/// still walks.
const GATE_STANDING_DEG: f32 = 50.0;
/// The forward gate once the body is moving.
const GATE_MOVING_DEG: f32 = 75.0;
/// The forward gate near a jump, drop or bridge.
const GATE_TIGHT_DEG: f32 = 20.0;
/// The horizontal speed above which the body counts as already moving, in
/// blocks per tick.
const MOVING_SPEED: f64 = 0.05;
/// Sprint needs this much route left, in blocks.
const SPRINT_MIN_REMAINING: f64 = 2.5;
/// Sprint needs a yaw error below this.
const SPRINT_HEADING_DEG: f32 = 40.0;
/// A replanned follower releases sprint for this many ticks.
const SPRINT_RELEASE_TICKS: u64 = 12;
/// How far ahead, in blocks along the route, a hazard still changes the gate
/// and refuses sprint.
const HAZARD_WINDOW: f64 = 4.0;

/// What the route ahead asks of the follower this tick.
#[derive(Clone, Copy, Debug, Default)]
struct Hazards {
    /// A drop or bridge: tighten the forward gate.
    tight: bool,
    /// A move that needs care: no sprint.
    no_sprint: bool,
}

/// A path follower for one planned segment.
#[derive(Clone, Debug)]
pub struct Follower {
    steps: Vec<Step>,
    route: Route,
    /// The route waypoint being steered to.
    next: usize,
    /// The plan step the body has passed, for progress and the stall clock.
    next_step: usize,
    style: Personality,
    /// The walking pitch, in degrees down (Minecraft's positive pitch).
    walk_pitch: f32,
    max_ticks_per_step: u32,
    ticks_on_step: u32,
    stalled: bool,
    /// Keep holding forward while airborne after a planned jump. A fall (a
    /// descent or an unmodelled ledge) does not set it, so the body lands on
    /// the planned cell instead of drifting into the next one.
    air_forward: bool,
    /// The body position last tick, for the horizontal speed.
    last_position: Option<Vec3>,
    /// The yaw request currently sent. Steering replaces it only outside the
    /// personality's deadband.
    yaw_request: Option<f32>,
    /// The tick forward started being held on this flat run.
    forward_since: Option<u64>,
    /// Sprint is released until this tick after a replan.
    sprint_release_until: Option<u64>,
}

/// What the follower reports for the caller's progress check.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub step: usize,
    pub total: usize,
    pub deviation: f64,
    pub ticks_on_step: u32,
    pub stalled: bool,
    pub done: bool,
}

impl Follower {
    /// Build a follower over `steps`, smoothing its flat walking stretches
    /// through `world` and using `style` for look-ahead, deadband and sprint.
    /// The planning state's protected and blacklisted cells stay blocked.
    pub fn new(
        steps: Vec<Step>,
        world: &mut dyn KnownBlocks,
        state: &PlanningState,
        style: Personality,
    ) -> Self {
        let route = Route::from_steps(&steps, world, state);
        Self {
            steps,
            route,
            next: 0,
            next_step: 0,
            style,
            walk_pitch: 8.0,
            max_ticks_per_step: 200,
            ticks_on_step: 0,
            stalled: false,
            air_forward: false,
            last_position: None,
            yaw_request: None,
            forward_since: None,
            sprint_release_until: None,
        }
    }

    /// The pitch the walk requests, in degrees down (default 8).
    pub fn with_walk_pitch(mut self, degrees: f32) -> Self {
        self.walk_pitch = degrees;
        self
    }

    pub fn with_max_ticks_per_step(mut self, ticks: u32) -> Self {
        self.max_ticks_per_step = ticks.max(1);
        self
    }

    pub fn is_done(&self) -> bool {
        self.next >= self.route.points().len()
    }

    pub fn is_stalled(&self) -> bool {
        self.stalled
    }

    /// The plan step the follower is walking to, `steps.len()` when done.
    pub fn next_step(&self) -> usize {
        self.next_step.min(self.steps.len())
    }

    /// The cell the follower is steering to, if any.
    pub fn next_cell(&self) -> Option<BlockPos> {
        self.route.points().get(self.next).map(|point| point.pos)
    }

    /// Build this tick's frame. The caller validates and applies it, then
    /// steps physics.
    pub fn step(
        &mut self,
        tick: u64,
        generation: u32,
        body: &praxion_headless::Body,
    ) -> ControlFrame {
        let mut frame = ControlFrame::new(tick, generation, Held::NONE);
        self.sprint_release_until
            .get_or_insert(tick.saturating_add(SPRINT_RELEASE_TICKS));
        if self.route.points().is_empty() {
            return frame;
        }

        let position = body.position();
        let speed = self.horizontal_speed(position);
        self.advance_if_reached(position);
        if self.is_done() {
            return frame;
        }
        let passed = self.route.steps_passed(position);
        if passed > self.next_step {
            self.next_step = passed;
            self.ticks_on_step = 0;
        }

        let point = self.route.points()[self.next];
        let walk = self.is_walk(&point);
        let eye = Vec3::new(position.x, position.y + 1.62, position.z);
        let target = center(point.pos);
        let (bearing, yaw) = if walk {
            // In a tight place (an unknown cell or a drop beside the route) look
            // at the next waypoint itself: a look-ahead around the corner would
            // cut through the cell the route planned around.
            let carrot = if point.open {
                self.route.carrot(position, self.style.look_ahead)
            } else {
                target
            };
            let (bearing, _) = aim(eye, Vec3::new(carrot.x, eye.y, carrot.z));
            let yaw = self.steer(bearing, self.style.deadband_deg);
            (bearing, yaw)
        } else {
            let (bearing, _) = aim(eye, Vec3::new(target.x, eye.y, target.z));
            let yaw = self.steer(bearing, 0.0);
            (bearing, yaw)
        };
        frame.look = praxion_headless::Look::Aim {
            yaw,
            pitch: self.walk_pitch,
        };

        let step = &self.steps[point.step];

        let error = wrap_degrees(bearing - body.look().y_rot()).abs();
        let hazards = self.hazards(position);
        let near = !walk || !point.open || hazards.tight;
        let gate = if near {
            GATE_TIGHT_DEG
        } else if speed > MOVING_SPEED {
            GATE_MOVING_DEG
        } else {
            GATE_STANDING_DEG
        };

        if error < gate {
            // A planned jump or a swim keeps forward control while airborne.
            // A fall does not: holding forward through a descent carried the
            // body past its landing cell into the next ledge (seed 303: the
            // plan to y74 left the body at y67, stalled and unable to climb
            // back). Once the ground is gone, gravity should finish the step.
            let air_control = matches!(
                step.kind,
                MoveKind::StepUp
                    | MoveKind::Pillar { .. }
                    | MoveKind::Bridge { .. }
                    | MoveKind::Swim
                    | MoveKind::SwimUp
                    | MoveKind::EnterWater
                    | MoveKind::ExitWater { .. }
            );
            if body.on_ground() {
                self.air_forward = air_control;
            }
            // A step's own air control (a jump, a swim) applies whether or
            // not it started on the ground. A body in water always swims:
            // stopping forward there leaves it floating, short of the step.
            self.air_forward |= air_control;
            if body.on_ground() || self.air_forward || body.is_in_water() {
                frame.held.forward = true;
            }
            let horizontal = horizontal_distance(position, target);
            match step.kind {
                MoveKind::StepUp | MoveKind::Pillar { .. } => {
                    if body.on_ground() && horizontal < 1.6 && position.y < target.y + 0.1 {
                        frame.held.jump = true;
                        self.air_forward = true;
                    }
                }
                MoveKind::Bridge { .. } => frame.held.sneak = true,
                // Water: holding jump is how the client rises in water. An
                // exit that climbs one block needs it too; at the same level
                // forward is enough.
                MoveKind::SwimUp => frame.held.jump = true,
                MoveKind::ExitWater { rise } => {
                    if rise || body.is_in_water() {
                        frame.held.jump = true;
                    }
                }
                MoveKind::EnterWater | MoveKind::Swim => {}
                _ => {}
            }
            // A body in water needs jump to rise and clear the bank; explore
            // avoids water paths, so this only recovers an unexpected dip.
            if body.is_in_water() && horizontal < 1.6 && position.y < target.y + 0.1 {
                frame.held.jump = true;
            }
            // A diagonal corner or a lip the path did not model can hold the
            // body at a constant offset. Jumping while forward clears most of
            // them; without it the follower stalls on step 0 forever.
            if self.ticks_on_step > 15
                && body.on_ground()
                && !matches!(
                    step.kind,
                    MoveKind::Bridge { .. }
                        | MoveKind::Swim
                        | MoveKind::SwimUp
                        | MoveKind::EnterWater
                        | MoveKind::ExitWater { .. }
                )
            {
                frame.held.jump = true;
                self.air_forward = true;
            }
        }
        self.forward_since = match (frame.held.forward, walk) {
            (true, true) => Some(self.forward_since.unwrap_or(tick)),
            _ => None,
        };
        if frame.held.forward {
            let remaining = self.route.remaining(position);
            let delay = u64::from(self.style.sprint_delay_ticks);
            let delay_elapsed = self
                .forward_since
                .is_some_and(|since| tick >= since.saturating_add(delay));
            let released = self.sprint_release_until.is_some_and(|until| tick < until);
            if walk
                && point.open
                && self.style.sprints
                && remaining > SPRINT_MIN_REMAINING
                && error < SPRINT_HEADING_DEG
                && delay_elapsed
                && !released
                && !hazards.no_sprint
            {
                frame.held.sprint = true;
            }
        }

        self.ticks_on_step = self.ticks_on_step.saturating_add(1);
        if self.ticks_on_step > self.max_ticks_per_step {
            self.stalled = true;
        }
        frame
    }

    /// Progress for the caller's log and replan decision.
    pub fn progress(&self, body: &praxion_headless::Body) -> Progress {
        let deviation = match self.route.points().get(self.next) {
            Some(point) => horizontal_distance(body.position(), center(point.pos)),
            None => 0.,
        };
        Progress {
            step: self.next_step(),
            total: self.steps.len(),
            deviation,
            ticks_on_step: self.ticks_on_step,
            stalled: self.stalled,
            done: self.is_done(),
        }
    }

    /// Keep the current yaw request until the bearing moves outside `deadband`.
    /// This is the discrete "mouse gesture" steering, not a continuous servo.
    fn steer(&mut self, bearing: f32, deadband: f32) -> f32 {
        match self.yaw_request {
            Some(current) if wrap_degrees(bearing - current).abs() <= deadband => current,
            _ => {
                self.yaw_request = Some(bearing);
                bearing
            }
        }
    }

    fn horizontal_speed(&mut self, position: Vec3) -> f64 {
        let speed = self
            .last_position
            .map(|last| horizontal_distance(last, position))
            .unwrap_or(0.0);
        self.last_position = Some(position);
        speed
    }

    /// The hazards within [`HAZARD_WINDOW`] blocks ahead on the route.
    fn hazards(&self, position: Vec3) -> Hazards {
        let mut hazards = Hazards::default();
        for step in self.route.steps_within(self.next, position, HAZARD_WINDOW) {
            match self.steps[step].kind {
                MoveKind::Descend { .. } | MoveKind::Bridge { .. } => {
                    hazards.tight = true;
                    hazards.no_sprint = true;
                }
                MoveKind::Swim
                | MoveKind::SwimUp
                | MoveKind::EnterWater
                | MoveKind::ExitWater { .. }
                | MoveKind::StepUp
                | MoveKind::Pillar { .. } => hazards.no_sprint = true,
                _ => {}
            }
        }
        hazards
    }

    /// Whether this route point ends on a flat `Walk` step.
    fn is_walk(&self, point: &RoutePoint) -> bool {
        matches!(self.steps[point.step].kind, MoveKind::Walk)
    }

    fn advance_if_reached(&mut self, position: Vec3) {
        while let Some(point) = self.route.points().get(self.next) {
            let last = self.next + 1 == self.route.points().len();
            let walk = self.is_walk(point);
            let reached = if last || !walk {
                reached_tight(position, point.pos)
            } else {
                let next = self.route.points()[self.next + 1].pos;
                reached_walk(position, point, next)
            };
            if reached {
                self.next += 1;
                self.ticks_on_step = 0;
            } else {
                break;
            }
        }
    }
}

/// The tight rule: the current 0.35 block, used for a final waypoint and every
/// non-walking move.
fn reached_tight(position: Vec3, target: BlockPos) -> bool {
    let center = center(target);
    horizontal_distance(position, center) < 0.35 && (position.y - center.y).abs() < 0.6
}

/// A flat walking waypoint: within 0.6, or within 1.2 once the body has passed
/// it along the next segment without drifting sideways off that segment. The
/// loose rule needs an `open` waypoint; a tight one keeps the 0.6 rule.
fn reached_walk(position: Vec3, target: &RoutePoint, next: BlockPos) -> bool {
    let here = center(target.pos);
    let distance = horizontal_distance(position, here);
    if (position.y - here.y).abs() >= 0.6 {
        return false;
    }
    if distance < 0.6 {
        return true;
    }
    if !target.open || distance >= 1.2 {
        return false;
    }
    let next = center(next);
    let forward_x = next.x - here.x;
    let forward_z = next.z - here.z;
    let forward = (forward_x * forward_x + forward_z * forward_z).sqrt();
    let past_x = position.x - here.x;
    let past_z = position.z - here.z;
    let along = (past_x * forward_x + past_z * forward_z) / forward;
    let sideways = (past_x * forward_z - past_z * forward_x).abs() / forward;
    along > 0.0 && sideways < 0.6
}

fn horizontal_distance(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use azalea_core::position::Vec3;
    use azalea_registry::builtin::BlockKind;
    use praxion_headless::{Body, Held, Look};

    use super::*;
    use crate::world::TestWorld;

    fn body_at(x: f64, y: f64, z: f64) -> Body {
        Body::new(Vec3::new(x, y, z))
    }

    fn step(from: BlockPos, to: BlockPos, kind: MoveKind) -> Step {
        Step {
            from,
            to,
            kind,
            primitives: Vec::new(),
            cost: 1.,
        }
    }

    fn stone() -> azalea_block::BlockState {
        azalea_block::BlockState::from(BlockKind::Stone)
    }

    /// A flat physics world: stone at y = 63, air above and below.
    struct GroundWorld;

    impl praxion_headless::Blocks for GroundWorld {
        fn block(&self, pos: BlockPos) -> Option<azalea_block::BlockState> {
            if pos.y == 63 { Some(stone()) } else { None }
        }
    }

    fn settle(body: &mut Body) {
        for _ in 0..5 {
            body.step(&Held::NONE, &GroundWorld);
        }
        assert!(body.on_ground());
    }

    fn walk_steps(last_x: i32) -> Vec<Step> {
        (0..last_x)
            .map(|x| {
                step(
                    BlockPos::new(x, 64, 0),
                    BlockPos::new(x + 1, 64, 0),
                    MoveKind::Walk,
                )
            })
            .collect()
    }

    #[test]
    fn the_follower_aims_before_it_moves() {
        let mut body = body_at(0.5, 64., 0.5);
        // South is already yaw 0 and the camera already sits at the walking
        // pitch, so no gesture is needed. This body is airborne (there is no
        // world here), so use a StepUp: a Walk would wait for the ground
        // before moving forward.
        body.set_look(azalea_entity::LookDirection::new(0., 8.));
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 65, 1),
            MoveKind::StepUp,
        )];
        let mut world = TestWorld::new();
        let mut follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        let frame = follower.step(0, body.generation(), &body);
        assert!(frame.held.forward);
        match frame.look {
            Look::Aim { yaw, .. } => assert!(yaw.abs() < 1e-3, "south is already yaw 0"),
            other => panic!("the follower requests its target, got {other:?}"),
        }
        frame.apply(&mut body, 0).unwrap();
    }

    #[test]
    fn a_turn_is_spread_over_ticks_and_forward_waits() {
        let body = body_at(0.5, 64., 0.5);
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(1, 64, 0),
            MoveKind::Walk,
        )];
        let mut world = TestWorld::flat(63, 8);
        let mut follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        let frame = follower.step(0, body.generation(), &body);
        assert!(!frame.held.forward, "forward waits for the heading gate");
        match frame.look {
            Look::Aim { yaw, .. } => assert!(yaw < 0., "east needs a negative yaw turn"),
            other => panic!("expected an aim, got {other:?}"),
        }
    }

    #[test]
    fn a_descend_does_not_hold_forward_through_the_fall() {
        // Airborne after walking off the ledge: forward must be released so
        // the body drops onto the planned cell instead of drifting into the
        // next ledge (seed 303 ended 7 blocks below its plan this way).
        let body = body_at(0.5, 64., 0.5);
        assert!(!body.on_ground());
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 63, 1),
            MoveKind::Descend { drop: 1 },
        )];
        let mut world = TestWorld::new();
        let mut follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        let frame = follower.step(0, body.generation(), &body);
        assert!(
            !frame.held.forward,
            "a falling body must not keep pushing forward"
        );
    }

    #[test]
    fn a_step_up_keeps_forward_while_airborne() {
        let mut body = body_at(0.5, 64., 0.5);
        body.set_look(azalea_entity::LookDirection::new(0., 8.));
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 65, 1),
            MoveKind::StepUp,
        )];
        let mut world = TestWorld::new();
        let mut follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        let frame = follower.step(0, body.generation(), &body);
        assert!(frame.held.forward, "a jump needs its forward momentum");
    }

    #[test]
    fn a_waypoint_is_not_passed_sideways_before_the_corner() {
        // The review's corner: a kept waypoint with the body at the inner side
        // of the corner, 0.85 away. The projection along the next segment says
        // "past", but the body is 0.8 off that segment, so it must not pass.
        let point = RoutePoint {
            pos: BlockPos::new(1, 64, 0),
            step: 0,
            open: true,
        };
        let next = BlockPos::new(1, 64, 4);
        let inner = Vec3::new(0.7, 64.0, 0.79);
        assert!(
            !reached_walk(inner, &point, next),
            "0.85 away but 0.8 off the next segment"
        );

        // Past it and on the next segment, the loose rule still passes.
        assert!(reached_walk(Vec3::new(1.5, 64.0, 1.3), &point, next));

        // A tight (not open) waypoint keeps the 0.6 rule.
        let closed = RoutePoint {
            open: false,
            ..point
        };
        assert!(!reached_walk(Vec3::new(1.5, 64.0, 1.3), &closed, next));
    }

    #[test]
    fn a_descend_three_and_a_half_blocks_ahead_refuses_sprint() {
        let mut world = TestWorld::flat(63, 8);
        // Two walls keep the three flat waypoints: each corner's diagonal is
        // blocked, but the planned L-shaped walk is open.
        world.set(0, 64, 1, stone());
        world.set(2, 64, 0, stone());
        let path = vec![
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
            step(
                BlockPos::new(1, 64, 1),
                BlockPos::new(2, 64, 1),
                MoveKind::Walk,
            ),
            step(
                BlockPos::new(2, 64, 1),
                BlockPos::new(2, 63, 2),
                MoveKind::Descend { drop: 1 },
            ),
        ];
        let follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        let body = Vec3::new(1.0, 64.0, 0.5);
        let hazards = follower.hazards(body);
        assert!(hazards.tight, "the drop tightens the gate");
        assert!(hazards.no_sprint, "the drop refuses sprint");

        // One block further back the drop is 4.0 blocks ahead, outside the
        // window.
        let hazards = follower.hazards(Vec3::new(0.5, 64.0, 0.5));
        assert!(!hazards.tight);
        assert!(!hazards.no_sprint);
    }

    #[test]
    fn reaching_the_last_node_finishes_the_path() {
        let body = body_at(0.5, 64., 1.5);
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 64, 1),
            MoveKind::Walk,
        )];
        let mut world = TestWorld::flat(63, 8);
        let mut follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        follower.step(0, body.generation(), &body);
        assert!(follower.is_done());
        assert!(follower.progress(&body).done);
    }

    #[test]
    fn the_deadband_keeps_a_small_bearing_change_quiet() {
        let mut world = TestWorld::flat(63, 8);
        let mut follower = Follower::new(
            walk_steps(4),
            &mut world,
            &PlanningState::default(),
            Personality {
                deadband_deg: 10.0,
                ..Personality::default()
            },
        );
        assert_eq!(follower.steer(0.0, 10.0), 0.0);
        assert_eq!(follower.steer(5.0, 10.0), 0.0, "inside the deadband");
        assert_eq!(follower.steer(15.0, 10.0), 15.0, "outside it");
    }

    #[test]
    fn a_heading_error_under_the_gate_walks_on_flat_ground() {
        let mut world = TestWorld::flat(63, 8);
        let mut body = body_at(0.5, 64.0, 0.5);
        settle(&mut body);
        // The route runs east (bearing -90); 40 degrees off is still inside
        // the standing gate of 50.
        body.set_look(azalea_entity::LookDirection::new(-50.0, 8.0));
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(1, 64, 0),
            MoveKind::Walk,
        )];
        let mut follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        let frame = follower.step(0, body.generation(), &body);
        assert!(frame.held.forward);
    }

    #[test]
    fn a_descend_ahead_tightens_the_gate() {
        let mut world = TestWorld::flat(63, 8);
        world.remove(2, 63, 0);
        world.set(2, 62, 0, stone());
        let mut body = body_at(0.5, 64.0, 0.5);
        settle(&mut body);
        body.set_look(azalea_entity::LookDirection::new(-50.0, 8.0));
        let path = vec![
            step(
                BlockPos::new(0, 64, 0),
                BlockPos::new(1, 64, 0),
                MoveKind::Walk,
            ),
            step(
                BlockPos::new(1, 64, 0),
                BlockPos::new(2, 63, 0),
                MoveKind::Descend { drop: 1 },
            ),
        ];
        let mut follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        let frame = follower.step(0, body.generation(), &body);
        assert!(
            !frame.held.forward,
            "40 degrees is outside the 20 degree gate near a drop"
        );
    }

    /// Drive a straight east route until `ticks` have elapsed, returning
    /// whether sprint was held on each tick.
    fn sprint_ticks(style: Personality, ticks: usize) -> (Vec<bool>, Follower) {
        let mut world = TestWorld::flat(63, 32);
        let mut body = body_at(0.5, 64.0, 0.5);
        settle(&mut body);
        body.set_look(azalea_entity::LookDirection::new(-90.0, 8.0));
        let mut follower =
            Follower::new(walk_steps(20), &mut world, &PlanningState::default(), style);
        let mut held = Vec::new();
        for tick in 0..ticks as u64 {
            let frame = follower.step(tick, body.generation(), &body);
            held.push(frame.held.sprint);
            frame.apply(&mut body, tick).unwrap();
            body.step(&frame.held, &GroundWorld);
        }
        (held, follower)
    }

    #[test]
    fn sprint_waits_for_the_delay_and_the_replan_release() {
        let style = Personality {
            sprints: true,
            sprint_delay_ticks: 30,
            ..Personality::default()
        };
        let (held, _) = sprint_ticks(style, 40);
        assert!(held[..12].iter().all(|&sprint| !sprint));
        assert!(held[..30].iter().all(|&sprint| !sprint), "{held:?}");
        assert!(held[30..].iter().any(|&sprint| sprint), "{held:?}");
    }

    #[test]
    fn no_sprint_without_the_personality_or_with_little_route_left() {
        let no_sprint = Personality {
            sprints: false,
            ..Personality::default()
        };
        let (held, _) = sprint_ticks(no_sprint, 20);
        assert!(held.iter().all(|&sprint| !sprint));

        let mut world = TestWorld::flat(63, 8);
        let mut body = body_at(0.5, 64.0, 0.5);
        settle(&mut body);
        body.set_look(azalea_entity::LookDirection::new(-90.0, 8.0));
        // Two blocks of route: under the 2.5 block sprint threshold.
        let path = vec![
            step(
                BlockPos::new(0, 64, 0),
                BlockPos::new(1, 64, 0),
                MoveKind::Walk,
            ),
            step(
                BlockPos::new(1, 64, 0),
                BlockPos::new(2, 64, 0),
                MoveKind::Walk,
            ),
        ];
        let mut follower = Follower::new(
            path,
            &mut world,
            &PlanningState::default(),
            Personality::default(),
        );
        follower.step(0, body.generation(), &body);
        let frame = follower.step(20, body.generation(), &body);
        assert!(!frame.held.sprint);
    }
}
