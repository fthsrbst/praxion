//! Rule selection of the real `find_ore` skill: search the surface, tunnel
//! when the search finds nothing, end once the ore is near. The skill files are
//! loaded as shipped, so a change to them is covered here.

// `common` reaches these through `super`.
#[allow(unused_imports)]
use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};
use praxion_rules::ArgValue;

mod common;

use common::{Mock, core_program};

fn start(ore: &str) -> Interpreter {
    let mut interp = Interpreter::new(core_program());
    interp
        .start_skill("find_ore", vec![ArgValue::Block(ore.into())])
        .unwrap();
    interp
}

fn searching_mock() -> Mock {
    Mock {
        duration: 1,
        // Every explore ends `not_found`: the ore is never in sight.
        continuous_outcome: Some(Outcome::Failed("not_found".into())),
        ..Mock::default()
    }
}

#[test]
fn a_failed_search_falls_through_to_branch_mine_without_oscillating() {
    let mut facts = Mock::default();
    facts.bools.insert("coal_ore".into(), false);
    let mut act = searching_mock();
    let mut interp = start("coal_ore");

    let mut tunnelled = false;
    for tick in 0..20 {
        let decision = interp.tick(&mut facts, &mut act);
        assert!(
            !matches!(decision, Decision::Failed(_) | Decision::Done),
            "tick {tick}: the skill ended early: {decision:?}"
        );
        if !act.starts.is_empty() {
            tunnelled = true;
            break;
        }
    }
    assert!(tunnelled, "the search never gave way to the tunnel");
    assert_eq!(act.starts[0].primitive, "branch_mine");
    assert_eq!(act.starts[0].args[0].name(), Some("coal_ore"));
    assert!(
        !act.applies.is_empty(),
        "the body searched the surface first"
    );
    assert!(act.applies.iter().all(|a| a.primitive == "explore"));

    // A real tunnel takes a while: keep it in flight. The interpreter
    // evaluates no guard meanwhile, so nothing re-applies explore.
    act.flight = Some((interp.tick_count() + 5, Outcome::Confirmed));
    let applies_before = act.applies.len();
    for _ in 0..4 {
        let decision = interp.tick(&mut facts, &mut act);
        assert!(matches!(decision, Decision::Waiting(_)), "{decision:?}");
    }
    assert_eq!(act.applies.len(), applies_before, "no oscillation");
    assert_eq!(act.starts.len(), 1, "one tunnel, not a restart");

    // The tunnel confirms because the ore is now near: the skill is done.
    facts.bools.insert("coal_ore".into(), true);
    let mut decision = interp.tick(&mut facts, &mut act);
    for _ in 0..3 {
        if decision == Decision::Done {
            break;
        }
        decision = interp.tick(&mut facts, &mut act);
    }
    assert_eq!(decision, Decision::Done);
}

#[test]
fn ore_in_reach_is_done_without_searching_or_tunnelling() {
    let mut facts = Mock::default();
    facts.bools.insert("iron_ore".into(), true);
    let mut act = Mock::default();
    let mut interp = start("iron_ore");
    assert_eq!(interp.tick(&mut facts, &mut act), Decision::Done);
    assert!(act.starts.is_empty());
    assert!(act.applies.is_empty());
}

#[test]
fn nothing_found_fails_not_found_instead_of_looping() {
    let mut facts = Mock::default();
    facts.bools.insert("copper_ore".into(), false);
    let mut act = searching_mock();
    act.outcomes
        .insert("branch_mine".into(), Outcome::Failed("not_found".into()));
    let mut interp = start("copper_ore");

    let mut failure = None;
    for _ in 0..200 {
        if let Decision::Failed(kind) = interp.tick(&mut facts, &mut act) {
            failure = Some(kind);
            break;
        }
    }
    assert_eq!(failure.as_deref(), Some("not_found"));
    let tunnels = act
        .starts
        .iter()
        .filter(|a| a.primitive == "branch_mine")
        .count();
    assert_eq!(tunnels, 2, "two tunnels, then the cap ends the skill");
}
