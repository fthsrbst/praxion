//! Rule selection of `get_wood`, `cut_log` and the axe skills as shipped
//! (decision 2026-11-09 "Five next areas"): a mine that cannot reach its log
//! hands over to `pillar` through the child skill, and an axe is crafted from
//! surplus planks.

// `common` reaches these through `super`.
#[allow(unused_imports)]
use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};
use praxion_rules::ArgValue;

mod common;

use common::{Mock, core_program};

fn start(skill: &str, args: Vec<ArgValue>) -> Interpreter {
    let mut interp = Interpreter::new(core_program());
    interp.start_skill(skill, args).unwrap();
    interp
}

/// Run the skill and return the primitives it started, in order, until it
/// ends or `ticks` pass.
fn run(
    interp: &mut Interpreter,
    facts: &mut Mock,
    actuator: &mut Mock,
    ticks: u64,
) -> (Vec<String>, Option<Decision>) {
    let mut ended = None;
    for _ in 0..ticks {
        let decision = interp.tick(facts, actuator);
        if matches!(decision, Decision::Done | Decision::Failed(_)) {
            ended = Some(decision);
            break;
        }
    }
    (
        actuator.starts.iter().map(|s| s.primitive.clone()).collect(),
        ended,
    )
}

fn logs_near() -> Mock {
    let mut facts = Mock::default();
    facts.bools.insert("#logs".into(), true);
    facts
}

#[test]
fn a_log_that_cannot_be_hit_is_answered_with_pillar_when_a_log_is_held() {
    let mut facts = logs_near();
    facts.ints.insert("#logs".into(), 1);
    let mut act = Mock::default();
    act.outcomes
        .insert("mine".into(), Outcome::Failed("out_of_reach".into()));
    // n = 5 so the held log does not end the skill.
    let mut interp = start("get_wood", vec![ArgValue::Int(5)]);
    let (started, _) = run(&mut interp, &mut facts, &mut act, 40);
    let first_pillar = started.iter().position(|p| p == "pillar").expect("it climbed");
    assert!(
        started[..first_pillar].iter().all(|p| p == "mine"),
        "mining was tried before the climb: {started:?}"
    );
    assert_eq!(act.starts[first_pillar].args[0].name(), Some("#logs"));
}

#[test]
fn with_no_log_to_climb_on_the_skill_fails_unreachable_at_once() {
    let mut facts = logs_near();
    let mut act = Mock::default();
    act.outcomes
        .insert("mine".into(), Outcome::Failed("out_of_reach".into()));
    let mut interp = start("get_wood", vec![ArgValue::Int(1)]);
    let (started, ended) = run(&mut interp, &mut facts, &mut act, 40);
    assert_eq!(ended, Some(Decision::Failed("unreachable".into())), "{started:?}");
    assert!(started.iter().all(|p| p == "mine"), "no pillar without a block: {started:?}");
}

#[test]
fn a_reachable_log_is_just_mined() {
    let mut facts = logs_near();
    let mut act = Mock::default();
    let mut interp = start("get_wood", vec![ArgValue::Int(1)]);
    let (started, _) = run(&mut interp, &mut facts, &mut act, 6);
    assert_eq!(started.first().map(String::as_str), Some("mine"));
    assert!(!started.iter().any(|p| p == "pillar"));
}

#[test]
fn an_axe_is_crafted_from_surplus_planks_beside_the_table_before_chopping() {
    let mut facts = logs_near();
    facts.ints.insert("#planks".into(), 6);
    facts.ints.insert("stick".into(), 2);
    facts.bools.insert("crafting_table".into(), true);
    let mut act = Mock::default();
    let mut interp = start("get_wood", vec![ArgValue::Int(1)]);
    let (started, _) = run(&mut interp, &mut facts, &mut act, 3);
    assert_eq!(started.first().map(String::as_str), Some("craft"));
    assert_eq!(act.starts[0].args[0].name(), Some("wooden_axe"));
}

#[test]
fn the_axe_waits_for_six_planks_an_empty_hand_slot_for_it_and_the_table() {
    let base = || {
        let mut facts = logs_near();
        facts.ints.insert("#planks".into(), 6);
        facts.ints.insert("stick".into(), 2);
        facts.bools.insert("crafting_table".into(), true);
        facts
    };
    // Five planks: the first pickaxe and table come first.
    let mut facts = base();
    facts.ints.insert("#planks".into(), 5);
    // No table near.
    let mut no_table = base();
    no_table.bools.insert("crafting_table".into(), false);
    // An axe is already held.
    let mut has_axe = base();
    has_axe.bools.insert("#axes".into(), true);
    for (name, mut facts) in [("five planks", facts), ("no table", no_table), ("has an axe", has_axe)] {
        let mut act = Mock::default();
        let mut interp = start("get_wood", vec![ArgValue::Int(1)]);
        let (started, _) = run(&mut interp, &mut facts, &mut act, 3);
        assert_eq!(started.first().map(String::as_str), Some("mine"), "{name}: {started:?}");
    }
}

#[test]
fn the_axe_skills_craft_when_they_have_the_materials_and_a_table() {
    let mut facts = Mock::default();
    facts.ints.insert("#planks".into(), 3);
    facts.ints.insert("stick".into(), 2);
    facts.bools.insert("crafting_table".into(), true);
    let mut act = Mock::default();
    let mut interp = start("get_wooden_axe", vec![]);
    let (started, _) = run(&mut interp, &mut facts, &mut act, 3);
    assert_eq!(started.first().map(String::as_str), Some("craft"));
    assert_eq!(act.starts[0].args[0].name(), Some("wooden_axe"));

    let mut facts = Mock::default();
    facts.ints.insert("cobblestone".into(), 3);
    facts.ints.insert("stick".into(), 2);
    facts.bools.insert("crafting_table".into(), true);
    let mut act = Mock::default();
    let mut interp = start("get_stone_axe", vec![]);
    let (started, _) = run(&mut interp, &mut facts, &mut act, 3);
    assert_eq!(started.first().map(String::as_str), Some("craft"));
    assert_eq!(act.starts[0].args[0].name(), Some("stone_axe"));
}
