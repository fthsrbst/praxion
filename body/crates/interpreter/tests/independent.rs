//! Independent tests for the rule-list interpreter (step 7 review).
//!
//! Each test pins a sentence of architecture §6.1 or of the composition and
//! preemption paragraphs. The sentence is quoted in a comment above the test.

use std::collections::{BTreeMap, BTreeSet};

use praxion_rules::{ArgValue, FactCall, Program};

use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth, kleene_and,
    kleene_not, kleene_or,
};

#[derive(Clone, Debug)]
struct Mock {
    bools: BTreeMap<String, bool>,
    ints: BTreeMap<String, i64>,
    unknown: BTreeSet<String>,
    duration: u64,
    outcome: Option<Outcome>,
    flight: Option<(u64, Outcome)>,
    starts: Vec<StartAction>,
    applies: Vec<StartAction>,
    cancels: u32,
    releases: u32,
}

impl Default for Mock {
    fn default() -> Self {
        Self {
            bools: BTreeMap::new(),
            ints: BTreeMap::new(),
            unknown: BTreeSet::new(),
            duration: 1,
            outcome: None,
            flight: None,
            starts: Vec::new(),
            applies: Vec::new(),
            cancels: 0,
            releases: 0,
        }
    }
}

impl Facts for Mock {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        if self.unknown.contains(&call.name) {
            return Truth::Unknown(call.clone());
        }
        let value = match call.name.as_str() {
            "has" | "near" => call
                .args
                .first()
                .and_then(ArgValue::name)
                .and_then(|name| self.bools.get(name))
                .copied()
                .unwrap_or(false),
            _ => false,
        };
        if value {
            Truth::True
        } else {
            Truth::False
        }
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        if self.unknown.contains(&call.name) {
            return IntValue::Unknown(call.clone());
        }
        match call.name.as_str() {
            "count" => IntValue::Known(
                call.args
                    .first()
                    .and_then(ArgValue::name)
                    .and_then(|name| self.ints.get(name))
                    .copied()
                    .unwrap_or(0),
            ),
            _ => IntValue::Unknown(call.clone()),
        }
    }
}

impl Actuator for Mock {
    fn start(&mut self, action: &StartAction, tick: u64) -> u32 {
        self.starts.push(action.clone());
        let outcome = self.outcome.clone().unwrap_or(Outcome::Confirmed);
        self.flight = Some((tick + self.duration, outcome));
        self.duration as u32
    }

    fn poll(&mut self, tick: u64) -> Option<Outcome> {
        match &self.flight {
            Some((settle_at, _)) if tick >= *settle_at => {
                let (_, outcome) = self.flight.take().expect("checked above");
                Some(outcome)
            }
            _ => None,
        }
    }

    fn cancel(&mut self) -> Option<Outcome> {
        if self.flight.take().is_some() {
            self.cancels += 1;
            Some(Outcome::Preempted)
        } else {
            None
        }
    }

    fn apply(&mut self, action: &StartAction, _tick: u64) {
        self.applies.push(action.clone());
    }

    fn release(&mut self) {
        self.releases += 1;
    }
}

fn program(source: &str) -> Program {
    praxion_rules::load(source).unwrap_or_else(|errors| {
        panic!(
            "program did not load: {:?}",
            errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        )
    })
}

fn wait_ticks(decision: &Decision) -> i64 {
    match decision {
        Decision::Run { action, .. } if action.primitive == "wait" => {
            action.args[0].as_int().expect("wait takes an int")
        }
        other => panic!("expected a wait run, got {other:?}"),
    }
}

// §6.1 Kleene table:
// | a | b | a AND b | a OR b | NOT a |
// T/T -> T/T/F? (see table in §6.1)
#[test]
fn kleene_table_is_exhaustive_independent() {
    let unknown = || Truth::Unknown(FactCall { name: "x".into(), args: Vec::new() });
    let values = [Truth::True, unknown(), Truth::False];
    let table = [
        (0usize, 0usize, Truth::True, Truth::True),
        (0, 1, unknown(), Truth::True),
        (0, 2, Truth::False, Truth::True),
        (1, 0, unknown(), Truth::True),
        (1, 1, unknown(), unknown()),
        (1, 2, Truth::False, unknown()),
        (2, 0, Truth::False, Truth::True),
        (2, 1, Truth::False, unknown()),
        (2, 2, Truth::False, Truth::False),
    ];
    for (a, b, and, or) in table {
        assert_eq!(kleene_and(values[a].clone(), values[b].clone()), and, "AND {a},{b}");
        assert_eq!(kleene_or(values[a].clone(), values[b].clone()), or, "OR {a},{b}");
    }
    assert_eq!(kleene_not(Truth::True), Truth::False);
    assert_eq!(kleene_not(Truth::False), Truth::True);
    assert_eq!(kleene_not(unknown()), unknown());
}

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

// §6.1 step 2: "If a discrete action (dig, place, click) is in flight, guards
// are evaluated but no other rule takes over until the action settles (receipt
// or timeout)."
#[test]
fn step2_discrete_in_flight_blocks_switching() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 1 -> wait(2)\n    true -> mine(stone)\n",
    );
    let mut facts = Mock::default();
    facts.ints.insert("x".into(), 0);
    let mut act = Mock::default();
    act.duration = 3;
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Run { .. }));
    facts.ints.insert("x".into(), 1);
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Waiting(_)));
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Waiting(_)));
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 2);
}

// §6.1 step 3: "Otherwise the highest rule whose guard is T fires. A
// continuous action (walking a segment) is re-selected every tick, so a higher
// rule becoming T switches at once."
#[test]
fn step3_highest_true_fires_and_continuous_reselects() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 1 -> wait(1)\n    true -> wait(2)\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 2);
    facts.ints.insert("x".into(), 1);
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 1);
    assert_eq!(act.applies.len(), 2);
}

// §6.1 step 4: "A rule whose guard is U is not eligible. The highest such rule
// offers an observe(x) subgoal for its unknown fact, at most one per tick".
#[test]
fn step4_highest_unknown_offers_one_observe() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 3 -> mine(stone)\n    has(tool) -> mine(dirt)\n",
    );
    let mut facts = Mock::default();
    facts.unknown.insert("count".into());
    facts.unknown.insert("has".into());
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    let expected = FactCall { name: "count".into(), args: vec![ArgValue::Item("x".into())] };
    assert_eq!(interp.tick(&mut facts, &mut act), Decision::Observe(expected.clone()));
    assert_eq!(interp.tick(&mut facts, &mut act), Decision::Observe(expected));
    assert!(act.starts.is_empty() && act.applies.is_empty());
}

// §6.1 step 4 extra: a lower T rule fires before observing.
#[test]
fn step4_lower_true_fires_before_observing() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 3 -> mine(stone)\n    true -> wait(1)\n",
    );
    let mut facts = Mock::default();
    facts.unknown.insert("count".into());
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 1);
}

// §6.1 step 5: "If no rule is eligible, the result is failed(no_rule) and the
// brain is woken."
#[test]
fn step5_no_eligible_rule_fails_no_rule() {
    let program = program("skill s()\n  rules:\n    false -> done\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert_eq!(interp.tick(&mut facts, &mut act), Decision::Failed("no_rule".into()));
}

// `until` [D]: released when G turns T.
#[test]
fn until_released_exactly_when_goal_turns_true() {
    let program = program(
        "skill s()\n  rules:\n    count(y) >= 1 -> wait(2)\n    true -> wait(1) until count(x) >= 3\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 1);
    assert!(interp.until_active());
    facts.ints.insert("x".into(), 2);
    facts.ints.insert("y".into(), 1);
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 1);
    assert!(interp.until_active());
    facts.ints.insert("x".into(), 3);
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 2);
    assert!(!interp.until_active());
}

// `until` released when own guard falls.
#[test]
fn until_released_when_own_guard_falls() {
    let program = program(
        "skill s()\n  rules:\n    count(z) >= 1 -> wait(1) until count(x) >= 100\n    count(y) >= 1 -> wait(2)\n    true -> wait(3)\n",
    );
    let mut facts = Mock::default();
    facts.ints.insert("z".into(), 1);
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 1);
    assert!(interp.until_active());
    facts.ints.insert("z".into(), 0);
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 3);
    assert!(!interp.until_active());
}

// `until` bounded by max_ticks.
#[test]
fn until_bounded_by_max_ticks() {
    let program = program(
        "skill s()\n  rules:\n    count(y) >= 1 -> wait(2)\n    true -> wait(1) until count(x) >= 100 max_ticks 5\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 1);
    assert!(interp.until_active());
    facts.ints.insert("y".into(), 1);
    for _ in 2..=5 {
        assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 1);
        assert!(interp.until_active());
    }
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 2);
    assert!(!interp.until_active());
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

// Composition: "A child's failed(kind) makes the calling rule ineligible for
// that tick, so the parent falls to its next rule."
#[test]
fn child_failed_makes_calling_rule_ineligible_for_that_tick() {
    let program = program(
        r#"
skill child()
  rules:
    true -> fail(broken)
skill parent()
  rules:
    has(tool) -> do child()
    true -> wait(5)
"#,
    );
    let mut facts = Mock::default();
    facts.bools.insert("tool".into(), true);
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("parent", Vec::new()).unwrap();
    assert_eq!(wait_ticks(&interp.tick(&mut facts, &mut act)), 5);
    assert_eq!(interp.last_failure(), Some("broken"));
    assert_eq!(interp.frame_depth(), 1);
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

// Composition/preemption: "A preempted discrete action is abandoned, not
// resumed; its receipt settles preempted." and no duplicate start after resume.
#[test]
fn preemption_abandons_discrete_action_no_duplicate() {
    let program = program("skill s()\n  rules:\n    true -> mine(stone)\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interp = Interpreter::new(program);
    interp.start_skill("s", Vec::new()).unwrap();
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Run { .. }));
    assert_eq!(act.starts.len(), 1);
    assert_eq!(interp.preempt(&mut act), Some(Outcome::Preempted));
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Waiting(_)));
    interp.resume();
    assert!(matches!(interp.tick(&mut facts, &mut act), Decision::Run { .. }));
    assert_eq!(act.starts.len(), 2, "exactly one new start after resume");
    assert_eq!(act.cancels, 1);
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
