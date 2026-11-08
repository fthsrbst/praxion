//! Rule selection of the real `get_cobblestone` skill: explore for stone, dig
//! a staircase when exploring finds none, mine once stone is near. The skill
//! files are loaded as shipped, so a change to them is covered here.

use std::fs;
use std::path::PathBuf;

// `common` reaches these through `super`.
#[allow(unused_imports)]
use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};
use praxion_rules::{CORE_COMPOSITION_DEPTH, Program, load_files_with_depth};

mod common;

use common::Mock;

fn core_program() -> Program {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let files: Vec<(String, String)> = fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "skill"))
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, fs::read_to_string(&path).unwrap())
        })
        .collect();
    load_files_with_depth(&files, CORE_COMPOSITION_DEPTH).unwrap_or_else(|errors| {
        panic!(
            "{:?}",
            errors.iter().map(ToString::to_string).collect::<Vec<_>>()
        )
    })
}

#[test]
fn explore_fails_into_a_dig_without_oscillating_or_failing_the_skill() {
    let mut facts = Mock::default();
    facts.bools.insert("wooden_pickaxe".into(), true);
    facts.bools.insert("stone".into(), false);
    facts.bools.insert("crafting_table".into(), false);
    facts.ints.insert("cobblestone".into(), 0);
    let mut act = Mock {
        duration: 1,
        // Every explore ends `not_found`: flat terrain, stone never in sight.
        continuous_outcome: Some(Outcome::Failed("not_found".into())),
        ..Mock::default()
    };
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("get_cobblestone", Vec::new()).unwrap();

    // The search runs first and fails only after the cap inside find_stone.
    let mut dug_at = None;
    for tick in 0..40 {
        let decision = interp.tick(&mut facts, &mut act);
        assert!(
            !matches!(decision, Decision::Failed(_) | Decision::Done),
            "tick {tick}: the skill ended early: {decision:?}"
        );
        if !act.starts.is_empty() {
            dug_at = Some(tick);
            break;
        }
    }
    let dug_at = dug_at.expect("the search never gave way to the dig");
    assert_eq!(act.starts[0].primitive, "dig_down");
    assert_eq!(act.starts[0].args[0].name(), Some("stone"));
    assert!(!act.applies.is_empty(), "the body explored first");
    assert!(act.applies.iter().all(|a| a.primitive == "explore"));

    // A real dig takes a while: keep it in flight for four more ticks. The
    // interpreter evaluates no guard meanwhile, so nothing re-applies explore.
    act.flight = Some((interp.tick_count() + 5, Outcome::Confirmed));
    let applies_before = act.applies.len();
    for _ in 0..4 {
        let decision = interp.tick(&mut facts, &mut act);
        assert!(
            matches!(decision, Decision::Waiting(_)),
            "dig_down in flight blocks switching: {decision:?}"
        );
    }
    assert_eq!(
        act.applies.len(),
        applies_before,
        "no oscillation back to explore"
    );
    assert_eq!(
        act.starts.len(),
        1,
        "one dig, not a restart (tick {dug_at})"
    );

    // The dig confirms because stone is now near: the mine rule takes over.
    facts.bools.insert("stone".into(), true);
    let decision = interp.tick(&mut facts, &mut act);
    assert!(matches!(decision, Decision::Run { .. }), "{decision:?}");
    assert_eq!(act.starts.last().unwrap().primitive, "mine");
}

#[test]
fn stone_in_reach_mines_without_exploring_or_digging() {
    let mut facts = Mock::default();
    facts.bools.insert("wooden_pickaxe".into(), true);
    facts.bools.insert("stone".into(), true);
    facts.ints.insert("cobblestone".into(), 0);
    let mut act = Mock::default();
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("get_cobblestone", Vec::new()).unwrap();
    interp.tick(&mut facts, &mut act);
    assert_eq!(act.starts.len(), 1);
    assert_eq!(act.starts[0].primitive, "mine");
    assert!(act.applies.is_empty());
}

#[test]
fn a_failing_dig_alternates_with_the_search_until_the_cap_ends_the_skill() {
    let mut facts = Mock::default();
    facts.bools.insert("wooden_pickaxe".into(), true);
    facts.bools.insert("stone".into(), false);
    facts.ints.insert("cobblestone".into(), 0);
    let mut act = Mock {
        duration: 1,
        continuous_outcome: Some(Outcome::Failed("not_found".into())),
        ..Mock::default()
    };
    act.outcomes
        .insert("dig_down".into(), Outcome::Failed("unsafe_drop".into()));
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("get_cobblestone", Vec::new()).unwrap();

    let mut failure = None;
    for _ in 0..200 {
        if let Decision::Failed(kind) = interp.tick(&mut facts, &mut act) {
            failure = Some(kind);
            break;
        }
    }
    // The cap is per rule and the search rule hits it first: its third failed
    // frame ends the skill (not_found) before a third dig is tried.
    assert_eq!(failure.as_deref(), Some("not_found"));
    let digs = act
        .starts
        .iter()
        .filter(|a| a.primitive == "dig_down")
        .count();
    assert_eq!(
        digs, 2,
        "two dig attempts, then the cap: not an endless loop"
    );
}
