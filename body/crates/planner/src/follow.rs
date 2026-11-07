//! The path follower: a path becomes one [`ControlFrame`] per tick.
//!
//! The order is the contract's "camera then move" (§5.2): request the next
//! node's look and hold no movement key until the aim is inside the tolerance,
//! then hold forward. The look request is an absolute target; the driver's
//! [`crate::Gaze`] turns it into the human gesture. A step-up or pillar also
//! holds jump while grounded; a bridge holds sneak.
//!
//! The follower reports progress and deviation, and marks itself `stalled`
//! after `max_ticks_per_step` ticks on one step so the caller can replan. It
//! never digs or places: those primitives belong to the layer above, which
//! runs them before the movement segment.

use praxion_headless::{ControlFrame, Held};
use praxion_world::{BlockPos, Vec3};

use crate::look::{WALK_TOLERANCE_DEG, look_towards};
use crate::moves::MoveKind;
use crate::world::center;
use crate::Step;

/// A path follower for one planned segment.
#[derive(Clone, Debug)]
pub struct Follower {
    steps: Vec<Step>,
    next: usize,
    /// The walking pitch, in degrees down (Minecraft's positive pitch).
    walk_pitch: f32,
    max_ticks_per_step: u32,
    ticks_on_step: u32,
    stalled: bool,
    /// Keep holding forward while airborne after a planned jump. A fall (a
    /// descent or an unmodelled ledge) does not set it, so the body lands on
    /// the planned cell instead of drifting into the next one.
    air_forward: bool,
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
    pub fn new(steps: Vec<Step>) -> Self {
        Self {
            steps,
            next: 0,
            walk_pitch: 8.0,
            max_ticks_per_step: 200,
            ticks_on_step: 0,
            stalled: false,
            air_forward: false,
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
        self.next >= self.steps.len()
    }

    pub fn is_stalled(&self) -> bool {
        self.stalled
    }

    /// The step index the follower is walking to, `steps.len()` when done.
    pub fn next_step(&self) -> usize {
        self.next
    }

    /// The cell the follower is walking to, if any.
    pub fn next_cell(&self) -> Option<BlockPos> {
        self.steps.get(self.next).map(|step| step.to)
    }

    /// Build this tick's frame. The caller validates and applies it, then
    /// steps physics.
    pub fn step(&mut self, tick: u64, generation: u32, body: &praxion_headless::Body) -> ControlFrame {
        let mut frame = ControlFrame::new(tick, generation, Held::NONE);
        if self.next >= self.steps.len() {
            return frame;
        }

        self.advance_if_reached(body.position());
        if self.next >= self.steps.len() {
            return frame;
        }

        let step = &self.steps[self.next];
        let position = body.position();
        let target = center(step.to);
        let eye = Vec3::new(position.x, position.y + 1.62, position.z);
        // Look a little below the target's column, so the pitch is the
        // personality's walking pitch instead of a flat 0.
        let horizontal = horizontal_distance(position, target);
        let drop = horizontal * f64::from(self.walk_pitch.to_radians().tan());
        let aim_point = Vec3::new(target.x, eye.y - drop, target.z);
        let (look, aligned) =
            look_towards(body.look(), eye, aim_point, WALK_TOLERANCE_DEG);
        frame.look = look;

        if aligned {
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
            if body.is_in_water()
                && horizontal < 1.6
                && position.y < target.y + 0.1
            {
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

        self.ticks_on_step = self.ticks_on_step.saturating_add(1);
        if self.ticks_on_step > self.max_ticks_per_step {
            self.stalled = true;
        }
        frame
    }

    /// Progress for the caller's log and replan decision.
    pub fn progress(&self, body: &praxion_headless::Body) -> Progress {
        let deviation = match self.steps.get(self.next) {
            Some(step) => horizontal_distance(body.position(), center(step.to)),
            None => 0.,
        };
        Progress {
            step: self.next,
            total: self.steps.len(),
            deviation,
            ticks_on_step: self.ticks_on_step,
            stalled: self.stalled,
            done: self.is_done(),
        }
    }

    fn advance_if_reached(&mut self, position: Vec3) {
        while let Some(step) = self.steps.get(self.next) {
            if reached(position, step.to) {
                self.next += 1;
                self.ticks_on_step = 0;
            } else {
                break;
            }
        }
    }
}

fn reached(position: Vec3, target: BlockPos) -> bool {
    let center = center(target);
    horizontal_distance(position, center) < 0.35
        && (position.y - center.y).abs() < 0.6
}

fn horizontal_distance(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

#[cfg(test)]
mod tests {
    use azalea_core::position::Vec3;
    use praxion_headless::{Body, Look};

    use super::*;

    fn body() -> Body {
        Body::new(Vec3::new(0.5, 64., 0.5))
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

    #[test]
    fn the_follower_aims_before_it_moves() {
        let mut body = body();
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
        let mut follower = Follower::new(path);
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
        let body = body();
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(1, 64, 0),
            MoveKind::Walk,
        )];
        let mut follower = Follower::new(path);
        let frame = follower.step(0, body.generation(), &body);
        assert!(!frame.held.forward, "forward waits for the aim tolerance");
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
        let body = Body::new(Vec3::new(0.5, 64., 0.5));
        assert!(!body.on_ground());
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 63, 1),
            MoveKind::Descend { drop: 1 },
        )];
        let mut follower = Follower::new(path);
        let frame = follower.step(0, body.generation(), &body);
        assert!(
            !frame.held.forward,
            "a falling body must not keep pushing forward"
        );
    }

    #[test]
    fn a_step_up_keeps_forward_while_airborne() {
        let mut body = Body::new(Vec3::new(0.5, 64., 0.5));
        body.set_look(azalea_entity::LookDirection::new(0., 8.));
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 65, 1),
            MoveKind::StepUp,
        )];
        let mut follower = Follower::new(path);
        let frame = follower.step(0, body.generation(), &body);
        assert!(frame.held.forward, "a jump needs its forward momentum");
    }

    #[test]
    fn reaching_the_last_node_finishes_the_path() {
        let body = Body::new(Vec3::new(0.5, 64., 1.5));
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 64, 1),
            MoveKind::Walk,
        )];
        let mut follower = Follower::new(path);
        follower.step(0, body.generation(), &body);
        assert!(follower.is_done());
        assert!(follower.progress(&body).done);
    }
}
