//! Hearing: the sounds a reflex may act on (architecture §5.1, fair
//! perception).
//!
//! A player hears what the server sends every client: the sound event, its
//! position and, when a mob made it, the mob's entity id. That is a fair cue a
//! player can use behind their own back or through a wall, where sight has
//! nothing. This module is the one place that turns those packets into reflex
//! facts.
//!
//! The snapshot is taken once per tick in [`View::capture`](crate::View::capture)
//! and kept in [`ReflexState`](crate::ReflexState) beside the own-entity facts,
//! for the same reason: it is a server fact of this tick, not a decision, and a
//! reflex stays a pure function of `(view, state, settings)`. Keeping it out of
//! the [`View`](crate::View) struct also keeps every hand-built view (the
//! arbiter's and the body's tests build them field by field) source compatible.
//!
//! Two reactions read it. The lethal one is part of
//! [`ReflexKind::Creeper`](crate::ReflexKind): it flees a primed creeper the
//! view cannot see, through the one flee the sight reflex already runs. The
//! alert one is a gaze turn toward a hostile sound whose source is not tracked
//! yet, so the existing sight reflexes can take over; the arbiter holds it for
//! [`ALERT_TURN_TICKS`], because a gaze gesture outlives one frame.

use praxion_headless::Look;
use praxion_perception::{Answer, Perception, SoundFact};
use praxion_planner::aim;
use praxion_world::{Vec3, World};

use crate::view::View;

/// Sounds farther than this are not heard, in blocks. The perception API's
/// hearing radius is the same number (architecture: hearing is fair).
pub const HEARD_RADIUS: u32 = 16;

/// A sound older than this is no longer a fact of "now", in ticks (two
/// seconds).
pub const HEARD_WINDOW_TICKS: u64 = 40;

/// How long a creeper's fuse burns after it primes, in ticks. A primed sound
/// older than this is no longer a cue: the creeper that primed that long ago
/// has exploded. See [`fuse_over`].
pub const CREEPER_FUSE_TICKS: u64 = 30;

/// A sound this close to the body's own eye (blocks, three-dimensional) is the
/// body's own. A `Sound` packet names no entity, so a sound the body made at
/// itself would otherwise read as a mob standing on it.
pub const OWN_SOUND_RADIUS: f64 = 1.0;

/// An explosion this close to where a fuse was heard (blocks) is that fuse's
/// explosion: a swelling creeper stands still, so its feet are within a blast
/// radius of where the fuse was heard.
pub const EXPLOSION_MATCH_BLOCKS: f64 = 3.0;

/// The snapshot keeps at most this many classified sounds, newest first. More
/// than a first night has worth acting on in one tick.
pub const MAX_HEARD: usize = 8;

/// A heard primed creeper this close (horizontal, blocks) is a lethal threat:
/// the fuse is about to end. The sight reflex's own range is
/// `Settings::creeper_flee_range`; a sound is a weaker cue than a sight, so its
/// trigger range is smaller.
pub const PRIMED_FLEE_RANGE: f64 = 7.0;

/// At most one alert head turn per this many ticks: a mob that steps every few
/// ticks must not wrench the camera every time.
pub const ALERT_COOLDOWN_TICKS: u64 = 60;

/// A hostile sound is a stimulus only while it is this fresh (ticks). One tick
/// of slack absorbs the offset between the connection's tick and the
/// perception tick; an older sound was already heard.
pub const ALERT_FRESH_TICKS: u64 = 2;

/// How long one alert head turn asks for the same aim, in ticks (1.5 s). The
/// gaze controller needs the request on every tick to finish its gesture: a
/// turn is a reaction time (about five ticks) plus a gesture of up to eleven
/// ticks and its correction. A request for one tick would be replaced by the
/// next frame before the camera moved.
pub const ALERT_TURN_TICKS: u64 = 30;

/// What a heard sound means to the reflexes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundClass {
    /// `entity.creeper.primed`: the fuse is lit.
    CreeperPrimed,
    /// A mob that fights, or a shot: worth looking at.
    Hostile,
}

/// The sound-id prefixes a reflex cares about and the class of each. One table,
/// so a new hostile mob is one line and never a new branch.
///
/// Ids arrive as `minecraft:<id>`; [`classify`] strips the namespace. The
/// primed fuse comes first because it is the only lethal entry; nothing else
/// shares its prefix.
pub const SOUND_TABLE: &[(&str, SoundClass)] = &[
    ("entity.creeper.primed", SoundClass::CreeperPrimed),
    ("entity.arrow.shoot", SoundClass::Hostile),
    ("entity.skeleton.shoot", SoundClass::Hostile),
    ("entity.zombie.ambient", SoundClass::Hostile),
    ("entity.zombie.hurt", SoundClass::Hostile),
    ("entity.zombie.step", SoundClass::Hostile),
    ("entity.husk.ambient", SoundClass::Hostile),
    ("entity.husk.hurt", SoundClass::Hostile),
    ("entity.husk.step", SoundClass::Hostile),
    ("entity.drowned.ambient", SoundClass::Hostile),
    ("entity.drowned.hurt", SoundClass::Hostile),
    ("entity.drowned.step", SoundClass::Hostile),
    ("entity.skeleton.ambient", SoundClass::Hostile),
    ("entity.skeleton.hurt", SoundClass::Hostile),
    ("entity.skeleton.step", SoundClass::Hostile),
    ("entity.stray.ambient", SoundClass::Hostile),
    ("entity.stray.hurt", SoundClass::Hostile),
    ("entity.stray.step", SoundClass::Hostile),
    ("entity.spider.ambient", SoundClass::Hostile),
    ("entity.spider.hurt", SoundClass::Hostile),
    ("entity.spider.step", SoundClass::Hostile),
    ("entity.cave_spider.ambient", SoundClass::Hostile),
    ("entity.cave_spider.hurt", SoundClass::Hostile),
    ("entity.cave_spider.step", SoundClass::Hostile),
    ("entity.creeper.hurt", SoundClass::Hostile),
    ("entity.creeper.step", SoundClass::Hostile),
    ("entity.enderman.ambient", SoundClass::Hostile),
    ("entity.enderman.hurt", SoundClass::Hostile),
    ("entity.enderman.step", SoundClass::Hostile),
    ("entity.witch.ambient", SoundClass::Hostile),
    ("entity.witch.hurt", SoundClass::Hostile),
    ("entity.witch.step", SoundClass::Hostile),
    ("entity.phantom.ambient", SoundClass::Hostile),
    ("entity.phantom.hurt", SoundClass::Hostile),
    ("entity.phantom.step", SoundClass::Hostile),
];

/// The class of a sound-event id, or `None` when no reflex acts on it.
///
/// `minecraft:entity.creeper.primed` and `entity.creeper.primed` are the same
/// sound.
pub fn classify(id: &str) -> Option<SoundClass> {
    let id = strip_namespace(id);
    SOUND_TABLE
        .iter()
        .find(|(prefix, _)| id.starts_with(prefix))
        .map(|(_, class)| *class)
}

/// The sound a lit fuse ends with: a creeper's or a TNT block's explosion. It
/// is no stimulus of its own, it ends the fuse that was heard before it.
const EXPLOSION_IDS: &[&str] = &["entity.generic.explode"];

/// Whether a sound-event id is an explosion.
fn is_explosion(id: &str) -> bool {
    EXPLOSION_IDS.contains(&strip_namespace(id))
}

/// One sound the reflexes heard, with everything a reaction needs.
#[derive(Clone, Debug, PartialEq)]
pub struct HeardSound {
    /// The sound-event id without the `minecraft:` namespace, e.g.
    /// `entity.creeper.primed`.
    pub id: String,
    pub class: SoundClass,
    /// Where the server placed the sound: a mob's feet for a `SoundEntity`
    /// packet, the player's chosen point for a `Sound` packet.
    pub position: Vec3,
    /// How many ticks before this view the sound arrived.
    pub age_ticks: u64,
    /// The entity that made it, when the packet named one.
    pub entity: Option<i32>,
}

impl HeardSound {
    /// The horizontal distance from the body to the sound. Mobs stand on the
    /// ground, so the eye-to-feet distance would inflate a "five blocks away"
    /// creeper, exactly like [`View::threat_within`].
    pub fn distance_to(&self, view: &View) -> f64 {
        view.horizontal_to_point(self.position)
    }
}

/// One alert head turn in progress: the sound it looks at and its last tick.
#[derive(Clone, Debug)]
struct AlertTurn {
    sound: HeardSound,
    /// The turn asks for the same aim through this tick (inclusive).
    until: u64,
}

/// The alert turn due this tick: what to look at, and whether this is the tick
/// it starts (the one tick that logs the reaction).
#[derive(Clone, Debug, PartialEq)]
pub struct Alert {
    pub sound: HeardSound,
    pub first: bool,
}

/// The sounds heard this tick, the alert turn's clock and the turn in progress.
#[derive(Clone, Debug, Default)]
pub struct Hearing {
    sounds: Vec<HeardSound>,
    /// The tick of the last alert head turn, if there ever was one.
    last_alert: Option<u64>,
    /// The turn whose aim is still being requested, if any.
    turn: Option<AlertTurn>,
}

impl Hearing {
    /// Replace the snapshot. An empty list is a value: nothing was heard.
    pub fn capture(&mut self, sounds: Vec<HeardSound>) {
        self.sounds = sounds;
    }

    /// The snapshot, newest first.
    pub fn sounds(&self) -> &[HeardSound] {
        &self.sounds
    }

    /// The nearest primed creeper within `range` blocks (horizontal), if any.
    ///
    /// A fuse that has burned its [`CREEPER_FUSE_TICKS`] or ended in an
    /// explosion never reaches the snapshot (see [`heard_sounds`]), so this is
    /// the live fuse only.
    pub fn primed_within(&self, view: &View, range: f64) -> Option<&HeardSound> {
        self.sounds
            .iter()
            .filter(|sound| sound.class == SoundClass::CreeperPrimed)
            .filter(|sound| sound.distance_to(view) <= range)
            .min_by(|a, b| a.distance_to(view).total_cmp(&b.distance_to(view)))
    }

    /// The alert head turn due this tick, if any.
    ///
    /// A turn runs for [`ALERT_TURN_TICKS`] on the sound that started it, so
    /// the gaze controller can finish the gesture; it starts from a freshly
    /// heard hostile sound whose source the view does not track yet. One the
    /// view already sees is in the sight reflexes' hands, and an old sound
    /// points at where a mob used to be. A new turn starts at most once per
    /// [`ALERT_COOLDOWN_TICKS`].
    ///
    /// `may_look` is the caller's gate on the body: an alert frame carries the
    /// look and nothing else, so the body may only spend ticks on it while no
    /// reflex, need or primitive frame wants the look. A turn is dropped, not
    /// paused, when the gate closes; the sound's clock is not resumed later.
    /// A refused turn never spends the cooldown.
    pub fn alert(&mut self, view: &View, tick: u64, may_look: bool) -> Option<Alert> {
        if !may_look {
            self.turn = None;
            return None;
        }
        if let Some(turn) = &self.turn {
            if tick <= turn.until {
                return Some(Alert {
                    sound: turn.sound.clone(),
                    first: false,
                });
            }
            self.turn = None;
        }
        if self
            .last_alert
            .is_some_and(|last| tick < last.saturating_add(ALERT_COOLDOWN_TICKS))
        {
            return None;
        }
        let sound = self
            .sounds
            .iter()
            .filter(|sound| sound.class == SoundClass::Hostile)
            .filter(|sound| sound.age_ticks <= ALERT_FRESH_TICKS)
            .find(|sound| !source_tracked(view, sound))?
            .clone();
        self.last_alert = Some(tick);
        self.turn = Some(AlertTurn {
            sound: sound.clone(),
            until: tick.saturating_add(ALERT_TURN_TICKS - 1),
        });
        Some(Alert { sound, first: true })
    }
}

/// The classified sounds of this tick, from the perception API. Old sounds,
/// sounds no reflex cares about and the body's own sounds are dropped here,
/// once.
///
/// A primed fuse is dropped as well once it is over: the cue is a lethal threat
/// only while the fuse burns, and an explosion heard afterwards where the fuse
/// was heard ends it sooner (see [`fuse_over`]). Without that the fuse would
/// stay a cue for the whole [`HEARD_WINDOW_TICKS`], ten ticks past the blast,
/// and the creeper reflex would flee from a creeper that no longer exists.
pub fn heard_sounds(perception: &mut Perception, world: &World) -> Vec<HeardSound> {
    let Answer::Known { value: facts, .. } = perception.sounds_near(world, HEARD_RADIUS) else {
        return Vec::new();
    };
    // The body's own eye, for the own-sound guard: the server places the body's
    // own steps, shots and hurts on the body, never on a mob.
    let eye = perception.eye(world);
    let explosions: Vec<(Vec3, u64)> = facts
        .iter()
        .filter(|fact| fact.age_ticks <= HEARD_WINDOW_TICKS)
        .filter(|fact| is_explosion(&fact.id))
        .map(|fact| (fact.position, fact.age_ticks))
        .collect();
    let mut heard: Vec<HeardSound> = facts
        .into_iter()
        .filter(|fact| fact.age_ticks <= HEARD_WINDOW_TICKS)
        .filter(|fact| !own_sound(eye, fact.position))
        .filter_map(|fact| {
            let class = classify(&fact.id)?;
            if class == SoundClass::CreeperPrimed && fuse_over(&fact, &explosions) {
                return None;
            }
            Some(HeardSound {
                id: strip_namespace(&fact.id).to_owned(),
                class,
                position: fact.position,
                age_ticks: fact.age_ticks,
                entity: fact.entity,
            })
        })
        .collect();
    // The ring answers oldest first; the newest sounds are the ones worth
    // acting on, and the alert takes the newest first.
    if heard.len() > MAX_HEARD {
        let drop = heard.len() - MAX_HEARD;
        heard.drain(..drop);
    }
    heard.reverse();
    heard
}

/// Whether a heard fuse is over: it has burned its full length, or an explosion
/// at the place it was heard came after it.
///
/// `explosions` holds every explosion of this snapshot with its age. An
/// explosion older than the fuse does not count: a second creeper priming where
/// the first one blew up is a new fuse, not the old one ending.
fn fuse_over(fuse: &SoundFact, explosions: &[(Vec3, u64)]) -> bool {
    fuse.age_ticks > CREEPER_FUSE_TICKS
        || explosions.iter().any(|&(position, age_ticks)| {
            age_ticks < fuse.age_ticks
                && position.distance_to(fuse.position) <= EXPLOSION_MATCH_BLOCKS
        })
}

/// Whether a sound sits on the body's own eye, which makes it the body's own
/// sound rather than a cue from somewhere else.
fn own_sound(eye: Option<Vec3>, position: Vec3) -> bool {
    eye.is_some_and(|eye| eye.distance_to(position) <= OWN_SOUND_RADIUS)
}

/// The look request of an alert turn: aim at the sound, nothing else. The
/// camera's own gesture controller animates the turn.
pub fn alert_look(view: &View, sound: &HeardSound) -> Look {
    let (yaw, pitch) = aim(view.eye, sound.position);
    Look::Aim { yaw, pitch }
}

/// One log line per reaction to a sound, in the reflex action shape:
/// `reflex_action kind=alert action=look target=<id|-> distance=<blocks>
/// renewed=false heard=<sound id>`.
pub fn alert_note(view: &View, sound: &HeardSound) -> String {
    let target = sound
        .entity
        .map_or_else(|| "-".to_owned(), |id| id.to_string());
    format!(
        "reflex_action kind=alert action=look target={target} distance={:.2} renewed=false heard={}",
        sound.distance_to(view),
        sound.id
    )
}

/// A reflex action line whose action came from hearing, with the sound named.
/// The flee the creeper reflex runs on a heard fuse is the same flee it runs on
/// a seen creeper; only the log line says which cue it was.
pub fn with_heard(note: String, sound: &HeardSound) -> String {
    format!("{note} heard={}", sound.id)
}

/// Whether the view already tracks the sound's source, which makes the sight
/// reflexes the owner of the reaction.
///
/// A `SoundEntity` packet names its mob, so the id decides. A positional sound
/// names nobody; a tracked mob standing within two blocks of it is its source
/// (mob feet to sound point).
fn source_tracked(view: &View, sound: &HeardSound) -> bool {
    let tracked = || {
        view.creeper
            .into_iter()
            .chain(view.melee_mobs())
            .chain(view.endermen.iter().flatten().copied())
    };
    match sound.entity {
        Some(id) => {
            tracked().any(|entity| entity.id == id)
                || view.arrows.iter().flatten().any(|arrow| arrow.id == id)
        }
        None => tracked().any(|entity| horizontal(entity.position, sound.position) <= 2.0),
    }
}

/// The horizontal distance between two points.
fn horizontal(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

/// `minecraft:entity.creeper.primed` and `entity.creeper.primed` name the same
/// sound.
fn strip_namespace(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_classifies_the_primed_fuse_and_a_hostile_ambient() {
        assert_eq!(
            classify("minecraft:entity.creeper.primed"),
            Some(SoundClass::CreeperPrimed)
        );
        assert_eq!(
            classify("entity.creeper.primed"),
            Some(SoundClass::CreeperPrimed)
        );
        assert_eq!(
            classify("minecraft:entity.zombie.ambient"),
            Some(SoundClass::Hostile)
        );
        assert_eq!(
            classify("minecraft:entity.arrow.shoot"),
            Some(SoundClass::Hostile)
        );
        // The fuse is never a mere hostile step.
        assert_ne!(classify("entity.creeper.primed"), Some(SoundClass::Hostile));
        // A sound no reflex acts on stays unknown.
        assert_eq!(classify("minecraft:block.stone.break"), None);
        // An explosion is no stimulus either: it ends a fuse, it never starts a
        // reaction of its own.
        assert_eq!(classify("minecraft:entity.generic.explode"), None);
        assert!(is_explosion("minecraft:entity.generic.explode"));
        assert!(is_explosion("entity.generic.explode"));
        assert!(!is_explosion("entity.creeper.primed"));
    }

    fn fuse(age_ticks: u64) -> SoundFact {
        SoundFact {
            id: "minecraft:entity.creeper.primed".to_owned(),
            position: Vec3::new(0.5, 64.0, -5.0),
            tick: 0,
            age_ticks,
            entity: None,
        }
    }

    #[test]
    fn a_fuse_is_over_when_it_has_burned_or_exploded_where_it_was_heard() {
        let heard = fuse(10);
        // The blast, a moment after the fuse and where the fuse was heard.
        assert!(fuse_over(&heard, &[(Vec3::new(0.5, 64.0, -5.0), 2)]));
        // A creeper that took a step while swelling still matches.
        assert!(fuse_over(&heard, &[(Vec3::new(2.0, 64.0, -4.0), 2)]));
        // An explosion farther away is another explosion.
        assert!(!fuse_over(&heard, &[(Vec3::new(0.5, 64.0, 5.0), 2)]));
        // One heard before the fuse: a second creeper priming after the first
        // one blew up is a new fuse, not the old one ending.
        assert!(!fuse_over(&heard, &[(Vec3::new(0.5, 64.0, -5.0), 11)]));
        // The fuse's own length is the cap: the blast tick is still a cue, the
        // tick after it is not.
        assert!(!fuse_over(&fuse(CREEPER_FUSE_TICKS), &[]));
        assert!(fuse_over(&fuse(CREEPER_FUSE_TICKS + 1), &[]));
    }

    #[test]
    fn a_sound_on_the_bodys_own_eye_is_the_bodys_own() {
        let eye = Vec3::new(0.5, 65.62, 0.5);
        assert!(own_sound(Some(eye), eye));
        assert!(own_sound(Some(eye), eye + Vec3::new(0.5, 0.0, 0.0)));
        // A mob's sound is placed at its feet, at least the eye height away.
        assert!(!own_sound(Some(eye), Vec3::new(0.5, 64.0, 0.5)));
        // Without an eye nothing is dropped: there are no sounds either.
        assert!(!own_sound(None, eye));
    }
}
