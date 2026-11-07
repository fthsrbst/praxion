//! The body's decision layer: one `Driver` tick at 20 Hz.
//!
//! Each call to [`BodyDriver::frame`] runs the order decided for Demo-0 step
//! 10a:
//!
//! ```text
//! receipts.observe (this tick's decoded packets, stamped with the tick)
//!   -> perception.begin_tick
//!   -> View::capture (reflex facts)
//!   -> arbiter.tick (reflexes, needs, interpreter over the facts)
//!   -> the owner's frame: a reflex frame, a maintenance frame, or the
//!      primitive runner driven by the interpreter's decision
//!   -> perception.sweep (the leftover ray budget)
//!   -> event log flush, tick timing
//! ```
//!
//! The session owns the client tick loop and the physics body. The driver keeps
//! a mirror [`Body`] only because the arbiter's public API needs `&mut Body` to
//! bump the generation; the mirror is refreshed from the session's body at the
//! start of every tick and every emitted frame is re-stamped with the session's
//! generation, so a frame can never go stale between here and the actuator.
//!
//! When a reflex preempts the skill, the in-flight primitive is finished as
//! `Preempted` on that tick: a craft with an open table returns one close frame
//! (merged into the reflex's frame), and an action the interpreter selects on
//! the same tick starts one tick later. Otherwise the abandoned primitive would
//! keep the actuator busy and the skill's retry would be dropped.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use praxion_arbiter::{Arbiter, FrameSource, Needs};
use praxion_eventlog::{EventKind, Outcome as ReceiptOutcome, Versions, Writer};
use praxion_headless::{AppliedReport, Body, ControlFrame, Driver, Held, Look, TickView};
use praxion_interpreter::{
    Actuator, Decision, Facts as InterpreterFacts, InterpreterEvent, IntValue, Interpreter, Outcome,
    StartAction, Truth,
};
use praxion_perception::{Config as PerceptionConfig, Perception};
use praxion_planner::{Block, Cue, Gaze, KnownBlocks};
use praxion_primitives::{Ctx, IntentSpec, PrimitiveEvent, Runner, fact_known};
use praxion_receipts::{Engine, Intent, ReceiptId};
use praxion_reflexes::{EndermanGuard, LatencyClass, ReflexKind, Settings, View};
use praxion_rules::{ArgValue, FactCall, Program};
use praxion_world::{BlockPos, Vec3, World};

use crate::summary::{write_summary, Shared};

/// The body driver's settings.
pub struct BodyOptions {
    pub skill: String,
    pub args: Vec<ArgValue>,
    /// Client ticks to wait after the world is usable, so the harness can set
    /// the scene (give items, place the zombie) before the skill starts.
    pub start_delay_ticks: u64,
    /// The movement-style seed: the same seed gives the same personality.
    pub style_seed: u64,
    pub versions: Versions,
}

/// The owner of a tick's look request, for the gaze's reaction delay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LookSource {
    Reflex(Option<ReflexKind>),
    Maintenance,
    /// The running primitive, keyed by its start tick.
    Primitive(Option<u64>),
}

/// The demo's driver.
pub struct BodyDriver {
    options: BodyOptions,
    shared: Arc<Mutex<Shared>>,
    run_dir: PathBuf,
    log: Writer,
    perception: Arc<Mutex<Perception>>,
    engine: Engine,
    runner: Runner,
    interpreter: Interpreter,
    bridge: BridgeActuator,
    arbiter: Arbiter,
    settings: Settings,
    look_guard: EndermanGuard,
    /// The body's one camera controller: every frame's look request goes
    /// through it (architecture §5.2).
    gaze: Gaze,
    /// Who asked for last tick's look; a change is a new stimulus.
    look_source: Option<LookSource>,
    /// See the module note: the arbiter needs `&mut Body`, the session owns the
    /// real one.
    mirror: Body,
    started: bool,
    done: bool,
    pending: Option<IntentSpec>,
    /// The block a registered place or dig intent changes, so its confirmed
    /// receipt can update the known map at once.
    block_intent: Option<(u64, BlockPos)>,
    last_report: Option<(u64, AppliedReport)>,
    observe_receipt: Option<(ReceiptId, FactCall)>,
    settled_intent: Option<u64>,
    intent_counter: u64,
    counted_receipts: HashSet<u64>,
    last_started: Option<Instant>,
    /// The tick of the first usable world; `start_delay_ticks` counts from here.
    first_tick: Option<u64>,
    /// An action the interpreter selected on the tick a preempted primitive had
    /// to close its menu. It starts on the next tick, after the close.
    deferred_start: Option<StartAction>,
    /// The primitive currently owned by the runner, for `primitive_finished`.
    active_primitive: Option<ActivePrimitive>,
    /// The runner's outcome was handed to the interpreter; do not re-deliver
    /// it to a later flight (a finished continuous primitive leaves it set).
    outcome_delivered: bool,
    /// The last `rule_selected` written, so a steady rule costs no events.
    last_rule_logged: Option<(String, usize)>,
    desync_logged: bool,
}

/// One primitive execution as the log sees it.
struct ActivePrimitive {
    primitive: String,
    args: Vec<String>,
    start_tick: u64,
}

impl BodyDriver {
    pub fn new(
        options: BodyOptions,
        run_dir: PathBuf,
        program: Program,
        settings: Settings,
        shared: Arc<Mutex<Shared>>,
    ) -> std::io::Result<Self> {
        let log = Writer::create(&run_dir)?;
        let gaze = Gaze::new(options.style_seed);
        let mut driver = Self {
            options,
            shared,
            run_dir,
            log,
            perception: Arc::new(Mutex::new(Perception::new(PerceptionConfig::default()))),
            engine: Engine::new(),
            runner: Runner::new(),
            interpreter: Interpreter::new(program),
            bridge: BridgeActuator::default(),
            arbiter: Arbiter::new(0),
            settings,
            look_guard: EndermanGuard::new(),
            gaze,
            look_source: None,
            mirror: Body::new(Vec3::ZERO),
            started: false,
            done: false,
            pending: None,
            block_intent: None,
            last_report: None,
            observe_receipt: None,
            settled_intent: None,
            intent_counter: 0,
            counted_receipts: HashSet::new(),
            last_started: None,
            first_tick: None,
            deferred_start: None,
            active_primitive: None,
            outcome_delivered: false,
            last_rule_logged: None,
            desync_logged: false,
        };
        let versions = driver.options.versions.clone();
        driver.log.run_started(0, now_ns(), versions)?;
        let style = driver.gaze.personality().describe();
        driver.log.note(0, now_ns(), style)?;
        Ok(driver)
    }

    /// Settle the goal once. Later calls (the process's max-ticks path or a
    /// second decision) never overwrite the first outcome.
    fn settle(&mut self, outcome: String, tick: u64, ts_ns: u64) {
        if self.done {
            return;
        }
        self.done = true;
        {
            let mut shared = lock(&self.shared);
            if shared.outcome.is_none() {
                shared.outcome = Some(outcome.clone());
            }
        }
        let _ = self
            .log
            .note(tick, ts_ns, format!("goal_settled outcome={outcome}"));
        let _ = self.log.flush();
        let shared = lock(&self.shared);
        if let Err(error) = write_summary(&self.run_dir, &shared) {
            log::warn!("body: cannot write summary.json: {error}");
        }
    }

    fn start_goal(&mut self, tick: u64, ts_ns: u64) {
        self.started = true;
        let skill = self.options.skill.clone();
        let args = self.options.args.clone();
        match self
            .arbiter
            .command(&mut self.interpreter, &mut self.mirror, tick, &skill, args)
        {
            Ok(accepted) => {
                for event in accepted.events {
                    let _ = self.log.append(tick, ts_ns, event);
                }
                self.log_interpreter_events(tick, ts_ns);
            }
            Err(error) => self.settle(format!("failed(start:{error})"), tick, ts_ns),
        }
    }

    /// Feed this tick's packets to the receipts engine, then register the
    /// primitive's intent from the previous tick's actuator report.
    fn observe_packets(&mut self, view: &TickView<'_>, world: &World, ts_ns: u64) {
        self.register_pending(view.tick, world);
        for packet in view.packets {
            if let Err(error) = self
                .engine
                .observe(&mut self.log, view.tick, ts_ns, &packet.packet, world)
            {
                log::warn!("body: receipt observe: {error}");
            }
        }
        if let Err(error) = self.engine.advance(&mut self.log, view.tick, ts_ns) {
            log::warn!("body: receipt advance: {error}");
        }
        self.tally_receipts();
        self.poll_receipt(world);
    }

    /// Count every settlement once, by receipt id.
    fn tally_receipts(&mut self) {
        let mut shared = lock(&self.shared);
        for settlement in self.engine.settlements() {
            if !self.counted_receipts.insert(settlement.receipt_id) {
                continue;
            }
            let key = outcome_key(settlement.outcome);
            *shared.receipts.entry(key.to_owned()).or_insert(0) += 1;
        }
    }

    /// Registers the pending primitive intent from the previous tick.
    fn register_pending(&mut self, tick: u64, world: &World) {
        let Some(spec) = self.pending.take() else {
            return;
        };
        let Some((report_tick, report)) = self.last_report.clone() else {
            return;
        };
        let intent_id = self.intent_counter + 1;
        let intent = match &spec {
            IntentSpec::Dig { window, .. } => {
                let Some(seq) = report.block_seq else {
                    return;
                };
                let Some((target, before)) = report.block_before else {
                    return;
                };
                Some(
                    Intent::dig(intent_id, target, seq, report_tick)
                        .with_window(*window)
                        .with_initial_block(before)
                        // The block update can already be in the world when
                        // the intent registers (the feed applies packets
                        // before the driver sees them); the observed state
                        // then proves the dig without a later packet.
                        .with_observed_block(world.block_state(target)),
                )
            }
            IntentSpec::Place { block, window, .. } => {
                let Some(seq) = report.block_seq else {
                    return;
                };
                let Some((target, before)) = report.block_before else {
                    return;
                };
                Some(
                    Intent::place(intent_id, target, block.clone(), seq, report_tick)
                        .with_window(*window)
                        .with_initial_block(before)
                        .with_observed_block(world.block_state(target)),
                )
            }
            IntentSpec::Inventory {
                primitive,
                item,
                delta,
                window,
            } => Some(
                Intent::inventory(intent_id, primitive.clone(), item.clone(), *delta, report_tick)
                    .with_window(*window),
            ),
            IntentSpec::StorageInventory {
                primitive,
                item,
                delta,
                window,
            } => Some(
                Intent::storage_inventory(
                    intent_id,
                    primitive.clone(),
                    item.clone(),
                    *delta,
                    report_tick,
                )
                .with_window(*window),
            ),
            IntentSpec::Movement {
                primitive,
                args,
                window,
            } => Some(
                Intent::movement(intent_id, primitive.clone(), report_tick)
                    .with_window(*window)
                    .with_args(args.clone()),
            ),
            IntentSpec::SlotClick {
                container_id,
                slot,
                window,
            } => {
                let Some(state_id) = report.menu_state_id else {
                    return;
                };
                Some(
                    Intent::slot_click(intent_id, *container_id, *slot, state_id, report_tick)
                        .with_window(*window),
                )
            }
            IntentSpec::Attack { entity_id, window } => {
                Some(Intent::attack(intent_id, *entity_id, report_tick).with_window(*window))
            }
            IntentSpec::Fact { fact, window } => Some(
                Intent::fact(intent_id, fact.to_string(), report_tick).with_window(*window),
            ),
        };
        let Some(intent) = intent else {
            return;
        };
        let ts = now_ns();
        match self.engine.register(&mut self.log, tick, ts, intent, world) {
            Ok(receipt) => {
                self.intent_counter = intent_id;
                self.runner.registered(intent_id);
                // The cell the receipt watches: the actuator's report, which
                // follows the block the click actually hit.
                self.block_intent = match &spec {
                    IntentSpec::Dig { .. } | IntentSpec::Place { .. } => report
                        .block_before
                        .map(|(target, _)| (intent_id, target)),
                    _ => None,
                };
                if let IntentSpec::Fact { fact, .. } = &spec {
                    self.observe_receipt = Some((receipt, fact.clone()));
                }
            }
            Err(error) => log::warn!("body: register intent: {error}"),
        }
    }

    fn poll_receipt(&mut self, world: &World) {
        let Some(intent_id) = self.runner.intent_id() else {
            return;
        };
        if self.settled_intent == Some(intent_id) {
            return;
        }
        let Some(outcome) = self.engine.outcome_for_intent(intent_id) else {
            return;
        };
        if outcome == ReceiptOutcome::Expired {
            return;
        }
        self.settled_intent = Some(intent_id);
        if outcome == ReceiptOutcome::Confirmed
            && let Some((block_intent, pos)) = self.block_intent
            && block_intent == intent_id
        {
            lock(&self.perception).note_changed(world, pos);
        }
        self.runner
            .settle(praxion_receipts::interpreter_outcome(outcome));
    }

    fn start_primitive(&mut self, action: &StartAction, tick: u64, ts_ns: u64) {
        self.finish_active_primitive(tick, ts_ns, None);
        if let Err(error) = self.runner.start(action) {
            self.settle(format!("failed(primitive:{error})"), tick, ts_ns);
            return;
        }
        self.outcome_delivered = false;
        self.active_primitive = Some(ActivePrimitive {
            primitive: action.primitive.clone(),
            args: action.args.iter().map(arg_string).collect(),
            start_tick: tick,
        });
    }

    /// Writes `primitive_finished` once for the primitive the runner owned.
    /// `forced` supplies the outcome when the driver itself ended it
    /// (`Preempted`); otherwise the runner's own outcome is used.
    fn finish_active_primitive(&mut self, tick: u64, ts_ns: u64, forced: Option<Outcome>) {
        if self.runner.is_active() {
            return;
        }
        let Some(active) = self.active_primitive.take() else {
            return;
        };
        let outcome = forced
            .or_else(|| self.runner.outcome().cloned())
            .unwrap_or(Outcome::Preempted);
        let (name, kind) = match &outcome {
            Outcome::Confirmed => ("confirmed", None),
            Outcome::Timeout => ("timeout", None),
            Outcome::Preempted => ("preempted", None),
            Outcome::Failed(kind) => ("failed", Some(kind.clone())),
        };
        let _ = self.log.append(
            tick,
            ts_ns,
            EventKind::PrimitiveFinished {
                primitive: active.primitive,
                args: active.args,
                outcome: name.to_owned(),
                failure_kind: kind,
                ticks: tick.saturating_sub(active.start_tick),
            },
        );
    }

    fn log_interpreter_events(&mut self, tick: u64, ts_ns: u64) {
        for event in self.interpreter.take_events() {
            let event = match event {
                InterpreterEvent::FramePushed { skill, depth, .. } => {
                    let _ = self.log.append(
                        tick,
                        ts_ns,
                        EventKind::SkillFrame {
                            skill,
                            depth,
                            outcome: "pushed".to_owned(),
                            failure_kind: None,
                        },
                    );
                    continue;
                }
                InterpreterEvent::FrameDone { skill, depth } => EventKind::SkillFrame {
                    skill,
                    depth,
                    outcome: "done".to_owned(),
                    failure_kind: None,
                },
                InterpreterEvent::FrameFailed { skill, depth, kind } => EventKind::SkillFrame {
                    skill,
                    depth,
                    outcome: "failed".to_owned(),
                    failure_kind: Some(kind),
                },
            };
            let _ = self.log.append(tick, ts_ns, event);
        }
        let changed = match self.interpreter.rule_fired_this_tick() {
            Some((skill, rule)) => {
                let pair = (skill.to_owned(), rule);
                if self.last_rule_logged.as_ref() == Some(&pair) {
                    None
                } else {
                    Some(pair)
                }
            }
            None => None,
        };
        if let Some((skill, rule)) = changed {
            self.last_rule_logged = Some((skill.clone(), rule));
            let _ = self.log.append(
                tick,
                ts_ns,
                EventKind::RuleSelected {
                    skill,
                    rule: rule as u32,
                },
            );
        }
    }

    /// Run one primitive tick for the interpreter's current decision.
    fn run_primitive(
        &mut self,
        view: &TickView<'_>,
        world: &World,
        skill: Option<&Decision>,
        ts_ns: u64,
    ) -> ControlFrame {
        self.finish_active_primitive(view.tick, ts_ns, None);
        // A released continuous primitive must stop here. Before, an
        // `explore` whose frame had finished kept walking for 289 ticks, and
        // the parent's craft waited behind it until the table was far away
        // (seed 303, a second table crafted). The interpreter already dropped
        // the action, so its `preempted` outcome is not delivered.
        if std::mem::take(&mut self.bridge.released) && self.runner.is_continuous() {
            self.runner.cancel();
            self.finish_active_primitive(view.tick, ts_ns, Some(Outcome::Preempted));
            self.outcome_delivered = true;
        }
        if let Some(action) = self.deferred_start.take()
            && !self.runner.is_active()
        {
            self.start_primitive(&action, view.tick, ts_ns);
        }
        if let Some(action) = self.bridge.start.take()
            && !self.runner.is_active()
        {
            self.start_primitive(&action, view.tick, ts_ns);
        }
        if let Some(action) = self.bridge.continuous.take()
            && !self.runner.is_active()
        {
            self.start_primitive(&action, view.tick, ts_ns);
        }
        if let Some(Decision::Observe(fact)) = skill
            && !self.runner.is_active()
        {
            self.start_primitive(
                &StartAction {
                    primitive: "observe".to_owned(),
                    args: vec![ArgValue::Fact(fact.clone())],
                },
                view.tick,
                ts_ns,
            );
        }

        let result = {
            let mut perception = lock(&self.perception);
            let mut ctx = Ctx {
                tick: view.tick,
                body: &self.mirror,
                world,
                perception: &mut perception,
                style: self.gaze.personality(),
            };
            self.runner.tick(&mut ctx)
        };
        if let Some(pending) = result.pending {
            self.pending = Some(pending);
        }
        for event in result.events {
            match event {
                PrimitiveEvent::ExploreFrontier {
                    cell,
                    score,
                    candidates,
                } => {
                    let _ = self.log.append(
                        view.tick,
                        ts_ns,
                        EventKind::ExploreFrontier {
                            cell: format!("{}, {}, {}", cell.x, cell.y, cell.z),
                            score,
                            candidates,
                        },
                    );
                }
            }
        }
        if result.outcome.is_some() {
            // The interpreter polls this through `bridge.outcome` on the next
            // tick, sourced once from `Runner::outcome` (the runner keeps
            // returning a finished outcome until the next start). Emitting the
            // `primitive_finished` event here is enough for this tick.
            self.finish_active_primitive(view.tick, ts_ns, None);
        }

        // An `observe` receipt is confirmed when perception now knows the fact.
        if let Some((receipt, fact)) = self.observe_receipt.clone() {
            let known = {
                let mut perception = lock(&self.perception);
                let mut ctx = Ctx {
                    tick: view.tick,
                    body: &self.mirror,
                    world,
                    perception: &mut perception,
                    style: self.gaze.personality(),
                };
                fact_known(&mut ctx, &fact)
            };
            if known {
                let _ = self
                    .engine
                    .confirm(&mut self.log, receipt, view.tick, now_ns(), "fact_known");
                self.observe_receipt = None;
            }
        }

        result.frame
    }

    /// The arbiter preempted the skill: finish the in-flight primitive as
    /// `Preempted`. A craft with an open table returns one close frame; the
    /// caller merges it into this tick's frame so the server-side window is not
    /// left open across the reflex.
    fn cancel_primitive(
        &mut self,
        view: &TickView<'_>,
        world: &World,
    ) -> Option<ControlFrame> {
        if !self.runner.is_active() {
            return None;
        }
        self.runner
            .settle(praxion_receipts::interpreter_outcome(ReceiptOutcome::Preempted));
        let result = {
            let mut perception = lock(&self.perception);
            let mut ctx = Ctx {
                tick: view.tick,
                body: &self.mirror,
                world,
                perception: &mut perception,
                style: self.gaze.personality(),
            };
            self.runner.tick(&mut ctx)
        };
        // Do not feed this outcome to the interpreter: `preempt`/`cancel`
        // already dropped the flight, and a later flight would poll it as its
        // own. The receipt still settles `preempted` on its own.
        self.finish_active_primitive(view.tick, now_ns(), Some(Outcome::Preempted));
        // `preempt` already dropped the flight; this outcome must not reach a
        // later flight's poll.
        self.outcome_delivered = true;
        Some(result.frame)
    }

    fn log_events(&mut self, tick: u64, ts_ns: u64, events: Vec<EventKind>) {
        let mut shared = lock(&self.shared);
        for event in events {
            if let EventKind::OwnerChanged { ref owner, .. } = event
                && let Some(kind) = owner.strip_prefix("reflex:")
            {
                *shared.reflex_activations.entry(kind.to_owned()).or_insert(0) += 1;
            }
            let _ = self.log.append(tick, ts_ns, event);
        }
    }

    fn note_world(&mut self, world: &World, tick: u64, ts_ns: u64) {
        if let Some(reason) = world.status().reason()
            && !self.desync_logged
        {
            self.desync_logged = true;
            let _ = self.log.world_desync(tick, ts_ns, reason);
        }
        let warnings: BTreeMap<String, u64> = world
            .warning_counts()
            .iter()
            .map(|(kind, count)| (kind.as_str().to_owned(), *count))
            .collect();
        if !warnings.is_empty() {
            lock(&self.shared).warnings = warnings;
        }
    }
}

impl Driver for BodyDriver {
    fn frame(&mut self, view: &TickView<'_>) -> ControlFrame {
        let started = Instant::now();
        if let Some(previous) = self.last_started.replace(started) {
            let period = started.duration_since(previous).as_nanos() as u64;
            lock(&self.shared).record_period(period);
        }
        let work_start = Instant::now();
        let ts_ns = now_ns();
        {
            let mut shared = lock(&self.shared);
            shared.ticks = view.tick + 1;
        }

        let Some(world) = view.world else {
            return self.finish_tick(view, ControlFrame::new(view.tick, view.body.generation(), Held::NONE), work_start, ts_ns, Cue::Continue);
        };
        // The arbiter wants `&mut Body`; the session owns the real one.
        self.mirror = view.body.clone();
        let first_tick = *self.first_tick.get_or_insert(view.tick);

        let settled = lock(&self.shared).outcome.is_some();
        if settled {
            return self.finish_tick(view, ControlFrame::new(view.tick, view.body.generation(), Held::NONE), work_start, ts_ns, Cue::Continue);
        }

        let stage = Instant::now();
        self.observe_packets(view, world, ts_ns);
        let observe_ns = stage.elapsed().as_nanos() as u64;
        let stage = Instant::now();
        {
            let mut perception = lock(&self.perception);
            perception.begin_tick(world);
        }
        let begin_ns = stage.elapsed().as_nanos() as u64;
        if !self.started {
            if view.tick < first_tick.saturating_add(self.options.start_delay_ticks) {
                return self.finish_tick(view, ControlFrame::new(view.tick, view.body.generation(), Held::NONE), work_start, ts_ns, Cue::Continue);
            }
            self.start_goal(view.tick, ts_ns);
            if self.done {
                return self.finish_tick(view, ControlFrame::new(view.tick, view.body.generation(), Held::NONE), work_start, ts_ns, Cue::Continue);
            }
        }

        // The previous tick's primitive outcome goes to the interpreter, once.
        if !self.outcome_delivered
            && let Some(outcome) = self.runner.outcome().cloned()
        {
            self.bridge.outcome = Some(outcome);
            self.outcome_delivered = true;
        }

        let reflex_view = {
            let mut perception = lock(&self.perception);
            View::capture(&mut perception, world, self.arbiter.engine_mut().state_mut())
        };
        let needs = Needs::from_view(&reflex_view);
        let stage = Instant::now();

        let ticked = {
            let perception = Arc::clone(&self.perception);
            let mut facts = FactsView {
                perception: Arc::clone(&perception),
                world,
            };
            let mut blocks = KnownView {
                perception,
                world,
            };
            self.arbiter.tick(
                &reflex_view,
                &mut self.interpreter,
                &mut facts,
                &mut self.bridge,
                needs,
                &self.settings,
                &mut self.mirror,
                view.tick,
                &mut blocks,
            )
        };
        self.log_events(view.tick, ts_ns, ticked.events.clone());
        self.log_interpreter_events(view.tick, ts_ns);
        let arbiter_ns = stage.elapsed().as_nanos() as u64;
        let stage = Instant::now();

        // The arbiter preempted the skill (a reflex took over). Abandon the
        // in-flight primitive now, not on resume: a craft must close its
        // server-side window, and the retry must not be dropped as "busy". An
        // action the interpreter selected on this tick waits one tick so the
        // close goes out alone.
        let cleanup = if std::mem::take(&mut self.bridge.cancelled) {
            if let Some(action) = self.bridge.start.take() {
                self.deferred_start = Some(action);
            }
            if let Some(action) = self.bridge.continuous.take() {
                self.deferred_start = Some(action);
            }
            self.cancel_primitive(view, world)
        } else {
            None
        };

        let mut guarded = false;
        let mut frame = match &ticked.source {
            FrameSource::Reflex(_) | FrameSource::Maintenance(_) => {
                ticked.source.frame().clone()
            }
            FrameSource::Skill(_) | FrameSource::Idle(_) => {
                let mut frame = self.run_primitive(view, world, ticked.skill.as_ref(), ts_ns);
                let wanted = frame.look;
                frame.look = self.look_guard.apply(wanted, view.body.look(), &reflex_view);
                guarded = frame.look != wanted;
                frame
            }
        };

        if let Some(cleanup) = cleanup
            && frame.menu.is_none()
        {
            frame.menu = cleanup.menu;
            frame.menu_mode = cleanup.menu_mode;
        }

        match &ticked.skill {
            Some(Decision::Done) => self.settle("done".to_owned(), view.tick, ts_ns),
            Some(Decision::Failed(kind)) => {
                self.settle(format!("failed({kind})"), view.tick, ts_ns)
            }
            _ => {}
        }
        let primitive_ns = stage.elapsed().as_nanos() as u64;
        let stage = Instant::now();

        {
            let mut perception = lock(&self.perception);
            perception.sweep(world);
        }
        let sweep_ns = stage.elapsed().as_nanos() as u64;
        let stage = Instant::now();
        self.note_world(world, view.tick, ts_ns);
        let _ = self.log.flush();
        let log_ns = stage.elapsed().as_nanos() as u64;
        {
            let mut shared = lock(&self.shared);
            shared.record_stage("observe", observe_ns);
            shared.record_stage("begin_tick", begin_ns);
            shared.record_stage("arbiter", arbiter_ns);
            shared.record_stage("primitive", primitive_ns);
            shared.record_stage("sweep", sweep_ns);
            shared.record_stage("log", log_ns);
        }

        // Why the camera moves (research §4.7): a lethal-fast reflex or the
        // enderman guard moves at once; a new source of the look request (a
        // new primitive or reflex) is a stimulus with a human reaction time.
        let source = match &ticked.source {
            FrameSource::Reflex(_) => LookSource::Reflex(ticked.reflex),
            FrameSource::Maintenance(_) => LookSource::Maintenance,
            FrameSource::Skill(_) | FrameSource::Idle(_) => {
                LookSource::Primitive(self.active_primitive.as_ref().map(|p| p.start_tick))
            }
        };
        let lethal = matches!(&ticked.source, FrameSource::Reflex(_))
            && ticked
                .reflex
                .is_some_and(|kind| kind.latency_class(&reflex_view) == LatencyClass::LethalFast);
        let cue = if lethal || guarded {
            Cue::Urgent
        } else if self.look_source.is_some_and(|last| last != source) {
            Cue::Stimulus
        } else {
            Cue::Continue
        };
        self.look_source = Some(source);

        // The mirror's generation can move (a reflex start or resume); the
        // session's body is the one that will validate the frame.
        frame.tick = view.tick;
        frame.generation = view.body.generation();
        self.finish_tick(view, frame, work_start, ts_ns, cue)
    }

    fn applied(&mut self, tick: u64, report: &AppliedReport) {
        self.last_report = Some((tick, report.clone()));
    }

    fn done(&self) -> bool {
        self.done
    }

    fn summary_json(&self) -> Option<String> {
        let shared = lock(&self.shared);
        serde_json::to_string(&crate::summary::summary(&shared)).ok()
    }
}

impl BodyDriver {
    fn finish_tick(
        &mut self,
        view: &TickView<'_>,
        mut frame: ControlFrame,
        work_start: Instant,
        ts_ns: u64,
        cue: Cue,
    ) -> ControlFrame {
        frame.tick = view.tick;
        frame.generation = view.body.generation();
        // Movement style is the only producer of look output (§5.2): the frame
        // carries the request, the gaze carries the camera.
        let request = frame.look;
        let look = self
            .gaze
            .step(request, view.body.look(), cue, view.tick);
        if let Look::Delta { dyaw, dpitch } = look
            && (dyaw != 0.0 || dpitch != 0.0)
        {
            let current = view.body.look();
            let _ = self.log.note(
                view.tick,
                ts_ns,
                format!(
                    "look yaw={:.3} pitch={:.3} dyaw={:.3} dpitch={:.3}",
                    current.y_rot() + dyaw,
                    current.x_rot() + dpitch,
                    dyaw,
                    dpitch
                ),
            );
        }
        frame.look = look;
        let work = work_start.elapsed().as_nanos() as u64;
        {
            let mut shared = lock(&self.shared);
            shared.record_work(work);
        }
        if work > 25_000_000 {
            let _ = self.log.note(
                view.tick,
                ts_ns,
                format!("slow_tick work_ms={:.3}", work as f64 / 1_000_000.0),
            );
        }
        frame
    }
}

/// The interpreter's facts, over the shared perception.
struct FactsView<'a> {
    perception: Arc<Mutex<Perception>>,
    world: &'a World,
}

impl InterpreterFacts for FactsView<'_> {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        let mut perception = lock(&self.perception);
        praxion_facts::bool_fact(&mut perception, self.world, call)
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        let mut perception = lock(&self.perception);
        praxion_facts::int_fact(&mut perception, self.world, call)
    }
}

/// The planner's view of perception's known map: no rays, unknown stays
/// unknown (docs/decisions.md 2026-11-01, "Perception keeps a known map").
struct KnownView<'a> {
    perception: Arc<Mutex<Perception>>,
    world: &'a World,
}

impl KnownBlocks for KnownView<'_> {
    fn block(&mut self, pos: praxion_world::BlockPos) -> Block {
        let perception = lock(&self.perception);
        match perception.known(pos) {
            Some(remembered) => Block::Known {
                state: remembered.state,
                age_ticks: perception.tick().saturating_sub(remembered.seen_tick),
            },
            None => Block::Unknown,
        }
    }

    fn min_y(&self) -> i32 {
        self.world.min_y().unwrap_or(-64)
    }

    fn height(&self) -> u32 {
        self.world.height().unwrap_or(384)
    }
}

/// The interpreter's actuator bridge: records the chosen action and the
/// primitive runner's outcome. Copied from the live driver so the two run the
/// same contract.
#[derive(Debug, Default)]
struct BridgeActuator {
    start: Option<StartAction>,
    outcome: Option<Outcome>,
    continuous: Option<StartAction>,
    cancelled: bool,
    /// The interpreter stopped its continuous primitive (the frame finished,
    /// or a discrete action replaced it); the driver stops the runner.
    released: bool,
}

impl Actuator for BridgeActuator {
    fn start(&mut self, action: &StartAction, _tick: u64) -> u32 {
        // A terminal outcome of a previous primitive must never be polled as
        // this flight's outcome.
        self.outcome = None;
        self.start = Some(action.clone());
        1
    }

    fn poll(&mut self, _tick: u64) -> Option<Outcome> {
        self.outcome.take()
    }

    fn cancel(&mut self) -> Option<Outcome> {
        self.cancelled = true;
        self.outcome.take()
    }

    fn apply(&mut self, action: &StartAction, _tick: u64) {
        // As in `start`: a new continuous action must never poll the outcome
        // of the one it replaced (a released `explore` ends `preempted`).
        self.outcome = None;
        self.continuous = Some(action.clone());
    }

    fn release(&mut self) {
        self.continuous = None;
        self.released = true;
    }
}

/// A log-only rendering of a resolved primitive argument.
fn arg_string(arg: &ArgValue) -> String {
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

fn outcome_key(outcome: ReceiptOutcome) -> &'static str {    match outcome {
        ReceiptOutcome::Confirmed => "confirmed",
        ReceiptOutcome::Unconfirmed => "unconfirmed",
        ReceiptOutcome::Timeout => "timeout",
        ReceiptOutcome::Preempted => "preempted",
        ReceiptOutcome::Expired => "expired",
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}
