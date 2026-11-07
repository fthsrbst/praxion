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

/// Guards a selection loop against a pathological ruleset. The load-time
/// depth cap makes this unreachable for a checked program.
const MAX_SELECTION_STEPS: usize = 64;

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
    /// A primitive of this rule failed this tick; the rule retries next tick
    /// instead of the skill failing.
    retry: Option<usize>,
    /// Consecutive failures per rule index, primitive or child skill. A rule's
    /// own success clears only its entry; another rule firing leaves it alone.
    failures: BTreeMap<usize, u32>,
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
            match outcome {
                Outcome::Confirmed => self.succeeded_rule(selected.depth, selected.rule),
                Outcome::Failed(kind) => {
                    if let Some(kind) = self.fail_rule(selected.depth, selected.rule, kind)
                        && let Some(decision) =
                            self.fail_skill(selected.depth, kind, actuator)
                    {
                        return decision;
                    }
                }
                Outcome::Timeout => {
                    if let Some(kind) =
                        self.fail_rule(selected.depth, selected.rule, "timeout".into())
                        && let Some(decision) =
                            self.fail_skill(selected.depth, kind, actuator)
                    {
                        return decision;
                    }
                }
                Outcome::Preempted => {
                    if let Some(kind) =
                        self.fail_rule(selected.depth, selected.rule, "preempted".into())
                        && let Some(decision) =
                            self.fail_skill(selected.depth, kind, actuator)
                    {
                        return decision;
                    }
                }
            }
        }

        // Step 2: a discrete action in flight blocks switching until it
        // settles; guards are not evaluated and no observe is offered.
        if let Some(flight) = self.in_flight.clone() {
            match actuator.poll(self.tick) {
                Some(outcome) => {
                    self.in_flight = None;
                    match outcome {
                        Outcome::Confirmed => self.succeeded(&flight),
                        Outcome::Failed(kind) => {
                            if let Some(kind) = self.fail_flight(&flight, kind) {
                                if let Some(decision) =
                                    self.fail_skill(flight.depth, kind, actuator)
                                {
                                    return decision;
                                }
                            }
                        }
                        Outcome::Timeout => {
                            if let Some(kind) =
                                self.fail_flight(&flight, "timeout".into())
                            {
                                if let Some(decision) =
                                    self.fail_skill(flight.depth, kind, actuator)
                                {
                                    return decision;
                                }
                            }
                        }
                        Outcome::Preempted => {
                            if let Some(kind) =
                                self.fail_flight(&flight, "preempted".into())
                            {
                                if let Some(decision) =
                                    self.fail_skill(flight.depth, kind, actuator)
                                {
                                    return decision;
                                }
                            }
                        }
                    }
                }
                None if self.tick >= flight.deadline => {
                    let _ = actuator.cancel();
                    self.in_flight = None;
                    if let Some(kind) = self.fail_flight(&flight, "timeout".into()) {
                        if let Some(decision) = self.fail_skill(flight.depth, kind, actuator) {
                            return decision;
                        }
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
    /// abandoned action settles `preempted`.
    pub fn preempt(&mut self, actuator: &mut dyn Actuator) -> Option<Outcome> {
        self.stop_continuous(actuator);
        let outcome = if self.in_flight.take().is_some() {
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

            // A commitment (`until`) keeps the current rule selected while its
            // own guard is T and the `until` guard is not T (F or U), bounded
            // by `max_ticks`. It is released the moment either stops holding.
            let mut forced = None;
            if let Some(until) = self.until {
                if until.depth >= self.stack.len() {
                    self.until = None;
                } else if until.depth == depth {
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
                    } else {
                        forced = Some(until.rule);
                    }
                }
                // A deeper frame is a child of a committed call; leave the
                // commitment on its parent frame.
            }

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

            let selected_skill = self.stack[depth].skill.clone();
            self.last_rule = Some((selected_skill, index));
            self.rule_fired_this_tick = true;

            // A continuous action runs only while its own rule is the one that
            // fires (§6.1 step 3: "a higher rule becoming T switches at
            // once"). Any other rule firing, in this frame or a deeper one,
            // releases it first; before, a running `explore` kept walking while
            // a `do` child or another rule took over.
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
                        return Decision::Done;
                    }
                    let call_rule = self.stack[depth].call_rule.unwrap_or(0);
                    self.events.push(InterpreterEvent::FrameDone {
                        skill: self.stack[depth].skill.clone(),
                        depth: (depth + 1) as u32,
                    });
                    self.stack.pop();
                    // A completed child is a success of the calling rule, so
                    // it clears only that rule's failure count.
                    if let Some(parent) = self.stack.get_mut(depth - 1) {
                        parent.failures.remove(&call_rule);
                    }
                    // A commitment owned by this frame ends with it; a parent's
                    // commitment survives a child completing.
                    if matches!(&self.until, Some(until) if until.depth >= depth) {
                        self.until = None;
                    }
                    return Decision::Waiting("child done".into());
                }
                ActionKind::Fail(kind) => {
                    self.stop_continuous(actuator);
                    if let Some(decision) = self.fail_skill(depth, kind, actuator) {
                        return decision;
                    }
                    continue;
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
                        return Decision::Failed("depth".into());
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
                    });
                    continue;
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
                        return Decision::Run {
                            action: run,
                            frame_path,
                        };
                    }
                    actuator.apply(&start, self.tick);
                    self.continuous = true;
                    self.continuous_action = Some(Continuous {
                        depth,
                        rule: index,
                    });
                    return Decision::Run {
                        action: run,
                        frame_path,
                    };
                }
            }
        }
    }

    /// Records a successful discrete action and clears its rule's count.
    fn succeeded(&mut self, flight: &InFlight) {
        self.succeeded_rule(flight.depth, flight.rule);
    }

    fn succeeded_rule(&mut self, depth: usize, rule: usize) {
        if depth < self.stack.len() {
            self.stack[depth].failures.remove(&rule);
        }
    }

    /// Records a primitive failure. Returns the failure kind when the rule has
    /// failed [`MAX_PRIMITIVE_FAILURES`] times in a row, meaning its skill
    /// fails; otherwise marks the rule to retry next tick.
    fn fail_flight(&mut self, flight: &InFlight, kind: String) -> Option<String> {
        self.fail_rule(flight.depth, flight.rule, kind)
    }

    fn fail_rule(&mut self, depth: usize, rule: usize, kind: String) -> Option<String> {
        self.last_failure = Some(kind.clone());
        let mut failed = false;
        if depth < self.stack.len() {
            let max = self.max_primitive_failures;
            let frame = &mut self.stack[depth];
            let count = frame.failures.entry(rule).or_insert(0);
            *count += 1;
            if *count >= max {
                // The skill fails; do not retry this rule.
                frame.failures.clear();
                failed = true;
            } else {
                // A primitive failure is retried on the next tick: the world
                // did not change, so the same rule is still the highest true
                // one.
                frame.retry = Some(rule);
            }
        }
        self.clear_until_at(depth, rule);
        if failed { Some(kind) } else { None }
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
        let max = self.max_primitive_failures;
        let count = {
            let count = self.stack[depth].failures.entry(rule).or_insert(0);
            *count += 1;
            *count
        };
        if count >= max {
            // The skill fails; do not retry this rule.
            self.stack[depth].failures.clear();
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
        if let Some(until) = &self.until {
            if until.depth == depth && until.rule != rule {
                self.until = None;
            }
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
