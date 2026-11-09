//! The arbiter: the single owner of the body.
//!
//! Order of a tick (architecture §6.1 step 1 first; the rest builds on it):
//!
//! ```text
//! reflex engine poll ──> started/running: preempt, bump generation, reflex frame
//!        │ ended: resume the interrupted intent
//!        ▼
//! survival hard need ──> preempt now, eat (survival tier)
//!        ▼
//! command skill tick ──> interpreter decision
//!        ▼
//! soft need at a safe point ──> suspend the command, eat, resume after
//!        ▼
//! skill or idle frame, Enderman look guard applied
//! ```
//!
//! The arbiter does not build primitives: a skill's decision is handed back to
//! the actuator layer together with this tick's frame (which carries the look
//! guard and no held keys). Reflexes own their own tiny motion routines.

use praxion_eventlog::EventKind;
use praxion_headless::{Body, Click, ControlFrame, Held};
use praxion_interpreter::{Actuator, Decision, Facts, Interpreter};
use praxion_planner::KnownBlocks;
use praxion_reflexes::{EndermanGuard, Engine, ReflexKind, ReflexTick, Settings, View};
use praxion_rules::{ArgValue, PrimitiveKind};

use crate::tier::{IntentStack, IntentState, NeedKind, Needs, Tier, arg_to_string};

/// Vanilla eating takes 1.6 s, 32 ticks. The survival action keeps the same
/// duration so a hunger test sees a real interruption window.
pub const EAT_TICKS: u64 = 32;

/// Who owns the body right now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    Idle,
    Skill { intent_id: u64 },
    Reflex(ReflexKind),
    Maintenance(NeedKind),
}

impl Owner {
    pub fn name(&self) -> String {
        match self {
            Owner::Idle => "idle".to_owned(),
            Owner::Skill { intent_id } => format!("skill:{intent_id}"),
            Owner::Reflex(kind) => format!("reflex:{}", kind.name()),
            Owner::Maintenance(need) => format!("maintenance:{}", need.name()),
        }
    }
}

/// Where this tick's frame came from.
#[derive(Clone, Debug)]
pub enum FrameSource {
    Reflex(ControlFrame),
    Maintenance(ControlFrame),
    Skill(ControlFrame),
    Idle(ControlFrame),
}

impl FrameSource {
    pub fn frame(&self) -> &ControlFrame {
        match self {
            FrameSource::Reflex(frame)
            | FrameSource::Maintenance(frame)
            | FrameSource::Skill(frame)
            | FrameSource::Idle(frame) => frame,
        }
    }
}

/// Everything the driver needs after one arbiter tick.
#[derive(Clone, Debug)]
pub struct ArbiterTick {
    pub source: FrameSource,
    pub owner: Owner,
    pub generation: u32,
    pub events: Vec<EventKind>,
    /// The reflex that owns or just released the body, if any.
    pub reflex: Option<ReflexKind>,
    /// The maintenance need being served, if any.
    pub maintenance: Option<NeedKind>,
    /// The interpreter's decision this tick, when a skill ran.
    pub skill: Option<Decision>,
    /// The intent that ran the interpreter this tick, when one did. The
    /// decision belongs to it even if a maintenance action suspends it before
    /// the tick ends.
    pub skill_intent: Option<u64>,
    /// A brain wake was emitted this tick (Demo-0: event log only).
    pub wake_brain: bool,
    /// The inventory slot the body finished eating, when it finished this tick.
    pub ate: Option<u16>,
}

/// The accepted command.
#[derive(Clone, Debug)]
pub struct CommandAccepted {
    pub intent_id: u64,
    pub generation: u32,
    /// The intent events of the push and suspension (for the event log).
    pub events: Vec<EventKind>,
}

/// Why the arbiter refused a command. The driver maps the variant to the
/// receipt's `reason`; it never classifies a string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandError {
    /// The intent stack has no room for another entry.
    StackFull,
    /// The interpreter refused to start the skill (unknown name, bad args).
    Skill(String),
}

#[derive(Clone, Debug)]
struct MaintenanceRun {
    need: NeedKind,
    intent_id: u64,
    slot: Option<u16>,
    started: u64,
    clicked: bool,
}

/// The body's decision layer.
#[derive(Clone, Debug)]
pub struct Arbiter {
    generation: u32,
    stack: IntentStack,
    owner: Owner,
    owner_since: u64,
    engine: Engine,
    look_guard: EndermanGuard,
    last_switch_tick: u64,
    last_switch_tier: Option<Tier>,
    maintenance: Option<MaintenanceRun>,
    last_decision: Option<Decision>,
    wake_emitted: bool,
    events: Vec<EventKind>,
    ate: Option<u16>,
    /// The intent that ran the interpreter this tick (see [`ArbiterTick`]).
    skill_intent: Option<u64>,
}

impl Arbiter {
    pub fn new(generation: u32) -> Self {
        Self {
            generation,
            stack: IntentStack::new(),
            owner: Owner::Idle,
            owner_since: 0,
            engine: Engine::new(),
            look_guard: EndermanGuard::new(),
            last_switch_tick: 0,
            last_switch_tier: None,
            maintenance: None,
            last_decision: None,
            wake_emitted: false,
            events: Vec::new(),
            ate: None,
            skill_intent: None,
        }
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    pub fn stack(&self) -> &IntentStack {
        &self.stack
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn engine_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }

    /// The tier of the last owner change, for the switch rule's tests.
    pub fn last_switch_tier(&self) -> Option<Tier> {
        self.last_switch_tier
    }

    /// Adopt a new user command. The old command is suspended, not cancelled,
    /// and the generation increments exactly once.
    ///
    /// The intent is pushed before the skill starts: the stack is the body's
    /// record of what was asked, so a started skill without an entry (or an
    /// entry without a skill) would leave the body owning nothing. A refused
    /// skill rolls the push back, so the two never disagree.
    pub fn command(
        &mut self,
        interpreter: &mut Interpreter,
        body: &mut Body,
        tick: u64,
        skill: &str,
        args: Vec<ArgValue>,
    ) -> Result<CommandAccepted, CommandError> {
        let strings: Vec<String> = args.iter().map(arg_to_string).collect();
        let pushed = self
            .stack
            .push(Tier::Command, skill, strings.clone())
            .map_err(|_| CommandError::StackFull)?;
        if let Err(refusal) = interpreter.start_skill(skill, args) {
            self.stack.rollback(pushed);
            return Err(CommandError::Skill(refusal));
        }

        let mut events = vec![EventKind::IntentPushed {
            intent_id: pushed.intent_id,
            tier: Tier::Command.name().to_owned(),
            skill: skill.to_owned(),
            args: strings,
        }];
        if let Some(suspended) = pushed.suspended {
            events.push(EventKind::IntentState {
                intent_id: suspended,
                state: IntentState::Suspended.name().to_owned(),
            });
            events.push(EventKind::ReplanNeeded {
                intent_id: suspended,
            });
        }

        body.bump_generation();
        self.generation = body.generation();
        // A reflex or a maintenance action owns the body right now; the new
        // command waits on the stack and runs when the body comes back. Only
        // an idle or skill owner changes to the new command immediately.
        if matches!(self.owner, Owner::Idle | Owner::Skill { .. }) {
            let owner = Owner::Skill {
                intent_id: pushed.intent_id,
            };
            self.owner = owner.clone();
            self.owner_since = tick;
            self.wake_emitted = false;
            self.record_switch(Tier::Command, tick);
            events.push(EventKind::OwnerChanged {
                generation: self.generation,
                owner: owner.name(),
            });
        }
        Ok(CommandAccepted {
            intent_id: pushed.intent_id,
            generation: self.generation,
            events,
        })
    }

    /// Carry on (`devam` / `resume` / `carry on`) with the brain down: give the
    /// body back to the newest suspended command, whose skill the caller then
    /// restarts from the top. A no-op while anything else owns the body (a
    /// running skill, a reflex or a meal), so the word never fights the body.
    ///
    /// Returns the intent that now owns the body and the events to log, like
    /// [`Arbiter::end_command`]. Decision 2026-11-09 "Five next areas": the four
    /// control verbs work with the brain down (architecture §5.4).
    pub fn resume_command(&mut self, body: &mut Body, tick: u64) -> (Option<u64>, Vec<EventKind>) {
        if !matches!(self.owner, Owner::Idle) {
            return (None, Vec::new());
        }
        let Some(intent) = self.resume_suspended_command() else {
            return (None, Vec::new());
        };
        body.bump_generation();
        self.set_owner(Owner::Skill { intent_id: intent }, body.generation(), tick);
        let mut events = Vec::new();
        events.append(&mut self.events);
        (Some(intent), events)
    }

    /// The driver settled a command: mark its intent `done` and hand the body
    /// back. When a suspended command is waiting, it becomes the running one
    /// again (a new command suspends the old one, so settling the new one must
    /// give the old one its turn back).
    ///
    /// The generation moves whenever the owner does; the returned events are
    /// the `intent_state`/`owner_changed` lines the driver logs in order. The
    /// interpreter still holds the settled skill's frames, so the caller must
    /// restart the resumed command with [`Interpreter::start_skill`] when the
    /// running intent changed.
    pub fn end_command(&mut self, intent_id: u64, body: &mut Body, tick: u64) -> Vec<EventKind> {
        if self
            .stack
            .get(intent_id)
            .is_none_or(|entry| entry.state == IntentState::Done)
        {
            // An already-done entry has nothing left to settle: a second pop or
            // settle must not write another `intent_state done`.
            return Vec::new();
        }
        if !self.stack.done(intent_id) {
            return Vec::new();
        }
        let mut events = vec![EventKind::IntentState {
            intent_id,
            state: IntentState::Done.name().to_owned(),
        }];
        if self.owner == (Owner::Skill { intent_id }) {
            body.bump_generation();
            match self.resume_suspended_command() {
                Some(next) => {
                    self.set_owner(Owner::Skill { intent_id: next }, body.generation(), tick);
                }
                None => self.set_owner(Owner::Idle, body.generation(), tick),
            }
            events.append(&mut self.events);
        }
        events
    }

    /// One tick. The caller must have called `Perception::begin_tick` and built
    /// the view; the body is stepped by the caller after returning.
    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        view: &View,
        interpreter: &mut Interpreter,
        facts: &mut dyn Facts,
        actuator: &mut dyn Actuator,
        needs: Needs,
        settings: &Settings,
        body: &mut Body,
        tick: u64,
        blocks: &mut dyn KnownBlocks,
    ) -> ArbiterTick {
        self.events.clear();
        self.ate = None;
        self.skill_intent = None;

        // 1. Reflexes always win. The engine bumps the body generation on a
        // start; the arbiter mirrors it and emits one owner change.
        match self.engine.poll(view, settings, body, tick, blocks) {
            ReflexTick::Started { kind, frame, .. } => {
                self.preempt_for_reflex(interpreter, actuator);
                self.set_owner(Owner::Reflex(kind), body.generation(), tick);
                let frame = self.guard(frame, body, view);
                return self.result(FrameSource::Reflex(frame), Some(kind), None, None);
            }
            ReflexTick::Running { kind, frame } => {
                self.maybe_wake(kind, tick, settings, needs);
                let frame = self.guard(frame, body, view);
                return self.result(FrameSource::Reflex(frame), Some(kind), None, None);
            }
            ReflexTick::Ended {
                kind,
                ran_ticks,
                timed_out,
            } => {
                if self.owner == Owner::Reflex(kind) {
                    self.resume_after_reflex(interpreter, body, tick);
                }
                if timed_out {
                    self.wake(
                        "reflex",
                        format!(
                            "{} hit max_reflex_ticks after {ran_ticks} ticks",
                            kind.name()
                        ),
                    );
                } else if ran_ticks > u64::from(settings.wake_after_ticks) {
                    self.wake("reflex", format!("{} ran {ran_ticks} ticks", kind.name()));
                }
            }
            ReflexTick::Idle => {}
        }

        // 2. A maintenance action in progress.
        let ended = self
            .maintenance
            .as_ref()
            .is_some_and(|run| self.maintenance_ended(run, tick, needs, settings));
        if ended {
            self.finish_maintenance(interpreter, body, tick);
        } else if self.maintenance.is_some() {
            let run = self.maintenance.as_mut().expect("checked above");
            let frame = maintenance_frame(tick, body.generation(), run);
            let need = run.need;
            let frame = self.guard(frame, body, view);
            return self.result(FrameSource::Maintenance(frame), None, Some(need), None);
        }

        // 3. Survival hard threshold: preempt now, no safe-point wait.
        if needs.food <= settings.eat_hard_threshold {
            match view.best_food.map(|food| food.slot) {
                Some(slot) => {
                    self.start_maintenance(
                        NeedKind::Eat,
                        slot,
                        Tier::Survival,
                        interpreter,
                        actuator,
                        body,
                        tick,
                    );
                    let run = self.maintenance.as_mut().expect("just started");
                    let frame = maintenance_frame(tick, body.generation(), run);
                    let frame = self.guard(frame, body, view);
                    return self.result(
                        FrameSource::Maintenance(frame),
                        None,
                        Some(NeedKind::Eat),
                        None,
                    );
                }
                None => self.wake(
                    "hunger",
                    "food at the hard threshold and no food in the inventory".to_owned(),
                ),
            }
        }

        // 4. The command (or any lower-tier intent) runs the interpreter.
        let mut decision = None;
        if let Some(intent) = self.stack.running().cloned() {
            if self.owner == Owner::Idle {
                self.set_owner(
                    Owner::Skill {
                        intent_id: intent.intent_id,
                    },
                    body.generation(),
                    tick,
                );
            }
            let outcome = interpreter.tick(facts, actuator);
            self.last_decision = Some(outcome.clone());
            self.skill_intent = Some(intent.intent_id);
            decision = Some(outcome);
        }

        // 5. The soft threshold is served at the command's next safe point.
        if needs.food <= settings.eat_soft_threshold
            && let Some(slot) = view.best_food.map(|food| food.slot)
        {
            let from = self.tier();
            let until = interpreter.until_active();
            if self.is_safe_point(interpreter)
                && self.may_switch(from, Tier::Maintenance, 1.0, 0.0, until, settings, tick)
            {
                self.start_maintenance(
                    NeedKind::Eat,
                    slot,
                    Tier::Maintenance,
                    interpreter,
                    actuator,
                    body,
                    tick,
                );
                let run = self.maintenance.as_mut().expect("just started");
                let frame = maintenance_frame(tick, body.generation(), run);
                let frame = self.guard(frame, body, view);
                return self.result(
                    FrameSource::Maintenance(frame),
                    None,
                    Some(NeedKind::Eat),
                    decision,
                );
            }
        }

        // 6. Nothing to preempt: the skill's frame (look guarded) or idle.
        let frame = ControlFrame::new(tick, body.generation(), Held::NONE);
        let frame = self.guard(frame, body, view);
        let source = if matches!(self.owner, Owner::Skill { .. }) {
            FrameSource::Skill(frame)
        } else {
            FrameSource::Idle(frame)
        };
        self.result(source, None, None, decision)
    }

    /// The same-tier switch rule: `gain > k x loss`, no `until`, and no second
    /// switch inside `min_switch_ticks`.
    #[allow(clippy::too_many_arguments)]
    pub fn may_switch(
        &self,
        from: Tier,
        to: Tier,
        gain: f32,
        loss: f32,
        until_active: bool,
        settings: &Settings,
        tick: u64,
    ) -> bool {
        if until_active {
            return false;
        }
        if from == to {
            if tick
                < self
                    .last_switch_tick
                    .saturating_add(u64::from(settings.min_switch_ticks))
            {
                return false;
            }
            gain > settings.switch_gain_factor * loss
        } else {
            // A threshold promotion or a safe-point hand-over to another tier
            // is the rule's own path; only a same-tier switch needs the gain
            // test.
            true
        }
    }

    /// Apply [`Arbiter::may_switch`] and, when it permits, record the switch so
    /// the dwell applies to the next one. The soft-need path calls this; the
    /// A-B-A test calls it directly.
    #[allow(clippy::too_many_arguments)]
    pub fn switch_allowed(
        &mut self,
        from: Tier,
        to: Tier,
        gain: f32,
        loss: f32,
        until_active: bool,
        settings: &Settings,
        tick: u64,
    ) -> bool {
        if self.may_switch(from, to, gain, loss, until_active, settings, tick) {
            self.record_switch(to, tick);
            true
        } else {
            false
        }
    }

    fn tier(&self) -> Tier {
        match self.owner {
            Owner::Reflex(_) | Owner::Maintenance(NeedKind::Eat) => Tier::Survival,
            Owner::Skill { .. } => Tier::Command,
            Owner::Maintenance(_) => Tier::Maintenance,
            Owner::Idle => Tier::Projects,
        }
    }

    fn set_owner(&mut self, owner: Owner, generation: u32, tick: u64) {
        if self.owner == owner {
            self.generation = generation;
            return;
        }
        self.owner = owner.clone();
        self.generation = generation;
        self.owner_since = tick;
        self.wake_emitted = false;
        self.events.push(EventKind::OwnerChanged {
            generation,
            owner: owner.name(),
        });
    }

    fn record_switch(&mut self, tier: Tier, tick: u64) {
        self.last_switch_tick = tick;
        self.last_switch_tier = Some(tier);
    }

    /// Suspend the current skill and abandon its in-flight discrete action.
    fn preempt_for_reflex(&mut self, interpreter: &mut Interpreter, actuator: &mut dyn Actuator) {
        if let Some(run) = self.maintenance.take() {
            // The meal is over, not paused: a reflex interrupted it. The body
            // writes `done` for its own intent; the receipt of the cancelled
            // click already settles `preempted`.
            self.stack.done(run.intent_id);
            self.events.push(EventKind::IntentState {
                intent_id: run.intent_id,
                state: IntentState::Done.name().to_owned(),
            });
        }
        if let Owner::Skill { intent_id } = self.owner {
            self.stack.set_state(intent_id, IntentState::Suspended);
            self.events.push(EventKind::IntentState {
                intent_id,
                state: IntentState::Suspended.name().to_owned(),
            });
            self.events.push(EventKind::ReplanNeeded { intent_id });
            // Abandons an in-flight discrete action; its receipt settles
            // `preempted` in the actuator.
            interpreter.preempt(actuator);
        }
    }

    /// A reflex ended: the interrupted skill re-evaluates from the top.
    fn resume_after_reflex(&mut self, interpreter: &mut Interpreter, body: &mut Body, tick: u64) {
        interpreter.resume();
        if let Some(intent) = self.resume_suspended_command() {
            body.bump_generation();
            self.set_owner(Owner::Skill { intent_id: intent }, body.generation(), tick);
        } else {
            body.bump_generation();
            self.set_owner(Owner::Idle, body.generation(), tick);
        }
    }

    fn resume_after_maintenance(
        &mut self,
        interpreter: &mut Interpreter,
        body: &mut Body,
        tick: u64,
    ) {
        interpreter.resume();
        if let Some(intent) = self.resume_suspended_command() {
            body.bump_generation();
            self.set_owner(Owner::Skill { intent_id: intent }, body.generation(), tick);
        } else {
            body.bump_generation();
            self.set_owner(Owner::Idle, body.generation(), tick);
        }
    }

    fn resume_suspended_command(&mut self) -> Option<u64> {
        let entry = self
            .stack
            .entries()
            .iter()
            .rev()
            .find(|entry| {
                entry.tier == Tier::Command
                    && matches!(
                        entry.state,
                        IntentState::Suspended
                            | IntentState::FrozenForCarryOn
                            | IntentState::Running
                    )
            })?
            .intent_id;
        if self.stack.get(entry).is_some_and(|entry| {
            matches!(
                entry.state,
                IntentState::Suspended | IntentState::FrozenForCarryOn
            )
        }) {
            self.stack.set_state(entry, IntentState::Running);
            self.events.push(EventKind::IntentState {
                intent_id: entry,
                state: IntentState::Running.name().to_owned(),
            });
        }
        Some(entry)
    }

    #[allow(clippy::too_many_arguments)]
    fn start_maintenance(
        &mut self,
        need: NeedKind,
        slot: u16,
        tier: Tier,
        interpreter: &mut Interpreter,
        actuator: &mut dyn Actuator,
        body: &mut Body,
        tick: u64,
    ) {
        let skill = match need {
            NeedKind::Eat => "eat",
            NeedKind::Sleep => "sleep",
            NeedKind::Shelter => "shelter",
        };
        let pushed = match self.stack.push(tier, skill, vec![slot.to_string()]) {
            Ok(pushed) => pushed,
            Err(_) => {
                self.wake(
                    "intent_stack",
                    "the stack is full; cannot serve a need".to_owned(),
                );
                return;
            }
        };
        self.events.push(EventKind::IntentPushed {
            intent_id: pushed.intent_id,
            tier: tier.name().to_owned(),
            skill: skill.to_owned(),
            args: vec![slot.to_string()],
        });
        if let Some(suspended) = pushed.suspended {
            self.events.push(EventKind::IntentState {
                intent_id: suspended,
                state: IntentState::Suspended.name().to_owned(),
            });
            self.events.push(EventKind::ReplanNeeded {
                intent_id: suspended,
            });
        }
        // The safe-point path has no in-flight action; the hard path cancels
        // one and its receipt settles `preempted`.
        interpreter.preempt(actuator);
        // Keep the starvation reflex from restarting the same meal while this
        // survival action runs; a lava reflex still preempts.
        self.engine
            .state_mut()
            .set_cooldown(ReflexKind::Starvation, tick + EAT_TICKS + 10);
        body.bump_generation();
        self.maintenance = Some(MaintenanceRun {
            need,
            intent_id: pushed.intent_id,
            slot: Some(slot),
            started: tick,
            clicked: false,
        });
        self.set_owner(Owner::Maintenance(need), body.generation(), tick);
        self.record_switch(tier, tick);
    }

    fn maintenance_ended(
        &self,
        run: &MaintenanceRun,
        tick: u64,
        needs: Needs,
        settings: &Settings,
    ) -> bool {
        tick.saturating_sub(run.started) >= EAT_TICKS || needs.food > settings.eat_soft_threshold
    }

    fn finish_maintenance(&mut self, interpreter: &mut Interpreter, body: &mut Body, tick: u64) {
        let Some(run) = self.maintenance.take() else {
            return;
        };
        self.stack.done(run.intent_id);
        self.events.push(EventKind::IntentState {
            intent_id: run.intent_id,
            state: IntentState::Done.name().to_owned(),
        });
        self.ate = run.slot;
        self.resume_after_maintenance(interpreter, body, tick);
    }

    fn is_safe_point(&self, interpreter: &Interpreter) -> bool {
        if interpreter.until_active() || interpreter.suspended() {
            return false;
        }
        match &self.last_decision {
            None => true,
            Some(Decision::Run { action, .. }) => action.kind == PrimitiveKind::Continuous,
            Some(Decision::Waiting(message)) => !message.ends_with(" in flight"),
            Some(Decision::Observe(_)) => false,
            Some(Decision::Done | Decision::Failed(_)) => true,
        }
    }

    fn guard(&self, mut frame: ControlFrame, body: &Body, view: &View) -> ControlFrame {
        frame.look = self.look_guard.apply(frame.look, body.look(), view);
        frame
    }

    fn maybe_wake(&mut self, kind: ReflexKind, tick: u64, settings: &Settings, needs: Needs) {
        if self.wake_emitted {
            return;
        }
        if tick.saturating_sub(self.owner_since) >= u64::from(settings.wake_after_ticks)
            || needs.health <= settings.critical_health
        {
            self.wake("reflex", format!("{} running", kind.name()));
        }
    }

    fn wake(&mut self, cause: &str, detail: String) {
        if self.wake_emitted {
            return;
        }
        self.wake_emitted = true;
        self.events.push(EventKind::WakeBrain {
            cause: cause.to_owned(),
            detail,
        });
    }

    fn result(
        &mut self,
        source: FrameSource,
        reflex: Option<ReflexKind>,
        maintenance: Option<NeedKind>,
        skill: Option<Decision>,
    ) -> ArbiterTick {
        let wake_brain = self
            .events
            .iter()
            .any(|event| matches!(event, EventKind::WakeBrain { .. }));
        ArbiterTick {
            source,
            owner: self.owner.clone(),
            generation: self.generation,
            events: std::mem::take(&mut self.events),
            reflex,
            maintenance,
            skill,
            skill_intent: self.skill_intent.take(),
            wake_brain,
            ate: self.ate,
        }
    }
}

/// Build a maintenance frame and remember that the one use-click went out.
fn maintenance_frame(tick: u64, generation: u32, run: &mut MaintenanceRun) -> ControlFrame {
    let mut frame = ControlFrame::new(tick, generation, Held::NONE);
    if !run.clicked {
        frame.clicks.push(Click::Use);
        run.clicked = true;
    }
    frame
}

#[cfg(test)]
mod carry_on {
    use praxion_world::Vec3;

    use super::*;

    /// A brain preempts a command and dies: the preempted intent is suspended
    /// and nothing owns the body. `devam` must give the body back (review
    /// finding 3: the four control words work with the brain down).
    #[test]
    fn carry_on_resumes_the_newest_suspended_command() {
        let mut arbiter = Arbiter::new(0);
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        let pushed = arbiter
            .stack
            .push(Tier::Command, "stall", Vec::new())
            .expect("the stack has room");
        arbiter
            .stack
            .set_state(pushed.intent_id, IntentState::Suspended);
        arbiter.owner = Owner::Idle;

        let (resumed, events) = arbiter.resume_command(&mut body, 7);
        assert_eq!(resumed, Some(pushed.intent_id));
        assert_eq!(
            arbiter.owner,
            Owner::Skill {
                intent_id: pushed.intent_id
            }
        );
        assert_eq!(
            arbiter.stack.get(pushed.intent_id).map(|entry| entry.state),
            Some(IntentState::Running)
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                EventKind::IntentState { state, .. } if state == "running"
            )),
            "the log must show the intent as running: {events:?}"
        );
        // A second carry-on has nothing left and changes nothing.
        let generation = arbiter.generation();
        assert_eq!(arbiter.resume_command(&mut body, 8).0, None);
        assert_eq!(arbiter.generation(), generation);
    }

    /// A running skill owns the body; `devam` must not steal it.
    #[test]
    fn carry_on_never_takes_the_body_from_a_running_skill() {
        let mut arbiter = Arbiter::new(0);
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        let pushed = arbiter
            .stack
            .push(Tier::Command, "stall", Vec::new())
            .expect("the stack has room");
        arbiter.owner = Owner::Skill {
            intent_id: pushed.intent_id,
        };
        let generation = arbiter.generation();
        assert_eq!(arbiter.resume_command(&mut body, 7).0, None);
        assert_eq!(arbiter.generation(), generation);
        assert_eq!(
            arbiter.stack.get(pushed.intent_id).map(|entry| entry.state),
            Some(IntentState::Running)
        );
    }
}
