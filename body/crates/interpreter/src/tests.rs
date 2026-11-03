//! Interpreter semantics (§6.1), against a small mock world.

use std::collections::{BTreeMap, BTreeSet};

use praxion_rules::{ArgValue, FactCall, Program};

use crate::{
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
    /// Per-primitive terminal outcomes, ahead of `outcome`, so a test can make
    /// two rules fail with different kinds.
    outcomes: BTreeMap<String, Outcome>,
    /// The terminal outcome a continuous action reports, for the tests of the
    /// explore/continuous path.
    continuous_outcome: Option<Outcome>,
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
            outcomes: BTreeMap::new(),
            continuous_outcome: None,
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
        let outcome = self
            .outcomes
            .get(&action.primitive)
            .cloned()
            .or_else(|| self.outcome.clone())
            .unwrap_or(Outcome::Confirmed);
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

    fn apply(&mut self, action: &StartAction, tick: u64) {
        self.applies.push(action.clone());
        if let Some(outcome) = self.continuous_outcome.clone() {
            self.flight = Some((tick + self.duration, outcome));
        }
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

fn started(interpreter: &mut Interpreter, facts: &mut Mock, actuator: &mut Mock) -> Decision {
    interpreter.tick(facts, actuator)
}

fn wait_ticks(decision: &Decision) -> i64 {
    match decision {
        Decision::Run { action, .. } if action.primitive == "wait" => {
            action.args[0].as_int().expect("wait takes an int")
        }
        other => panic!("expected a wait run, got {other:?}"),
    }
}

#[test]
fn kleene_table_is_exhaustive() {
    let unknown = || Truth::Unknown(FactCall {
        name: "x".into(),
        args: Vec::new(),
    });
    let values = [Truth::True, Truth::Unknown(FactCall {
        name: "x".into(),
        args: Vec::new(),
    }), Truth::False];
    // The §6.1 table, row by row: (a, b, a AND b, a OR b).
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
        assert_eq!(
            kleene_and(values[a].clone(), values[b].clone()),
            and,
            "AND row {a},{b}"
        );
        assert_eq!(
            kleene_or(values[a].clone(), values[b].clone()),
            or,
            "OR row {a},{b}"
        );
    }
    assert_eq!(kleene_not(Truth::True), Truth::False);
    assert_eq!(kleene_not(Truth::False), Truth::True);
    assert_eq!(kleene_not(unknown()), unknown());
}

#[test]
fn resume_equals_start() {
    let program = program("skill s()\n  rules:\n    true -> mine(stone)\n");
    let mut facts = Mock::default();

    let mut first_act = Mock::default();
    let mut first = Interpreter::new(program.clone());
    first.start_skill("s", Vec::new()).unwrap();
    let at_start = started(&mut first, &mut facts, &mut first_act);

    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();
    assert_eq!(started(&mut interpreter, &mut facts, &mut act), at_start);
    assert_eq!(
        interpreter.preempt(&mut act),
        Some(Outcome::Preempted),
        "the in-flight action settles preempted"
    );
    interpreter.resume();
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        at_start,
        "after resume the stack re-evaluates from the top"
    );
    assert_eq!(act.starts.len(), 2, "one start before and one after");
    assert_eq!(act.cancels, 1);
}

#[test]
fn setback_falls_and_luck_jumps() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 2 -> wait(9)\n    count(x) >= 1 -> wait(8)\n    true -> wait(7)\n",
    );
    let mut facts = Mock::default();
    facts.ints.insert("x".into(), 2);
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 9);
    facts.ints.insert("x".into(), 1);
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 8);
    facts.ints.insert("x".into(), 0);
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 7);
    facts.ints.insert("x".into(), 2);
    assert_eq!(
        wait_ticks(&started(&mut interpreter, &mut facts, &mut act)),
        9,
        "luck jumps back to the highest rule"
    );
}

#[test]
fn discrete_action_blocks_switching_until_settled() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 1 -> wait(2)\n    true -> mine(stone)\n",
    );
    let mut facts = Mock::default();
    facts.ints.insert("x".into(), 0);
    let mut act = Mock::default();
    act.duration = 3;
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Run { .. }
    ));
    facts.ints.insert("x".into(), 1);
    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Waiting(_)
    ));
    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Waiting(_)
    ));
    assert_eq!(
        wait_ticks(&started(&mut interpreter, &mut facts, &mut act)),
        2,
        "after the mine settles the higher rule takes over"
    );
}

#[test]
fn continuous_action_is_reselected_every_tick() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 1 -> wait(1)\n    true -> wait(2)\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 2);
    facts.ints.insert("x".into(), 1);
    assert_eq!(
        wait_ticks(&started(&mut interpreter, &mut facts, &mut act)),
        1,
        "a higher continuous rule switches on the next tick"
    );
    assert_eq!(act.applies.len(), 2);
}

#[test]
fn until_holds_and_is_bounded_by_max_ticks() {
    let program = program(
        "skill s()\n  rules:\n    count(y) >= 1 -> wait(2)\n    true -> wait(1) until count(x) >= 100 max_ticks 5\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
    assert!(interpreter.until_active());
    // A higher rule becomes true, but the commitment keeps the current rule.
    facts.ints.insert("y".into(), 1);
    for _ in 2..=5 {
        assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
        assert!(interpreter.until_active());
    }
    assert_eq!(
        wait_ticks(&started(&mut interpreter, &mut facts, &mut act)),
        2,
        "the commitment ends at max_ticks"
    );
    assert!(!interpreter.until_active());
}

#[test]
fn until_is_released_exactly_when_the_goal_turns_true() {
    let program = program(
        "skill s()\n  rules:\n    count(y) >= 1 -> wait(2)\n    true -> wait(1) until count(x) >= 3\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
    assert!(interpreter.until_active());
    // The goal is still false (2 < 3), so the commitment holds even when a
    // higher rule becomes true.
    facts.ints.insert("x".into(), 2);
    facts.ints.insert("y".into(), 1);
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
    assert!(interpreter.until_active());
    // The goal turns true: the commitment ends on this tick, not the next.
    facts.ints.insert("x".into(), 3);
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 2);
    assert!(!interpreter.until_active());
}

#[test]
fn until_is_released_when_the_own_guard_falls() {
    let program = program(
        "skill s()\n  rules:\n    count(z) >= 1 -> wait(1) until count(x) >= 100\n    count(y) >= 1 -> wait(2)\n    true -> wait(3)\n",
    );
    let mut facts = Mock::default();
    facts.ints.insert("z".into(), 1);
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
    assert!(interpreter.until_active());
    // The own guard falls even though the goal is still false.
    facts.ints.insert("z".into(), 0);
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 3);
    assert!(!interpreter.until_active());
}

#[test]
fn a_depth_four_program_loads_with_the_core_cap_and_runs() {
    let source = r#"
skill a()
  rules:
    true -> do b()
skill b()
  rules:
    true -> do c()
skill c()
  rules:
    true -> do d()
skill d()
  rules:
    true -> wait(1)
"#;
    assert!(
        praxion_rules::load(source).is_err(),
        "the default cap of 2 rejects this program"
    );
    let program =
        praxion_rules::load_with_depth(source, praxion_rules::CORE_COMPOSITION_DEPTH).unwrap();
    assert_eq!(program.max_depth(), 4);
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("a", Vec::new()).unwrap();
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
    assert_eq!(interpreter.frame_depth(), 4);
    assert_eq!(interpreter.current_skill(), Some("d"));
}

#[test]
fn unknown_guard_offers_one_observe() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 3 -> mine(stone)\n    has(tool) -> mine(dirt)\n",
    );
    let mut facts = Mock::default();
    facts.unknown.insert("count".into());
    facts.unknown.insert("has".into());
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    let expected = FactCall {
        name: "count".into(),
        args: vec![ArgValue::Item("x".into())],
    };
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Observe(expected.clone()),
        "the highest unknown rule is the one offered"
    );
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Observe(expected),
        "one observation per tick"
    );
    assert!(act.starts.is_empty() && act.applies.is_empty());
}

#[test]
fn a_lower_true_rule_fires_before_observing() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 3 -> mine(stone)\n    true -> wait(1)\n",
    );
    let mut facts = Mock::default();
    facts.unknown.insert("count".into());
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
}

#[test]
fn no_eligible_rule_fails() {
    let program = program("skill s()\n  rules:\n    false -> done\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Failed("no_rule".into())
    );
}

#[test]
fn child_failure_falls_to_the_parents_next_rule() {
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
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("parent", Vec::new()).unwrap();

    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 5);
    assert_eq!(interpreter.last_failure(), Some("broken"));
    assert_eq!(interpreter.frame_depth(), 1);
}

#[test]
fn explicit_fail_at_the_root_is_the_decision() {
    let program = program("skill s()\n  rules:\n    true -> fail(broken)\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Failed("broken".into())
    );
}

#[test]
fn three_consecutive_child_failures_fail_the_parent() {
    let program = program(
        r#"
skill child()
  rules:
    true -> fail(broken)
skill parent()
  rules:
    true -> do child()
"#,
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("parent", Vec::new()).unwrap();

    for _ in 0..2 {
        assert_eq!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Waiting("retry".into()),
            "the child fails once; the parent retries on the next tick"
        );
    }
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Failed("broken".into()),
        "the third consecutive child failure fails the parent with its kind"
    );
    assert_eq!(interpreter.last_failure(), Some("broken"));
    assert!(interpreter.finished());
}

#[test]
fn a_child_success_resets_the_parents_failure_streak() {
    let program = program(
        r#"
skill child()
  rules:
    has(ok) -> done
    true -> fail(broken)
skill parent()
  rules:
    true -> do child()
"#,
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("parent", Vec::new()).unwrap();

    // Two failures, then a success, then two failures: no parent failure.
    for _ in 0..2 {
        assert_eq!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Waiting("retry".into())
        );
    }
    facts.bools.insert("ok".into(), true);
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Waiting("child done".into())
    );
    facts.bools.insert("ok".into(), false);
    for _ in 0..2 {
        assert_eq!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Waiting("retry".into()),
            "the success reset the count, so two fresh failures are safe"
        );
    }
    assert!(!interpreter.finished());
}

#[test]
fn a_different_parent_rule_keeps_the_child_failure_count() {
    let program = program(
        r#"
skill child()
  rules:
    true -> fail(broken)
skill parent()
  rules:
    has(tool) -> do child()
    count(pause) >= 1 -> wait(5)
"#,
    );
    let mut facts = Mock::default();
    facts.bools.insert("tool".into(), true);
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("parent", Vec::new()).unwrap();

    // Two child failures, then the other rule fires for one tick.
    for _ in 0..2 {
        assert_eq!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Waiting("retry".into())
        );
    }
    facts.bools.insert("tool".into(), false);
    facts.ints.insert("pause".into(), 1);
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 5);
    // The other rule firing does not clear the child rule's count: the third
    // child failure fails the parent with the child's kind.
    facts.bools.insert("tool".into(), true);
    facts.ints.insert("pause".into(), 0);
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Failed("broken".into())
    );
    assert!(interpreter.finished());
}

#[test]
fn a_b_a_alternation_still_fails_the_repeating_rule() {
    // The live shape: a parent alternates two rules, each of whose children
    // fails `not_found`. Each rule keeps its own count, so A's third failure
    // fails the frame with A's kind even though B fired in between.
    let program = program(
        r#"
skill s()
  rules:
    count(phase) == 0 or count(phase) == 2 -> mine(stone)
    count(phase) == 1 -> place(dirt)
"#,
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    act.outcomes
        .insert("mine".into(), Outcome::Failed("no_block".into()));
    act.outcomes
        .insert("place".into(), Outcome::Failed("no_space".into()));
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    let phase = |facts: &mut Mock, value: i64| {
        facts.ints.insert("phase".into(), value);
    };
    let run_failing_rule = |facts: &mut Mock, act: &mut Mock, interpreter: &mut Interpreter| {
        assert!(
            matches!(started(interpreter, facts, act), Decision::Run { .. }),
            "the selected rule starts its primitive"
        );
        assert_eq!(
            started(interpreter, facts, act),
            Decision::Waiting("retry".into()),
            "the failed primitive is retried next tick"
        );
    };

    phase(&mut facts, 0);
    run_failing_rule(&mut facts, &mut act, &mut interpreter); // A: 1
    phase(&mut facts, 1);
    run_failing_rule(&mut facts, &mut act, &mut interpreter); // B: 1
    phase(&mut facts, 2);
    run_failing_rule(&mut facts, &mut act, &mut interpreter); // A: 2
    phase(&mut facts, 1);
    run_failing_rule(&mut facts, &mut act, &mut interpreter); // B: 2
    phase(&mut facts, 0);
    assert!(
        matches!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Run { .. }
        ),
        "A starts its third attempt"
    );
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Failed("no_block".into()),
        "A's third failure fails the frame, with A's kind"
    );
    assert!(interpreter.finished());
}

#[test]
fn a_child_no_rule_failure_counts_towards_the_parent_streak() {
    let program = program(
        r#"
skill child()
  rules:
    false -> done
skill parent()
  rules:
    true -> do child()
"#,
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("parent", Vec::new()).unwrap();

    for _ in 0..2 {
        assert_eq!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Waiting("retry".into())
        );
    }
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Failed("no_rule".into())
    );
}

#[test]
fn primitive_failure_retries_on_the_next_tick() {
    let program = program("skill s()\n  rules:\n    true -> mine(stone)\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    act.outcome = Some(Outcome::Failed("no_block".into()));
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Run { .. }
    ));
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Waiting("retry".into()),
        "the failed primitive is not retried in the same tick"
    );
    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Run { .. }
    ));
    assert_eq!(interpreter.last_failure(), Some("no_block"));
}

#[test]
fn three_consecutive_primitive_failures_fail_the_skill() {
    let program = program("skill s()\n  rules:\n    true -> mine(stone)\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    act.outcome = Some(Outcome::Failed("no_block".into()));
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    for _ in 0..2 {
        assert!(matches!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Run { .. }
        ));
        assert_eq!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Waiting("retry".into())
        );
    }
    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Run { .. }
    ));
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Failed("no_block".into()),
        "the third consecutive failure fails the skill with the last kind"
    );
    assert_eq!(interpreter.last_failure(), Some("no_block"));
}

#[test]
fn another_rule_firing_keeps_the_failure_count() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 1 -> wait(1)\n    true -> mine(stone)\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    act.outcome = Some(Outcome::Failed("no_block".into()));
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    // Two failures of the mine rule.
    for _ in 0..2 {
        assert!(matches!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Run { .. }
        ));
        assert_eq!(
            started(&mut interpreter, &mut facts, &mut act),
            Decision::Waiting("retry".into())
        );
    }
    // A different rule fires for one tick; it does not reset mine's count.
    facts.ints.insert("x".into(), 1);
    act.outcome = None;
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
    facts.ints.insert("x".into(), 0);
    act.outcome = Some(Outcome::Failed("no_block".into()));
    // The wait settles, then mine's third failure fails the skill.
    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Run { .. }
    ));
    assert_eq!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Failed("no_block".into()),
        "the count survived the other rule firing"
    );
}

#[test]
fn preemption_abandons_the_action_and_starts_no_duplicate() {
    let program = program("skill s()\n  rules:\n    true -> mine(stone)\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Run { .. }
    ));
    assert_eq!(act.starts.len(), 1);
    assert_eq!(
        interpreter.preempt(&mut act),
        Some(Outcome::Preempted)
    );
    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Waiting(_)
    ));
    interpreter.resume();
    assert!(matches!(
        started(&mut interpreter, &mut facts, &mut act),
        Decision::Run { .. }
    ));
    assert_eq!(act.starts.len(), 2, "exactly one new start after resume");
    assert_eq!(act.cancels, 1);
}

#[test]
fn start_fills_defaults_and_resolves_parameters() {
    let program = program(
        "skill s(target: block, n: int = 3)\n  rules:\n    true -> mine($target) until count(oak_log) >= $n\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter
        .start_skill("s", vec![ArgValue::Block("stone".into())])
        .unwrap();
    match started(&mut interpreter, &mut facts, &mut act) {
        Decision::Run { action, .. } => {
            assert_eq!(action.primitive, "mine");
            assert_eq!(action.args, vec![ArgValue::Block("stone".into())]);
            assert!(interpreter.until_active());
        }
        other => panic!("expected a run, got {other:?}"),
    }
}

#[test]
fn a_terminal_continuous_failure_counts_toward_the_rule_streak() {
    // `explore` is continuous and reports only when it actually ends. Before
    // the fix its `failed(not_found)` was dropped, so the skill restarted the
    // same 1200-tick search forever.
    let program = program("skill s()\n  rules:\n    true -> explore(oak_log)\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    act.continuous_outcome = Some(Outcome::Failed("not_found".into()));
    act.duration = 1;
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();

    let mut decisions = Vec::new();
    for _ in 0..20 {
        let decision = interpreter.tick(&mut facts, &mut act);
        decisions.push(decision.clone());
        if let Decision::Failed(kind) = decision {
            assert_eq!(kind, "not_found");
            return;
        }
    }
    panic!("the skill never failed: {decisions:?}");
}

#[test]
fn a_continuous_success_clears_the_streak() {
    let program = program(
        "skill s()\n  rules:\n    count(x) >= 1 -> wait(1)\n    true -> explore(oak_log)\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    act.continuous_outcome = Some(Outcome::Failed("not_found".into()));
    act.duration = 1;
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();
    // Two failures, below the limit.
    for _ in 0..4 {
        let _ = interpreter.tick(&mut facts, &mut act);
    }
    act.continuous_outcome = Some(Outcome::Confirmed);
    for _ in 0..2 {
        let _ = interpreter.tick(&mut facts, &mut act);
    }
    assert!(
        !interpreter.finished(),
        "a confirmed continuous action resets the streak"
    );
}

#[test]
fn frame_events_cover_push_done_and_failure() {
    let program = program(
        "skill child()\n  rules:\n    not near(oak_log, 5) -> fail(no_wood)\n    true -> done\nskill s()\n  rules:\n    true -> do child()\n",
    );
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.set_max_primitive_failures(1);
    interpreter.start_skill("s", Vec::new()).unwrap();
    let start_events = interpreter.take_events();
    assert!(
        matches!(
            start_events.as_slice(),
            [crate::InterpreterEvent::FramePushed { skill, depth: 1, .. }] if skill == "s"
        ),
        "root push: {start_events:?}"
    );
    // `near` is false by default in the mock, so the child fails at once.
    let decision = started(&mut interpreter, &mut facts, &mut act);
    assert!(matches!(decision, Decision::Failed(ref kind) if kind == "no_wood"), "{decision:?}");
    let events = interpreter.take_events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            crate::InterpreterEvent::FramePushed { skill, depth: 2, .. } if skill == "child"
        )),
        "child push: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            crate::InterpreterEvent::FrameFailed { skill, depth: 2, kind } if skill == "child" && kind == "no_wood"
        )),
        "child failure: {events:?}"
    );
    assert!(
        events.iter().any(|event| matches!(
            event,
            crate::InterpreterEvent::FrameFailed { skill, depth: 1, kind } if skill == "s" && kind == "no_wood"
        )),
        "parent failure: {events:?}"
    );
    assert_eq!(interpreter.last_rule(), Some(("child", 0)));
}

#[test]
fn a_root_done_emits_frame_done() {
    let program = program("skill s()\n  rules:\n    true -> done\n");
    let mut facts = Mock::default();
    let mut act = Mock::default();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("s", Vec::new()).unwrap();
    let _ = interpreter.take_events();
    let decision = started(&mut interpreter, &mut facts, &mut act);
    assert!(matches!(decision, Decision::Done), "{decision:?}");
    let events = interpreter.take_events();
    assert!(
        events.iter().any(|event| matches!(
            event,
            crate::InterpreterEvent::FrameDone { skill, depth: 1 } if skill == "s"
        )),
        "root done: {events:?}"
    );
}
