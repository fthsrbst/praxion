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
use praxion_headless::{Body, ControlFrame, Held};
use praxion_interpreter::{Actuator, Decision, Facts, Interpreter};
use praxion_planner::KnownBlocks;
use praxion_reflexes::{EndermanGuard, Engine, ReflexKind, ReflexTick, Settings, View};
use praxion_rules::{ArgValue, PrimitiveKind};

use crate::tier::{IntentStack, IntentState, NeedKind, Needs, Tier, arg_to_string};

/// The starvation reflex stays quiet this long after each tick of a meal, so
/// it never restarts a meal the interpreter is already serving. The cooldown is
/// refreshed every tick of the meal (a meal may take several items), so only a
/// meal that ended early lets the reflex back in.
const MEAL_COOLDOWN_TICKS: u64 = 10;

/// After a meal failed (`no_item`, `not_hungry`) the need is not offered again
/// for this many ticks, so a failing meal cannot restart every tick.
const MEAL_RETRY_TICKS: u64 = 200;

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
    /// The item the body finished eating, when the `eat_food` skill ended
    /// `done` this tick.
    pub ate: Option<String>,
    /// The intent whose skill the arbiter just loaded into the interpreter (a
    /// need's skill, started this tick). The driver records it as the
    /// interpreter's owner and drops any action the previous skill queued.
    pub loaded_intent: Option<u64>,
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

/// A need being served by a skill through the interpreter (decision
/// 2026-11-09 "Five next areas": one eating path, the tested `eat` primitive).
#[derive(Clone, Debug)]
struct MealRun {
    need: NeedKind,
    intent_id: u64,
    /// The food the skill eats, named by item (never a slot number).
    item: String,
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
    meal: Option<MealRun>,
    /// No new meal before this tick (a failed meal cools down).
    meal_blocked_until: u64,
    last_decision: Option<Decision>,
    wake_emitted: bool,
    events: Vec<EventKind>,
    ate: Option<String>,
    loaded_intent: Option<u64>,
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
            meal: None,
            meal_blocked_until: 0,
            last_decision: None,
            wake_emitted: false,
            events: Vec::new(),
            ate: None,
            loaded_intent: None,
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
        self.command_in(Tier::Command, interpreter, body, tick, skill, args)
    }

    /// [`Arbiter::command`] with the tier named. The body's own night fallback
    /// pushes `night_watch` at [`Tier::Survival`] (architecture §5.4); any later
    /// command suspends it like any running intent, so it yields to the brain
    /// and the user and resumes when their goal settles.
    pub fn command_in(
        &mut self,
        tier: Tier,
        interpreter: &mut Interpreter,
        body: &mut Body,
        tick: u64,
        skill: &str,
        args: Vec<ArgValue>,
    ) -> Result<CommandAccepted, CommandError> {
        let strings: Vec<String> = args.iter().map(arg_to_string).collect();
        // A command during a meal ends the meal: its skill would otherwise
        // run under the meal's ownership once the interpreter holds the
        // command's skill. The need comes back on its own if still hungry.
        let dropped_meal = self.meal.take();
        if let Some(run) = &dropped_meal {
            self.stack.done(run.intent_id);
        }
        let pushed = self
            .stack
            .push(tier, skill, strings.clone())
            .map_err(|_| CommandError::StackFull)?;
        if let Err(refusal) = interpreter.start_skill(skill, args) {
            self.stack.rollback(pushed);
            if dropped_meal.is_some() {
                // The meal is gone and the command was refused: nobody owns
                // the body until the next tick picks an owner.
                self.owner = Owner::Idle;
            }
            return Err(CommandError::Skill(refusal));
        }

        let mut events = Vec::new();
        if let Some(run) = &dropped_meal {
            events.push(EventKind::IntentState {
                intent_id: run.intent_id,
                state: IntentState::Done.name().to_owned(),
            });
        }
        events.push(EventKind::IntentPushed {
            intent_id: pushed.intent_id,
            tier: tier.name().to_owned(),
            skill: skill.to_owned(),
            args: strings,
        });
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
        if dropped_meal.is_some() || matches!(self.owner, Owner::Idle | Owner::Skill { .. }) {
            let owner = Owner::Skill {
                intent_id: pushed.intent_id,
            };
            self.owner = owner.clone();
            self.owner_since = tick;
            self.wake_emitted = false;
            self.record_switch(tier, tick);
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
        self.loaded_intent = None;
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

        // 2. A need's skill in progress: the interpreter runs it like any
        // skill, and the driver turns its decisions into primitives. The
        // arbiter only watches for its end.
        if let Some(run) = self.meal.clone() {
            self.engine
                .state_mut()
                .set_cooldown(ReflexKind::Starvation, tick + MEAL_COOLDOWN_TICKS);
            let decision = interpreter.tick(facts, actuator);
            self.last_decision = Some(decision.clone());
            self.skill_intent = Some(run.intent_id);
            match &decision {
                Decision::Done => self.finish_meal(interpreter, body, tick, None),
                Decision::Failed(kind) => {
                    self.finish_meal(interpreter, body, tick, Some(kind.clone()));
                }
                _ => {}
            }
            let frame = ControlFrame::new(tick, body.generation(), Held::NONE);
            let frame = self.guard(frame, body, view);
            let source = if self.meal.is_some() || matches!(self.owner, Owner::Skill { .. }) {
                FrameSource::Skill(frame)
            } else {
                FrameSource::Idle(frame)
            };
            return self.result(source, None, Some(run.need), Some(decision));
        }

        // 3. Survival hard threshold: preempt now, no safe-point wait.
        if needs.food <= settings.eat_hard_threshold && tick >= self.meal_blocked_until {
            match view.best_food.map(|food| food_name(food.kind.to_str())) {
                Some(item) => {
                    if self.start_need(
                        NeedKind::Eat,
                        &item,
                        Tier::Survival,
                        interpreter,
                        actuator,
                        body,
                        tick,
                    ) {
                        let frame = ControlFrame::new(tick, body.generation(), Held::NONE);
                        let frame = self.guard(frame, body, view);
                        return self.result(
                            FrameSource::Maintenance(frame),
                            None,
                            Some(NeedKind::Eat),
                            None,
                        );
                    }
                }
                // No food to eat: wake the brain so it can set a `get_food` goal
                // (decision 2026-11-09 "Five next areas"). The arbiter does not
                // push `get_food` itself: hunting runs for minutes, and a need
                // at the survival tier would take the body from the user's
                // command for all of it, with no goal budget or receipt of its
                // own. The brain already owns goals.
                None => self.wake(
                    "hunger_no_food",
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
            && tick >= self.meal_blocked_until
            && let Some(item) = view.best_food.map(|food| food_name(food.kind.to_str()))
        {
            let from = self.tier();
            let until = interpreter.until_active();
            if self.is_safe_point(interpreter)
                && self.may_switch(from, Tier::Maintenance, 1.0, 0.0, until, settings, tick)
                && self.start_need(
                    NeedKind::Eat,
                    &item,
                    Tier::Maintenance,
                    interpreter,
                    actuator,
                    body,
                    tick,
                )
            {
                // The command's action queued this tick is dropped by the
                // driver; the meal's first primitive starts next tick.
                let frame = ControlFrame::new(tick, body.generation(), Held::NONE);
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

    /// The body died and respawned (`Event::Died`): a new player stands in a
    /// new place. Reset what the old life left behind (the reflex state and
    /// routine, a meal in progress, an in-flight action), hand the body to the
    /// command that was running (it re-evaluates from the top) or to no one,
    /// and move to a new generation so a stale frame can never apply. The
    /// returned events are the stack lines for the driver's log. Decision
    /// 2026-11-09 "Five next areas", critique finding 11.
    pub fn on_death(
        &mut self,
        interpreter: &mut Interpreter,
        actuator: &mut dyn Actuator,
        body: &mut Body,
        tick: u64,
    ) -> Vec<EventKind> {
        self.events.clear();
        self.engine.on_death();
        self.meal_blocked_until = 0;
        if let Some(run) = self.meal.take() {
            self.stack.done(run.intent_id);
            self.events.push(EventKind::IntentState {
                intent_id: run.intent_id,
                state: IntentState::Done.name().to_owned(),
            });
        }
        interpreter.preempt(actuator);
        match self.owner {
            // A reflex or a meal owned the body: the command underneath, if
            // any, gets it back.
            Owner::Reflex(_) | Owner::Maintenance(_) => {
                self.resume_after_reflex(interpreter, body, tick);
            }
            Owner::Skill { .. } | Owner::Idle => {
                interpreter.resume();
                body.bump_generation();
                self.generation = body.generation();
            }
        }
        self.wake_emitted = false;
        std::mem::take(&mut self.events)
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
        if let Some(run) = self.meal.take() {
            // The meal is over, not paused: a reflex interrupted it. The body
            // writes `done` for its own intent; the receipt of the cancelled
            // `eat` already settles `preempted`.
            self.stack.done(run.intent_id);
            self.events.push(EventKind::IntentState {
                intent_id: run.intent_id,
                state: IntentState::Done.name().to_owned(),
            });
            interpreter.preempt(actuator);
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
                // A command, or the body's own night fallback (a command pushed
                // at the survival tier). A meal is also survival tier but it is
                // served by its `MealRun`, never resumed as a command.
                let resumable_tier = entry.tier == Tier::Command
                    || (entry.tier == Tier::Survival
                        && self
                            .meal
                            .as_ref()
                            .is_none_or(|meal| meal.intent_id != entry.intent_id));
                resumable_tier
                    && matches!(entry.state, IntentState::Suspended | IntentState::Running)
            })?
            .intent_id;
        if self
            .stack
            .get(entry)
            .is_some_and(|entry| entry.state == IntentState::Suspended)
        {
            self.stack.set_state(entry, IntentState::Running);
            self.events.push(EventKind::IntentState {
                intent_id: entry,
                state: IntentState::Running.name().to_owned(),
            });
        }
        Some(entry)
    }

    /// Serve a need with its skill: push the intent, load the skill into the
    /// interpreter and let the normal interpreter path run it. A slot number
    /// never becomes an argument: the skill gets the food's item name.
    ///
    /// Returns whether the need started. A skill the program does not have is
    /// refused here with a wake (a typed error), and the stack is left as it was.
    #[allow(clippy::too_many_arguments)]
    fn start_need(
        &mut self,
        need: NeedKind,
        item: &str,
        tier: Tier,
        interpreter: &mut Interpreter,
        actuator: &mut dyn Actuator,
        body: &mut Body,
        tick: u64,
    ) -> bool {
        let skill = match need {
            NeedKind::Eat => "eat_food",
            // No skill serves these yet; a name nobody defined must not be
            // pushed (critique 2026-11-09, finding 10).
            NeedKind::Sleep | NeedKind::Shelter => {
                self.wake(
                    "need_skill",
                    format!("no skill serves the {} need", need.name()),
                );
                return false;
            }
        };
        let pushed = match self.stack.push(tier, skill, vec![item.to_owned()]) {
            Ok(pushed) => pushed,
            Err(_) => {
                self.wake(
                    "intent_stack",
                    "the stack is full; cannot serve a need".to_owned(),
                );
                return false;
            }
        };
        // The safe-point path has no in-flight action; the hard path cancels
        // one and its receipt settles `preempted`.
        interpreter.preempt(actuator);
        if let Err(refusal) = interpreter.start_skill(skill, vec![ArgValue::Item(item.to_owned())])
        {
            self.stack.rollback(pushed);
            interpreter.resume();
            self.meal_blocked_until = tick + MEAL_RETRY_TICKS;
            self.wake("need_skill", format!("cannot start {skill}: {refusal}"));
            return false;
        }
        self.events.push(EventKind::IntentPushed {
            intent_id: pushed.intent_id,
            tier: tier.name().to_owned(),
            skill: skill.to_owned(),
            args: vec![item.to_owned()],
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
        // Keep the starvation reflex from restarting the same meal while this
        // survival action runs; a lava reflex still preempts.
        self.engine
            .state_mut()
            .set_cooldown(ReflexKind::Starvation, tick + MEAL_COOLDOWN_TICKS);
        body.bump_generation();
        self.meal = Some(MealRun {
            need,
            intent_id: pushed.intent_id,
            item: item.to_owned(),
        });
        self.loaded_intent = Some(pushed.intent_id);
        self.set_owner(Owner::Maintenance(need), body.generation(), tick);
        self.record_switch(tier, tick);
        true
    }

    /// The need's skill ended: the body alone writes `done` for its intent and
    /// hands the body back. A failed meal wakes the brain and is not offered
    /// again for a while.
    fn finish_meal(
        &mut self,
        interpreter: &mut Interpreter,
        body: &mut Body,
        tick: u64,
        failure: Option<String>,
    ) {
        let Some(run) = self.meal.take() else {
            return;
        };
        self.stack.done(run.intent_id);
        self.events.push(EventKind::IntentState {
            intent_id: run.intent_id,
            state: IntentState::Done.name().to_owned(),
        });
        match failure {
            None => self.ate = Some(run.item),
            Some(kind) => {
                self.meal_blocked_until = tick + MEAL_RETRY_TICKS;
                self.wake(
                    "hunger",
                    format!("{} ended failed({kind}) for {}", run.need.name(), run.item),
                );
            }
        }
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
            ate: self.ate.take(),
            loaded_intent: self.loaded_intent.take(),
        }
    }
}

/// The food's item name without the namespace, the form skills and receipts
/// use (`bread`, never a slot number).
fn food_name(name: &str) -> String {
    name.strip_prefix("minecraft:").unwrap_or(name).to_owned()
}
