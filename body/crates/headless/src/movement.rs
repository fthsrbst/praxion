//! The only world state the session keeps: where the server thinks we are.
//!
//! Step 5a has no physics, so the body never moves on its own. This module keeps
//! the last teleport target, the last position and look that were sent, and
//! vanilla's `positionReminder` (a position update at least every 20 ticks).
//! The per-tick packet choice copies `azalea-client`'s `send_position`
//! (`azalea-client/src/plugins/movement.rs:89-197`): pos+rot, pos, rot, status
//! only, or nothing.

use azalea_core::position::{ChunkPos, Vec3};
use azalea_entity::LookDirection;
use azalea_protocol::common::movements::{MoveFlags, PositionMoveRotation, RelativeMovements};
use azalea_protocol::packets::Packet;
use azalea_protocol::packets::game::{
    ServerboundGamePacket, s_move_player_pos::ServerboundMovePlayerPos,
    s_move_player_pos_rot::ServerboundMovePlayerPosRot, s_move_player_rot::ServerboundMovePlayerRot,
    s_move_player_status_only::ServerboundMovePlayerStatusOnly,
};

/// Vanilla's `positionReminder`: send a position packet at least every 20 ticks.
const POSITION_REMINDER_TICKS: u32 = 20;
/// Squared position delta that counts as movement (vanilla's `Mth.square(2.0E-4)`).
const POSITION_EPSILON_SQUARED: f64 = 2.0e-4 * 2.0e-4;

/// Position, look and the bookkeeping for the position packets.
///
/// Nothing here simulates movement. `velocity` in the teleport packet and the
/// `relative` velocity flags are ignored because step 5a has no physics; when
/// physics arrives it will own the position and this struct will only read it.
#[derive(Clone, Debug)]
pub(crate) struct Movement {
    position: Vec3,
    look: LookDirection,
    /// Ticks since the last position packet.
    position_remainder: u32,
    last_sent_position: Vec3,
    last_sent_look: LookDirection,
    on_ground: bool,
    last_on_ground: bool,
}

impl Default for Movement {
    fn default() -> Self {
        Self {
            position: Vec3::ZERO,
            look: LookDirection::default(),
            position_remainder: 0,
            last_sent_position: Vec3::ZERO,
            last_sent_look: LookDirection::default(),
            on_ground: false,
            last_on_ground: false,
        }
    }
}

impl Movement {
    /// Where we are, as far as the server told us.
    pub(crate) fn position(&self) -> Vec3 {
        self.position
    }

    /// Where we look.
    pub(crate) fn look(&self) -> LookDirection {
        self.look
    }

    /// The chunk we are in, used to spot our own chunk in a chunk batch.
    pub(crate) fn chunk_pos(&self) -> ChunkPos {
        ChunkPos::from(&self.position)
    }

    /// Apply a `player_position` (teleport) packet.
    ///
    /// Relative flags mean "add this to the current value"; absolute ones mean
    /// "this is the new value" (vanilla `RelativeMovements::apply`). The
    /// accepted position also becomes the last sent one, because the
    /// acknowledge packet carries it: the server already knows it.
    pub(crate) fn teleport(
        &mut self,
        change: &PositionMoveRotation,
        relative: &RelativeMovements,
    ) {
        self.position = Vec3::new(
            apply_change(self.position.x, relative.x, change.pos.x),
            apply_change(self.position.y, relative.y, change.pos.y),
            apply_change(self.position.z, relative.z, change.pos.z),
        );
        self.look = LookDirection::new(
            apply_change(self.look.y_rot(), relative.y_rot, change.look_direction.y_rot()),
            apply_change(self.look.x_rot(), relative.x_rot, change.look_direction.x_rot()),
        );
        self.last_sent_position = self.position;
        self.last_sent_look = self.look;
        self.position_remainder = 0;
    }

    /// Apply a `player_rotation` packet (no reply is needed).
    pub(crate) fn rotate(&mut self, y_rot: f32, relative_y: bool, x_rot: f32, relative_x: bool) {
        self.look = LookDirection::new(
            apply_change(self.look.y_rot(), relative_y, y_rot),
            apply_change(self.look.x_rot(), relative_x, x_rot),
        );
    }

    /// The movement packet for this tick, if vanilla would send one.
    pub(crate) fn tick(&mut self) -> Option<ServerboundGamePacket> {
        self.position_remainder += 1;

        let x_delta = self.position.x - self.last_sent_position.x;
        let y_delta = self.position.y - self.last_sent_position.y;
        let z_delta = self.position.z - self.last_sent_position.z;
        let sending_position =
            x_delta * x_delta + y_delta * y_delta + z_delta * z_delta > POSITION_EPSILON_SQUARED
                || self.position_remainder >= POSITION_REMINDER_TICKS;
        let sending_direction = self.look.y_rot() != self.last_sent_look.y_rot()
            || self.look.x_rot() != self.last_sent_look.x_rot();

        let flags = MoveFlags {
            on_ground: self.on_ground,
            horizontal_collision: false,
        };
        let packet = if sending_position && sending_direction {
            Some(
                ServerboundMovePlayerPosRot {
                    pos: self.position,
                    look_direction: self.look,
                    flags,
                }
                .into_variant(),
            )
        } else if sending_position {
            Some(
                ServerboundMovePlayerPos {
                    pos: self.position,
                    flags,
                }
                .into_variant(),
            )
        } else if sending_direction {
            Some(
                ServerboundMovePlayerRot {
                    look_direction: self.look,
                    flags,
                }
                .into_variant(),
            )
        } else if self.last_on_ground != self.on_ground {
            Some(ServerboundMovePlayerStatusOnly { flags }.into_variant())
        } else {
            None
        };

        if sending_position {
            self.last_sent_position = self.position;
            self.position_remainder = 0;
        }
        if sending_direction {
            self.last_sent_look = self.look;
        }
        self.last_on_ground = self.on_ground;

        packet
    }
}

fn apply_change<T: std::ops::Add<Output = T>>(base: T, relative: bool, change: T) -> T {
    if relative { base + change } else { change }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn absolute(pos: (f64, f64, f64)) -> (PositionMoveRotation, RelativeMovements) {
        (
            PositionMoveRotation {
                pos: Vec3::new(pos.0, pos.1, pos.2),
                delta: Vec3::ZERO,
                look_direction: LookDirection::default(),
            },
            RelativeMovements::all_absolute(),
        )
    }

    #[test]
    fn a_still_body_sends_a_position_every_twenty_ticks() {
        let mut movement = Movement::default();
        for tick in 1..POSITION_REMINDER_TICKS {
            assert!(movement.tick().is_none(), "tick {tick} sent a packet");
        }
        assert!(matches!(
            movement.tick(),
            Some(ServerboundGamePacket::MovePlayerPos(_))
        ));
        for tick in 1..POSITION_REMINDER_TICKS {
            assert!(movement.tick().is_none(), "tick {tick} after the update sent a packet");
        }
        assert!(matches!(
            movement.tick(),
            Some(ServerboundGamePacket::MovePlayerPos(_))
        ));
    }

    #[test]
    fn an_absolute_teleport_moves_us_and_is_not_resent_at_once() {
        let mut movement = Movement::default();
        let (change, relative) = absolute((10.5, 64.0, -3.5));
        movement.teleport(&change, &relative);

        assert_eq!(movement.position(), Vec3::new(10.5, 64.0, -3.5));
        assert_eq!(movement.chunk_pos(), ChunkPos::new(0, -1));
        assert!(
            movement.tick().is_none(),
            "the acknowledge packet already carried this position"
        );
    }

    #[test]
    fn a_relative_teleport_adds_to_the_current_position() {
        let mut movement = Movement::default();
        let (change, relative) = absolute((10.0, 64.0, 3.0));
        movement.teleport(&change, &relative);

        let change = PositionMoveRotation {
            pos: Vec3::new(2.0, -1.0, 0.5),
            delta: Vec3::ZERO,
            look_direction: LookDirection::new(90.0, 0.0),
        };
        let mut relative = RelativeMovements::all_absolute();
        relative.x = true;
        relative.y = true;
        movement.teleport(&change, &relative);

        assert_eq!(movement.position(), Vec3::new(12.0, 63.0, 0.5));
        assert_eq!(movement.look().y_rot(), 90.0);
    }

    #[test]
    fn a_rotation_change_is_sent_alone() {
        let mut movement = Movement::default();
        let (change, relative) = absolute((0.0, 64.0, 0.0));
        movement.teleport(&change, &relative);
        movement.tick();

        movement.rotate(45.0, false, -10.0, false);
        assert!(matches!(
            movement.tick(),
            Some(ServerboundGamePacket::MovePlayerRot(p))
                if p.look_direction.y_rot() == 45.0 && p.look_direction.x_rot() == -10.0
        ));
        assert!(movement.tick().is_none(), "the rotation is already sent");
    }

    #[test]
    fn a_relative_rotation_adds_to_the_current_one() {
        let mut movement = Movement::default();
        let (change, relative) = absolute((0.0, 64.0, 0.0));
        movement.teleport(&change, &relative);
        movement.tick();

        movement.rotate(30.0, false, 0.0, false);
        movement.tick();
        movement.rotate(15.0, true, 5.0, true);

        assert!(matches!(
            movement.tick(),
            Some(ServerboundGamePacket::MovePlayerRot(p))
                if p.look_direction.y_rot() == 45.0 && p.look_direction.x_rot() == 5.0
        ));
    }
}
