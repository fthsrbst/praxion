//! The `nearby` facts a snapshot carries: what the body can see and hear
//! around itself (proposal 2026-11-12 "Snapshot facts: nearby").
//!
//! Every value comes from the perception API. A block count is the cells the
//! known map already remembers within [`BLOCK_RADIUS`], an entity count is a
//! line-of-sight query within [`ENTITY_RADIUS`], and the sounds are the ones
//! perception heard within [`SOUND_RADIUS`]. The driver never reads the raw
//! world store here (architecture §5.1).
//!
//! The object is omitted when all three parts are empty, so a quiet snapshot
//! stays byte-identical and `PORT_VERSION` stays 0.

use std::collections::BTreeMap;

use praxion_perception::{Answer, Caller, Perception, SoundFact};
use praxion_port::Nearby;
use praxion_world::{BlockState, World};

/// Entities within this many blocks of the eye are counted.
pub const ENTITY_RADIUS: u32 = 32;
/// Known blocks within this many blocks of the eye are counted.
pub const BLOCK_RADIUS: u32 = 16;
/// Sounds within this many blocks of the eye are heard.
pub const SOUND_RADIUS: u32 = 16;
/// A sound older than this many ticks is no longer news.
pub const SOUND_MAX_AGE_TICKS: u64 = 100;
/// The most distinct sounds one snapshot reports.
pub const SOUND_LIMIT: usize = 8;

/// The notable blocks that are neither a log nor an ore, by id without the
/// namespace. A bed of any colour is reported as `bed` and a wall-mounted torch
/// as `torch` instead.
const NOTABLE: &[&str] = &[
    "water",
    "lava",
    "crafting_table",
    "furnace",
    "chest",
    "bell",
    "hay_block",
    "torch",
];

/// The `nearby` object for the snapshot, or `None` when there is nothing to
/// report.
pub fn nearby(world: &World, perception: &mut Perception) -> Option<Nearby> {
    assemble(
        entity_counts(world, perception),
        block_counts(world, perception),
        sound_ids(perception.sounds_near(world, SOUND_RADIUS)),
    )
}

/// Count visible entities per kind. An unknown answer (a desynced world, no
/// eye, or a spent ray budget or entity cap) reports nothing rather than a
/// partial count.
fn entity_counts(world: &World, perception: &mut Perception) -> BTreeMap<String, u32> {
    let Answer::Known { value: counts, .. } =
        perception.entity_kinds_near(Caller::Other, world, ENTITY_RADIUS)
    else {
        return BTreeMap::new();
    };
    counts
        .into_iter()
        .map(|(kind, count)| (bare(kind.to_str()).to_owned(), count))
        .collect()
}

/// Count the notable known blocks per id. `for_each_known` spends no ray and
/// visits nothing when the eye is unknown, so a missing position yields an
/// empty map.
fn block_counts(world: &World, perception: &mut Perception) -> BTreeMap<String, u32> {
    let mut counts = BTreeMap::new();
    // An unknown eye visits nothing, so the map stays empty and the snapshot
    // says nothing about blocks rather than inventing them.
    let _ = perception.for_each_known(world, BLOCK_RADIUS, |_, state| {
        if let Some(id) = notable(state) {
            *counts.entry(id.to_owned()).or_insert(0) += 1;
        }
    });
    counts
}

/// The id the snapshot reports for a known block, or `None` when the block is
/// not notable.
fn notable(state: BlockState) -> Option<&'static str> {
    notable_id(state.as_block_kind().to_str())
}

/// The report id of a block kind's registry id, or `None` when it is not
/// notable.
///
/// A log or wood block keeps its own id (the `#logs` block tag), an ore keeps
/// its own `*_ore` id, a bed of any colour is reported as `bed`, and a
/// wall-mounted torch is the `torch` it was placed from. A soul or redstone
/// wall torch is not folded: the standing torch of that kind is not notable
/// either, so there would be no entry to fold it into.
fn notable_id(id: &'static str) -> Option<&'static str> {
    let id = bare(id);
    if id.ends_with("_bed") {
        return Some("bed");
    }
    if id == "wall_torch" {
        return Some("torch");
    }
    let notable = id.ends_with("_ore")
        || NOTABLE.contains(&id)
        || praxion_gamedata::block_in_tag(id, "logs");
    notable.then_some(id)
}

/// The newest distinct sound ids of one `sounds_near` answer, at most
/// [`SOUND_LIMIT`], without the namespace.
fn sound_ids(answer: Answer<Vec<SoundFact>>) -> Vec<String> {
    let Answer::Known { value: sounds, .. } = answer else {
        return Vec::new();
    };
    sound_ids_of(&sounds)
}

/// The newest distinct sound ids, at most [`SOUND_LIMIT`], without the
/// namespace. A sound older than [`SOUND_MAX_AGE_TICKS`] is dropped, and an id
/// heard twice is reported once, at its newest occurrence.
fn sound_ids_of(sounds: &[SoundFact]) -> Vec<String> {
    let mut fresh: Vec<&SoundFact> = sounds
        .iter()
        .filter(|sound| sound.age_ticks <= SOUND_MAX_AGE_TICKS)
        .collect();
    fresh.sort_by_key(|sound| sound.age_ticks);
    let mut ids: Vec<&str> = Vec::new();
    for sound in fresh {
        let id = bare(&sound.id);
        if ids.contains(&id) {
            continue;
        }
        ids.push(id);
        if ids.len() == SOUND_LIMIT {
            break;
        }
    }
    ids.into_iter().map(str::to_owned).collect()
}

/// The `nearby` object, or `None` when all three parts are empty.
fn assemble(
    entities: BTreeMap<String, u32>,
    blocks: BTreeMap<String, u32>,
    sounds: Vec<String>,
) -> Option<Nearby> {
    let nearby = Nearby {
        entities,
        blocks,
        sounds,
    };
    (!nearby.is_empty()).then_some(nearby)
}

/// An id without the `minecraft:` namespace.
fn bare(id: &str) -> &str {
    id.strip_prefix("minecraft:").unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use praxion_world::Vec3;

    fn sound(id: &str, age_ticks: u64) -> SoundFact {
        SoundFact {
            id: id.to_owned(),
            position: Vec3::ZERO,
            tick: 1_000 - age_ticks,
            age_ticks,
            entity: None,
        }
    }

    #[test]
    fn a_quiet_snapshot_has_no_nearby_key() {
        assert_eq!(assemble(BTreeMap::new(), BTreeMap::new(), Vec::new()), None);
    }

    #[test]
    fn one_part_is_enough_to_report_the_rest_empty() {
        let nearby = assemble(
            [("cow".to_owned(), 2)].into_iter().collect(),
            BTreeMap::new(),
            Vec::new(),
        )
        .expect("a visible cow is worth reporting");
        assert_eq!(nearby.entities, [("cow".to_owned(), 2)].into_iter().collect());
        assert!(nearby.blocks.is_empty());
        assert!(nearby.sounds.is_empty());
    }

    #[test]
    fn an_empty_world_reports_nothing_nearby() {
        // The fill function end to end: no position, no known cell, no sound.
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        perception.begin_tick(&world);
        assert_eq!(nearby(&world, &mut perception), None);
    }

    #[test]
    fn logs_and_ores_keep_their_id_and_a_bed_becomes_bed() {
        assert_eq!(notable_id("minecraft:oak_log"), Some("oak_log"));
        assert_eq!(
            notable_id("minecraft:deepslate_coal_ore"),
            Some("deepslate_coal_ore")
        );
        assert_eq!(notable_id("minecraft:red_bed"), Some("bed"));
        assert_eq!(notable_id("minecraft:water"), Some("water"));
        assert_eq!(notable_id("minecraft:torch"), Some("torch"));
        assert_eq!(notable_id("minecraft:stone"), None);
        assert_eq!(notable_id("minecraft:dirt"), None);
        assert_eq!(notable_id("minecraft:oak_planks"), None);
    }

    #[test]
    fn a_wall_torch_is_the_torch_it_was_placed_from() {
        // A cave lit with wall torches has just as many torches as one lit with
        // standing ones, and the planner reads these counts.
        assert_eq!(notable_id("minecraft:wall_torch"), Some("torch"));
        // The soul and redstone wall torches are left out with their standing
        // forms: neither is notable, so there is nothing to fold them into.
        assert_eq!(notable_id("minecraft:soul_torch"), None);
        assert_eq!(notable_id("minecraft:soul_wall_torch"), None);
        assert_eq!(notable_id("minecraft:redstone_torch"), None);
        assert_eq!(notable_id("minecraft:redstone_wall_torch"), None);
    }

    /// The first registry state whose kind is `minecraft:<name>`.
    fn state(name: &str) -> BlockState {
        let wanted = format!("minecraft:{name}");
        for id in 0..=u32::from(u16::MAX) {
            let Ok(state) = BlockState::try_from(id) else {
                break;
            };
            if state.as_block_kind().to_str() == wanted {
                return state;
            }
        }
        panic!("no block named {name}");
    }

    #[test]
    fn a_stored_state_is_named_the_way_the_snapshot_reports_it() {
        // `notable` is the door between a remembered state and the wire id: a
        // log keeps its own id, a bed loses its colour, and an unremarkable
        // block is not reported at all.
        assert_eq!(notable(state("oak_log")), Some("oak_log"));
        assert_eq!(
            notable(state("stripped_spruce_log")),
            Some("stripped_spruce_log")
        );
        assert_eq!(notable(state("white_bed")), Some("bed"));
        assert_eq!(notable(state("iron_ore")), Some("iron_ore"));
        assert_eq!(notable(state("furnace")), Some("furnace"));
        assert_eq!(notable(state("torch")), Some("torch"));
        assert_eq!(notable(state("wall_torch")), Some("torch"));
        assert_eq!(notable(state("soul_wall_torch")), None);
        assert_eq!(notable(state("stone")), None);
    }

    #[test]
    fn sounds_are_newest_first_distinct_and_fresh() {
        let sounds = [
            sound("minecraft:entity.zombie.ambient", 90),
            sound("minecraft:entity.creeper.primed", 10),
            sound("minecraft:entity.zombie.ambient", 5),
            sound("minecraft:block.stone.break", SOUND_MAX_AGE_TICKS + 1),
        ];
        assert_eq!(
            sound_ids_of(&sounds),
            vec![
                "entity.zombie.ambient".to_owned(),
                "entity.creeper.primed".to_owned(),
            ]
        );
    }

    #[test]
    fn at_most_eight_distinct_sounds_are_reported() {
        let sounds: Vec<SoundFact> = (0..12)
            .map(|index| sound(&format!("minecraft:note.block.{index}"), index))
            .collect();
        let ids = sound_ids_of(&sounds);
        assert_eq!(ids.len(), SOUND_LIMIT);
        assert_eq!(ids[0], "note.block.0", "newest first");
        assert_eq!(ids[SOUND_LIMIT - 1], "note.block.7");
    }
}
