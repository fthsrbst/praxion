//! The Enderman look guard: never rest a look on an Enderman's head for more
//! than one tick while one is within 64 blocks.
//!
//! The guard is not a reflex action. It is applied to every `Look` the body
//! emits, whoever owns the body (a reflex, a skill or idle). It works on the
//! body's actual current look, so it catches a frame that says `Look::None`
//! but leaves the camera on the head from the previous tick.

use azalea_entity::LookDirection;
use praxion_headless::Look;
use praxion_planner::{aim, wrap_degrees};
use praxion_world::Vec3;

use crate::view::View;

/// How close (in degrees) to the head counts as looking at it. The head is a
/// small box; eight degrees is inside it at three blocks and outside it at
/// thirty.
pub const HEAD_TOLERANCE_DEG: f32 = 8.0;

/// The yaw offset applied on the second tick, in degrees. Enough to leave the
/// head's box at any distance inside 64 blocks.
pub const BREAK_YAW_DEG: f32 = 35.0;

/// The pitch offset applied on the second tick, in degrees, and it is
/// *upward*: Minecraft's pitch is positive downwards, so a negative offset
/// tilts the camera up and off the head. (The old comment said "downward",
/// which would have been a positive number.)
pub const BREAK_PITCH_DEG: f32 = -20.0;

/// Stateless by design: the previous look is the body's current look, so there
/// is no counter that can drift from reality.
#[derive(Clone, Copy, Debug, Default)]
pub struct EndermanGuard;

impl EndermanGuard {
    pub fn new() -> Self {
        Self
    }

    /// Apply the guard. `desired` is the frame's look, `current` the body's
    /// look before the frame.
    pub fn apply(&self, desired: Look, current: LookDirection, view: &View) -> Look {
        let (yaw, pitch) = absolute(desired, current);
        for enderman in view.endermen.iter().flatten() {
            let head = crate::enderman::head_of(enderman.position);
            let (head_yaw, head_pitch) = aim(view.eye, head);
            let on_head_now =
                points_at(current.y_rot(), current.x_rot(), head_yaw, head_pitch);
            let on_head_desired = points_at(yaw, pitch, head_yaw, head_pitch);
            if on_head_now && on_head_desired {
                let yaw = wrap_degrees(current.y_rot() + BREAK_YAW_DEG);
                let pitch = (current.x_rot() + BREAK_PITCH_DEG).clamp(-90.0, 90.0);
                return Look::Aim { yaw, pitch };
            }
        }
        desired
    }
}

fn absolute(desired: Look, current: LookDirection) -> (f32, f32) {
    match desired {
        Look::None => (current.y_rot(), current.x_rot()),
        Look::Aim { yaw, pitch } => (yaw, pitch),
        Look::Delta { dyaw, dpitch } => (current.y_rot() + dyaw, current.x_rot() + dpitch),
    }
}

fn points_at(yaw: f32, pitch: f32, target_yaw: f32, target_pitch: f32) -> bool {
    wrap_degrees(target_yaw - yaw).abs() <= HEAD_TOLERANCE_DEG
        && (target_pitch - pitch).abs() <= HEAD_TOLERANCE_DEG
}

/// The point the guard protects, exposed for tests and logs.
pub fn head_of(feet: Vec3) -> Vec3 {
    feet + Vec3::new(0.0, 1.5, 0.0)
}
