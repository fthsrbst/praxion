//! Rule selection of the real `go_to_surface` skill: done under open sky,
//! otherwise climb; a climb that fails `no_way_up` fails the skill with that
//! kind after the usual three tries. The skill files are loaded as shipped.

// `common` reaches these through `super`.
#[allow(unused_imports)]
use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};

mod common;

use common::{Mock, core_program};

fn start() -> Interpreter {
    let mut interp = Interpreter::new(core_program());
    interp.start_skill("go_to_surface", vec![]).unwrap();
    interp
}

#[test]
fn a_body_under_open_sky_is_done_without_climbing() {
    let mut facts = Mock::default();
    facts.bools.insert("sky_above".into(), true);
    let mut act = Mock::default();
    let mut interp = start();
    assert_eq!(interp.tick(&mut facts, &mut act), Decision::Done);
    assert!(act.starts.is_empty(), "no primitive started");
}

#[test]
fn a_body_under_rock_climbs_and_is_done_once_the_sky_is_over_it() {
    let mut facts = Mock::default();
    let mut act = Mock {
        duration: 1,
        ..Mock::default()
    };
    let mut interp = start();

    let mut climbing = false;
    for tick in 0..10 {
        let decision = interp.tick(&mut facts, &mut act);
        assert!(
            !matches!(decision, Decision::Failed(_) | Decision::Done),
            "tick {tick}: ended early: {decision:?}"
        );
        if !act.starts.is_empty() {
            climbing = true;
            break;
        }
    }
    assert!(climbing, "the skill never started the climb");
    assert_eq!(act.starts[0].primitive, "climb_out");
    assert!(act.starts[0].args.is_empty(), "climb_out takes no argument");

    // The climb confirms because the body now stands under open sky.
    facts.bools.insert("sky_above".into(), true);
    let mut decision = interp.tick(&mut facts, &mut act);
    for _ in 0..4 {
        if decision == Decision::Done {
            break;
        }
        decision = interp.tick(&mut facts, &mut act);
    }
    assert_eq!(decision, Decision::Done);
    assert_eq!(act.starts.len(), 1, "one climb, not a restart");
}

#[test]
fn a_climb_that_cannot_go_on_fails_no_way_up_after_three_tries() {
    let mut facts = Mock::default();
    let mut act = Mock {
        duration: 1,
        ..Mock::default()
    };
    act.outcomes
        .insert("climb_out".into(), Outcome::Failed("no_way_up".into()));
    let mut interp = start();

    let mut failure = None;
    for _ in 0..200 {
        if let Decision::Failed(kind) = interp.tick(&mut facts, &mut act) {
            failure = Some(kind);
            break;
        }
    }
    assert_eq!(failure.as_deref(), Some("no_way_up"));
    assert_eq!(act.starts.len(), 3, "three tries, then the cap ends the skill");
}
