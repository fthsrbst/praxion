//! Hearing tests: the `heard` snapshot and the two reactions, built from sound
//! packets alone.
//!
//! The worlds are the same scripted packet path the body uses (`common`), so a
//! test can only assert what a player could hear: the sound packet, its
//! position and, when the server names one, its entity. Nothing here reads the
//! raw store.

mod common;

use azalea_protocol::packets::game::{ServerboundGamePacket, s_client_tick_end::ServerboundClientTickEnd};
use azalea_registry::builtin::{BlockKind, EntityKind, SoundEvent};
use praxion_reflexes::{
    ALERT_COOLDOWN_TICKS, HEARD_WINDOW_TICKS, PRIMED_FLEE_RANGE, ReflexAction, ReflexKind,
    SoundClass, View,
};
use praxion_world::{BlockPos, Vec3};

use common::{Sim, eye, state, test_settings};

const FEET: Vec3 = Vec3::new(0.5, 64.0, 0.5);

/// A creeper's fuse length in ticks: the contract value, pinned here so that a
/// wider window in `hearing.rs` cannot pass unnoticed.
const FUSE_TICKS: u64 = 30;

fn fresh() -> Sim {
    Sim::new(test_settings(), FEET)
}

/// Start perception's tick and capture the view, as the driver does.
fn capture(sim: &mut Sim) -> View {
    sim.perception.begin_tick(&sim.world);
    sim.view()
}

/// End the world's tick, as the client's `ClientTickEnd` packet does. The
/// scripted connection sends none of its own, so a sound would otherwise keep
/// the world tick it arrived at and age out of the heard window at once.
fn end_world_tick(sim: &mut Sim) {
    sim.world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
    );
}

/// A wall behind the body (the body faces +z) at `z = -1`, three blocks high and
/// three wide, so nothing behind it is in sight.
fn wall_behind(sim: &mut Sim) {
    for x in -1..=1 {
        for y in 64..=66 {
            sim.set_block(BlockPos::new(x, y, -1), state(BlockKind::Stone));
        }
    }
}

#[test]
fn a_primed_creeper_behind_a_wall_flees_from_the_sound() {
    let mut sim = fresh();
    wall_behind(&mut sim);
    // The body faces +z and a wall stands behind it at z = -1. The creeper is
    // five blocks behind that wall, so no ray reaches it and the view cannot
    // see it; the fuse's sound packet is the only cue, exactly the report this
    // fixes.
    let sound = Vec3::new(0.5, 64.0, -5.0);
    sim.add_mob(9, EntityKind::Creeper, sound);
    sim.add_sound(SoundEvent::EntityCreeperPrimed, sound);

    let view = capture(&mut sim);
    assert!(view.creeper.is_none(), "the wall hides the creeper");
    let heard = sim.engine.state().heard();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(heard[0].class, SoundClass::CreeperPrimed);
    assert_eq!(heard[0].id, "entity.creeper.primed");
    assert_eq!(heard[0].position, sound);
    assert!(heard[0].distance_to(&view) <= PRIMED_FLEE_RANGE);

    // The human floor: the reflex acts on the third tick of hearing, the same
    // class as a seen creeper outside three blocks.
    sim.step();
    sim.step();
    assert_eq!(sim.engine.active(), None, "two ticks is 100 ms");
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Creeper));
    match sim.engine.active_action() {
        Some(ReflexAction::Flee { from, distance }) => {
            assert_eq!(from, sound, "the flee is away from the heard fuse");
            assert_eq!(distance, sim.settings.flee_distance);
        }
        other => panic!("expected the one creeper flee, got {other:?}"),
    }

    // The flee runs: the body ends up well away from where it heard the fuse.
    assert!(
        sim.run_until(90, |sim| sim.distance_to(sound) > 5.0),
        "the body never fled the heard creeper: {:?}",
        sim.body.position()
    );
}

#[test]
fn a_far_fuse_is_not_heard_and_does_not_flee() {
    let mut sim = fresh();
    // 17 blocks is outside the hearing radius (16).
    sim.add_sound(SoundEvent::EntityCreeperPrimed, Vec3::new(0.5, 64.0, -17.0));
    let view = capture(&mut sim);
    assert!(
        sim.engine.state().heard().is_empty(),
        "a sound outside the hearing radius is not a fact"
    );
    assert!(view.creeper.is_none());
    sim.run(5);
    assert_eq!(sim.engine.active(), None);
}

#[test]
fn an_old_fuse_is_not_heard_and_does_not_flee() {
    let mut sim = fresh();
    // The scripted world's own tick does not advance (no `ClientTickEnd`
    // packets), so a sound pushed after the perception tick has moved carries
    // tick 0 and is as old as the ticks already run: the age filter is the one
    // the live path uses.
    sim.run(HEARD_WINDOW_TICKS + 1);
    sim.add_sound(SoundEvent::EntityCreeperPrimed, Vec3::new(0.5, 64.0, -5.0));
    let view = capture(&mut sim);
    assert!(
        sim.engine.state().heard().is_empty(),
        "a sound older than the window is not a fact of now"
    );
    assert!(view.creeper.is_none());
    sim.run(5);
    assert_eq!(sim.engine.active(), None);
}

#[test]
fn a_fuse_is_no_cue_once_the_fuse_has_burned() {
    // The heard window is longer than the fuse, so without a cap the sound
    // would keep the creeper reflex alive ten ticks after the blast. The
    // creeper is never removed and never in sight (the wall hides it), so the
    // fuse's sound is the only thing that could make the reflex fire.
    let mut sim = fresh();
    wall_behind(&mut sim);
    let sound = Vec3::new(0.5, 64.0, -5.0);
    sim.add_mob(9, EntityKind::Creeper, sound);
    end_world_tick(&mut sim);
    sim.add_sound(SoundEvent::EntityCreeperPrimed, sound);
    let _ = capture(&mut sim);

    // Every tick of the burn is a cue, and the reflex would hold it.
    for age in 1..=FUSE_TICKS {
        end_world_tick(&mut sim);
        let view = capture(&mut sim);
        let heard = sim.engine.state().heard();
        assert_eq!(heard.len(), 1, "age {age}: {heard:?}");
        assert_eq!(heard[0].age_ticks, age, "the sound ages with the world");
        assert_eq!(heard[0].class, SoundClass::CreeperPrimed);
        assert!(view.creeper.is_none(), "the wall still hides the creeper");
        assert!(
            sim.engine
                .state()
                .primed_within(&view, PRIMED_FLEE_RANGE + f64::from(sim.settings.flee_distance))
                .is_some(),
            "age {age}: the fuse is still burning"
        );
    }

    // The tick after it, the creeper has exploded: no sound, no trigger, no
    // hold.
    end_world_tick(&mut sim);
    let view = capture(&mut sim);
    assert!(
        sim.engine.state().heard().is_empty(),
        "a fuse ten ticks past the blast: {:?}",
        sim.engine.state().heard()
    );
    assert!(
        sim.engine
            .state()
            .primed_within(&view, PRIMED_FLEE_RANGE + f64::from(sim.settings.flee_distance))
            .is_none()
    );
    sim.run(5);
    assert_eq!(sim.engine.active(), None, "nothing to flee from any more");
}

#[test]
fn an_explosion_cancels_the_fuse_it_ends() {
    let mut sim = fresh();
    wall_behind(&mut sim);
    let sound = Vec3::new(0.5, 64.0, -5.0);
    sim.add_mob(9, EntityKind::Creeper, sound);
    end_world_tick(&mut sim);
    sim.add_sound(SoundEvent::EntityCreeperPrimed, sound);
    let _ = capture(&mut sim);
    // Ten ticks of fuse, then the blast in the creeper's own place.
    for _ in 0..10 {
        end_world_tick(&mut sim);
        let _ = capture(&mut sim);
    }
    assert_eq!(sim.engine.state().heard().len(), 1, "the fuse is still live");
    end_world_tick(&mut sim);
    sim.add_sound(SoundEvent::EntityGenericExplode, sound);
    let view = capture(&mut sim);
    assert!(
        sim.engine.state().heard().is_empty(),
        "the explosion ends the fuse it belongs to: {:?}",
        sim.engine.state().heard()
    );
    assert!(
        sim.engine
            .state()
            .primed_within(&view, PRIMED_FLEE_RANGE + f64::from(sim.settings.flee_distance))
            .is_none()
    );

    // An explosion somewhere else, and one heard before the fuse, leave it
    // alone: only the blast at the fuse's place, after it, is its end.
    let mut far = fresh();
    wall_behind(&mut far);
    far.add_mob(9, EntityKind::Creeper, sound);
    end_world_tick(&mut far);
    far.add_sound(SoundEvent::EntityCreeperPrimed, sound);
    let _ = capture(&mut far);
    end_world_tick(&mut far);
    far.add_sound(SoundEvent::EntityGenericExplode, Vec3::new(0.5, 64.0, 8.0));
    let _ = capture(&mut far);
    assert_eq!(far.engine.state().heard().len(), 1, "another explosion");

    let mut later = fresh();
    wall_behind(&mut later);
    later.add_mob(9, EntityKind::Creeper, sound);
    end_world_tick(&mut later);
    later.add_sound(SoundEvent::EntityGenericExplode, sound);
    let _ = capture(&mut later);
    for _ in 0..5 {
        end_world_tick(&mut later);
        let _ = capture(&mut later);
    }
    end_world_tick(&mut later);
    later.add_sound(SoundEvent::EntityCreeperPrimed, sound);
    let view = capture(&mut later);
    assert!(view.creeper.is_none(), "the wall hides the creeper");
    let heard = later.engine.state().heard();
    assert_eq!(heard.len(), 1, "a new fuse is not the old one ending: {heard:?}");
    assert_eq!(heard[0].class, SoundClass::CreeperPrimed);
}

#[test]
fn the_bodys_own_sound_is_not_a_cue() {
    // A shot's positional sound sits on the shooter, and a `Sound` packet
    // names nobody: heard from inside the body's own eye it would read as a
    // hostile mob standing on the body.
    let mut sim = fresh();
    end_world_tick(&mut sim);
    sim.add_sound(SoundEvent::EntityArrowShoot, eye(FEET));
    let view = capture(&mut sim);
    assert!(view.melee.is_none(), "nothing was seen either");
    assert!(
        sim.engine.state().heard().is_empty(),
        "the body's own sound: {:?}",
        sim.engine.state().heard()
    );
    assert!(
        sim.engine.state_mut().alert(&view, 0, true).is_none(),
        "and it spends no alert turn"
    );
}

#[test]
fn a_hostile_sound_of_a_tracked_mob_does_not_turn() {
    let mut sim = fresh();
    // The zombie is in the open and in sight: its sound adds nothing.
    let position = Vec3::new(0.5, 64.0, 5.0);
    sim.add_mob(21, azalea_registry::builtin::EntityKind::Zombie, position);
    sim.add_entity_sound(SoundEvent::EntityZombieAmbient, 21);

    let view = capture(&mut sim);
    assert!(view.melee.is_some(), "the zombie is tracked by sight");
    let heard = sim.engine.state().heard();
    assert_eq!(heard.len(), 1, "{heard:?}");
    assert_eq!(heard[0].class, SoundClass::Hostile);
    assert_eq!(heard[0].entity, Some(21));
    assert!(
        sim.engine.state_mut().alert(&view, 0, true).is_none(),
        "a tracked source is the sight reflexes' business"
    );
}

#[test]
fn a_hostile_sound_behind_a_wall_turns_the_head() {
    let mut sim = fresh();
    wall_behind(&mut sim);
    // A zombie five blocks behind the wall: the server names it, but no ray
    // reaches it, so the view does not track it.
    let position = Vec3::new(0.5, 64.0, -5.0);
    sim.add_mob(21, EntityKind::Zombie, position);
    sim.add_entity_sound(SoundEvent::EntityZombieAmbient, 21);

    let view = capture(&mut sim);
    assert!(view.melee.is_none(), "the wall hides the zombie");
    let heard = sim.engine.state().heard();
    assert_eq!(heard.len(), 1, "{heard:?}");
    let alert = sim
        .engine
        .state_mut()
        .alert(&view, 0, true)
        .expect("an untracked hostile sound is a stimulus");
    assert!(alert.first, "the tick the turn starts logs the reaction");
    assert_eq!(alert.sound.id, "entity.zombie.ambient");
    // The turn aims at where the sound came from.
    let (yaw, pitch) = praxion_planner::aim(view.eye, position);
    assert_eq!(
        praxion_reflexes::alert_look(&view, &alert.sound),
        praxion_headless::Look::Aim { yaw, pitch }
    );
    // And the log line names the reaction and the sound.
    let note = praxion_reflexes::alert_note(&view, &alert.sound);
    assert!(
        note.starts_with("reflex_action kind=alert action=look target=21"),
        "{note}"
    );
    assert!(note.ends_with("heard=entity.zombie.ambient"), "{note}");
    // The turn is held for the gaze gesture, with the reaction logged once.
    let held = sim
        .engine
        .state_mut()
        .alert(&view, praxion_reflexes::ALERT_TURN_TICKS - 1, true)
        .expect("the turn still wants the same aim");
    assert!(!held.first);
    assert_eq!(held.sound, alert.sound);
    assert!(
        sim.engine
            .state_mut()
            .alert(&view, praxion_reflexes::ALERT_TURN_TICKS, true)
            .is_none(),
        "the turn ends and the cooldown holds the next one"
    );
}

#[test]
fn one_alert_turn_per_cooldown() {
    // A stepping mob makes the same sound every tick. The body looks once, for
    // the whole turn, and the cooldown then holds every repeat for 60 ticks.
    let mut sim = fresh();
    wall_behind(&mut sim);
    sim.add_mob(21, azalea_registry::builtin::EntityKind::Zombie, Vec3::new(0.5, 64.0, -5.0));

    let mut starts = Vec::new();
    let mut turn_ticks = 0;
    for tick in 0..130 {
        end_world_tick(&mut sim);
        sim.add_entity_sound(SoundEvent::EntityZombieAmbient, 21);
        let view = capture(&mut sim);
        if let Some(alert) = sim.engine.state_mut().alert(&view, tick, true) {
            if alert.first {
                starts.push(tick);
            }
            turn_ticks += 1;
        }
    }
    assert_eq!(starts, vec![0, 60, 120], "one turn per {ALERT_COOLDOWN_TICKS} ticks");
    // Each full turn asks for its aim for ALERT_TURN_TICKS; the last one is cut
    // by the end of the run.
    assert_eq!(turn_ticks, 30 + 30 + 10);
}

#[test]
fn a_busy_body_never_looks() {
    let mut sim = fresh();
    wall_behind(&mut sim);
    sim.add_mob(21, azalea_registry::builtin::EntityKind::Zombie, Vec3::new(0.5, 64.0, -5.0));
    sim.add_entity_sound(SoundEvent::EntityZombieAmbient, 21);
    let view = capture(&mut sim);

    // A command or a reflex holds the look: the glance yields and spends no
    // cooldown, so the sound is still available when the body is free again.
    assert!(sim.engine.state_mut().alert(&view, 0, false).is_none());
    assert!(sim.engine.state_mut().alert(&view, 0, true).is_some());
}

#[test]
fn an_old_hostile_sound_does_not_turn() {
    let mut sim = fresh();
    wall_behind(&mut sim);
    sim.add_mob(21, azalea_registry::builtin::EntityKind::Zombie, Vec3::new(0.5, 64.0, -5.0));
    sim.add_entity_sound(SoundEvent::EntityZombieAmbient, 21);
    sim.run(3);
    // Three steps on, the sound is still in the heard window but no longer a
    // fresh stimulus.
    let view = capture(&mut sim);
    assert_eq!(sim.engine.state().heard().len(), 1);
    assert!(sim.engine.state().heard()[0].age_ticks > 2);
    assert!(sim.engine.state_mut().alert(&view, 10, true).is_none());
}
