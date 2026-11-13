//! Rule selection of the real `get_cobblestone` skill: dig for stone where the
//! body stands, explore the surface only when the ground refuses the stair,
//! mine once stone is near. The skill files are loaded as shipped, so a change
//! to them is covered here.

// `common` reaches these through `super`.
#[allow(unused_imports)]
use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};
mod common;

use common::{Mock, core_program};

/// A body with a pickaxe, no stone near, nothing mined yet: the flat-terrain
/// start the dig exists for.
fn flat_terrain() -> Mock {
    let mut facts = Mock::default();
    facts.bools.insert("#pickaxes".into(), true);
    facts.bools.insert("stone".into(), false);
    facts.bools.insert("crafting_table".into(), false);
    facts.ints.insert("cobblestone".into(), 0);
    facts
}

#[test]
fn the_dig_comes_first_and_the_search_only_after_the_ground_refuses_it() {
    // Live run A (2026-11-12): the body walked three 1200-tick explores over
    // open ground before it dug a single block. Stone lies three to six blocks
    // under flat dirt, so the dig is the first move and the walk is the
    // fallback.
    let mut facts = flat_terrain();
    let mut act = Mock {
        duration: 1,
        continuous_outcome: Some(Outcome::Failed("not_found".into())),
        ..Mock::default()
    };
    // Flat ground with water next to the stair: the dig refuses it.
    act.outcomes
        .insert("dig_down".into(), Outcome::Failed("unsafe_drop".into()));
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("get_cobblestone", Vec::new()).unwrap();

    let mut explored_at = None;
    for tick in 0..40 {
        let decision = interp.tick(&mut facts, &mut act);
        assert!(
            !matches!(decision, Decision::Failed(_) | Decision::Done),
            "tick {tick}: the skill ended early: {decision:?}"
        );
        if !act.applies.is_empty() {
            explored_at = Some(tick);
            break;
        }
    }
    let explored_at = explored_at.expect("the search never took over from the dig");
    let digs = act
        .starts
        .iter()
        .filter(|a| a.primitive == "dig_down")
        .count();
    assert_eq!(
        digs, 3,
        "the dig ran to its cap before the first walk (tick {explored_at})"
    );
    assert!(
        act.starts[..digs.min(act.starts.len())]
            .iter()
            .all(|a| a.primitive == "dig_down"),
        "nothing was tried before the dig: {:?}",
        act.starts
    );
    assert_eq!(act.starts[0].args[0].name(), Some("stone"));
    assert!(act.applies.iter().all(|a| a.primitive == "explore"));
}

#[test]
fn a_confirmed_dig_hands_the_body_to_mine() {
    // The dig confirms (stone is now in reach) and the mine rule takes over:
    // no walk was ever started.
    let mut facts = flat_terrain();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("get_cobblestone", Vec::new()).unwrap();
    assert!(matches!(
        interp.tick(&mut facts, &mut act),
        Decision::Waiting(_) | Decision::Run { .. }
    ));

    let mut mined = false;
    for _ in 0..10 {
        // The staircase cut the stone into view.
        facts.bools.insert("stone".into(), true);
        interp.tick(&mut facts, &mut act);
        if act.starts.last().is_some_and(|a| a.primitive == "mine") {
            mined = true;
            break;
        }
    }
    assert!(mined, "the mine rule follows the dig: {:?}", act.starts);
    assert!(act.applies.is_empty(), "no explore before the stone was near");
}

#[test]
fn stone_in_reach_mines_without_exploring_or_digging() {
    let mut facts = flat_terrain();
    facts.bools.insert("stone".into(), true);
    let mut act = Mock::default();
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("get_cobblestone", Vec::new()).unwrap();
    interp.tick(&mut facts, &mut act);
    assert_eq!(act.starts.len(), 1);
    assert_eq!(act.starts[0].primitive, "mine");
    assert!(act.applies.is_empty());
}

#[test]
fn any_pickaxe_counts_not_only_the_wooden_one() {
    // A body that holds a stone pickaxe and no wooden one mines stone at once
    // instead of walking into the wood chain (`has(#pickaxes)`).
    let mut facts = flat_terrain();
    facts.bools.insert("stone".into(), true);
    let mut act = Mock::default();
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("get_cobblestone", Vec::new()).unwrap();
    interp.tick(&mut facts, &mut act);
    assert_eq!(
        act.starts[0].primitive, "mine",
        "a stone pickaxe mines stone: {:?}",
        act.starts
    );
}

#[test]
fn without_a_pickaxe_the_wood_chain_still_comes_first() {
    let mut facts = flat_terrain();
    facts.bools.insert("#pickaxes".into(), false);
    let mut act = Mock::default();
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("get_cobblestone", Vec::new()).unwrap();
    let decision = interp.tick(&mut facts, &mut act);
    assert!(
        matches!(decision, Decision::Waiting(_) | Decision::Run { .. }),
        "{decision:?}"
    );
    assert!(
        act.starts.iter().all(|a| a.primitive != "dig_down"),
        "no pickaxe, no dig: {:?}",
        act.starts
    );
}

#[test]
fn a_failing_dig_alternates_with_the_search_until_the_cap_ends_the_skill() {
    let mut facts = flat_terrain();
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
    for _ in 0..400 {
        if let Decision::Failed(kind) = interp.tick(&mut facts, &mut act) {
            failure = Some(kind);
            break;
        }
    }
    // The cap is per rule and the dig side runs first: after alternating, the
    // dig rule reaches three failed frames and the skill fails with the dig's
    // own kind, not with an endless search.
    assert_eq!(failure.as_deref(), Some("unsafe_drop"));
    let digs = act
        .starts
        .iter()
        .filter(|a| a.primitive == "dig_down")
        .count();
    let explores = act.applies.iter().filter(|a| a.primitive == "explore").count();
    assert_eq!(digs, 9, "three dig frames of three digs each");
    assert_eq!(
        explores, 6,
        "two search frames of three explores each, between the dig frames"
    );
}
