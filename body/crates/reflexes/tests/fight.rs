//! The melee reflex after run 9 (decisions 2026-11-10 "The skill that started a
//! fight owns it" and "Unarmed at night: avoid contact"): engagement, the exit
//! range, the cooldown after a fight, renewal in place, preemption by a
//! higher-priority reflex, and the unarmed response.

mod common;

use azalea_registry::builtin::{EntityKind, ItemKind};
use praxion_reflexes::{
    MELEE_COOLDOWN_TICKS, ReflexAction, ReflexKind, ReflexState, Settings, action_note,
    sprint_outruns,
};
use praxion_world::Vec3;

use common::{Sim, test_settings};

fn fresh() -> Sim {
    Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5))
}

/// Move a mob by re-adding it under the same id.
fn put(sim: &mut Sim, id: i32, kind: EntityKind, pos: Vec3) {
    sim.remove_mob(id);
    sim.add_mob(id, kind, pos);
}

#[test]
fn the_melee_reflex_leaves_the_engaged_mob_to_the_skill() {
    let mut sim = fresh();
    sim.engine.set_engagement(Some("minecraft:zombie"));
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    sim.run(8);
    assert_eq!(sim.engine.active(), None, "the skill owns the fight it started");
    assert_eq!(sim.engine.state().visible_since(ReflexKind::Melee), None);

    // The skill lets go (another primitive): the reflex takes the mob after
    // its human floor.
    sim.engine.set_engagement(None);
    sim.run(3);
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee));
}

#[test]
fn an_engagement_never_holds_back_a_low_health_flee() {
    let mut sim = fresh();
    sim.add_item(36, ItemKind::StoneSword, 1);
    sim.set_health(6.0, 20);
    sim.engine.set_engagement(Some("zombie"));
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    sim.run(3);
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee));
    assert!(
        matches!(sim.engine.active_action(), Some(ReflexAction::Flee { .. })),
        "{:?}",
        sim.engine.active_action()
    );
}

#[test]
fn an_engagement_with_one_kind_leaves_another_kind_to_the_reflex() {
    let mut sim = fresh();
    sim.engine.set_engagement(Some("skeleton"));
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    sim.run(3);
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee));
}

#[test]
fn a_running_fight_holds_to_its_exit_range_then_cools_down() {
    let mut sim = fresh();
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    sim.run(3);
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee));

    // Past the trigger range (4) but inside the exit range (4 + 2): the run 9
    // reflex ended here and grabbed the mob back three ticks later.
    put(&mut sim, 2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 5.5));
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee), "inside the exit range");

    // Beyond the exit range: the fight ends.
    put(&mut sim, 2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 9.0));
    sim.step();
    assert_eq!(sim.engine.active(), None, "beyond the exit range");

    // Back inside the trigger range at once: the cooldown keeps the reflex out
    // for a second, so the skill underneath gets the body.
    put(&mut sim, 2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    let ended = sim.tick - 1;
    let mut restarted = None;
    for _ in 0..(MELEE_COOLDOWN_TICKS + 5) {
        sim.step();
        if sim.engine.active().is_some() {
            restarted = Some(sim.tick - 1);
            break;
        }
    }
    let restarted = restarted.expect("the reflex comes back after its cooldown");
    assert!(
        restarted >= ended + MELEE_COOLDOWN_TICKS,
        "ended at {ended}, restarted at {restarted}"
    );
}

#[test]
fn a_long_fight_renews_its_attack_window_in_place() {
    // Run 9: the 30-tick attack window ended the routine and the reflex
    // re-acquired one tick later, with a new generation and a new gaze
    // stimulus every time.
    let mut sim = fresh();
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
    sim.run(3);
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee));
    let generation = sim.body.generation();
    for _ in 0..80 {
        sim.step();
        assert_eq!(sim.engine.active(), Some(ReflexKind::Melee), "tick {}", sim.tick);
    }
    assert_eq!(sim.body.generation(), generation, "renewed in place, no new generation");
}

#[test]
fn an_attack_becomes_a_flee_in_place_when_health_falls() {
    let mut sim = fresh();
    sim.add_item(36, ItemKind::StoneSword, 1);
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
    sim.run(3);
    assert!(matches!(sim.engine.active_action(), Some(ReflexAction::Attack { .. })));
    let generation = sim.body.generation();
    sim.set_health(6.0, 20);
    sim.step();
    assert!(
        matches!(sim.engine.active_action(), Some(ReflexAction::Flee { .. })),
        "{:?}",
        sim.engine.active_action()
    );
    assert_eq!(sim.body.generation(), generation);
}

#[test]
fn a_creeper_preempts_a_running_fight() {
    let mut sim = fresh();
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
    sim.run(3);
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee));
    let generation = sim.body.generation();
    // Inside three blocks a creeper is lethal-fast: it acts on its first tick.
    sim.add_mob(3, EntityKind::Creeper, Vec3::new(-1.5, 64.0, 0.5));
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Creeper));
    assert!(sim.body.generation() > generation, "a new owner, a new generation");
}

#[test]
fn unarmed_at_low_health_fights_a_spider_and_flees_a_zombie() {
    // Run 9: an unarmed flee from a following spider did nothing.
    for (kind, flees) in [(EntityKind::Spider, false), (EntityKind::Zombie, true)] {
        let mut sim = fresh();
        sim.set_health(6.0, 20);
        sim.add_mob(2, kind, Vec3::new(0.5, 64.0, 3.0));
        sim.run(3);
        assert_eq!(sim.engine.active(), Some(ReflexKind::Melee), "{kind:?}");
        let action = sim.engine.active_action();
        assert_eq!(
            matches!(action, Some(ReflexAction::Flee { .. })),
            flees,
            "{kind:?}: {action:?}"
        );
        if !flees {
            assert!(
                matches!(action, Some(ReflexAction::Attack { weapon: None, .. })),
                "{kind:?}: {action:?}"
            );
        }
    }
}

#[test]
fn armed_at_low_health_still_flees_a_spider() {
    let mut sim = fresh();
    sim.add_item(36, ItemKind::StoneSword, 1);
    sim.set_health(6.0, 20);
    sim.add_mob(2, EntityKind::Spider, Vec3::new(0.5, 64.0, 3.0));
    sim.run(3);
    assert!(
        matches!(sim.engine.active_action(), Some(ReflexAction::Flee { .. })),
        "{:?}",
        sim.engine.active_action()
    );
}

#[test]
fn only_slow_mobs_are_outrun() {
    for kind in [EntityKind::Zombie, EntityKind::Husk, EntityKind::Drowned] {
        assert!(sprint_outruns(kind), "{kind:?}");
    }
    for kind in [EntityKind::Spider, EntityKind::Skeleton, EntityKind::Creeper] {
        assert!(!sprint_outruns(kind), "{kind:?}");
    }
}

#[test]
fn the_action_note_names_action_target_and_distance() {
    let mut sim = fresh();
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    sim.perception.begin_tick(&sim.world);
    let view = sim.view();
    let attack = ReflexAction::Attack {
        target: 2,
        weapon: None,
    };
    assert_eq!(
        action_note(ReflexKind::Melee, attack, &view, true),
        "reflex_action kind=melee action=attack target=2 distance=2.50 renewed=true"
    );
    let flee = ReflexAction::Flee {
        from: Vec3::new(0.5, 64.0, 3.0),
        distance: 8.0,
    };
    assert_eq!(
        action_note(ReflexKind::Melee, flee, &view, false),
        "reflex_action kind=melee action=flee target=2 distance=2.50 renewed=false"
    );
}

#[test]
fn an_engagement_is_namespace_blind_and_cleared_by_none() {
    let mut state = ReflexState::new();
    state.set_engagement(Some("minecraft:skeleton"));
    assert_eq!(state.engagement(), Some("skeleton"));
    state.set_engagement(None);
    assert_eq!(state.engagement(), None);
    let _ = Settings::default();
}

#[test]
fn an_engaged_mob_does_not_hide_a_second_mob_of_another_kind() {
    // hunt(zombie) is engaged with the zombie two blocks away; a spider at 3.5
    // is a different kind inside the trigger range. The nearest mob is the
    // engaged zombie, which once hid the spider from the melee trigger.
    let mut sim = fresh();
    sim.engine.set_engagement(Some("zombie"));
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.5));
    sim.add_mob(3, EntityKind::Spider, Vec3::new(0.5, 64.0, 4.0));
    sim.run(3);
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee));
    assert!(
        matches!(sim.engine.active_action(), Some(ReflexAction::Attack { target: 3, .. })),
        "the spider, not the engaged zombie: {:?}",
        sim.engine.active_action()
    );
    // The fight keeps going on the spider: it is not dropped next tick.
    sim.run(5);
    assert!(
        matches!(sim.engine.active_action(), Some(ReflexAction::Attack { target: 3, .. })),
        "{:?}",
        sim.engine.active_action()
    );
}

#[test]
fn an_axe_without_a_sword_counts_as_unarmed() {
    // "Armed" means a sword, as in night_watch and the brain (decision
    // 2026-11-10 "Unarmed at night: avoid contact"). With only an axe a flee
    // from a faster spider only turns the back to it, so the body fights back,
    // with the axe in hand.
    let mut sim = fresh();
    sim.add_item(36, ItemKind::WoodenAxe, 1);
    sim.set_health(6.0, 20);
    sim.add_mob(2, EntityKind::Spider, Vec3::new(0.5, 64.0, 3.0));
    sim.run(3);
    assert_eq!(
        sim.engine.active_action(),
        Some(ReflexAction::Attack {
            target: 2,
            weapon: Some(36),
        })
    );
}
