//! The interpreter state machine.

use std::collections::BTreeMap;

use praxion_rules::{
    ActionKind, ArgType, ArgValue, CmpOp, Expr, ExprKind, FactCall, PrimitiveKind, Program,
    ValueType,
};

use crate::{
    Actuator, Decision, Facts, FrameRef, IntValue, InterpreterEvent, Outcome, RunAction, StartAction,
    Truth, kleene_and, kleene_not, kleene_or,
};

/// `max_ticks` used when a rule has none and its action is not a primitive
/// (a `do`, `done` or `fail`).
pub const DEFAULT_MAX_TICKS: u32 = 1200;

/// Consecutive failures of the same rule, primitive or child skill, before that
/// skill fails with the last failure kind. Each rule in a frame keeps its own
/// count, and only that rule's own success clears it.
pub const MAX_PRIMITIVE_FAILURES: u32 = 3;

/// Consecutive preemptions of one rule's in-flight discrete action, with no
/// success of that rule in between, before that skill fails `no_progress`.
///
/// A preemption is not a failure: the reflex ends and the skill re-evaluates
/// from the top, and [`Interpreter::resume`] drops the per-rule failure counts
/// so the interrupted skill gets its counts back. That is right for a reflex
/// that cost the body nothing, and wrong for the loop it hid: a `mine` or a
/// `dig_down` under water was preempted by the drowning reflex, restarted from
/// zero on the very same block, and preempted again, eight times, until the
/// goal's budget ran out (live run A, 2026-11-12). The count lives in the rule
/// for the same reason [`MAX_PRIMITIVE_FAILURES`] does: it covers every
/// discrete primitive, not only the dig that was measured, and only a success
/// of that rule clears it (another rule firing in between does not).
/// `resume` keeps it, because it is the only record of a loop that survives a
/// reflex.
pub const MAX_PREEMPTIONS: u32 = 3;

/// The failure kind of a rule that keeps restarting without progress.
pub const NO_PROGRESS: &str = "no_progress";

/// Guards a selection loop against a pathological ruleset. The load-time
/// depth cap makes this unreachable for a checked program.
const MAX_SELECTION_STEPS: usize = 64;

/// The failure kind a primitive reports when it runs out of ticks.
const TIMEOUT_KIND: &str = "timeout";

#[derive(Clone, Debug)]
struct Frame {
    skill: String,
    /// Resolved parameters, defaults included, in declaration order.
    args: Vec<ArgValue>,
    /// The rule in the parent that called this skill.
    call_rule: Option<usize>,
    /// A rule that failed this tick and may not fire again until the next one.
    /// Set by a child skill's `failed(kind)`.
    ineligible: Option<usize>,
    /// A primitive or child of this rule failed this tick; the rule retries
    /// next tick instead of the skill failing. Kept apart from `ineligible`
    /// because a primitive failure sets only this one: the same rule stays the
    /// highest true rule and the frame waits for its retry.
    retry: Option<usize>,
    /// Consecutive failures per rule index, primitive or child skill. A rule's
    /// own success clears only its entry; another rule firing leaves it alone.
    failures: BTreeMap<usize, u32>,
    /// Consecutive preemptions per rule index of an action that was in flight.
    /// Kept apart from `failures` because a preemption is not a failure: it
    /// survives [`Interpreter::resume`], and when it reaches
    /// [`MAX_PREEMPTIONS`] the skill fails `no_progress`.
    preemptions: BTreeMap<usize, u32>,
}

#[derive(Clone, Debug)]
struct InFlight {
    depth: usize,
    rule: usize,
    primitive: String,
    deadline: u64,
}

/// The continuous primitive currently selected. Continuous primitives report
/// only their terminal outcome (the movement receipt in between is not an
/// outcome), so the interpreter keeps the rule and depth to attribute a
/// failure to the same per-rule count a discrete action uses.
#[derive(Clone, Debug)]
struct Continuous {
    depth: usize,
    rule: usize,
}

#[derive(Clone, Copy, Debug)]
struct Until {
    depth: usize,
    rule: usize,
    deadline: u64,
}

/// The interpreter. One per running skill stack.
#[derive(Clone, Debug)]
pub struct Interpreter {
    program: Program,
    stack: Vec<Frame>,
    in_flight: Option<InFlight>,
    until: Option<Until>,
    suspended: bool,
    finished: bool,
    continuous: bool,
    /// The selected continuous primitive, for terminal-outcome attribution.
    continuous_action: Option<Continuous>,
    tick: u64,
    max_primitive_failures: u32,
    last_failure: Option<String>,
    /// The last rule that fired, for the driver's change-only `rule_selected`.
    last_rule: Option<(String, usize)>,
    /// Whether a rule fired during the current `tick` call. A frame pop or a
    /// waiting tick does not re-report an old selection.
    rule_fired_this_tick: bool,
    /// Bounded transitions since the driver last drained them.
    events: Vec<InterpreterEvent>,
}

impl Interpreter {
    pub fn new(program: Program) -> Self {
        Self {
            program,
            stack: Vec::new(),
            in_flight: None,
            until: None,
            suspended: false,
            finished: false,
            continuous: false,
            continuous_action: None,
            tick: 0,
            max_primitive_failures: MAX_PRIMITIVE_FAILURES,
            last_failure: None,
            last_rule: None,
            rule_fired_this_tick: false,
            events: Vec::new(),
        }
    }

    /// Changes how many consecutive primitive failures a rule may collect
    /// before its skill fails. The default is [`MAX_PRIMITIVE_FAILURES`].
    pub fn set_max_primitive_failures(&mut self, max: u32) {
        self.max_primitive_failures = max.max(1);
    }

    pub fn program(&self) -> &Program {
        &self.program
    }

    /// Replaces the program a port session reloaded from disk. Every frame is
    /// dropped: the old stack names skills the new program may not hold. The
    /// caller reloads only while no intent owns the body, and the next
    /// `start_skill` builds a fresh stack.
    pub fn replace_program(&mut self, program: Program) {
        let max_primitive_failures = self.max_primitive_failures;
        *self = Self::new(program);
        self.max_primitive_failures = max_primitive_failures;
    }

    /// Pushes the root frame. `args` are positional; defaults fill the rest.
    pub fn start_skill(&mut self, name: &str, mut args: Vec<ArgValue>) -> Result<(), String> {
        let skill = self
            .program
            .skill(name)
            .ok_or_else(|| format!("unknown skill `{name}`"))?;
        if args.len() < skill.required_params() || args.len() > skill.params.len() {
            return Err(format!(
                "skill `{name}` takes {} to {} arguments, got {}",
                skill.required_params(),
                skill.params.len(),
                args.len()
            ));
        }
        for param in &skill.params[args.len()..] {
            let value = param
                .default
                .clone()
                .ok_or_else(|| format!("missing argument `{}`", param.name))?;
            args.push(value);
        }
        self.stack.clear();
        self.stack.push(Frame {
            skill: name.to_string(),
            args,
            call_rule: None,
            ineligible: None,
            retry: None,
            failures: BTreeMap::new(),
            preemptions: BTreeMap::new(),
        });
        self.in_flight = None;
        self.until = None;
        self.suspended = false;
        self.finished = false;
        self.continuous = false;
        self.continuous_action = None;
        self.tick = 0;
        self.last_failure = None;
        self.last_rule = None;
        self.events.clear();
        let args = self.stack[0].args.iter().map(log_arg).collect();
        self.events.push(InterpreterEvent::FramePushed {
            skill: name.to_string(),
            depth: 1,
            args,
        });
        Ok(())
    }

    /// Drains the transitions since the last call. The buffer is bounded by
    /// the per-tick selection cap and the stack depth.
    pub fn take_events(&mut self) -> Vec<InterpreterEvent> {
        std::mem::take(&mut self.events)
    }

    /// The rule that fired last, for the driver's change-only `rule_selected`.
    pub fn last_rule(&self) -> Option<(&str, usize)> {
        self.last_rule
            .as_ref()
            .map(|(skill, rule)| (skill.as_str(), *rule))
    }

    /// The rule that fired in this tick, if any. The driver logs a change only
    /// from this, so a waiting or frame-pop tick never re-announces an old
    /// selection.
    pub fn rule_fired_this_tick(&self) -> Option<(&str, usize)> {
        self.rule_fired_this_tick.then(|| self.last_rule()).flatten()
    }

    /// Runs one tick (§6.1 steps 2 to 5, plus composition and preemption).
    pub fn tick(&mut self, facts: &mut dyn Facts, actuator: &mut dyn Actuator) -> Decision {
        if self.finished {
            return Decision::Waiting("finished".into());
        }
        if self.stack.is_empty() {
            return Decision::Waiting("not started".into());
        }
        self.tick += 1;
        self.rule_fired_this_tick = false;
        // A failure or a child failure only blocks its own tick.
        for frame in &mut self.stack {
            frame.ineligible = None;
            frame.retry = None;
        }
        if self.suspended {
            return Decision::Waiting("suspended".into());
        }

        // A continuous primitive reports only when it actually ends. Before
        // this, its movement receipt settled `confirmed` (no correction), which
        // is not the primitive's outcome: an `explore` that has walked 300 of
        // its 1200 ticks is still running. Attribute the terminal outcome to
        // the rule that selected it, exactly like a discrete action.
        if self.in_flight.is_none()
            && let Some(selected) = self.continuous_action.clone()
            && let Some(outcome) = actuator.poll(self.tick)
        {
            self.continuous = false;
            self.continuous_action = None;
            if let Some(decision) = self.settle(outcome, selected.depth, selected.rule, actuator) {
                return decision;
            }
        }

        // Step 2: a discrete action in flight blocks switching until it
        // settles; guards are not evaluated and no observe is offered.
        if let Some(flight) = self.in_flight.clone() {
            match actuator.poll(self.tick) {
                Some(outcome) => {
                    self.in_flight = None;
                    if let Some(decision) =
                        self.settle(outcome, flight.depth, flight.rule, actuator)
                    {
                        return decision;
                    }
                }
                None if self.tick >= flight.deadline => {
                    let _ = actuator.cancel();
                    self.in_flight = None;
                    if let Some(decision) = self.settle(
                        Outcome::Timeout,
                        flight.depth,
                        flight.rule,
                        actuator,
                    ) {
                        return decision;
                    }
                }
                None => {
                    return Decision::Waiting(format!("{} in flight", flight.primitive));
                }
            }
        }

        self.select(facts, actuator)
    }

    /// Suspends the stack and abandons an in-flight discrete action. The
    /// abandoned action settles `preempted`, and it counts against its rule:
    /// [`MAX_PREEMPTIONS`] of them in a row without that rule succeeding fail
    /// the skill `no_progress` when the stack resumes.
    pub fn preempt(&mut self, actuator: &mut dyn Actuator) -> Option<Outcome> {
        self.stop_continuous(actuator);
        let flight = self.in_flight.take();
        if let Some(flight) = &flight {
            self.count_preemption(flight.depth, flight.rule);
        }
        let outcome = if flight.is_some() {
            actuator.cancel()
        } else {
            None
        };
        self.until = None;
        self.suspended = true;
        outcome
    }

    /// Resumes a suspended stack. Every frame re-evaluates from the top.
    pub fn resume(&mut self) {
        self.suspended = false;
        self.until = None;
        self.in_flight = None;
        for frame in &mut self.stack {
            frame.ineligible = None;
            frame.retry = None;
            frame.failures.clear();
        }
    }

    pub fn until_active(&self) -> bool {
        self.until.is_some()
    }

    pub fn suspended(&self) -> bool {
        self.suspended
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    pub fn tick_count(&self) -> u64 {
        self.tick
    }

    pub fn last_failure(&self) -> Option<&str> {
        self.last_failure.as_deref()
    }

    pub fn frame_depth(&self) -> usize {
        self.stack.len()
    }

    /// The skill of the innermost frame, for diagnostics.
    pub fn current_skill(&self) -> Option<&str> {
        self.stack.last().map(|frame| frame.skill.as_str())
    }

    fn select(&mut self, facts: &mut dyn Facts, actuator: &mut dyn Actuator) -> Decision {
        let mut steps = 0;
        loop {
            steps += 1;
            if steps > MAX_SELECTION_STEPS {
                self.finished = true;
                self.stop_continuous(actuator);
                return Decision::Failed("depth".into());
            }
            let depth = self.stack.len() - 1;

            let forced = self.forced_rule(depth, facts);

            // Step 3: the highest rule whose guard is T fires.
            let mut fired = forced;
            let mut unknown = None;
            let mut retrying = false;
            if fired.is_none() {
                let frame = &self.stack[depth];
                let skill = self
                    .program
                    .skill(&frame.skill)
                    .expect("a frame always names a checked skill");
                for (index, rule) in skill.rules.iter().enumerate() {
                    if frame.ineligible == Some(index) {
                        continue;
                    }
                    match self.eval_bool(&rule.guard, frame, facts) {
                        Truth::True => {
                            // The highest true rule decides. If its primitive
                            // just failed, retry it next tick; do not let a
                            // lower rule take its place this tick.
                            if frame.retry == Some(index) {
                                retrying = true;
                            } else {
                                fired = Some(index);
                            }
                            break;
                        }
                        Truth::Unknown(fact) => {
                            if unknown.is_none() {
                                unknown = Some(fact);
                            }
                        }
                        Truth::False => {}
                    }
                }
            }

            if retrying {
                return Decision::Waiting("retry".into());
            }

            let Some(index) = fired else {
                // Step 4: the highest U rule offers one observation.
                if let Some(fact) = unknown {
                    self.stop_continuous(actuator);
                    return Decision::Observe(fact);
                }
                // A rule whose child failed this tick waits for the retry
                // instead of failing `no_rule` when it is the only true rule.
                if self.stack[depth].retry.is_some() {
                    return Decision::Waiting("retry".into());
                }
                // Step 5: no eligible rule.
                if depth == 0 {
                    self.finished = true;
                    self.stop_continuous(actuator);
                    return Decision::Failed("no_rule".into());
                }
                // The child had no eligible rule, so it failed with `no_rule`.
                // That is a failure of the calling rule, not of the parent on
                // this tick (see `note_child_failure`).
                if let Some(decision) = self.fail_skill(depth, "no_rule".into(), actuator) {
                    return decision;
                }
                continue;
            };

            // A rule that was preempted MAX_PREEMPTIONS times in a row without
            // succeeding is restarting the same action with nothing to show:
            // fail it typed instead of letting it restart again (see
            // [`MAX_PREEMPTIONS`]).
            if self.stack[depth]
                .preemptions
                .get(&index)
                .is_some_and(|count| *count >= MAX_PREEMPTIONS)
            {
                if let Some(decision) = self.fail_skill(depth, NO_PROGRESS.into(), actuator) {
                    return decision;
                }
                continue;
            }

            if let Some(decision) = self.dispatch_action(depth, index, actuator) {
                return decision;
            }
        }
    }

    /// The rule a live `until` commitment forces this tick, if any. The
    /// commitment keeps the current rule selected while its own guard is T and
    /// the `until` guard is not T (F or U), bounded by `max_ticks`. It is
    /// released the moment either stops holding. A deeper frame is a child of
    /// a committed call, so the commitment stays on its parent frame.
    fn forced_rule(&mut self, depth: usize, facts: &mut dyn Facts) -> Option<usize> {
        let until = self.until?;
        if until.depth >= self.stack.len() {
            self.until = None;
            return None;
        }
        if until.depth < depth {
            return None;
        }
        let frame = &self.stack[depth];
        let skill = self
            .program
            .skill(&frame.skill)
            .expect("a frame always names a checked skill");
        let rule = &skill.rules[until.rule];
        let own = self.eval_bool(&rule.guard, frame, facts);
        let released = rule
            .until
            .as_ref()
            .map(|guard| self.eval_bool(guard, frame, facts) == Truth::True)
            .unwrap_or(true);
        if self.tick >= until.deadline || own != Truth::True || released {
            self.until = None;
            None
        } else {
            Some(until.rule)
        }
    }

    /// Runs the action of the rule that just fired. `None` means keep selecting
    /// this tick: a `do` pushed a child frame, or a `fail` popped one.
    fn dispatch_action(
        &mut self,
        depth: usize,
        index: usize,
        actuator: &mut dyn Actuator,
    ) -> Option<Decision> {
        let selected_skill = self.stack[depth].skill.clone();
        self.last_rule = Some((selected_skill, index));
        self.rule_fired_this_tick = true;

        // A continuous action runs only while its own rule is the one that
        // fires (§6.1 step 3: "a higher rule becoming T switches at once").
        // Any other rule firing, in this frame or a deeper one, releases it
        // first; before, a running `explore` kept walking while a `do` child or
        // another rule took over.
        if self
            .continuous_action
            .as_ref()
            .is_some_and(|running| (running.depth, running.rule) != (depth, index))
        {
            self.stop_continuous(actuator);
        }

        let (action, rule_max_ticks, has_until) = {
            let frame = &self.stack[depth];
            let skill = self
                .program
                .skill(&frame.skill)
                .expect("a frame always names a checked skill");
            let rule = &skill.rules[index];
            (rule.action.kind.clone(), rule.max_ticks, rule.until.is_some())
        };
        let default_ticks = match &action {
            ActionKind::Primitive { name, .. } => self
                .program
                .primitives()
                .get(name)
                .map(|record| record.default_max_ticks)
                .unwrap_or(DEFAULT_MAX_TICKS),
            _ => DEFAULT_MAX_TICKS,
        };
        let max_ticks = rule_max_ticks.unwrap_or(default_ticks);
        self.ensure_until(depth, index, has_until, max_ticks);

        match action {
            ActionKind::Done => {
                self.stop_continuous(actuator);
                if depth == 0 {
                    self.finished = true;
                    self.events.push(InterpreterEvent::FrameDone {
                        skill: self.stack[0].skill.clone(),
                        depth: 1,
                    });
                    return Some(Decision::Done);
                }
                let call_rule = self.stack[depth].call_rule.unwrap_or(0);
                self.events.push(InterpreterEvent::FrameDone {
                    skill: self.stack[depth].skill.clone(),
                    depth: (depth + 1) as u32,
                });
                self.stack.pop();
                // A completed child is a success of the calling rule, so it
                // clears only that rule's failure count.
                if let Some(parent) = self.stack.get_mut(depth - 1) {
                    parent.failures.remove(&call_rule);
                    // A child that completed is progress for the rule that
                    // called it, exactly like its own primitive confirming.
                    parent.preemptions.remove(&call_rule);
                }
                // A commitment owned by this frame ends with it; a parent's
                // commitment survives a child completing.
                if matches!(&self.until, Some(until) if until.depth >= depth) {
                    self.until = None;
                }
                Some(Decision::Waiting("child done".into()))
            }
            ActionKind::Fail(kind) => {
                self.stop_continuous(actuator);
                self.fail_skill(depth, kind, actuator)
            }
            ActionKind::Do {
                skill: callee,
                args,
            } => {
                let values = {
                    let frame = &self.stack[depth];
                    let target = self
                        .program
                        .skill(&callee)
                        .expect("a `do` always names a checked skill");
                    args.iter()
                        .zip(&target.params)
                        .map(|(arg, param)| self.resolve_value(arg, param.ty, frame))
                        .collect::<Vec<_>>()
                };
                if self.stack.len() >= self.program.max_depth() {
                    self.finished = true;
                    self.stop_continuous(actuator);
                    return Some(Decision::Failed("depth".into()));
                }
                self.events.push(InterpreterEvent::FramePushed {
                    skill: callee.clone(),
                    depth: (self.stack.len() + 1) as u32,
                    args: values.iter().map(log_arg).collect(),
                });
                self.stack.push(Frame {
                    skill: callee,
                    args: values,
                    call_rule: Some(index),
                    ineligible: None,
                    retry: None,
                    failures: BTreeMap::new(),
                    preemptions: BTreeMap::new(),
                });
                None
            }
            ActionKind::Primitive { name, args } => {
                let record = self
                    .program
                    .primitives()
                    .get(&name)
                    .expect("an action always names a checked primitive");
                let kind = record.kind;
                let values = {
                    let frame = &self.stack[depth];
                    args.iter()
                        .zip(&record.params)
                        .map(|(arg, param)| self.resolve_arg(arg, param.ty, frame))
                        .collect::<Vec<_>>()
                };
                let run = RunAction {
                    primitive: name.clone(),
                    args: values.clone(),
                    kind,
                };
                let start = StartAction {
                    primitive: name.clone(),
                    args: values,
                };
                let frame_path = self.frame_path(index);
                if kind == PrimitiveKind::Discrete {
                    self.stop_continuous(actuator);
                    let _duration = actuator.start(&start, self.tick);
                    self.in_flight = Some(InFlight {
                        depth,
                        rule: index,
                        primitive: name,
                        deadline: self.tick + u64::from(max_ticks),
                    });
                    return Some(Decision::Run {
                        action: run,
                        frame_path,
                    });
                }
                actuator.apply(&start, self.tick);
                self.continuous = true;
                self.continuous_action = Some(Continuous {
                    depth,
                    rule: index,
                });
                Some(Decision::Run {
                    action: run,
                    frame_path,
                })
            }
        }
    }

    fn succeeded_rule(&mut self, depth: usize, rule: usize) {
        if depth < self.stack.len() {
            self.stack[depth].failures.remove(&rule);
            // Progress: the restarts before this one are history.
            self.stack[depth].preemptions.remove(&rule);
        }
    }

    /// Counts one preemption of `rule`'s in-flight action at `depth`. The count
    /// survives [`Interpreter::resume`]: a preemption is not a failure of the
    /// rule, but [`MAX_PREEMPTIONS`] restarts without a success is a loop.
    fn count_preemption(&mut self, depth: usize, rule: usize) {
        if let Some(frame) = self.stack.get_mut(depth) {
            *frame.preemptions.entry(rule).or_insert(0) += 1;
        }
    }

    /// Attributes a settled outcome to `rule` in the frame at `depth`: a success
    /// clears that rule's count; a failure counts it and fails the skill when
    /// the rule reaches [`MAX_PRIMITIVE_FAILURES`]. Returns the decision when
    /// the skill fails.
    fn settle(
        &mut self,
        outcome: Outcome,
        depth: usize,
        rule: usize,
        actuator: &mut dyn Actuator,
    ) -> Option<Decision> {
        let kind = match outcome {
            Outcome::Confirmed => {
                self.succeeded_rule(depth, rule);
                return None;
            }
            Outcome::Failed(kind) => kind,
            Outcome::Timeout => TIMEOUT_KIND.into(),
            Outcome::Preempted => "preempted".into(),
        };
        let kind = self.fail_rule(depth, rule, kind)?;
        self.fail_skill(depth, kind, actuator)
    }

    /// Counts one failure of `rule` in the frame at `depth`. Returns `true`
    /// when the rule reached [`MAX_PRIMITIVE_FAILURES`], meaning its skill
    /// fails; a reached limit clears the frame's counts so the next skill
    /// failure starts from zero. Callers mark the rule to retry below the
    /// limit.
    fn count_failure(&mut self, depth: usize, rule: usize) -> bool {
        let max = self.max_primitive_failures;
        let frame = &mut self.stack[depth];
        let count = frame.failures.entry(rule).or_insert(0);
        *count += 1;
        if *count >= max {
            // The skill fails; do not retry this rule.
            frame.failures.clear();
            true
        } else {
            false
        }
    }

    fn fail_rule(&mut self, depth: usize, rule: usize, kind: String) -> Option<String> {
        self.last_failure = Some(kind.clone());
        let mut failed = false;
        if depth < self.stack.len() {
            failed = self.count_failure(depth, rule);
            if !failed {
                // A primitive failure is retried on the next tick: the world
                // did not change, so the same rule is still the highest true
                // one.
                self.stack[depth].retry = Some(rule);
            }
        }
        self.clear_until_at(depth, rule);
        failed.then_some(kind)
    }

    /// Fails the skill whose frame is `depth`, mirroring `fail(kind)`: a child
    /// pops and counts one failure of its calling rule, the root ends the
    /// interpreter.
    fn fail_skill(
        &mut self,
        depth: usize,
        kind: String,
        actuator: &mut dyn Actuator,
    ) -> Option<Decision> {
        self.last_failure = Some(kind.clone());
        if depth == 0 || depth >= self.stack.len() {
            self.finished = true;
            self.stop_continuous(actuator);
            if let Some(frame) = self.stack.first() {
                self.events.push(InterpreterEvent::FrameFailed {
                    skill: frame.skill.clone(),
                    depth: 1,
                    kind: kind.clone(),
                });
            }
            return Some(Decision::Failed(kind));
        }
        self.events.push(InterpreterEvent::FrameFailed {
            skill: self.stack[depth].skill.clone(),
            depth: (depth + 1) as u32,
            kind: kind.clone(),
        });
        let call_rule = self.stack[depth].call_rule.unwrap_or(0);
        self.stack.truncate(depth);
        let parent = depth - 1;
        self.clear_until_at(parent, call_rule);
        self.note_child_failure(parent, call_rule, kind, actuator)
    }

    /// Counts one failure of `rule` in the frame at `depth`, the same per-rule
    /// count a failed primitive of that rule uses. Below the limit the rule is
    /// ineligible for this tick and retried on the next one; at the limit the
    /// frame's skill fails and the failure propagates to its caller.
    fn note_child_failure(
        &mut self,
        depth: usize,
        rule: usize,
        kind: String,
        actuator: &mut dyn Actuator,
    ) -> Option<Decision> {
        if self.count_failure(depth, rule) {
            return self.fail_skill(depth, kind, actuator);
        }
        self.stack[depth].ineligible = Some(rule);
        // If no other rule is true this tick, the frame waits for the retry
        // instead of failing `no_rule`; the child is called again next tick.
        self.stack[depth].retry = Some(rule);
        None
    }

    fn clear_until_at(&mut self, depth: usize, rule: usize) {
        if matches!(&self.until, Some(until) if until.depth == depth && until.rule == rule) {
            self.until = None;
        }
    }

    fn ensure_until(&mut self, depth: usize, rule: usize, has_until: bool, max_ticks: u32) {
        if let Some(until) = &self.until
            && until.depth == depth
            && until.rule != rule
        {
            self.until = None;
        }
        if !has_until {
            if matches!(&self.until, Some(until) if until.depth == depth && until.rule == rule) {
                self.until = None;
            }
            return;
        }
        match &self.until {
            Some(until) if until.depth == depth && until.rule == rule => {}
            _ => {
                self.until = Some(Until {
                    depth,
                    rule,
                    deadline: self.tick + u64::from(max_ticks),
                });
            }
        }
    }

    fn stop_continuous(&mut self, actuator: &mut dyn Actuator) {
        self.continuous_action = None;
        if self.continuous {
            actuator.release();
            self.continuous = false;
        }
    }

    fn frame_path(&self, firing_rule: usize) -> Vec<FrameRef> {
        let last = self.stack.len() - 1;
        self.stack
            .iter()
            .enumerate()
            .map(|(index, frame)| FrameRef {
                skill: frame.skill.clone(),
                rule: if index == last {
                    firing_rule
                } else {
                    frame.call_rule.unwrap_or(0)
                },
            })
            .collect()
    }

    fn eval_bool(&self, expr: &Expr, frame: &Frame, facts: &mut dyn Facts) -> Truth {
        match &expr.kind {
            ExprKind::Bool(value) => {
                if *value {
                    Truth::True
                } else {
                    Truth::False
                }
            }
            ExprKind::Call { name, args } => {
                let call = self.resolve_call(name, args, frame);
                facts.bool_fact(&call)
            }
            ExprKind::Not(inner) => kleene_not(self.eval_bool(inner, frame, facts)),
            ExprKind::And(left, right) => kleene_and(
                self.eval_bool(left, frame, facts),
                self.eval_bool(right, frame, facts),
            ),
            ExprKind::Or(left, right) => kleene_or(
                self.eval_bool(left, frame, facts),
                self.eval_bool(right, frame, facts),
            ),
            ExprKind::Cmp { op, left, right } => {
                let left = self.eval_int(left, frame, facts);
                let right = self.eval_int(right, frame, facts);
                compare(*op, left, right)
            }
            ExprKind::Int(_) | ExprKind::Param(_) | ExprKind::Name(_) => {
                unreachable!("the type checker rejects a non-bool guard")
            }
        }
    }

    fn eval_int(&self, expr: &Expr, frame: &Frame, facts: &mut dyn Facts) -> IntValue {
        match &expr.kind {
            ExprKind::Int(value) => IntValue::Known(*value),
            ExprKind::Param(name) => {
                let index = self.param_index(&frame.skill, name);
                match frame.args.get(index) {
                    Some(ArgValue::Int(value)) => IntValue::Known(*value),
                    other => unreachable!("checked int parameter, found {other:?}"),
                }
            }
            ExprKind::Call { name, args } => {
                let call = self.resolve_call(name, args, frame);
                facts.int_fact(&call)
            }
            _ => unreachable!("the type checker rejects a non-int expression here"),
        }
    }

    fn resolve_call(&self, name: &str, args: &[Expr], frame: &Frame) -> FactCall {
        let signature = self
            .program
            .facts()
            .get(name)
            .expect("the type checker rejects unknown facts");
        let values = args
            .iter()
            .zip(&signature.params)
            .map(|(arg, ty)| self.resolve_value(arg, *ty, frame))
            .collect();
        FactCall {
            name: name.to_string(),
            args: values,
        }
    }

    fn resolve_value(&self, expr: &Expr, want: ValueType, frame: &Frame) -> ArgValue {
        match &expr.kind {
            ExprKind::Int(value) => ArgValue::Int(*value),
            ExprKind::Bool(value) => ArgValue::Bool(*value),
            ExprKind::Param(name) => {
                let index = self.param_index(&frame.skill, name);
                frame.args[index].clone()
            }
            ExprKind::Name(name) => match want {
                ValueType::Item => ArgValue::Item(name.clone()),
                ValueType::Block => ArgValue::Block(name.clone()),
                ValueType::EntityKind => ArgValue::EntityKind(name.clone()),
                ValueType::Int | ValueType::Bool => {
                    unreachable!("the type checker rejects a name here")
                }
            },
            _ => unreachable!("the type checker rejects a non-value argument here"),
        }
    }

    fn resolve_arg(&self, expr: &Expr, want: ArgType, frame: &Frame) -> ArgValue {
        match want {
            ArgType::Value(ty) => self.resolve_value(expr, ty, frame),
            ArgType::BlockOrEntity => match &expr.kind {
                ExprKind::Name(name) => ArgValue::Name(name.clone()),
                ExprKind::Param(name) => {
                    let index = self.param_index(&frame.skill, name);
                    frame.args[index].clone()
                }
                _ => unreachable!("the type checker rejects this move_to argument"),
            },
            ArgType::Fact => match &expr.kind {
                ExprKind::Call { name, args } => {
                    ArgValue::Fact(self.resolve_call(name, args, frame))
                }
                _ => unreachable!("the type checker rejects a non-fact observe argument"),
            },
        }
    }

    fn param_index(&self, skill: &str, name: &str) -> usize {
        self.program
            .skill(skill)
            .and_then(|skill| skill.param_index(name))
            .expect("the type checker rejects unknown parameters")
    }
}

/// A log-only rendering of a resolved argument.
fn log_arg(arg: &ArgValue) -> String {
    match arg {
        ArgValue::Int(value) => value.to_string(),
        ArgValue::Bool(value) => value.to_string(),
        ArgValue::Item(name)
        | ArgValue::Block(name)
        | ArgValue::EntityKind(name)
        | ArgValue::Name(name) => name.clone(),
        ArgValue::Fact(call) => call.to_string(),
    }
}

fn compare(op: CmpOp, left: IntValue, right: IntValue) -> Truth {    let (left, right) = match (left, right) {
        (IntValue::Known(left), IntValue::Known(right)) => (left, right),
        (IntValue::Unknown(fact), _) => return Truth::Unknown(fact),
        (_, IntValue::Unknown(fact)) => return Truth::Unknown(fact),
    };
    let value = match op {
        CmpOp::Lt => left < right,
        CmpOp::Le => left <= right,
        CmpOp::Eq => left == right,
        CmpOp::Ne => left != right,
        CmpOp::Ge => left >= right,
        CmpOp::Gt => left > right,
    };
    if value {
        Truth::True
    } else {
        Truth::False
    }
}
