//! The movement packets: what the last tick sent, and which packet this tick
//! needs.
//!
//! The positions and the look come from [`crate::physics::Body`]; this module
//! only keeps the bookkeeping that decides *whether* a packet is sent. The
//! choice is azalea's `send_position` (`azalea-client/src/plugins/movement.rs:
//! 89-197`): pos+rot, pos, rot, status only, or nothing, plus vanilla's
//! `positionReminder` (a position packet at least every 20 ticks).

use azalea_core::position::Vec3;
use azalea_entity::LookDirection;
use azalea_protocol::common::movements::MoveFlags;
use azalea_protocol::packets::Packet;
use azalea_protocol::packets::game::{
    ServerboundGamePacket, s_move_player_pos::ServerboundMovePlayerPos,
    s_move_player_pos_rot::ServerboundMovePlayerPosRot, s_move_player_rot::ServerboundMovePlayerRot,
    s_move_player_status_only::ServerboundMovePlayerStatusOnly,
};

/// Vanilla's `positionReminder`: send a position packet at least every 20 ticks.
pub(crate) const POSITION_REMINDER_TICKS: u32 = 20;
/// Squared position delta that counts as movement (vanilla's `Mth.square(2.0E-4)`).
const POSITION_EPSILON_SQUARED: f64 = 2.0e-4 * 2.0e-4;

/// What the server has already been told about our position and look.
#[derive(Clone, Debug)]
pub(crate) struct Movement {
    /// Ticks since the last position packet.
    position_remainder: u32,
    last_sent_position: Vec3,
    last_sent_look: LookDirection,
    last_on_ground: bool,
}

impl Default for Movement {
    fn default() -> Self {
        Self {
            position_remainder: 0,
            last_sent_position: Vec3::ZERO,
            last_sent_look: LookDirection::default(),
            last_on_ground: false,
        }
    }
}

impl Movement {
    /// A teleport arrived: the acknowledge packet carries this position and look,
    /// so neither is resent for it.
    pub(crate) fn teleported(&mut self, position: Vec3, look: LookDirection) {
        self.last_sent_position = position;
        self.last_sent_look = look;
        self.position_remainder = 0;
    }

    /// The movement packet for this tick, if a vanilla client would send one.
    pub(crate) fn packet(
        &mut self,
        position: Vec3,
        look: LookDirection,
        on_ground: bool,
        horizontal_collision: bool,
    ) -> Option<ServerboundGamePacket> {
        self.position_remainder += 1;

        let x_delta = position.x - self.last_sent_position.x;
        let y_delta = position.y - self.last_sent_position.y;
        let z_delta = position.z - self.last_sent_position.z;
        let sending_position =
            x_delta * x_delta + y_delta * y_delta + z_delta * z_delta > POSITION_EPSILON_SQUARED
                || self.position_remainder >= POSITION_REMINDER_TICKS;
        let sending_direction = look.y_rot() != self.last_sent_look.y_rot()
            || look.x_rot() != self.last_sent_look.x_rot();

        let flags = MoveFlags {
            on_ground,
            horizontal_collision,
        };
        let packet = if sending_position && sending_direction {
            Some(
                ServerboundMovePlayerPosRot {
                    pos: position,
                    look_direction: look,
                    flags,
                }
                .into_variant(),
            )
        } else if sending_position {
            Some(
                ServerboundMovePlayerPos {
                    pos: position,
                    flags,
                }
                .into_variant(),
            )
        } else if sending_direction {
            Some(
                ServerboundMovePlayerRot {
                    look_direction: look,
                    flags,
                }
                .into_variant(),
            )
        } else if self.last_on_ground != on_ground {
            Some(ServerboundMovePlayerStatusOnly { flags }.into_variant())
        } else {
            None
        };

        if sending_position {
            self.last_sent_position = position;
            self.position_remainder = 0;
        }
        if sending_direction {
            self.last_sent_look = look;
        }
        self.last_on_ground = on_ground;

        packet
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A movement that already knows we stand on the ground, with the reminder
    /// counter at zero.
    fn primed() -> Movement {
        let mut movement = Movement::default();
        movement.teleported(Vec3::ZERO, LookDirection::default());
        movement.packet(Vec3::ZERO, LookDirection::default(), true, false);
        movement.teleported(Vec3::ZERO, LookDirection::default());
        movement
    }

    #[test]
    fn a_still_body_sends_a_position_every_twenty_ticks() {
        let mut movement = primed();
        for tick in 1..POSITION_REMINDER_TICKS {
            assert!(
                movement
                    .packet(Vec3::ZERO, LookDirection::default(), true, false)
                    .is_none(),
                "tick {tick} sent a packet"
            );
        }
        assert!(matches!(
            movement.packet(Vec3::ZERO, LookDirection::default(), true, false),
            Some(ServerboundGamePacket::MovePlayerPos(_))
        ));
        for tick in 1..POSITION_REMINDER_TICKS {
            assert!(
                movement
                    .packet(Vec3::ZERO, LookDirection::default(), true, false)
                    .is_none(),
                "tick {tick} after the update sent a packet"
            );
        }
        assert!(matches!(
            movement.packet(Vec3::ZERO, LookDirection::default(), true, false),
            Some(ServerboundGamePacket::MovePlayerPos(_))
        ));
    }

    #[test]
    fn a_teleport_is_not_resent_at_once() {
        let mut movement = Movement::default();
        let position = Vec3::new(10.5, 64.0, -3.5);
        movement.teleported(position, LookDirection::default());

        assert!(
            movement
                .packet(position, LookDirection::default(), false, false)
                .is_none(),
            "the acknowledge packet already carried this position"
        );
    }

    #[test]
    fn a_small_move_is_skipped_but_a_large_one_is_sent() {
        let mut movement = primed();

        // 1e-3 is below vanilla's 2e-4 per axis, so nothing is sent.
        assert!(
            movement
                .packet(Vec3::new(0.0001, 0.0, 0.0), LookDirection::default(), true, false)
                .is_none()
        );
        assert!(matches!(
            movement.packet(Vec3::new(0.1, 0.0, 0.0), LookDirection::default(), true, false),
            Some(ServerboundGamePacket::MovePlayerPos(_))
        ));
    }

    #[test]
    fn a_rotation_change_is_sent_alone() {
        let mut movement = Movement::default();
        let position = Vec3::new(0.0, 64.0, 0.0);
        movement.teleported(position, LookDirection::default());

        assert!(matches!(
            movement.packet(position, LookDirection::new(45.0, -10.0), false, false),
            Some(ServerboundGamePacket::MovePlayerRot(p))
                if p.look_direction.y_rot() == 45.0 && p.look_direction.x_rot() == -10.0
        ));
        assert!(
            movement
                .packet(position, LookDirection::new(45.0, -10.0), false, false)
                .is_none(),
            "the rotation is already sent"
        );
    }

    #[test]
    fn position_and_rotation_together_take_the_posrot_packet() {
        let mut movement = Movement::default();
        movement.teleported(Vec3::ZERO, LookDirection::default());

        assert!(matches!(
            movement.packet(Vec3::new(1.0, 0.0, 0.0), LookDirection::new(90.0, 0.0), true, true),
            Some(ServerboundGamePacket::MovePlayerPosRot(p))
                if p.flags.on_ground && p.flags.horizontal_collision
        ));
    }

    #[test]
    fn a_ground_change_alone_is_the_status_packet() {
        let mut movement = Movement::default();
        movement.teleported(Vec3::ZERO, LookDirection::default());

        // The first tick learns we are on the ground and says so.
        assert!(matches!(
            movement.packet(Vec3::ZERO, LookDirection::default(), true, false),
            Some(ServerboundGamePacket::MovePlayerStatusOnly(_))
        ));
        assert!(
            movement
                .packet(Vec3::ZERO, LookDirection::default(), true, false)
                .is_none()
        );
    }
}
