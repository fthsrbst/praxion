//! Rule selection of the survival core skills (`hunt`, `get_food`,
//! `cook_food`, `night_watch`, `take_shelter`) as shipped, over scripted
//! facts. Decision 2026-11-09 "Five next areas".
//!
//! `near_entity` answers unknown, never false, for a mob nobody sees (the
//! facts crate keeps three-valued answers), so a scripted fact that was not
//! set is unknown here too.

use std::collections::BTreeMap;

// `common` reaches these through `super`.
#[allow(unused_imports)]
use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};
use praxion_rules::{ArgValue, FactCall};

mod common;

use common::{Mock, core_program};

/// Facts answered from a table keyed by the call as text, e.g.
/// `near_entity(cow,24)`; `near_entity` without an entry is unknown.
#[derive(Default)]
struct Scripted {
    bools: BTreeMap<String, bool>,
    ints: BTreeMap<String, i64>,
}

fn key(call: &FactCall) -> String {
    let args: Vec<String> = call
        .args
        .iter()
        .map(|arg| match arg {
            ArgValue::Int(value) => value.to_string(),
            other => other.name().unwrap_or("?").to_owned(),
        })
        .collect();
    format!("{}({})", call.name, args.join(","))
}

impl Facts for Scripted {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        match self.bools.get(&key(call)) {
            Some(true) => Truth::True,
            Some(false) => Truth::False,
            None if call.name == "near_entity" => Truth::Unknown(call.clone()),
            // The allowlist itself is pinned in praxion-facts and gamedata; here
            // an unscripted `huntable` is the allowed case the tests play.
            None if call.name == "huntable" => Truth::True,
            None => Truth::False,
        }
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        IntValue::Known(self.ints.get(&key(call)).copied().unwrap_or(match call.name.as_str() {
            "health" => 20,
            _ => 0,
        }))
    }
}

impl Scripted {
    fn near(mut self, text: &str) -> Self {
        self.bools.insert(text.to_owned(), true);
        self
    }
    fn int(mut self, text: &str, value: i64) -> Self {
        self.ints.insert(text.to_owned(), value);
        self
    }
}

fn start(skill: &str, args: Vec<ArgValue>) -> Interpreter {
    let mut interp = Interpreter::new(core_program());
    interp.start_skill(skill, args).unwrap();
    interp
}

fn hunt(mob: &str, drop: &str) -> Interpreter {
    start(
        "hunt",
        vec![ArgValue::EntityKind(mob.into()), ArgValue::Item(drop.into())],
    )
}

/// The primitive the skill runs on its first decision, and its first argument.
fn first_action(interp: &mut Interpreter, facts: &mut Scripted) -> Decision {
    let mut actuator = Mock::default();
    // A skill that calls a child needs one tick to enter it.
    for _ in 0..4 {
        let decision = interp.tick(facts, &mut actuator);
        if !matches!(decision, Decision::Waiting(_)) {
            return decision;
        }
    }
    panic!("the skill never decided");
}

fn primitive(decision: &Decision) -> (String, Option<String>) {
    match decision {
        Decision::Run { action, .. } => (
            action.primitive.clone(),
            action.args.first().and_then(ArgValue::name).map(str::to_owned),
        ),
        other => panic!("expected a run, got {other:?}"),
    }
}

#[test]
fn hunt_retreats_at_eight_health_before_anything_else() {
    let mut facts = Scripted::default()
        .int("health()", 8)
        .near("near_entity(zombie,3)");
    let decision = first_action(&mut hunt("zombie", "rotten_flesh"), &mut facts);
    assert_eq!(decision, Decision::Failed("health_low".into()));
}

#[test]
fn hunt_attacks_a_mob_in_reach_then_approaches_then_picks_up() {
    let mut facts = Scripted::default().near("near_entity(cow,3)").near("near_entity(cow,24)");
    let decision = first_action(&mut hunt("cow", "beef"), &mut facts);
    assert_eq!(primitive(&decision), ("attack".into(), Some("cow".into())));

    let mut facts = Scripted::default().near("near_entity(cow,24)");
    let decision = first_action(&mut hunt("cow", "beef"), &mut facts);
    assert_eq!(primitive(&decision), ("move_to".into(), Some("cow".into())));

    // The mob is dead; the drop lies on the ground.
    let mut facts = Scripted::default().near("near_entity(item,8)");
    let decision = first_action(&mut hunt("cow", "beef"), &mut facts);
    assert_eq!(primitive(&decision), ("pickup".into(), Some("beef".into())));
}

#[test]
fn an_approach_beats_a_pickup_so_a_drop_cannot_pull_the_body_out_of_a_fight() {
    let mut facts = Scripted::default()
        .near("near_entity(zombie,24)")
        .near("near_entity(item,8)");
    let decision = first_action(&mut hunt("zombie", "rotten_flesh"), &mut facts);
    assert_eq!(primitive(&decision).0, "move_to");
}

#[test]
fn hunt_ends_when_no_mob_and_no_drop_are_in_sight() {
    // `near_entity` is unknown (nobody in sight), and an unknown guard never
    // fires: the last rule ends the call.
    let mut facts = Scripted::default();
    let decision = first_action(&mut hunt("cow", "beef"), &mut facts);
    assert_eq!(decision, Decision::Done);
}

#[test]
fn get_food_is_done_when_enough_food_is_held() {
    let mut facts = Scripted::default().int("count_food()", 2).near("near_entity(cow,24)");
    let decision = first_action(&mut start("get_food", vec![]), &mut facts);
    assert_eq!(decision, Decision::Done);
}

#[test]
fn get_food_hunts_the_animal_in_sight_cow_first() {
    let mut facts = Scripted::default()
        .near("near_entity(pig,24)")
        .near("near_entity(cow,24)")
        .near("near_entity(cow,3)");
    // cow in reach: hunt(cow, beef) attacks it.
    let decision = first_action(&mut start("get_food", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("attack".into(), Some("cow".into())));

    let mut facts = Scripted::default().near("near_entity(chicken,24)");
    let decision = first_action(&mut start("get_food", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("move_to".into(), Some("chicken".into())));
}

#[test]
fn get_food_explores_for_a_cow_when_no_animal_is_in_sight() {
    let mut facts = Scripted::default();
    let decision = first_action(&mut start("get_food", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("explore".into(), Some("cow".into())));
}

#[test]
fn cook_food_smelts_raw_meat_only_beside_a_furnace() {
    let mut facts = Scripted::default().int("count(beef)", 1);
    facts.bools.insert("near(furnace,4)".into(), false);
    assert_eq!(
        first_action(&mut start("cook_food", vec![]), &mut facts),
        Decision::Done,
        "no furnace in reach: nothing to do"
    );
    let mut facts = Scripted::default().int("count(beef)", 1).near("near(furnace,4)");
    let decision = first_action(&mut start("cook_food", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("smelt".into(), Some("cooked_beef".into())));
    let mut facts = Scripted::default().int("count(mutton)", 2).near("near(furnace,4)");
    let decision = first_action(&mut start("cook_food", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("smelt".into(), Some("cooked_mutton".into())));
}

#[test]
fn night_watch_ends_in_the_morning() {
    let mut facts = Scripted::default();
    // `is_night()` is false.
    assert_eq!(
        first_action(&mut start("night_watch", vec![]), &mut facts),
        Decision::Done
    );
}

fn night() -> Scripted {
    let mut facts = Scripted::default();
    facts.bools.insert("is_night()".into(), true);
    facts
}

#[test]
fn an_armed_healthy_body_hunts_the_hostile_in_sight_at_night() {
    let mut facts = night()
        .int("count(#swords)", 1)
        .near("near_entity(skeleton,16)")
        .near("near_entity(skeleton,24)");
    let decision = first_action(&mut start("night_watch", vec![]), &mut facts);
    // hunt(skeleton, bone): the skeleton is outside reach, so it approaches.
    assert_eq!(primitive(&decision), ("move_to".into(), Some("skeleton".into())));
}

#[test]
fn a_hurt_body_takes_shelter_instead_of_hunting() {
    let mut facts = night()
        .int("count(#swords)", 1)
        .int("health()", 10)
        .near("near_entity(zombie,16)")
        .near("near_entity(zombie,24)");
    let decision = first_action(&mut start("night_watch", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("dig_down".into(), Some("stone".into())));
}

#[test]
fn an_unarmed_body_takes_shelter_instead_of_hunting() {
    let mut facts = night()
        .int("count(#swords)", 0)
        .near("near_entity(zombie,16)")
        .near("near_entity(zombie,24)");
    let decision = first_action(&mut start("night_watch", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("dig_down".into(), Some("stone".into())));
}

#[test]
fn an_armed_healthy_body_waits_when_nothing_hostile_is_in_sight() {
    let mut facts = night().int("count(#swords)", 1);
    let decision = first_action(&mut start("night_watch", vec![]), &mut facts);
    assert_eq!(primitive(&decision).0, "wait");
}

#[test]
fn a_sheltered_body_waits_until_morning() {
    let mut facts = night();
    facts.bools.insert("near(stone,5)".into(), true);
    facts.bools.insert("near(stone,3)".into(), true);
    let decision = first_action(&mut start("take_shelter", vec![]), &mut facts);
    assert_eq!(primitive(&decision).0, "wait");
    let mut facts = Scripted::default();
    assert_eq!(
        first_action(&mut start("take_shelter", vec![]), &mut facts),
        Decision::Done,
        "the morning ends it"
    );
}

#[test]
fn take_shelter_does_not_dig_while_stone_lies_four_blocks_off() {
    // `dig_down` stops as soon as stone is known within five blocks
    // (`EXPLORE_NEAR_RADIUS`). On a hillside the nearest stone can be four
    // blocks off: `near(stone, 5)` true, `near(stone, 3)` false. A three-block
    // rule would fire `dig_down` every tick, each call confirming at once, for
    // the whole night (review finding 1). The rule must use the primitive's
    // threshold, so the body waits instead.
    let mut facts = night();
    facts.bools.insert("near(stone,5)".into(), true);
    facts.bools.insert("near(stone,3)".into(), false);
    let decision = first_action(&mut start("take_shelter", vec![]), &mut facts);
    assert_eq!(primitive(&decision).0, "wait");

    // Stone farther than that is not where `dig_down` would stop: it digs.
    let mut facts = night();
    facts.bools.insert("near(stone,5)".into(), false);
    facts.bools.insert("near(stone,3)".into(), false);
    let decision = first_action(&mut start("take_shelter", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("dig_down".into(), Some("stone".into())));
}
