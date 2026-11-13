//! The pure half of the hands: `ControlFrame` to `praxion-hands` lines, and the
//! `AppliedReport` read from the operator client's own packets.
//!
//! Nothing here touches the OS, a socket, a thread or the disk. The helper
//! (`hands/macos/praxion_hands.swift`) owns every input call; this module only
//! speaks its line protocol (step S3b of
//! `docs/proposals/2026-11-12-macos-real-client-body.md`):
//!
//! ```text
//! ControlFrame --Translator--> HandsLine --encode()--> "F tick gen keys dx dy buttons slot"
//! tee packets --observed_report--> AppliedReport (block_seq/hit/face/before, attack_entity)
//! ```
//!
//! Two facts drive the translation and are measured, not guessed:
//!
//! - one mouse pixel turns the camera `(0.6 s + 0.2)^3 * 1.2` degrees, where `s`
//!   is the client's `mouseSensitivity` (0.15 degrees at s = 0.5, exact from 50
//!   to 800 pixels in both directions);
//! - positive `dx` raises yaw and positive `dy` raises pitch, so a wanted
//!   degrees-per-tick change becomes `degrees / gain` pixels with no sign flip.
//!
//! The look is a closed loop: a posted delta shows up on the tee a tick or two
//! later, so [`Translator`] keeps the degrees it has sent in the last
//! `look_latency_ticks` ticks and subtracts them again from an absolute aim.

use std::collections::VecDeque;

use azalea_block::BlockState;
use azalea_core::position::BlockPos;
use azalea_protocol::packets::game::{ServerboundGamePacket, s_player_action};
use praxion_decoder::Packet;

use crate::frame::{Click, ControlFrame, Held, Look};
use crate::interact::AppliedReport;
use crate::worldfeed::ObservedPacket;

/// The client's `options.txt` settings the hands depend on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClientOptions {
    /// `mouseSensitivity`, 0 to 1 in the client.
    pub mouse_sensitivity: f64,
    /// `invertYMouse`; the hands only work with the default (false).
    pub invert_y_mouse: bool,
}

impl ClientOptions {
    /// Read the settings and check every key binding the hands press.
    ///
    /// The hands press W/A/S/D/Space/1-9 and the two mouse buttons by ANSI key
    /// code, so a client whose `options.txt` binds something else must be
    /// refused (naming the binding) rather than played with the wrong keys.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut values = std::collections::HashMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            values.insert(key.trim(), value.trim());
        }

        let sensitivity = values
            .get("mouseSensitivity")
            .ok_or("mouseSensitivity is missing from the client's options.txt")?;
        let mouse_sensitivity: f64 = sensitivity
            .parse()
            .map_err(|_| format!("mouseSensitivity wants a number, got {sensitivity:?}"))?;
        // Zero is a real client setting: the gain stays positive because of
        // the `+ 0.2` in the measured formula. A negative or non-finite value
        // could only come from a corrupt file.
        if !mouse_sensitivity.is_finite() || mouse_sensitivity < 0.0 {
            return Err(format!(
                "mouseSensitivity must be zero or positive, got {mouse_sensitivity}"
            ));
        }

        match values.get("invertYMouse") {
            None => return Err("invertYMouse is missing from the client's options.txt".to_owned()),
            Some(&"false") => {}
            Some(value) => {
                return Err(format!(
                    "invertYMouse is {value:?}; the hands need the default false"
                ));
            }
        }

        for (key, default) in REQUIRED_BINDINGS {
            match values.get(key) {
                None => {
                    return Err(format!(
                        "{key} is missing; the hands need the default {default}"
                    ));
                }
                Some(value) if *value == default => {}
                Some(value) => {
                    return Err(format!("{key} is {value:?}; the default is {default}"));
                }
            }
        }

        Ok(Self {
            mouse_sensitivity,
            invert_y_mouse: false,
        })
    }

    /// The measured degrees per mouse pixel: `(0.6 s + 0.2)^3 * 1.2`.
    pub fn gain_degrees_per_pixel(&self) -> f64 {
        (0.6 * self.mouse_sensitivity + 0.2).powi(3) * 1.2
    }
}

/// The bindings the hands press, with their vanilla defaults.
const REQUIRED_BINDINGS: [(&str, &str); 16] = [
    ("key_key.forward", "key.keyboard.w"),
    ("key_key.back", "key.keyboard.s"),
    ("key_key.left", "key.keyboard.a"),
    ("key_key.right", "key.keyboard.d"),
    ("key_key.jump", "key.keyboard.space"),
    ("key_key.attack", "key.mouse.left"),
    ("key_key.use", "key.mouse.right"),
    ("key_key.hotbar.1", "key.keyboard.1"),
    ("key_key.hotbar.2", "key.keyboard.2"),
    ("key_key.hotbar.3", "key.keyboard.3"),
    ("key_key.hotbar.4", "key.keyboard.4"),
    ("key_key.hotbar.5", "key.keyboard.5"),
    ("key_key.hotbar.6", "key.keyboard.6"),
    ("key_key.hotbar.7", "key.keyboard.7"),
    ("key_key.hotbar.8", "key.keyboard.8"),
    ("key_key.hotbar.9", "key.keyboard.9"),
];

/// The most pixels one tick may ask for. The helper refuses more, and a jump
/// larger than this is a sign the closed loop lost its state.
const MAX_PIXELS_PER_TICK: f64 = 600.0;

/// One line for the helper's `F` protocol line.
///
/// `slot` is a digit 1-9 or `None` (`-`): a digit taps that hotbar key, so the
/// translator only names a slot when it changes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HandsLine {
    pub tick: u64,
    pub generation: u32,
    /// Forward 0, back 1, left 2, right 3, jump 4.
    pub keys: u8,
    /// Mouse pixels this tick, yaw (positive raises yaw).
    pub dx: i32,
    /// Mouse pixels this tick, pitch (positive raises pitch).
    pub dy: i32,
    /// Left (attack) 0, right (use) 1.
    pub buttons: u8,
    /// A hotbar key to tap this tick, 1-9.
    pub slot: Option<u8>,
    /// The frame asked for something the hands cannot do yet (sneak, sprint, a
    /// menu action or chat). The rest of the frame is still sent.
    pub unsupported: bool,
}

impl HandsLine {
    /// The protocol line, without the trailing newline.
    pub fn encode(&self) -> String {
        let slot = match self.slot {
            Some(digit) => digit.to_string(),
            None => "-".to_owned(),
        };
        format!(
            "F {} {} {} {} {} {} {}",
            self.tick, self.generation, self.keys, self.dx, self.dy, self.buttons, slot
        )
    }
}

/// Turns frames into helper lines, keeping the closed loop's state between
/// ticks.
#[derive(Clone, Debug)]
pub struct Translator {
    gain: f64,
    look_latency_ticks: usize,
    /// `(yaw, pitch)` degrees sent in each of the last ticks, oldest first.
    in_flight: VecDeque<(f64, f64)>,
    /// Degrees a tick could not send because they were a sub-pixel amount.
    carry_yaw: f64,
    carry_pitch: f64,
    /// The last hotbar digit sent, so a repeated request is not tapped again.
    last_slot: Option<u8>,
}

impl Translator {
    /// `gain_degrees_per_pixel` comes from [`ClientOptions`];
    /// `look_latency_ticks` is how many ticks a posted delta takes to show up in
    /// the client's own rotation packet.
    pub fn new(gain_degrees_per_pixel: f64, look_latency_ticks: usize) -> Self {
        assert!(
            gain_degrees_per_pixel.is_finite() && gain_degrees_per_pixel > 0.0,
            "the mouse gain must be a positive number of degrees per pixel"
        );
        Self {
            gain: gain_degrees_per_pixel,
            look_latency_ticks,
            in_flight: VecDeque::new(),
            carry_yaw: 0.0,
            carry_pitch: 0.0,
            last_slot: None,
        }
    }

    /// Translate one frame. `observed_yaw` and `observed_pitch` are the rotation
    /// the client last reported, in degrees; an aim is relative to them plus
    /// whatever is still in flight.
    pub fn line(
        &mut self,
        frame: &ControlFrame,
        observed_yaw: f32,
        observed_pitch: f32,
    ) -> HandsLine {
        let held = frame.held;
        let keys = held_keys(&held);

        let mut unsupported = false;
        // Shift and Control are modifier keys on macOS and the spike refuses
        // them; they get their own spike before S3 uses them.
        if held.sneak || held.sprint {
            unsupported = true;
        }
        // Menus are S4 and chat stays on the brain side.
        if frame.menu.is_some() || frame.chat.is_some() {
            unsupported = true;
        }

        let mut buttons = 0;
        if frame.clicks.contains(&Click::Attack) {
            buttons |= 1 << 0;
        }
        if frame.clicks.contains(&Click::Use) {
            buttons |= 1 << 1;
        }

        let slot = match frame.select_slot {
            Some(slot) if slot <= 8 => {
                let digit = slot + 1;
                if self.last_slot == Some(digit) {
                    None
                } else {
                    self.last_slot = Some(digit);
                    Some(digit)
                }
            }
            Some(_) => {
                // The wire only has nine slots; a tenth is a frame bug, not a
                // key the helper could press.
                unsupported = true;
                None
            }
            None => None,
        };

        let (dx, dy) = self.look(
            frame.look,
            f64::from(observed_yaw),
            f64::from(observed_pitch),
        );

        HandsLine {
            tick: frame.tick,
            generation: frame.generation,
            keys,
            dx,
            dy,
            buttons,
            slot,
            unsupported,
        }
    }

    /// Clear the closed loop after a takeover or a respawn: the camera is
    /// wherever the operator left it and nothing of ours is in flight.
    pub fn reset(&mut self) {
        self.in_flight.clear();
        self.carry_yaw = 0.0;
        self.carry_pitch = 0.0;
        self.last_slot = None;
    }

    /// The pixels for this tick's look, and the degrees that stay in flight.
    fn look(&mut self, look: Look, observed_yaw: f64, observed_pitch: f64) -> (i32, i32) {
        let (wanted_yaw, wanted_pitch) = match look {
            Look::None => (0.0, 0.0),
            Look::Delta { dyaw, dpitch } => (f64::from(dyaw), f64::from(dpitch)),
            Look::Aim { yaw, pitch } => {
                let (in_flight_yaw, in_flight_pitch) = self.in_flight_sum();
                // An aim is absolute: the error is against where the camera
                // will be once the posted deltas land.
                let yaw_error = wrap_degrees(f64::from(yaw) - (observed_yaw + in_flight_yaw));
                let pitch_target = f64::from(pitch).clamp(-90.0, 90.0);
                let pitch_error = pitch_target - (observed_pitch + in_flight_pitch);
                (yaw_error, pitch_error)
            }
        };

        let (dx, dy) = self.pixels(wanted_yaw, wanted_pitch);
        if self.look_latency_ticks > 0 {
            self.in_flight
                .push_back((f64::from(dx) * self.gain, f64::from(dy) * self.gain));
            while self.in_flight.len() > self.look_latency_ticks {
                self.in_flight.pop_front();
            }
        }
        (dx, dy)
    }

    /// Degrees to pixels: round toward zero, carry the sub-pixel rest, and cap
    /// each axis. A capped axis drops its rest instead of carrying it, because
    /// the next tick's error against the observed rotation covers the distance.
    fn pixels(&mut self, wanted_yaw: f64, wanted_pitch: f64) -> (i32, i32) {
        let yaw_degrees = wanted_yaw + self.carry_yaw;
        let pitch_degrees = wanted_pitch + self.carry_pitch;
        let yaw_pixels = (yaw_degrees / self.gain).trunc();
        let pitch_pixels = (pitch_degrees / self.gain).trunc();
        let dx = yaw_pixels.clamp(-MAX_PIXELS_PER_TICK, MAX_PIXELS_PER_TICK) as i32;
        let dy = pitch_pixels.clamp(-MAX_PIXELS_PER_TICK, MAX_PIXELS_PER_TICK) as i32;

        self.carry_yaw = if f64::from(dx) == yaw_pixels {
            yaw_degrees - f64::from(dx) * self.gain
        } else {
            0.0
        };
        self.carry_pitch = if f64::from(dy) == pitch_pixels {
            pitch_degrees - f64::from(dy) * self.gain
        } else {
            0.0
        };
        (dx, dy)
    }

    fn in_flight_sum(&self) -> (f64, f64) {
        self.in_flight
            .iter()
            .fold((0.0, 0.0), |(yaw, pitch), (yaw_tick, pitch_tick)| {
                (yaw + yaw_tick, pitch + pitch_tick)
            })
    }
}

/// Wrap an angle difference into -180..180, the shortest way round.
fn wrap_degrees(degrees: f64) -> f64 {
    let mut wrapped = degrees % 360.0;
    if wrapped >= 180.0 {
        wrapped -= 360.0;
    }
    if wrapped < -180.0 {
        wrapped += 360.0;
    }
    wrapped
}

/// The report a hands session owes the driver: what the operator's own client
/// sent this tick.
///
/// A vanilla client plans the packets; the body only reads them. `before`
/// supplies a block's state as of before the action (the shadow loop keeps the
/// last tick's states of the cells the crosshair and the packets named); the
/// caller falls back to the mirror when it has none.
///
/// Only the operator client's serverbound play packets can match: the tee tags
/// both directions of one client connection with one id, and in real-client
/// mode the loop follows that one connection. Clientbound packets and the other
/// states are ignored.
pub fn observed_report(
    packets: &[ObservedPacket],
    before: &dyn Fn(BlockPos) -> Option<BlockState>,
    clicks: usize,
) -> AppliedReport {
    let mut report = AppliedReport {
        clicks,
        ..AppliedReport::default()
    };
    for observed in packets {
        let Packet::PlayServerbound(packet) = &observed.packet else {
            continue;
        };
        match packet {
            ServerboundGamePacket::PlayerAction(action) => {
                if !is_destroy_action(action.action) {
                    continue;
                }
                report.block_seq = Some(action.seq);
                report.block_hit = Some(action.pos);
                report.block_face = Some(action.direction);
                report.block_before = Some((action.pos, before(action.pos)));
            }
            ServerboundGamePacket::UseItemOn(use_on) => {
                let hit = &use_on.block_hit;
                report.block_seq = Some(use_on.seq);
                report.block_hit = Some(hit.block_pos);
                report.block_face = Some(hit.direction);
                // The cell the click would place a block in, exactly as the
                // actuator's own report names it (`interact.rs`).
                let placed = hit.block_pos.offset_with_direction(hit.direction);
                report.block_before = Some((placed, before(placed)));
            }
            ServerboundGamePacket::Attack(attack) => {
                report.attack_entity = Some(attack.entity_id.0);
            }
            _ => {}
        }
    }
    report
}

/// The 26.3 `ServerboundPlayerActionPacket$Action` ordinals the report needs,
/// from `login::send_player_action`.
const ACTION_START: u8 = 0;
const ACTION_STOP: u8 = 3;

/// Whether a decoded `PlayerAction` starts or stops a destroy.
///
/// The pinned azalea enum predates `CHANGE_DESTROY_DIRECTION` (ordinal 1), so
/// its *discriminants* are still the wire ordinals although the names are one
/// lower from abort on: a real stop (3) decodes as `DropAllItems`, a real abort
/// (2) as `StopDestroyBlock`, a real change of direction (1) as
/// `AbortDestroyBlock`. Matching the raw discriminant is what makes the report
/// right. Ordinals 7 (swap) and 8 (stab) are unknown to the pinned enum and
/// decode as its first variant, `StartDestroyBlock`; they would read as a start.
fn is_destroy_action(action: s_player_action::Action) -> bool {
    matches!(action as u8, ACTION_START | ACTION_STOP)
}

/// The held controls a frame asks for, for tests and callers that want the bit
/// values without building a frame.
pub(crate) fn held_keys(held: &Held) -> u8 {
    let mut keys = 0;
    if held.forward {
        keys |= 1 << 0;
    }
    if held.back {
        keys |= 1 << 1;
    }
    if held.left {
        keys |= 1 << 2;
    }
    if held.right {
        keys |= 1 << 3;
    }
    if held.jump {
        keys |= 1 << 4;
    }
    keys
}

#[cfg(test)]
mod tests {
    use azalea_block::BlockState;
    use azalea_core::{direction::Direction, entity_id::MinecraftEntityId, position::BlockPos};
    use azalea_protocol::packets::game::{
        ServerboundAttack, ServerboundGamePacket, ServerboundPlayerAction, ServerboundUseItemOn,
        s_interact::InteractionHand, s_player_action, s_use_item_on::BlockHit,
    };
    use praxion_decoder::Packet;

    use super::*;

    fn options_text() -> String {
        let mut text = String::from("mouseSensitivity:0.5\ninvertYMouse:false\n");
        for (key, value) in REQUIRED_BINDINGS {
            text.push_str(&format!("{key}:{value}\n"));
        }
        text
    }

    fn frame(tick: u64) -> ControlFrame {
        ControlFrame::new(tick, 1, Held::NONE)
    }

    fn setup() -> Translator {
        // 0.5 degrees per pixel keeps the arithmetic exact.
        Translator::new(0.5, 2)
    }

    #[test]
    fn the_default_options_parse_and_give_the_measured_gain() {
        let options = ClientOptions::parse(&options_text()).expect("default options");
        assert_eq!(options.mouse_sensitivity, 0.5);
        assert!(!options.invert_y_mouse);
        assert!((options.gain_degrees_per_pixel() - 0.15).abs() < 1e-12);
    }

    #[test]
    fn a_non_default_binding_is_refused_by_name() {
        for (key, wrong) in [
            ("key_key.forward", "key.keyboard.up"),
            ("key_key.back", "key.keyboard.down"),
            ("key_key.left", "key.keyboard.left"),
            ("key_key.right", "key.keyboard.right"),
            ("key_key.jump", "key.keyboard.v"),
            ("key_key.attack", "key.mouse.middle"),
            ("key_key.use", "key.mouse.middle"),
            ("key_key.hotbar.3", "key.keyboard.f3"),
            ("key_key.hotbar.9", "key.keyboard.0"),
        ] {
            let text = options_text().replace(
                &format!("{key}:{}", REQUIRED_BINDINGS.iter().find(|(k, _)| *k == key).unwrap().1),
                &format!("{key}:{wrong}"),
            );
            let error = ClientOptions::parse(&text).expect_err(key);
            assert!(
                error.contains(key) && error.contains(wrong),
                "{key}: {error}"
            );
        }
    }

    #[test]
    fn a_missing_binding_and_a_missing_sensitivity_are_named() {
        let text = options_text().replace("key_key.jump:key.keyboard.space\n", "");
        let error = ClientOptions::parse(&text).expect_err("no jump binding");
        assert!(error.starts_with("key_key.jump"), "{error}");

        let text = options_text().replace("mouseSensitivity:0.5\n", "");
        let error = ClientOptions::parse(&text).expect_err("no sensitivity");
        assert!(error.starts_with("mouseSensitivity"), "{error}");
    }

    #[test]
    fn inverted_or_nonsense_mouse_settings_are_refused() {
        let text = options_text().replace("invertYMouse:false", "invertYMouse:true");
        let error = ClientOptions::parse(&text).expect_err("inverted mouse");
        assert!(error.contains("invertYMouse"), "{error}");

        let text = options_text().replace("mouseSensitivity:0.5", "mouseSensitivity:lots");
        assert!(ClientOptions::parse(&text).is_err());

        let text = options_text().replace("mouseSensitivity:0.5", "mouseSensitivity:-0.5");
        assert!(ClientOptions::parse(&text).is_err());

        // Zero is a legal client setting: the smallest gain, still positive.
        let text = options_text().replace("mouseSensitivity:0.5", "mouseSensitivity:0");
        let options = ClientOptions::parse(&text).expect("zero sensitivity is legal");
        assert!(options.gain_degrees_per_pixel() > 0.0);
    }

    #[test]
    fn held_controls_become_the_documented_key_bits() {
        assert_eq!(held_keys(&Held::NONE), 0);
        assert_eq!(held_keys(&Held { forward: true, ..Held::NONE }), 1);
        assert_eq!(held_keys(&Held { back: true, ..Held::NONE }), 2);
        assert_eq!(held_keys(&Held { left: true, ..Held::NONE }), 4);
        assert_eq!(held_keys(&Held { right: true, ..Held::NONE }), 8);
        assert_eq!(held_keys(&Held { jump: true, ..Held::NONE }), 16);
        let all = Held {
            forward: true,
            back: true,
            left: true,
            right: true,
            jump: true,
            sneak: false,
            sprint: false,
        };
        assert_eq!(held_keys(&all), 0b1_1111);
    }

    #[test]
    fn sneak_sprint_menu_and_chat_are_unsupported_and_not_sent() {
        let mut translator = setup();

        let mut sneak = frame(1);
        sneak.held = Held {
            forward: true,
            sneak: true,
            ..Held::NONE
        };
        let line = translator.line(&sneak, 0.0, 0.0);
        assert!(line.unsupported, "sneak is not in S3");
        assert_eq!(line.keys, 1, "the rest of the frame is still applied");

        let mut sprint = frame(2);
        sprint.held = Held {
            sprint: true,
            ..Held::NONE
        };
        assert!(translator.line(&sprint, 0.0, 0.0).unsupported);

        let mut menu = frame(3);
        menu.menu = Some(crate::frame::MenuAction {
            window: 0,
            slot: 1,
            button: 0,
        });
        assert!(translator.line(&menu, 0.0, 0.0).unsupported);

        let mut chat = frame(4);
        chat.chat = Some("hello".to_owned());
        assert!(translator.line(&chat, 0.0, 0.0).unsupported);

        let plain = frame(5);
        assert!(!translator.line(&plain, 0.0, 0.0).unsupported);
    }

    #[test]
    fn clicks_become_the_documented_button_bits_every_tick() {
        let mut translator = setup();
        let mut attack = frame(1);
        attack.clicks.push(Click::Attack);
        assert_eq!(translator.line(&attack, 0.0, 0.0).buttons, 1);
        attack.clicks.push(Click::Use);
        attack.tick = 2;
        assert_eq!(translator.line(&attack, 0.0, 0.0).buttons, 3);
        let none = frame(3);
        assert_eq!(translator.line(&none, 0.0, 0.0).buttons, 0);
    }

    #[test]
    fn a_slot_is_tapped_once_and_a_repeat_is_not_sent_again() {
        let mut translator = setup();
        let mut select = frame(1);
        select.select_slot = Some(2);
        assert_eq!(translator.line(&select, 0.0, 0.0).slot, Some(3));

        select.tick = 2;
        assert_eq!(
            translator.line(&select, 0.0, 0.0).slot,
            None,
            "the same slot is not tapped again"
        );

        select.tick = 3;
        select.select_slot = Some(8);
        assert_eq!(translator.line(&select, 0.0, 0.0).slot, Some(9));

        select.tick = 4;
        select.select_slot = None;
        assert_eq!(translator.line(&select, 0.0, 0.0).slot, None);

        // Going back to the first slot is a change again.
        select.tick = 5;
        select.select_slot = Some(0);
        assert_eq!(translator.line(&select, 0.0, 0.0).slot, Some(1));
    }

    #[test]
    fn a_frame_encodes_as_the_helper_line() {
        let line = HandsLine {
            tick: 42,
            generation: 3,
            keys: 5,
            dx: -12,
            dy: 7,
            buttons: 2,
            slot: Some(4),
            unsupported: false,
        };
        assert_eq!(line.encode(), "F 42 3 5 -12 7 2 4");
        let mut unslotted = line.clone();
        unslotted.slot = None;
        assert_eq!(unslotted.encode(), "F 42 3 5 -12 7 2 -");
    }

    #[test]
    fn a_delta_becomes_pixels_and_the_subpixel_rest_is_carried() {
        let mut translator = Translator::new(1.0, 2);
        let mut delta = frame(0);
        delta.look = Look::Delta {
            dyaw: 0.4,
            dpitch: 0.0,
        };
        // 0.4 degrees is under a pixel: nothing this tick, nothing the next,
        // and one pixel when the third tick's rest crosses a whole pixel.
        assert_eq!(translator.line(&delta, 0.0, 0.0).dx, 0);
        delta.tick = 1;
        assert_eq!(translator.line(&delta, 0.0, 0.0).dx, 0);
        delta.tick = 2;
        assert_eq!(translator.line(&delta, 0.0, 0.0).dx, 1);

        // The 0.2 degrees still pending join the next request.
        let mut big = frame(3);
        big.look = Look::Delta {
            dyaw: 1.0,
            dpitch: 0.0,
        };
        assert_eq!(translator.line(&big, 0.0, 0.0).dx, 1);
    }

    #[test]
    fn both_axes_carry_their_own_rest() {
        let mut translator = Translator::new(1.0, 2);
        let mut delta = frame(0);
        delta.look = Look::Delta {
            dyaw: 0.4,
            dpitch: -0.4,
        };
        assert_eq!(
            translator.line(&delta, 0.0, 0.0),
            HandsLine {
                tick: 0,
                generation: 1,
                keys: 0,
                dx: 0,
                dy: 0,
                buttons: 0,
                slot: None,
                unsupported: false,
            }
        );
        delta.tick = 1;
        assert_eq!(translator.line(&delta, 0.0, 0.0).dx, 0);
        delta.tick = 2;
        let line = translator.line(&delta, 0.0, 0.0);
        assert_eq!((line.dx, line.dy), (1, -1));
    }

    #[test]
    fn an_aim_takes_the_shortest_way_across_180() {
        // From 170 to -170 is +20 degrees, not -340: 40 pixels at half a
        // degree each.
        let mut translator = setup();
        let mut aim = frame(0);
        aim.look = Look::Aim {
            yaw: -170.0,
            pitch: 0.0,
        };
        assert_eq!(translator.line(&aim, 170.0, 0.0).dx, 40);

        // The same across the negative edge: 179 to -179 is +2 degrees.
        let mut translator = setup();
        aim.tick = 1;
        aim.look = Look::Aim {
            yaw: -179.0,
            pitch: 0.0,
        };
        assert_eq!(translator.line(&aim, 179.0, 0.0).dx, 4);
    }

    #[test]
    fn an_aim_target_pitch_is_clamped_to_the_vanilla_range() {
        let mut translator = Translator::new(1.0, 0);
        let mut aim = frame(0);
        aim.look = Look::Aim {
            yaw: 0.0,
            pitch: 120.0,
        };
        assert_eq!(translator.line(&aim, 0.0, 0.0).dy, 90);
        aim.tick = 1;
        aim.look = Look::Aim {
            yaw: 0.0,
            pitch: -120.0,
        };
        assert_eq!(translator.line(&aim, 0.0, 0.0).dy, -90);
    }

    #[test]
    fn a_second_aim_to_the_same_target_waits_for_the_first_to_land() {
        let mut translator = Translator::new(1.0, 2);
        let mut aim = frame(0);
        aim.look = Look::Aim {
            yaw: 10.0,
            pitch: 0.0,
        };
        assert_eq!(translator.line(&aim, 0.0, 0.0).dx, 10);
        aim.tick = 1;
        assert_eq!(
            translator.line(&aim, 0.0, 0.0).dx,
            0,
            "the first ten degrees are still in flight"
        );
        // Two more ticks age the first send out of the window; the observed
        // rotation has not caught up yet, so the aim is sent again.
        aim.tick = 2;
        assert_eq!(translator.line(&aim, 0.0, 0.0).dx, 0);
        // The window now holds the two quiet ticks, so the aim is sent again
        // until the client's rotation catches up.
        aim.tick = 3;
        assert_eq!(translator.line(&aim, 0.0, 0.0).dx, 10);
    }

    #[test]
    fn a_zero_latency_ring_never_reports_degrees_in_flight() {
        let mut translator = Translator::new(1.0, 0);
        let mut aim = frame(0);
        aim.look = Look::Aim {
            yaw: 10.0,
            pitch: 0.0,
        };
        assert_eq!(translator.line(&aim, 0.0, 0.0).dx, 10);
        aim.tick = 1;
        assert_eq!(translator.line(&aim, 0.0, 0.0).dx, 10);
    }

    #[test]
    fn a_look_is_clamped_to_six_hundred_pixels_and_the_rest_is_not_carried() {
        let mut translator = Translator::new(1.0, 0);
        let mut delta = frame(0);
        delta.look = Look::Delta {
            dyaw: 601.0,
            dpitch: 0.0,
        };
        assert_eq!(translator.line(&delta, 0.0, 0.0).dx, 600);

        // One degree was clamped away; if it had been carried this tick would
        // send two pixels instead of one.
        delta.tick = 1;
        delta.look = Look::Delta {
            dyaw: 1.0,
            dpitch: 0.0,
        };
        assert_eq!(translator.line(&delta, 0.0, 0.0).dx, 1);
    }

    #[test]
    fn a_negative_look_is_clamped_too() {
        let mut translator = Translator::new(1.0, 0);
        let mut delta = frame(0);
        delta.look = Look::Delta {
            dyaw: -5000.0,
            dpitch: -5000.0,
        };
        let line = translator.line(&delta, 0.0, 0.0);
        assert_eq!((line.dx, line.dy), (-600, -600));
    }

    #[test]
    fn a_reset_forgets_the_ring_the_carry_and_the_slot() {
        let mut translator = setup();
        let mut aim = frame(0);
        aim.look = Look::Aim {
            yaw: 10.0,
            pitch: 0.0,
        };
        aim.select_slot = Some(1);
        translator.line(&aim, 0.0, 0.0);

        translator.reset();

        aim.tick = 1;
        let line = translator.line(&aim, 0.0, 0.0);
        assert_eq!(line.dx, 20, "nothing is in flight after a reset");
        assert_eq!(line.slot, Some(2), "the slot was forgotten");
    }

    /// The state of the one cell the destroy tests use.
    fn before(target: BlockPos) -> Option<BlockState> {
        (target == BlockPos::new(1, 64, 2))
            .then_some(BlockState::from(azalea_block::blocks::Stone))
    }

    fn observed(packet: ServerboundGamePacket) -> ObservedPacket {
        ObservedPacket {
            conn_id: 0,
            packet: Packet::PlayServerbound(packet),
        }
    }

    #[test]
    fn a_destroy_start_and_stop_fill_the_block_report() {
        let pos = BlockPos::new(1, 64, 2);
        for (ordinal, action) in [
            (ACTION_START, s_player_action::Action::StartDestroyBlock),
            // A real stop decodes as `DropAllItems` in the pinned enum.
            (ACTION_STOP, s_player_action::Action::DropAllItems),
        ] {
            assert_eq!(ordinal, action as u8);
            let report = observed_report(
                &[observed(ServerboundGamePacket::PlayerAction(
                    ServerboundPlayerAction {
                        action,
                        pos,
                        direction: Direction::Up,
                        seq: 9,
                    },
                ))],
                &|pos: BlockPos| before(pos),
                1,
            );
            assert_eq!(report.block_seq, Some(9));
            assert_eq!(report.block_hit, Some(pos));
            assert_eq!(report.block_face, Some(Direction::Up));
            assert_eq!(
                report.block_before,
                Some((pos, Some(BlockState::from(azalea_block::blocks::Stone))))
            );
            assert_eq!(report.clicks, 1);
        }
    }

    #[test]
    fn an_abort_or_a_change_of_direction_is_not_a_destroy_start_or_stop() {
        let pos = BlockPos::new(1, 64, 2);
        for action in [
            // Real abort (2) and change of direction (1).
            s_player_action::Action::StopDestroyBlock,
            s_player_action::Action::AbortDestroyBlock,
            // Real drops and the release of a use are not destroys either.
            s_player_action::Action::DropItem,
            s_player_action::Action::ReleaseUseItem,
            s_player_action::Action::SwapItemWithOffhand,
        ] {
            let report = observed_report(
                &[observed(ServerboundGamePacket::PlayerAction(
                    ServerboundPlayerAction {
                        action,
                        pos,
                        direction: Direction::Up,
                        seq: 9,
                    },
                ))],
                &|pos: BlockPos| before(pos),
                0,
            );
            assert_eq!(report.block_seq, None, "{action:?}");
            assert_eq!(report.block_before, None, "{action:?}");
        }
    }

    #[test]
    fn a_use_item_on_names_the_cell_the_block_would_be_placed_in() {
        let clicked = BlockPos::new(1, 64, 2);
        let placed = clicked.offset_with_direction(Direction::Up);
        let report = observed_report(
            &[observed(ServerboundGamePacket::UseItemOn(
                ServerboundUseItemOn {
                    hand: InteractionHand::MainHand,
                    block_hit: BlockHit {
                        block_pos: clicked,
                        direction: Direction::Up,
                        location: azalea_core::position::Vec3::new(1.5, 65.0, 2.5),
                        inside: false,
                        world_border: false,
                    },
                    seq: 4,
                },
            ))],
            &|pos: BlockPos| before(pos),
            0,
        );
        assert_eq!(report.block_seq, Some(4));
        assert_eq!(report.block_hit, Some(clicked));
        assert_eq!(report.block_face, Some(Direction::Up));
        assert_eq!(report.block_before, Some((placed, None)));
    }

    #[test]
    fn an_attack_packet_names_the_entity() {
        let report = observed_report(
            &[observed(ServerboundGamePacket::Attack(ServerboundAttack {
                entity_id: MinecraftEntityId(42),
            }))],
            &|pos: BlockPos| before(pos),
            0,
        );
        assert_eq!(report.attack_entity, Some(42));
        assert_eq!(report.block_seq, None);
    }

    #[test]
    fn the_last_block_packet_in_the_burst_wins() {
        let first = BlockPos::new(1, 64, 2);
        let second = BlockPos::new(3, 64, 4);
        let report = observed_report(
            &[
                observed(ServerboundGamePacket::PlayerAction(
                    ServerboundPlayerAction {
                        action: s_player_action::Action::StartDestroyBlock,
                        pos: first,
                        direction: Direction::Up,
                        seq: 1,
                    },
                )),
                observed(ServerboundGamePacket::PlayerAction(
                    ServerboundPlayerAction {
                        action: s_player_action::Action::DropAllItems,
                        pos: second,
                        direction: Direction::Down,
                        seq: 2,
                    },
                )),
            ],
            &|pos: BlockPos| before(pos),
            0,
        );
        assert_eq!(report.block_seq, Some(2));
        assert_eq!(report.block_hit, Some(second));
        assert_eq!(report.block_face, Some(Direction::Down));
    }

    #[test]
    fn clientbound_packets_are_ignored() {
        let clientbound = ObservedPacket {
            conn_id: 0,
            packet: Packet::PlayClientbound(
                azalea_protocol::packets::game::ClientboundGamePacket::SetHealth(
                    azalea_protocol::packets::game::ClientboundSetHealth {
                        health: 0.0,
                        food: 0,
                        saturation: 0.0,
                    },
                ),
            ),
        };
        let report = observed_report(&[clientbound], &|pos: BlockPos| before(pos), 3);
        assert_eq!(report.block_seq, None);
        assert_eq!(report.attack_entity, None);
        assert_eq!(report.clicks, 3, "the click count still comes from the frame");
    }
}
