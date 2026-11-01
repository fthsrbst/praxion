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
fn another_rule_firing_resets_the_failure_streak() {
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
    // A different rule fires, which resets the streak.
    facts.ints.insert("x".into(), 1);
    assert_eq!(wait_ticks(&started(&mut interpreter, &mut facts, &mut act)), 1);
    facts.ints.insert("x".into(), 0);
    // A fresh mine failure starts the count again, so two more do not fail.
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
