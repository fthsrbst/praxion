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
//!
//! The primitive/receipt plumbing follows `praxion-primitives`' `LiveDriver`,
//! which runs the same contract for one skill without reflexes.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use praxion_arbiter::{Arbiter, FrameSource, Needs};
use praxion_eventlog::{EventKind, Outcome as ReceiptOutcome, Versions, Writer};
use praxion_headless::{AppliedReport, Body, ControlFrame, Driver, Held, TickView};
use praxion_interpreter::{
    Actuator, Decision, Facts as InterpreterFacts, IntValue, Interpreter, Outcome, StartAction,
    Truth,
};
use praxion_perception::{Config as PerceptionConfig, Perception};
use praxion_planner::{Block, KnownBlocks, LookStyle};
use praxion_primitives::{Ctx, IntentSpec, Runner, fact_known};
use praxion_receipts::{Engine, Intent, ReceiptId};
use praxion_reflexes::{EndermanGuard, Settings, View};
use praxion_rules::{ArgValue, FactCall, Program};
use praxion_world::{World, Vec3};

use crate::summary::{write_summary, Shared};

/// The body driver's settings.
pub struct BodyOptions {
    pub skill: String,
    pub args: Vec<ArgValue>,
    /// Client ticks to wait after the world is usable, so the harness can set
    /// the scene (give items, place the zombie) before the skill starts.
    pub start_delay_ticks: u64,
    pub look: LookStyle,
    pub versions: Versions,
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
    /// See the module note: the arbiter needs `&mut Body`, the session owns the
    /// real one.
    mirror: Body,
    started: bool,
    done: bool,
    pending: Option<IntentSpec>,
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
    desync_logged: bool,
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
            mirror: Body::new(Vec3::ZERO),
            started: false,
            done: false,
            pending: None,
            last_report: None,
            observe_receipt: None,
            settled_intent: None,
            intent_counter: 0,
            counted_receipts: HashSet::new(),
            last_started: None,
            first_tick: None,
            deferred_start: None,
            desync_logged: false,
        };
        let versions = driver.options.versions.clone();
        driver.log.run_started(0, now_ns(), versions)?;
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
            eprintln!("body: cannot write summary.json: {error}");
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
                eprintln!("body: receipt observe: {error}");
            }
        }
        if let Err(error) = self.engine.advance(&mut self.log, view.tick, ts_ns) {
            eprintln!("body: receipt advance: {error}");
        }
        self.tally_receipts();
        self.poll_receipt();
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

    /// Registers the pending primitive intent from the previous tick, exactly
    /// like `praxion-primitives`' live driver.
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
                        .with_initial_block(before),
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
            IntentSpec::Movement { primitive, window } => Some(
                Intent::movement(intent_id, primitive.clone(), report_tick).with_window(*window),
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
                if let IntentSpec::Fact { fact, .. } = &spec {
                    self.observe_receipt = Some((receipt, fact.clone()));
                }
            }
            Err(error) => eprintln!("body: register intent: {error}"),
        }
    }

    fn poll_receipt(&mut self) {
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
        self.runner
            .settle(praxion_receipts::interpreter_outcome(outcome));
    }

    fn start_primitive(&mut self, action: &StartAction, tick: u64, ts_ns: u64) {
        if let Err(error) = self.runner.start(action) {
            self.settle(format!("failed(primitive:{error})"), tick, ts_ns);
        }
    }

    /// Run one primitive tick for the interpreter's current decision.
    fn run_primitive(
        &mut self,
        view: &TickView<'_>,
        world: &World,
        skill: Option<&Decision>,
        reflex_view: &View,
        ts_ns: u64,
    ) -> ControlFrame {
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
                look: self.options.look,
            };
            self.runner.tick(&mut ctx)
        };
        if let Some(pending) = result.pending {
            self.pending = Some(pending);
        }
        if let Some(outcome) = result.outcome {
            self.bridge.outcome = Some(outcome);
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
                    look: self.options.look,
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

        let mut frame = result.frame;
        frame.look = self.look_guard.apply(frame.look, view.body.look(), reflex_view);
        frame
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
                look: self.options.look,
            };
            self.runner.tick(&mut ctx)
        };
        // Do not feed this outcome to the interpreter: `preempt`/`cancel`
        // already dropped the flight, and a later flight would poll it as its
        // own. The receipt still settles `preempted` on its own.
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
        if let Some(reason) = world.status().reason() {
            if !self.desync_logged {
                self.desync_logged = true;
                let _ = self.log.world_desync(tick, ts_ns, reason);
            }
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
            return self.finish_tick(view, ControlFrame::new(view.tick, view.body.generation(), Held::NONE), work_start, ts_ns);
        };
        // The arbiter wants `&mut Body`; the session owns the real one.
        self.mirror = view.body.clone();
        let first_tick = *self.first_tick.get_or_insert(view.tick);

        let settled = lock(&self.shared).outcome.is_some();
        if settled {
            return self.finish_tick(view, ControlFrame::new(view.tick, view.body.generation(), Held::NONE), work_start, ts_ns);
        }

        self.observe_packets(view, world, ts_ns);
        {
            let mut perception = lock(&self.perception);
            perception.begin_tick(world);
        }
        if !self.started {
            if view.tick < first_tick.saturating_add(self.options.start_delay_ticks) {
                return self.finish_tick(view, ControlFrame::new(view.tick, view.body.generation(), Held::NONE), work_start, ts_ns);
            }
            self.start_goal(view.tick, ts_ns);
            if self.done {
                return self.finish_tick(view, ControlFrame::new(view.tick, view.body.generation(), Held::NONE), work_start, ts_ns);
            }
        }

        // The previous tick's primitive outcome goes to the interpreter.
        if let Some(outcome) = self.runner.outcome().cloned() {
            self.bridge.outcome = Some(outcome);
        }

        let reflex_view = {
            let mut perception = lock(&self.perception);
            View::capture(&mut perception, world, self.arbiter.engine_mut().state_mut())
        };
        let needs = Needs::from_view(&reflex_view);

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

        let mut frame = match &ticked.source {
            FrameSource::Reflex(_) | FrameSource::Maintenance(_) => {
                ticked.source.frame().clone()
            }
            FrameSource::Skill(_) | FrameSource::Idle(_) => {
                self.run_primitive(view, world, ticked.skill.as_ref(), &reflex_view, ts_ns)
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

        {
            let mut perception = lock(&self.perception);
            perception.sweep(world);
        }
        self.note_world(world, view.tick, ts_ns);
        let _ = self.log.flush();

        // The mirror's generation can move (a reflex start or resume); the
        // session's body is the one that will validate the frame.
        frame.tick = view.tick;
        frame.generation = view.body.generation();
        self.finish_tick(view, frame, work_start, ts_ns)
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
    ) -> ControlFrame {
        frame.tick = view.tick;
        frame.generation = view.body.generation();
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
}

impl Actuator for BridgeActuator {
    fn start(&mut self, action: &StartAction, _tick: u64) -> u32 {
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
        self.continuous = Some(action.clone());
    }

    fn release(&mut self) {
        self.continuous = None;
    }
}

fn outcome_key(outcome: ReceiptOutcome) -> &'static str {
    match outcome {
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
