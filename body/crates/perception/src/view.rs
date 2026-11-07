//! The view scan: a sparse fan of rays inside the body's current look, written
//! to the known map. One scan casts half the fan; two scans cover the view.

use praxion_world::{BlockState, Vec3, World};

use crate::{Caller, Perception, sight};

/// Half-width of the view fan, in degrees.
const VIEW_YAW_HALF_DEGREES: f64 = 40.0;
/// Pitch offsets of the fan, in degrees: negative looks up, positive down.
const VIEW_PITCH_MIN_DEGREES: f64 = -30.0;
const VIEW_PITCH_MAX_DEGREES: f64 = 20.0;
/// Spacing between adjacent fan rays, in degrees.
const VIEW_STEP_DEGREES: f64 = 2.5;

impl Perception {
    /// Cast one half of the view fan and record each ray's first blocking cell
    /// in the known map. Rays come from `Caller::Other`'s share, never the
    /// reflex reserve; a short share makes the scan do less, never more.
    pub(crate) fn scan_view(&mut self, world: &World) {
        let Some(eye) = self.valid_eye(world) else {
            return;
        };
        let (yaw, pitch) = world.player_rotation();
        let range = f64::from(self.config.range);
        let phase = usize::from(self.view_scan_phase);
        self.view_scan_phase = !self.view_scan_phase;
        let yaw_rows = ((VIEW_YAW_HALF_DEGREES * 2.0) / VIEW_STEP_DEGREES) as usize + 1;
        let pitch_rows =
            ((VIEW_PITCH_MAX_DEGREES - VIEW_PITCH_MIN_DEGREES) / VIEW_STEP_DEGREES) as usize + 1;
        let eye_cell = eye.to_block_pos_floor();
        let (mut cast, mut hits, mut farthest) = (0u32, 0u32, 0f64);
        'rows: for row in (phase..yaw_rows).step_by(2) {
            let yaw_offset = -VIEW_YAW_HALF_DEGREES + row as f64 * VIEW_STEP_DEGREES;
            for col in 0..pitch_rows {
                if self.remaining_rays(Caller::Other) == 0 {
                    break 'rows;
                }
                let pitch_offset = VIEW_PITCH_MIN_DEGREES + col as f64 * VIEW_STEP_DEGREES;
                let direction = look_direction(
                    f64::from(yaw) + yaw_offset,
                    (f64::from(pitch) + pitch_offset).clamp(-90.0, 90.0),
                );
                self.spend_ray(Caller::Other);
                cast += 1;
                let Some(hit) = sight::first_blocking_cell(
                    world,
                    eye,
                    direction * range,
                    None,
                    &mut self.table,
                    None,
                )
                .filter(|hit| *hit != eye_cell) else {
                    continue;
                };
                let Some(state) = world.block_state(hit) else {
                    continue;
                };
                if state == BlockState::AIR {
                    continue;
                }
                hits += 1;
                farthest = farthest.max(eye.distance_to(hit.center()));
                self.record_seen(world, hit, state, eye);
            }
        }
        log::debug!("view scan yaw={yaw:.0} pitch={pitch:.0} rays={cast} hits={hits} farthest={farthest:.1}");
    }
}

/// The unit direction of a look in Minecraft's yaw/pitch convention: yaw 0
/// faces +Z and 90 faces -X; positive pitch looks down.
fn look_direction(yaw: f64, pitch: f64) -> Vec3 {
    let yaw = yaw.to_radians();
    let pitch = pitch.to_radians();
    let horizontal = pitch.cos();
    Vec3::new(
        -yaw.sin() * horizontal,
        -pitch.sin(),
        yaw.cos() * horizontal,
    )
}
