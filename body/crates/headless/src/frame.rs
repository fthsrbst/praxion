//! The `ControlFrame`: everything a decision layer may tell the actuator for one
//! tick (architecture §5.2).
//!
//! ```text
//! ControlFrame { tick, generation, held, look, clicks, menu }
//! ```
//!
//! A frame carries *intent*, not a packet: the actuator applies it to the body,
//! steps physics once and sends what the server would have seen from a vanilla
//! client. The frame is validated before anything is applied, so a frame built
//! for an older body state can never be half-applied:
//!
//! - a frame whose `generation` is not the body's current one is **stale** and is
//!   dropped (a server teleport, a respawn or a chunk reload moved the body under
//!   the frame's feet);
//! - a frame whose `tick` is not the tick being simulated is dropped (late or
//!   early);
//! - a frame with `menu` actions is refused: this step has no container support,
//!   so the whole frame is dropped rather than applying its movement without its
//!   clicks.
//!
//! Clicks are accepted and counted but not executed yet: this step has no
//! inventory, no block interaction and no attack packet.

use azalea_entity::LookDirection;

use crate::physics::Body;

/// The controls a frame holds down for its tick.
///
/// This is the key state a keyboard and mouse would give a vanilla client; the
/// actuator turns it into `ServerboundPlayerInput` and the physics input vector.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Held {
    pub forward: bool,
    /// `back` in the frame, `backward` in the packet.
    pub back: bool,
    pub left: bool,
    pub right: bool,
    pub jump: bool,
    /// The sneak key. Vanilla picks the crouching pose from it.
    pub sneak: bool,
    /// The sprint key. Sprinting itself still needs enough forward impulse and no
    /// obstacle, exactly like vanilla.
    pub sprint: bool,
}

impl Held {
    /// Every control up. The body keeps this state while no frame moves.
    pub const NONE: Self = Self {
        forward: false,
        back: false,
        left: false,
        right: false,
        jump: false,
        sneak: false,
        sprint: false,
    };

    /// Whether any control that makes the body move is held.
    pub fn moves(&self) -> bool {
        self.forward || self.back || self.left || self.right || self.jump
    }

    /// The control booleans as the serverbound input packet carries them.
    pub fn packet(&self) -> azalea_protocol::packets::game::ServerboundPlayerInput {
        azalea_protocol::packets::game::ServerboundPlayerInput {
            forward: self.forward,
            backward: self.back,
            left: self.left,
            right: self.right,
            jump: self.jump,
            shift: self.sneak,
            sprint: self.sprint,
        }
    }
}

/// How the frame changes where the body looks.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Look {
    /// Keep looking where we look.
    #[default]
    None,
    /// Add to the current yaw and pitch, in degrees.
    Delta { dyaw: f32, dpitch: f32 },
    /// Look at an absolute yaw and pitch, in degrees.
    Aim { yaw: f32, pitch: f32 },
}

/// A click the frame asks for. Accepted and counted; not executed in this step.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Click {
    Attack,
    Use,
}

/// A container action. The session executes it; [`ControlFrame::apply`] alone
/// still refuses menu frames so a caller that has no inventory cannot half-apply
/// one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MenuAction {
    pub window: u32,
    pub slot: i16,
    pub button: i8,
}

/// How the frame's [`MenuAction`] is performed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MenuMode {
    /// Pick the slot up (button 0) or place one item (button 1).
    #[default]
    Click,
    /// Shift-click: quick-move the stack.
    ShiftClick,
    /// Swap the slot with a hotbar slot; `button` is the hotbar index 0 to 8.
    Swap,
    /// Drop one item (button 0) or the whole stack (button 1).
    Throw,
    /// Clone the slot (creative only).
    Clone,
    /// Close the window; the slot and button are ignored.
    Close,
}

/// What a decision layer asks the actuator to do for one tick.
#[derive(Clone, Debug, PartialEq)]
pub struct ControlFrame {
    /// The body tick this frame is for; must be the tick being simulated.
    pub tick: u64,
    /// The body state this frame was built against.
    pub generation: u32,
    pub held: Held,
    pub look: Look,
    /// Clicks this tick. Held across ticks, like a physical mouse button.
    pub clicks: Vec<Click>,
    /// The hotbar slot to select, if it should change.
    pub select_slot: Option<u8>,
    /// A container action. `None` means no menu action this tick.
    pub menu: Option<MenuAction>,
    /// How `menu` is performed.
    pub menu_mode: MenuMode,
}

impl ControlFrame {
    /// A frame that only holds `held` and changes nothing else.
    pub fn new(tick: u64, generation: u32, held: Held) -> Self {
        Self {
            tick,
            generation,
            held,
            look: Look::None,
            clicks: Vec::new(),
            select_slot: None,
            menu: None,
            menu_mode: MenuMode::Click,
        }
    }
}

/// Why a frame was not applied.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The body moved to a new generation after the frame was built.
    Stale { frame: u32, current: u32 },
    /// The frame is for a tick that is not the one being simulated.
    WrongTick { frame: u64, current: u64 },
    /// The frame carries container actions and this step has no menus.
    MenuUnsupported,
    /// A look field is NaN or infinite. The frame is refused before the body is
    /// touched; a NaN yaw would otherwise poison every later movement.
    InvalidNumber { field: &'static str },
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Stale { frame, current } => {
                write!(f, "stale frame generation {frame} (body is at {current})")
            }
            FrameError::WrongTick { frame, current } => {
                write!(f, "frame tick {frame}, simulating tick {current}")
            }
            FrameError::MenuUnsupported => write!(f, "menu actions are not supported yet"),
            FrameError::InvalidNumber { field } => {
                write!(f, "frame look field `{field}` is not finite")
            }
        }
    }
}

/// What applying a frame did, for the caller's log.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Applied {
    pub clicks: usize,
}

impl ControlFrame {
    /// Validate, then apply the frame's look and held state to `body`.
    ///
    /// Nothing is applied when validation fails, so the caller can keep the
    /// previous held state or fall back to a release frame. A frame with a menu
    /// action is refused: use [`ControlFrame::apply_with_menu`] in a session
    /// that owns an inventory.
    pub fn apply(&self, body: &mut Body, current_tick: u64) -> Result<Applied, FrameError> {
        self.apply_inner(body, current_tick, false)
    }

    /// Like [`ControlFrame::apply`], but accepts a menu action. The caller must
    /// execute the menu action after this returns.
    pub fn apply_with_menu(
        &self,
        body: &mut Body,
        current_tick: u64,
    ) -> Result<Applied, FrameError> {
        self.apply_inner(body, current_tick, true)
    }

    fn apply_inner(
        &self,
        body: &mut Body,
        current_tick: u64,
        menus_supported: bool,
    ) -> Result<Applied, FrameError> {
        if self.generation != body.generation() {
            return Err(FrameError::Stale {
                frame: self.generation,
                current: body.generation(),
            });
        }
        if self.tick != current_tick {
            return Err(FrameError::WrongTick {
                frame: self.tick,
                current: current_tick,
            });
        }
        if self.menu.is_some() && !menus_supported {
            return Err(FrameError::MenuUnsupported);
        }

        // Refuse non-finite look values before touching the body (review M1).
        match self.look {
            Look::None => {}
            Look::Delta { dyaw, dpitch } => {
                if !dyaw.is_finite() {
                    return Err(FrameError::InvalidNumber { field: "dyaw" });
                }
                if !dpitch.is_finite() {
                    return Err(FrameError::InvalidNumber { field: "dpitch" });
                }
            }
            Look::Aim { yaw, pitch } => {
                if !yaw.is_finite() {
                    return Err(FrameError::InvalidNumber { field: "yaw" });
                }
                if !pitch.is_finite() {
                    return Err(FrameError::InvalidNumber { field: "pitch" });
                }
            }
        }

        match self.look {
            Look::None => {}
            Look::Delta { dyaw, dpitch } => {
                let look = body.look();
                body.set_look(LookDirection::new(
                    look.y_rot() + dyaw,
                    (look.x_rot() + dpitch).clamp(-90., 90.),
                ));
            }
            Look::Aim { yaw, pitch } => {
                body.set_look(LookDirection::new(yaw, pitch.clamp(-90., 90.)));
            }
        }

        Ok(Applied {
            clicks: self.clicks.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use azalea_core::position::Vec3;

    use super::*;

    fn body() -> Body {
        Body::new(Vec3::new(0.5, 64.0, 0.5))
    }

    #[test]
    fn a_frame_from_an_older_generation_is_dropped() {
        let mut body = body();
        body.bump_generation();
        let frame = ControlFrame::new(0, 0, Held::NONE);
        assert_eq!(
            frame.apply(&mut body, 0),
            Err(FrameError::Stale { frame: 0, current: 1 })
        );
    }

    #[test]
    fn a_frame_for_another_tick_is_dropped() {
        let mut body = body();
        let frame = ControlFrame::new(7, body.generation(), Held::NONE);
        assert_eq!(
            frame.apply(&mut body, 8),
            Err(FrameError::WrongTick { frame: 7, current: 8 })
        );
    }

    #[test]
    fn a_menu_frame_is_refused_as_a_whole() {
        let mut body = body();
        let mut frame = ControlFrame::new(0, body.generation(), Held { forward: true, ..Held::NONE });
        frame.menu = Some(MenuAction { window: 0, slot: 1, button: 0 });
        assert_eq!(frame.apply(&mut body, 0), Err(FrameError::MenuUnsupported));
    }

    #[test]
    fn an_aim_sets_the_look_and_a_delta_adds_to_it() {
        let mut body = body();
        let mut frame = ControlFrame::new(0, body.generation(), Held::NONE);
        frame.look = Look::Aim { yaw: 90.0, pitch: 10.0 };
        frame.apply(&mut body, 0).unwrap();
        assert_eq!(body.look().y_rot(), 90.0);
        assert_eq!(body.look().x_rot(), 10.0);

        frame.tick = 1;
        frame.look = Look::Delta { dyaw: 15.0, dpitch: -20.0 };
        frame.apply(&mut body, 1).unwrap();
        assert_eq!(body.look().y_rot(), 105.0);
        assert_eq!(body.look().x_rot(), -10.0);

        // Pitch is clamped to the vanilla range.
        frame.tick = 2;
        frame.look = Look::Delta { dyaw: 0.0, dpitch: -200.0 };
        frame.apply(&mut body, 2).unwrap();
        assert_eq!(body.look().x_rot(), -90.0);
    }

    #[test]
    fn clicks_are_counted() {
        let mut body = body();
        let mut frame = ControlFrame::new(0, body.generation(), Held::NONE);
        frame.clicks = vec![Click::Attack, Click::Use];
        assert_eq!(frame.apply(&mut body, 0).unwrap().clicks, 2);
    }
}
