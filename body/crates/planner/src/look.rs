//! The stand-in for the movement-style layer (§5.2): the aim speed limit.
//!
//! Movement style is the only producer of look output in the real body. The
//! planner/follower only requests a target; this module turns it into a
//! per-tick rotation bounded by a human-like speed, and reports whether the
//! aim tolerance is met yet. A path segment must not start before it is.

use azalea_entity::LookDirection;
use praxion_headless::Look;
use praxion_world::Vec3;

/// The limits of the stand-in style. The real numbers come from session 13.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LookStyle {
    /// Maximum yaw change per tick, in degrees.
    pub max_yaw_deg: f32,
    /// Maximum pitch change per tick, in degrees.
    pub max_pitch_deg: f32,
    /// Both angles must be within this many degrees before a segment starts.
    pub tolerance_deg: f32,
}

impl Default for LookStyle {
    fn default() -> Self {
        // About 600 deg/s of yaw and 400 deg/s of pitch at 20 ticks/s: fast
        // enough to be responsive, slow enough to look human. Tuned in
        // session 13; a small dead zone avoids jitter.
        Self {
            max_yaw_deg: 30.,
            max_pitch_deg: 20.,
            tolerance_deg: 4.,
        }
    }
}

/// The yaw and pitch (degrees) that look from `eye` at `target`.
pub fn aim(eye: Vec3, target: Vec3) -> (f32, f32) {
    let dx = target.x - eye.x;
    let dy = target.y - eye.y;
    let dz = target.z - eye.z;
    let horizontal = (dx * dx + dz * dz).sqrt();
    // Azalea's `input_vector`: forward at yaw 0 is +z, and +x is yaw -90.
    let yaw = f64::atan2(-dx, dz).to_degrees() as f32;
    let pitch = f64::atan2(-dy, horizontal).to_degrees() as f32;
    (yaw, pitch)
}

/// The shortest signed difference `to - from`, in degrees.
pub fn wrap_degrees(degrees: f32) -> f32 {
    let mut value = degrees % 360.;
    if value > 180. {
        value -= 360.;
    } else if value < -180. {
        value += 360.;
    }
    value
}

/// One tick of aiming: the bounded look change and whether the aim is now
/// within the tolerance.
///
/// The output is an absolute `Aim` at the next intermediate angle, so it is
/// both "aim toward the next node" and "no faster than the style allows".
pub fn look_towards(
    style: LookStyle,
    current: LookDirection,
    eye: Vec3,
    target: Vec3,
) -> (Look, bool) {
    let (yaw, pitch) = aim(eye, target);
    let dyaw = wrap_degrees(yaw - current.y_rot());
    let dpitch = pitch - current.x_rot();
    let aligned = dyaw.abs() <= style.tolerance_deg && dpitch.abs() <= style.tolerance_deg;
    if aligned {
        return (Look::None, true);
    }
    let step_yaw = dyaw.clamp(-style.max_yaw_deg, style.max_yaw_deg);
    let step_pitch = dpitch.clamp(-style.max_pitch_deg, style.max_pitch_deg);
    (
        Look::Aim {
            yaw: current.y_rot() + step_yaw,
            pitch: current.x_rot() + step_pitch,
        },
        false,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aiming_uses_minecraft_yaw_conventions() {
        let eye = Vec3::new(0.5, 65.62, 0.5);
        let (yaw, pitch) = aim(eye, Vec3::new(0.5, 65.62, 3.5));
        assert!(yaw.abs() < 1e-3, "south is yaw 0, got {yaw}");
        assert!(pitch.abs() < 1e-3);

        let (yaw, _) = aim(eye, Vec3::new(3.5, 65.62, 0.5));
        assert!((yaw + 90.).abs() < 1e-3, "east is yaw -90, got {yaw}");

        let (yaw, _) = aim(eye, Vec3::new(0.5, 65.62, -3.5));
        assert!((yaw.abs() - 180.).abs() < 1e-3, "north is yaw 180, got {yaw}");
    }

    #[test]
    fn a_large_turn_is_spread_over_ticks() {
        let style = LookStyle {
            max_yaw_deg: 30.,
            max_pitch_deg: 20.,
            tolerance_deg: 3.,
        };
        let current = LookDirection::new(0., 0.);
        let eye = Vec3::new(0., 64., 0.);
        let target = Vec3::new(-10., 64., 0.);
        let (look, aligned) = look_towards(style, current, eye, target);
        assert!(!aligned);
        match look {
            Look::Aim { yaw, pitch } => {
                assert!((yaw - 30.).abs() < 1e-3);
                assert_eq!(pitch, 0.);
            }
            other => panic!("expected an aim, got {other:?}"),
        }
    }

    #[test]
    fn the_tolerance_stops_the_turn() {
        let style = LookStyle {
            max_yaw_deg: 30.,
            max_pitch_deg: 20.,
            tolerance_deg: 4.,
        };
        let current = LookDirection::new(2., 1.);
        let eye = Vec3::new(0., 64., 0.);
        let target = Vec3::new(0., 64., 5.);
        let (look, aligned) = look_towards(style, current, eye, target);
        assert!(aligned);
        assert_eq!(look, Look::None);
    }

    #[test]
    fn yaw_wraps_the_short_way() {
        assert_eq!(wrap_degrees(350.), -10.);
        assert_eq!(wrap_degrees(-350.), 10.);
        assert_eq!(wrap_degrees(10.), 10.);
    }
}
