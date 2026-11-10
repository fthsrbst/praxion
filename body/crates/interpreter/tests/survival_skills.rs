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
fn get_food_leaves_a_sealed_shelter_before_it_hunts() {
    // Run 9, third review: the critical-food interrupt started `get_food` in
    // a sealed shelter, which hunted from inside, saw nothing and failed with
    // the body still shut in. `get_food` now leaves first.
    let mut facts = Scripted::default().near("near_entity(cow,24)");
    facts.bools.insert("sheltered()".into(), true);
    let decision = first_action(&mut start("get_food", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("leave_shelter".into(), None));

    // Out of the shelter (the cover is gone): the same call hunts.
    let mut interp = start("get_food", vec![]);
    let mut facts = Scripted::default().near("near_entity(cow,24)");
    facts.bools.insert("sheltered()".into(), true);
    let mut actuator = Mock::default();
    actuator
        .outcomes
        .insert("leave_shelter".into(), Outcome::Confirmed);
    for _ in 0..20 {
        if actuator.starts.iter().any(|action| action.primitive == "leave_shelter") {
            facts.bools.insert("sheltered()".into(), false);
        }
        interp.tick(&mut facts, &mut actuator);
    }
    let starts: Vec<_> = actuator
        .starts
        .iter()
        .chain(&actuator.applies)
        .map(|action| action.primitive.as_str())
        .collect();
    assert_eq!(starts.first(), Some(&"leave_shelter"), "{starts:?}");
    assert!(starts.contains(&"move_to"), "then it went for the cow: {starts:?}");
}

#[test]
fn get_food_in_the_open_hunts_through_hunt_food() {
    let mut facts = Scripted::default().near("near_entity(cow,24)").near("near_entity(cow,3)");
    facts.bools.insert("sheltered()".into(), false);
    let decision = first_action(&mut start("get_food", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("attack".into(), Some("cow".into())));

    // `hunt_food` on its own: the same rules, the same goal check.
    let mut facts = Scripted::default().near("near_entity(sheep,24)");
    let decision = first_action(&mut start("hunt_food", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("move_to".into(), Some("sheep".into())));
    let mut facts = Scripted::default().int("count_food()", 3);
    let decision = first_action(&mut start("hunt_food", vec![ArgValue::Int(3)]), &mut facts);
    assert_eq!(decision, Decision::Done);
}

#[test]
fn get_food_passes_its_count_to_hunt_food() {
    // Three held, five wanted: `get_food(5)` must not stop at the default 2.
    let mut facts = Scripted::default().int("count_food()", 3);
    facts.bools.insert("sheltered()".into(), false);
    let decision = first_action(&mut start("get_food", vec![ArgValue::Int(5)]), &mut facts);
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
    let mut facts = Scripted::default().int("time_of_day()", 14000);
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
    assert_eq!(primitive(&decision), ("shelter".into(), None));
}

#[test]
fn an_unarmed_body_takes_shelter_instead_of_hunting() {
    let mut facts = night()
        .int("count(#swords)", 0)
        .near("near_entity(zombie,16)")
        .near("near_entity(zombie,24)");
    let decision = first_action(&mut start("night_watch", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("shelter".into(), None));
}

/// Tick `interp` `ticks` times against `facts` and `actuator`; every
/// primitive it ran, discrete (`starts`) and continuous (`applies`, the
/// `wait`s).
fn started(
    interp: &mut Interpreter,
    facts: &mut Scripted,
    actuator: &mut Mock,
    ticks: usize,
) -> Vec<String> {
    for _ in 0..ticks {
        if interp.finished() {
            break;
        }
        interp.tick(facts, actuator);
    }
    actuator
        .starts
        .iter()
        .chain(&actuator.applies)
        .map(|action| action.primitive.clone())
        .collect()
}

#[test]
fn an_unarmed_body_never_hunts_even_when_its_shelter_fails() {
    // Run 9: `take_shelter` failed, its calling rule was ineligible for that
    // tick, and the next true rule hunted the zombie in sight, unarmed.
    // Decision 2026-11-10 "Unarmed at night: avoid contact": every hunt rule
    // carries the armed-and-healthy guard, so a failed shelter falls through
    // to the wait.
    for (swords, health) in [(0, 20), (1, 10), (0, 6)] {
        let mut facts = night()
            .int("count(#swords)", swords)
            .int("health()", health)
            .near("near_entity(zombie,16)")
            .near("near_entity(zombie,24)")
            .near("near_entity(zombie,3)")
            .near("near_entity(skeleton,16)")
            .near("near_entity(spider,16)");
        let mut actuator = Mock::default();
        actuator
            .outcomes
            .insert("shelter".into(), Outcome::Failed("stuck".into()));
        let mut interp = start("night_watch", vec![]);
        let starts = started(&mut interp, &mut facts, &mut actuator, 60);
        assert!(
            starts.iter().any(|primitive| primitive == "shelter"),
            "swords {swords}, health {health}: it tried to shelter: {starts:?}"
        );
        assert!(
            starts.iter().all(|primitive| primitive != "move_to" && primitive != "attack"),
            "swords {swords}, health {health}: it went for a mob: {starts:?}"
        );
        assert!(
            starts.iter().any(|primitive| primitive == "wait"),
            "a failed shelter falls through to the wait: {starts:?}"
        );
    }
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
    facts.bools.insert("sheltered()".into(), true);
    let decision = first_action(&mut start("take_shelter", vec![]), &mut facts);
    assert_eq!(primitive(&decision).0, "wait");
    let mut facts = Scripted::default();
    facts.bools.insert("sheltered()".into(), false);
    assert_eq!(
        first_action(&mut start("take_shelter", vec![]), &mut facts),
        Decision::Done,
        "the morning ends it once the body is out"
    );
}

#[test]
fn a_body_still_sealed_in_at_morning_leaves_its_shelter_before_the_skill_ends() {
    // Decision 2026-11-10 "Shelter shape and exit": a body left sealed in at
    // dawn fails every ladder goal, so `take_shelter` opens the shelter itself.
    let mut facts = Scripted::default();
    facts.bools.insert("sheltered()".into(), true);
    let mut interp = start("take_shelter", vec![]);
    let decision = first_action(&mut interp, &mut facts);
    assert_eq!(primitive(&decision), ("leave_shelter".into(), None));

    // Out (the cover is gone, `sheltered()` is false): the skill is done.
    let mut actuator = Mock::default();
    let mut facts = Scripted::default();
    facts.bools.insert("sheltered()".into(), true);
    let mut interp = start("take_shelter", vec![]);
    let mut decisions = Vec::new();
    for _ in 0..8 {
        if interp.finished() {
            break;
        }
        let decision = interp.tick(&mut facts, &mut actuator);
        if actuator.starts.iter().any(|action| action.primitive == "leave_shelter") {
            facts.bools.insert("sheltered()".into(), false);
        }
        decisions.push(decision);
    }
    assert!(interp.finished(), "{decisions:?}");
    assert_eq!(decisions.last(), Some(&Decision::Done), "{decisions:?}");
    let starts: Vec<_> = actuator
        .starts
        .iter()
        .chain(&actuator.applies)
        .map(|action| action.primitive.as_str())
        .collect();
    assert_eq!(starts, ["leave_shelter"], "it left once, then ended");
}

#[test]
fn an_open_body_seals_itself_in_at_night() {
    // Decision 2026-11-10 "A sealed shelter now": not sheltered -> shelter.
    // The old staircase (`dig_down(stone)`) left the sky open (run 9 death 1).
    let mut facts = night();
    facts.bools.insert("sheltered()".into(), false);
    let decision = first_action(&mut start("take_shelter", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("shelter".into(), None));
}

#[test]
fn an_unknown_shelter_answer_builds_rather_than_waits_in_the_open() {
    // `sheltered()` unknown never fires its rule; the call falls to
    // `shelter()`, which confirms at once if the body is sealed after all.
    struct Unknown(Scripted);
    impl Facts for Unknown {
        fn bool_fact(&mut self, call: &FactCall) -> Truth {
            if call.name == "sheltered" {
                return Truth::Unknown(call.clone());
            }
            self.0.bool_fact(call)
        }
        fn int_fact(&mut self, call: &FactCall) -> IntValue {
            self.0.int_fact(call)
        }
    }
    let mut facts = Unknown(night());
    let mut interp = start("take_shelter", vec![]);
    let mut actuator = Mock::default();
    let decision = interp.tick(&mut facts, &mut actuator);
    assert_eq!(primitive(&decision), ("shelter".into(), None));
}

/// Facts where `sheltered()` cannot be answered.
struct ShelterUnknown(Scripted);

impl Facts for ShelterUnknown {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        if call.name == "sheltered" {
            return Truth::Unknown(call.clone());
        }
        self.0.bool_fact(call)
    }
    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        self.0.int_fact(call)
    }
}

#[test]
fn an_unknown_shelter_answer_in_the_morning_is_observed_not_done() {
    // Run 9 review item 2: `not is_night() -> done` ended the skill with the
    // body possibly still sealed in. Done needs a known open cell.
    let mut facts = ShelterUnknown(Scripted::default().int("time_of_day()", 1000));
    let mut interp = start("take_shelter", vec![]);
    let mut actuator = Mock::default();
    let decision = interp.tick(&mut facts, &mut actuator);
    match decision {
        Decision::Observe(fact) => assert_eq!(fact.name, "sheltered"),
        other => panic!("expected an observation of sheltered(), got {other:?}"),
    }
    assert!(!interp.finished());
}

#[test]
fn a_sealed_body_waits_between_the_end_of_night_and_sunrise() {
    // Run 9 review item 6: `is_night()` ends at 23000 while zombies, skeletons
    // and spiders are still out; the body leaves only from sunrise
    // (`time_of_day()` 0) to 12000.
    let mut facts = Scripted::default().int("time_of_day()", 23500);
    facts.bools.insert("sheltered()".into(), true);
    let decision = first_action(&mut start("take_shelter", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("wait".into(), None));

    let mut facts = Scripted::default().int("time_of_day()", 0);
    facts.bools.insert("sheltered()".into(), true);
    let decision = first_action(&mut start("take_shelter", vec![]), &mut facts);
    assert_eq!(primitive(&decision), ("leave_shelter".into(), None), "sunrise");

    // Out in the open at 23500 the skill ends, as `night_watch` does.
    let mut facts = Scripted::default().int("time_of_day()", 23500);
    facts.bools.insert("sheltered()".into(), false);
    assert_eq!(first_action(&mut start("take_shelter", vec![]), &mut facts), Decision::Done);
}
