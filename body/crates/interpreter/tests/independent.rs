//! Independent tests for the rule-list interpreter (step 7 review).
//!
//! Each test pins a sentence of architecture §6.1 or of the composition and
//! preemption paragraphs. The sentence is quoted in a comment above the test.
//! Shared fixtures live in `common`; the tests here assert angles the unit
//! suite does not.

use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};

mod common;

use common::*;

// §6.1 step 1: "A reflex or a take-over preempts."
#[test]
fn step1_preemption_suspends_and_abandons_in_flight() {
    let program = program("skill s()\n  rules:\n    true -> mine(stone)\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Run { .. }));
    assert_eq!(act.starts.len(), 1);
    assert_eq!(interp.preempt(&mut act), Some(Outcome::Preempted));
    assert!(interp.suspended());
    interp.resume();
    assert!(!interp.suspended());
}

// Composition: "each running skill is a frame on a call stack; the deepest
// frame with a T guard fires."
#[test]
fn composition_deepest_frame_fires() {
    let program = program(
        r#"
skill child()
  rules:
    true -> wait(2)
skill parent()
  rules:
    true -> do child()
"#,
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("parent", Vec::new()).unwrap();
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 2);
    assert_eq!(interp.frame_depth(), 2);
}

// Composition/preemption: "A preemption suspends the whole stack; afterwards
// every frame re-evaluates from the top."
#[test]
fn preemption_resumes_from_top() {
    let program = program(
        r#"
skill child()
  rules:
    true -> mine(stone)
skill parent()
  rules:
    true -> do child()
"#,
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("parent", Vec::new()).unwrap();
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Run { .. }));
    assert_eq!(interp.preempt(&mut act), Some(Outcome::Preempted));
    interp.resume();
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Run { .. }));
}

// Property: random fact flips every tick must not panic or start two discrete
// actions in one tick.
#[test]
fn property_fact_flips_never_panic_or_double_start() {
    for seed in 0u64..300 {
        let program = program(
            "skill s()\n  rules:\n    count(x) >= 2 -> wait(9)\n    count(x) >= 1 -> mine(stone)\n    true -> wait(7)\n",
        );
        let mut facts = Mock::default();
        let mut act = Mock::default();
        act.duration = 2;
        let mut interp = Interpreter::new(program);
        interp.start_skill("s", Vec::new()).unwrap();
        let mut rng = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut prev_starts = act.starts.len();
        for _ in 0..80 {
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            if rng % 2 == 0 {
                facts.ints.insert("x".into(), (rng % 4) as i64);
            } else {
                facts.ints.remove("x");
            }
            rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
            if rng % 3 == 0 {
                facts.unknown.insert("count".into());
            } else {
                facts.unknown.remove("count");
            }
            let _ = interp.tick(&mut facts, &mut act);
            assert!(
                act.starts.len() - prev_starts <= 1,
                "seed {seed}: two discrete starts in one tick"
            );
            prev_starts = act.starts.len();
        }
    }
}

// Property: random preemption points never duplicate a discrete action.
#[test]
fn property_random_preemption_no_duplicate() {
    for seed in 0u64..300 {
        let program = program("skill s()\n  rules:\n    true -> mine(stone)\n");
        let mut facts = Mock::default();
        let mut act = Mock::default();
        act.duration = 3;
        let mut interp = Interpreter::new(program);
        interp.start_skill("s", Vec::new()).unwrap();
        assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Run { .. }));
        assert_eq!(act.starts.len(), 1);
        let pt = ((seed % 2) + 1) as usize; // 1 or 2 ticks while in flight
        for _ in 0..pt {
            let _ = interp.tick(&mut facts, &mut act);
        }
        assert_eq!(interp.preempt(&mut act), Some(Outcome::Preempted));
        interp.resume();
        assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Run { .. }));
        assert_eq!(act.starts.len(), 2, "seed {seed}: duplicate start");
        assert_eq!(act.cancels, 1);
    }
}
