//! The reflex layer (architecture §3 and §4, session 7).
//!
//! A reflex is a pure function over one tick's [`View`] of perception:
//!
//! ```text
//! Perception + World
//!         │  View::capture (once per tick)
//!         ▼
//!      View ──evaluate(kind)──> ReflexAction
//!         │                         │
//!         │  latency gate           │  Motion::start (planner for flee paths)
//!         ▼                         ▼
//!   ReflexState               ControlFrame per tick
//! ```
//!
//! Nothing here talks to the network, the brain or a skill. A reflex carries a
//! [`LatencyClass`]: `human_floor` acts on the third tick of visibility (150 ms),
//! `lethal_fast` on the first. The first-night set is creeper, melee, arrows,
//! lava, drowning, fire and starvation, plus the Enderman look guard and the
//! fall guard on reflex motion.
//!
//! Hearing (2026-11-12) is fair perception and part of the same set: a primed
//! creeper the view cannot see flees through the existing creeper reflex, and a
//! freshly heard hostile sound the view cannot see turns the head toward it
//! (see the `hearing` module and [`alert_look`]).
//!
//! A cell the view could not see is [`Cell::Unknown`], never air (§5.1): it
//! cannot trigger a reflex and cannot be stepped into. The own entity facts
//! (air supply, burning) come from the server as HUD facts and live in
//! [`ReflexState`], where the drowning and fire triggers read them.
//!
//! The arbiter (`praxion-arbiter`) owns the body, bumps the generation and
//! preempts the interpreter; this crate only says what the reflex wants.

mod enderman;
mod engine;
mod hearing;
mod motion;
mod reflex;
mod settings;
mod view;

pub use enderman::{
    EndermanGuard, BREAK_PITCH_DEG, BREAK_YAW_DEG, HEAD_TOLERANCE_DEG, head_of,
};
pub use engine::{Engine, ReflexTick, human_floor_ticks};
pub use hearing::{
    ALERT_COOLDOWN_TICKS, ALERT_FRESH_TICKS, ALERT_TURN_TICKS, HEARD_RADIUS, HEARD_WINDOW_TICKS,
    Alert, HeardSound, MAX_HEARD, PRIMED_FLEE_RANGE, SOUND_TABLE, SoundClass, alert_look,
    alert_note, classify, heard_sounds, with_heard,
};
pub use motion::{Motion, SAFE_DROP, yaw_pitch_towards};
pub use reflex::{
    CREEPER_LETHAL_RANGE, FULL_AIR_TICKS, LatencyClass, MELEE_COOLDOWN_TICKS, MELEE_EXIT_MARGIN,
    ReflexAction, ReflexKind, ReflexState, action_for, action_note, evaluate, evaluate_all,
    fled_sound, is_melee_kind, sprint_outruns,
};
pub use settings::{HUMAN_FLOOR_TICKS, MAX_REFLEX_TICKS_LIMIT, Settings, SettingsError};
pub use view::{
    ArrowMemory, ArrowView, Cell, EntityView, FoodChoice, MAX_ARROWS, MAX_ENDERMEN,
    MAX_MELEE_OTHERS, View, WeaponChoice,
};

use praxion_perception::{Answer, Caller, Perception};
use praxion_planner::{Block, KnownBlocks};
use praxion_world::{BlockPos, World};

/// A [`KnownBlocks`] source for the planner that reads through perception on
/// demand. The production driver may instead hand the planner perception's
/// known map (the sweep fills it); this adapter is what a reflex uses when it
/// must plan before a sweep has covered the cells ahead.
pub struct ReflexBlocks<'a> {
    pub perception: &'a mut Perception,
    pub world: &'a World,
}

impl KnownBlocks for ReflexBlocks<'_> {
    fn block(&mut self, pos: BlockPos) -> Block {
        match self.perception.block(Caller::Reflex, self.world, pos) {
            Answer::Known {
                value,
                age_ticks,
                ..
            } => Block::Known {
                state: value.state,
                age_ticks,
            },
            Answer::Unknown(_) => Block::Unknown,
        }
    }
}
