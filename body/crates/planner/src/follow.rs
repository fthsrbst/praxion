//! The path follower: a path becomes one [`ControlFrame`] per tick.
//!
//! The order is the contract's "camera then move" (§5.2): aim at the next
//! node through [`LookStyle`] and hold no movement key until the aim is inside
//! the tolerance, then hold forward. A step-up or pillar also holds jump while
//! grounded; a bridge holds sneak.
//!
//! The follower reports progress and deviation, and marks itself `stalled`
//! after `max_ticks_per_step` ticks on one step so the caller can replan. It
//! never digs or places: those primitives belong to the layer above, which
//! runs them before the movement segment.

use praxion_headless::{ControlFrame, Held};
use praxion_world::{BlockPos, Vec3};

use crate::look::{LookStyle, look_towards};
use crate::moves::MoveKind;
use crate::world::center;
use crate::Step;

/// A path follower for one planned segment.
#[derive(Clone, Debug)]
pub struct Follower {
    steps: Vec<Step>,
    next: usize,
    style: LookStyle,
    max_ticks_per_step: u32,
    ticks_on_step: u32,
    stalled: bool,
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
    pub fn new(steps: Vec<Step>, style: LookStyle) -> Self {
        Self {
            steps,
            next: 0,
            style,
            max_ticks_per_step: 200,
            ticks_on_step: 0,
            stalled: false,
        }
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
        // Azalea's walking look-at keeps the target at eye height, so the
        // pitch stays level while the yaw turns to the next cell.
        let aim_point = Vec3::new(target.x, position.y + 1.62, target.z);
        let eye = Vec3::new(position.x, position.y + 1.62, position.z);
        let (look, aligned) = look_towards(self.style, body.look(), eye, aim_point);
        frame.look = look;

        if aligned {
            frame.held.forward = true;
            let horizontal = horizontal_distance(position, target);
            match step.kind {
                MoveKind::StepUp | MoveKind::Pillar { .. } => {
                    if body.on_ground() && horizontal < 1.6 && position.y < target.y + 0.1 {
                        frame.held.jump = true;
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
        // South is already yaw 0, so no turn is needed.
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 64, 1),
            MoveKind::Walk,
        )];
        let mut follower = Follower::new(path, LookStyle::default());
        let frame = follower.step(0, body.generation(), &body);
        assert!(frame.held.forward);
        assert_eq!(frame.look, Look::None);
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
        let mut follower = Follower::new(path, LookStyle::default());
        let frame = follower.step(0, body.generation(), &body);
        assert!(!frame.held.forward, "forward waits for the aim tolerance");
        match frame.look {
            Look::Aim { yaw, .. } => assert!(yaw < 0., "east needs a negative yaw turn"),
            other => panic!("expected an aim, got {other:?}"),
        }
    }

    #[test]
    fn reaching_the_last_node_finishes_the_path() {
        let body = Body::new(Vec3::new(0.5, 64., 1.5));
        let path = vec![step(
            BlockPos::new(0, 64, 0),
            BlockPos::new(0, 64, 1),
            MoveKind::Walk,
        )];
        let mut follower = Follower::new(path, LookStyle::default());
        follower.step(0, body.generation(), &body);
        assert!(follower.is_done());
        assert!(follower.progress(&body).done);
    }
}
