//! The body's decision layer: one `Driver` tick at 20 Hz.
//!
//! Each call to [`BodyDriver::frame`] runs the order decided for Demo-0 step
//! 10a:
//!
//! ```text
//! receipts.observe (this tick's decoded packets, stamped with the tick)
//!   -> perception.begin_tick
//!   -> the brain inbox drain (one goal-start path: a brain command or `--goal`)
//!   -> View::capture (reflex facts)
//!   -> arbiter.tick (reflexes, needs, interpreter over the facts)
//!   -> the owner's frame: a reflex frame, a maintenance frame, or the
//!      primitive runner driven by the interpreter's decision
//!   -> perception.sweep (the leftover ray budget)
//!   -> the snapshot publish, event log flush, tick timing
//! ```
//!
//! A goal settles per command: the `command_receipt` first, then `stack.done`,
//! the `summary.json` entry and the snapshot. It never ends the process; the
//! body idles with reflexes on until the next command.
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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use praxion_arbiter::{Arbiter, CommandError, FrameSource, IntentState, Needs, Owner};
use praxion_eventlog::{Event, EventKind, Outcome as ReceiptOutcome, Versions, Writer};
use praxion_headless::{AppliedReport, Body, ControlFrame, Driver, Held, Look, TickView};
use praxion_interpreter::{
    Actuator, Decision, Facts as InterpreterFacts, IntValue, Interpreter, InterpreterEvent,
    Outcome, StartAction, Truth,
};
use praxion_perception::{Config as PerceptionConfig, Perception};
use praxion_planner::{Block, Cue, Gaze, GazeMode, KnownBlocks};
use praxion_port::{
    BrainMessage, Command as PortCommand, ConnectionState, Facts as PortFacts, PortServer,
    Snapshot, StackEntry,
};
use praxion_primitives::{Ctx, IntentSpec, PrimitiveEvent, Runner, fact_known};
use praxion_receipts::{Engine, Intent, ReceiptId};
use praxion_reflexes::{EndermanGuard, LatencyClass, ReflexKind, Settings, View};
use praxion_rules::{ArgValue, FactCall, Program};
use praxion_world::{BlockPos, Vec3, World};

use crate::summary::{GoalResult, Shared, write_summary};

/// The command id of a `--goal` run. There is one goal-start path: `--goal`
/// becomes this injected command (proposal §2.6).
pub const CLI_COMMAND_ID: &str = "cli";

/// Client ticks between two published snapshots: one second at 20 Hz.
pub const SNAPSHOT_INTERVAL_TICKS: u64 = 20;

/// The default silence limit before the body notes `brain_silent` (proposal
/// §2.5). A body option so a unit test need not wait three seconds.
pub const DEFAULT_BRAIN_SILENCE: Duration = Duration::from_secs(3);

/// The synthetic root skill a condition is parsed and evaluated in, so the
/// rules parser is the only guard language (no second evaluator).
const CONDITION_SKILL: &str = "__condition";

/// The body driver's settings.
pub struct BodyOptions {
    /// The `--goal` skill, injected as command [`CLI_COMMAND_ID`]. `None` in a
    /// brain session without `--goal`.
    pub skill: Option<String>,
    pub args: Vec<ArgValue>,
    /// Client ticks to wait after the world is usable, so the harness can set
    /// the scene (give items, place the zombie) before the skill starts.
    pub start_delay_ticks: u64,
    /// The movement-style seed: the same seed gives the same personality.
    pub style_seed: u64,
    /// How long the brain may stay quiet before the body notes it.
    pub brain_silence: Duration,
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

/// The `--goal` command's lifecycle in a CLI run. Its three old flags (the
/// pending command, "injected", "settled") are the states of one value.
enum CliGoal {
    /// No `--goal`: a brain session with nothing injected.
    None,
    /// Waiting out the start delay before it is accepted.
    Waiting(PortCommand),
    /// Accepted (or rejected); it is not injected twice.
    Injected,
    /// Settled; the CLI process lingers and then exits.
    Settled,
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
    /// The brain's channel, when the process was started with `--port`.
    port: Option<Arc<PortServer>>,
    /// Commands accepted from the port, by intent id, until they settle.
    commands: BTreeMap<u64, CommandRecord>,
    /// The intent whose skill the interpreter currently holds. A resume path
    /// that never saw a settlement (a reflex or a meal ended, a goal was
    /// popped) can hand the body to an intent the interpreter does not hold;
    /// the driver restarts that record's skill when it sees the mismatch.
    interpreter_intent: Option<u64>,
    /// The `--goal` command, once the start delay has passed.
    cli: CliGoal,
    /// The port was connected last tick, so the connect edge publishes state.
    port_connected: bool,
    /// A `brain_silent` note is already written for this silence.
    brain_silent: bool,
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

/// One accepted command, until it settles.
struct CommandRecord {
    command_id: String,
    skill: String,
    /// The resolved arguments, so a suspended command can restart from scratch
    /// when a newer command gives the body back.
    args: Vec<ArgValue>,
    /// Ticks the goal spent as the running owner; the budget counts these,
    /// not the suspended time under a reflex, a meal or a newer command.
    run_ticks: u64,
    /// Per-goal tick limit; `None` means the process cap only.
    budget_ticks: Option<u64>,
    /// The guard text, already parsed once at accept time.
    condition: Option<String>,
}

/// The payload of a `command_receipt` event, built field by field.
struct Receipt {
    command_id: String,
    state: &'static str,
    intent_id: Option<u64>,
    generation: Option<u32>,
    outcome: Option<String>,
    failure_kind: Option<String>,
    reason: Option<&'static str>,
    condition: Option<String>,
}

impl Receipt {
    fn new(command_id: &str, state: &'static str) -> Self {
        Self {
            command_id: command_id.to_owned(),
            state,
            intent_id: None,
            generation: None,
            outcome: None,
            failure_kind: None,
            reason: None,
            condition: None,
        }
    }

    fn event(self) -> EventKind {
        EventKind::CommandReceipt {
            command_id: self.command_id,
            state: self.state.to_owned(),
            intent_id: self.intent_id,
            generation: self.generation,
            outcome: self.outcome,
            failure_kind: self.failure_kind,
            reason: self.reason.map(str::to_owned),
            condition: self.condition,
        }
    }
}

impl BodyDriver {
    pub fn new(
        options: BodyOptions,
        run_dir: PathBuf,
        program: Program,
        settings: Settings,
        shared: Arc<Mutex<Shared>>,
        port: Option<Arc<PortServer>>,
    ) -> std::io::Result<Self> {
        let log = Writer::create(&run_dir)?;
        let gaze = Gaze::new(options.style_seed);
        // `--goal` is injected as command `cli`, so the brain and the CLI use
        // one goal-start path (proposal §2.6). The generation is filled in at
        // injection: a reflex during the start delay must not make it stale.
        let cli = match options.skill.as_ref() {
            Some(skill) => CliGoal::Waiting(PortCommand {
                id: CLI_COMMAND_ID.to_owned(),
                skill: skill.clone(),
                args: options.args.iter().map(arg_string).collect(),
                tier: "command".to_owned(),
                expires_at_tick: u64::MAX,
                based_on_generation: 0,
                condition: None,
                budget_ticks: None,
            }),
            None => CliGoal::None,
        };
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
            port,
            commands: BTreeMap::new(),
            interpreter_intent: None,
            cli,
            port_connected: false,
            brain_silent: false,
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
        driver.record(0, now_ns(), EventKind::RunStarted { versions });
        let style = driver.gaze.personality().describe();
        driver.record(0, now_ns(), EventKind::Note { text: style });
        Ok(driver)
    }

    /// Log one event and, when a brain is connected, forward it through the
    /// port (proposal §2.2: the event's log `seq` is the wire `seq`).
    fn record(&mut self, tick: u64, ts_ns: u64, event: EventKind) {
        let forward = self
            .port
            .as_ref()
            .is_some_and(|_| praxion_port::forwards(&event));
        if forward {
            let copy = event.clone();
            if let Ok(seq) = self.log.append(tick, ts_ns, event)
                && let Some(port) = &self.port
            {
                port.publish(Event {
                    seq,
                    tick,
                    ts_ns,
                    event: copy,
                });
            }
        } else {
            let _ = self.log.append(tick, ts_ns, event);
        }
    }

    /// Write one command receipt.
    fn receipt(&mut self, tick: u64, ts_ns: u64, receipt: Receipt) {
        self.record(tick, ts_ns, receipt.event());
    }

    /// Inject the `--goal` command once the start delay has passed. It goes
    /// through the same `accept_command` as a brain command: one goal-start
    /// path.
    fn inject_cli(&mut self, tick: u64, ts_ns: u64, world: &World) {
        let waiting = std::mem::replace(&mut self.cli, CliGoal::None);
        let CliGoal::Waiting(mut command) = waiting else {
            self.cli = waiting;
            return;
        };
        self.cli = CliGoal::Injected;
        command.based_on_generation = self.arbiter.generation();
        self.accept_command(command, tick, ts_ns, world);
    }

    /// Drain the brain's inbox and accept what it holds. The order is the
    /// proposal's: `pending` first, expiry before anything starts, then the
    /// generation and skill checks (proposal §2.3).
    fn poll_port(&mut self, tick: u64, ts_ns: u64, world: &World) {
        let messages = self
            .port
            .as_ref()
            .map(|port| {
                port.set_tick(tick);
                port.poll_incoming()
            })
            .unwrap_or_default();
        for message in messages {
            match message {
                BrainMessage::Command(command) => self.accept_command(command, tick, ts_ns, world),
                BrainMessage::Pop { intent_id, .. } => self.pop_intent(intent_id, tick, ts_ns),
                // The handshake, pings and resync are the port's own business.
                BrainMessage::Hello { .. } | BrainMessage::ResyncRequest | BrainMessage::Ping => {}
            }
        }
        self.check_brain_silence(tick, ts_ns);
    }

    /// One command from `pending` to `accepted`, `expired` or `rejected`.
    fn accept_command(&mut self, command: PortCommand, tick: u64, ts_ns: u64, world: &World) {
        self.receipt(tick, ts_ns, Receipt::new(&command.id, "pending"));
        if tick > command.expires_at_tick {
            self.receipt(tick, ts_ns, Receipt::new(&command.id, "expired"));
            return;
        }
        if command.based_on_generation != self.arbiter.generation() {
            self.reject(&command.id, "stale_generation", tick, ts_ns);
            return;
        }
        let args = {
            let program = self.interpreter.program();
            if program.skill(&command.skill).is_none() {
                self.reject(&command.id, "unknown_skill", tick, ts_ns);
                return;
            }
            match crate::cli::resolve_goal_args(program, &command.skill, &command.args) {
                Ok(args) => args,
                Err(_) => {
                    self.reject(&command.id, "bad_args", tick, ts_ns);
                    return;
                }
            }
        };
        if command
            .condition
            .as_deref()
            .is_some_and(|text| condition_program(text).is_none())
        {
            self.reject(&command.id, "bad_condition", tick, ts_ns);
            return;
        }

        let record_args = args.clone();
        match self.arbiter.command(
            &mut self.interpreter,
            &mut self.mirror,
            tick,
            &command.skill,
            args,
        ) {
            Ok(accepted) => {
                // The new skill must never poll the old one's outcome as its
                // own first flight, and nothing the old skill queued may run
                // under it.
                self.abandon_flight();
                self.interpreter_intent = Some(accepted.intent_id);
                for event in accepted.events {
                    self.record(tick, ts_ns, event);
                }
                self.log_interpreter_events(tick, ts_ns);
                {
                    let mut shared = lock(&self.shared);
                    shared.skill = command.skill.clone();
                    shared.args = command.args.clone();
                    shared.outcome = None;
                }
                self.commands.insert(
                    accepted.intent_id,
                    CommandRecord {
                        command_id: command.id.clone(),
                        skill: command.skill.clone(),
                        args: record_args,
                        run_ticks: 0,
                        budget_ticks: command.budget_ticks,
                        condition: command.condition.clone(),
                    },
                );
                self.receipt(
                    tick,
                    ts_ns,
                    Receipt {
                        intent_id: Some(accepted.intent_id),
                        generation: Some(accepted.generation),
                        ..Receipt::new(&command.id, "accepted")
                    },
                );
                self.publish_state(world, tick);
            }
            Err(error) => {
                let reason = match error {
                    CommandError::StackFull => "stack_full",
                    CommandError::Skill(_) => "bad_args",
                };
                self.reject(&command.id, reason, tick, ts_ns);
            }
        }
    }

    /// A refused command writes its `rejected` receipt; a refused `--goal` is
    /// also the settled CLI goal, so the process must not run to its tick cap.
    fn reject(&mut self, command_id: &str, reason: &'static str, tick: u64, ts_ns: u64) {
        self.receipt(
            tick,
            ts_ns,
            Receipt {
                reason: Some(reason),
                ..Receipt::new(command_id, "rejected")
            },
        );
        if command_id == CLI_COMMAND_ID {
            self.settle_cli_failure(&format!("start:{reason}"), tick, ts_ns);
        }
    }

    /// A CLI goal that never started is settled anyway: the outcome names the
    /// start failure, `done()` turns true and the summary is written.
    fn settle_cli_failure(&mut self, kind: &str, tick: u64, ts_ns: u64) {
        let outcome = format!("failed({kind})");
        lock(&self.shared).outcome = Some(outcome.clone());
        self.record(
            tick,
            ts_ns,
            EventKind::Note {
                text: format!("goal_settled command={CLI_COMMAND_ID} outcome={outcome}"),
            },
        );
        let _ = self.log.flush();
        {
            let shared = lock(&self.shared);
            if let Err(error) = write_summary(&self.run_dir, &shared) {
                log::warn!("body: cannot write summary.json: {error}");
            }
        }
        self.cli = CliGoal::Settled;
    }

    /// Abandon the in-flight primitive of a goal that no longer runs: clear
    /// the interpreter's queued action and outcome, and stop the runner so a
    /// settled goal can never keep the body busy. A continuous primitive is
    /// released on the next frame; a discrete one is cancelled there.
    fn abandon_flight(&mut self) {
        self.bridge.outcome = None;
        self.bridge.start = None;
        self.bridge.continuous = None;
        self.deferred_start = None;
        self.outcome_delivered = true;
        // The new skill replaces the old one's in-flight primitive: end it
        // `preempted` now, as a reflex preemption does. Without this the runner
        // would stay busy and the new skill's first action would be dropped as
        // "busy". A continuous primitive is released; a discrete one is
        // cancelled (and may close a menu).
        if self.runner.is_continuous() {
            self.bridge.released = true;
        } else if self.runner.is_active() {
            self.bridge.cancelled = true;
        }
    }

    /// A brain may pop only a `done` or `suspended` intent (proposal §2.2).
    fn pop_intent(&mut self, intent_id: u64, tick: u64, ts_ns: u64) {
        let poppable = self
            .arbiter
            .stack()
            .get(intent_id)
            .is_some_and(|entry| entry.state != IntentState::Running);
        if !poppable {
            return;
        }
        if let Some(record) = self.commands.get(&intent_id) {
            // A popped goal is settled too: the brain must see its command end
            // exactly once, and the summary no longer owns the record.
            let command_id = record.command_id.clone();
            let generation = self.arbiter.generation();
            self.receipt(
                tick,
                ts_ns,
                Receipt {
                    intent_id: Some(intent_id),
                    generation: Some(generation),
                    outcome: Some("popped".to_owned()),
                    ..Receipt::new(&command_id, "settled")
                },
            );
        }
        let events = self.arbiter.end_command(intent_id, &mut self.mirror, tick);
        for event in events {
            self.record(tick, ts_ns, event);
        }
        self.commands.remove(&intent_id);
    }

    /// One goal ended: the receipt first, then `stack.done`, the summary entry
    /// and the snapshot. The process does not end (proposal §2.3).
    fn settle_goal(
        &mut self,
        intent_id: u64,
        failure_kind: Option<String>,
        tick: u64,
        ts_ns: u64,
        world: &World,
    ) {
        let Some(record) = self.commands.remove(&intent_id) else {
            return;
        };
        // A settled goal must never leave its primitive running with an idle
        // owner (review of A2-A3, finding 1).
        self.abandon_flight();
        let condition = record
            .condition
            .as_deref()
            .map(|text| self.condition_result(text, world));
        let generation = self.arbiter.generation();
        let outcome = if failure_kind.is_some() {
            "failed"
        } else {
            "done"
        };
        self.receipt(
            tick,
            ts_ns,
            Receipt {
                intent_id: Some(intent_id),
                generation: Some(generation),
                outcome: Some(outcome.to_owned()),
                failure_kind: failure_kind.clone(),
                condition,
                ..Receipt::new(&record.command_id, "settled")
            },
        );
        let events = self.arbiter.end_command(intent_id, &mut self.mirror, tick);
        for event in events {
            self.record(tick, ts_ns, event);
        }

        let summary_outcome =
            failure_kind.map_or_else(|| "done".to_owned(), |kind| format!("failed({kind})"));
        {
            let mut shared = lock(&self.shared);
            shared.outcome = Some(summary_outcome.clone());
            shared.goals.push(GoalResult {
                command_id: record.command_id.clone(),
                skill: record.skill.clone(),
                args: record.args.iter().map(arg_string).collect(),
                outcome: summary_outcome.clone(),
            });
        }
        self.record(
            tick,
            ts_ns,
            EventKind::Note {
                text: format!(
                    "goal_settled command={} outcome={summary_outcome}",
                    record.command_id
                ),
            },
        );
        let _ = self.log.flush();
        {
            let shared = lock(&self.shared);
            if let Err(error) = write_summary(&self.run_dir, &shared) {
                log::warn!("body: cannot write summary.json: {error}");
            }
        }
        if record.command_id == CLI_COMMAND_ID {
            self.cli = CliGoal::Settled;
        }
        self.publish_state(world, tick);
    }

    /// A resume path handed the body to an intent whose skill the interpreter
    /// does not hold. Abandon the previous goal's flight and start the record
    /// from the top. Returns whether it restarted, so the caller can discard
    /// the decision the stale frames produced this tick.
    fn sync_interpreter(&mut self) -> bool {
        let Owner::Skill { intent_id } = self.arbiter.owner().clone() else {
            return false;
        };
        if self.interpreter_intent == Some(intent_id) {
            return false;
        }
        self.restart_resumed(intent_id);
        true
    }

    /// Start the skill of `intent_id` from the top. The interpreter may still
    /// hold the settled goal's frames, so its queued action and any in-flight
    /// primitive are abandoned first.
    fn restart_resumed(&mut self, intent_id: u64) {
        let Some((skill, args)) = self
            .commands
            .get(&intent_id)
            .map(|record| (record.skill.clone(), record.args.clone()))
        else {
            return;
        };
        self.abandon_flight();
        match self.interpreter.start_skill(&skill, args) {
            Ok(()) => self.interpreter_intent = Some(intent_id),
            Err(error) => log::warn!("body: cannot resume intent {intent_id}: {error}"),
        }
    }

    /// Evaluate a goal's guard text on the body's own facts at settle time
    /// (proposal §3.5). `true`, `false` or `unknown`: a guard that does not
    /// parse or names an unknown fact is `unknown`, never `false`.
    fn condition_result(&mut self, text: &str, world: &World) -> String {
        let Some(program) = condition_program(text) else {
            return "unknown".to_owned();
        };
        let mut interpreter = Interpreter::new(program);
        if interpreter
            .start_skill(CONDITION_SKILL, Vec::new())
            .is_err()
        {
            return "unknown".to_owned();
        }
        let mut facts = FactsView {
            perception: Arc::clone(&self.perception),
            world,
        };
        let mut actuator = BridgeActuator::default();
        match interpreter.tick(&mut facts, &mut actuator) {
            Decision::Done => "true".to_owned(),
            Decision::Failed(_) => "false".to_owned(),
            _ => "unknown".to_owned(),
        }
    }

    /// The 3 s silence rule (proposal §2.5): note it once per silence, keep the
    /// running intent, then idle with reflexes on. A disconnected brain is not
    /// silence; it is a reconnect.
    fn check_brain_silence(&mut self, tick: u64, ts_ns: u64) {
        let silent = match &self.port {
            Some(port) if port.connection_state() == ConnectionState::Connected => port
                .last_brain_message()
                .is_some_and(|last| last.elapsed() >= self.options.brain_silence),
            _ => false,
        };
        if silent {
            if !self.brain_silent {
                self.brain_silent = true;
                self.record(
                    tick,
                    ts_ns,
                    EventKind::Note {
                        text: "brain_silent".to_owned(),
                    },
                );
            }
        } else {
            self.brain_silent = false;
        }
    }

    /// Publish the state the brain reads: every 20 ticks and on accept/settle
    /// (proposal §2.5). Called from the tick thread; the port stores it under
    /// its own mutex and the network thread sends it. No brain, no snapshot.
    fn publish_state(&mut self, world: &World, tick: u64) {
        let Some(port) = self.port.clone() else {
            return;
        };
        if port.connection_state() != ConnectionState::Connected {
            return;
        }
        let stack = self
            .arbiter
            .stack()
            .entries()
            .iter()
            .map(|entry| StackEntry {
                intent_id: entry.intent_id,
                tier: entry.tier.name().to_owned(),
                skill: entry.skill_ref.clone(),
                args: entry.args.clone(),
                state: entry.state.name().to_owned(),
            })
            .collect();
        port.publish_snapshot(Snapshot {
            seq: self.log.next_seq().saturating_sub(1),
            tick,
            generation: self.arbiter.generation(),
            owner: self.arbiter.owner().name(),
            stack,
            facts: port_facts(world),
            stale_ticks: 0,
        });
    }

    /// Feed this tick's packets to the receipts engine, then register the
    /// primitive's intent from the previous tick's actuator report.
    fn observe_packets(&mut self, view: &TickView<'_>, world: &World, ts_ns: u64) {
        self.register_pending(view.tick, world);
        for packet in view.packets {
            if let Err(error) =
                self.engine
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
                Intent::inventory(
                    intent_id,
                    primitive.clone(),
                    item.clone(),
                    *delta,
                    report_tick,
                )
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
            IntentSpec::Fact { fact, window } => {
                Some(Intent::fact(intent_id, fact.to_string(), report_tick).with_window(*window))
            }
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
                    IntentSpec::Dig { .. } | IntentSpec::Place { .. } => {
                        report.block_before.map(|(target, _)| (intent_id, target))
                    }
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

    fn start_primitive(&mut self, action: &StartAction, tick: u64, ts_ns: u64, world: &World) {
        self.finish_active_primitive(tick, ts_ns, None);
        if let Err(error) = self.runner.start(action) {
            if let Some(intent_id) = self.arbiter.stack().running().map(|entry| entry.intent_id) {
                self.settle_goal(
                    intent_id,
                    Some(format!("primitive:{error}")),
                    tick,
                    ts_ns,
                    world,
                );
            }
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
                    self.record(
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
            self.record(tick, ts_ns, event);
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
            self.record(
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
            self.start_primitive(&action, view.tick, ts_ns, world);
        }
        if let Some(action) = self.bridge.start.take()
            && !self.runner.is_active()
        {
            self.start_primitive(&action, view.tick, ts_ns, world);
        }
        if let Some(action) = self.bridge.continuous.take()
            && !self.runner.is_active()
        {
            self.start_primitive(&action, view.tick, ts_ns, world);
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
                world,
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
                let _ =
                    self.engine
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
    fn cancel_primitive(&mut self, view: &TickView<'_>, world: &World) -> Option<ControlFrame> {
        if !self.runner.is_active() {
            return None;
        }
        self.runner.settle(praxion_receipts::interpreter_outcome(
            ReceiptOutcome::Preempted,
        ));
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
        for event in events {
            if let EventKind::OwnerChanged { ref owner, .. } = event
                && let Some(kind) = owner.strip_prefix("reflex:")
            {
                let mut shared = lock(&self.shared);
                *shared
                    .reflex_activations
                    .entry(kind.to_owned())
                    .or_insert(0) += 1;
            }
            self.record(tick, ts_ns, event);
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
            return self.finish_tick(
                view,
                ControlFrame::new(view.tick, view.body.generation(), Held::NONE),
                work_start,
                ts_ns,
                Cue::Continue,
            );
        };
        // The arbiter wants `&mut Body`; the session owns the real one.
        self.mirror = view.body.clone();
        // The session's counter is a per-tick frame token. The owner generation
        // the brain checks must only go forward, so the mirror resumes from the
        // arbiter's value (proposal §2.4).
        self.mirror.set_generation(self.arbiter.generation());
        let first_tick = *self.first_tick.get_or_insert(view.tick);

        let stage = Instant::now();
        self.observe_packets(view, world, ts_ns);
        let observe_ns = stage.elapsed().as_nanos() as u64;
        let stage = Instant::now();
        {
            let mut perception = lock(&self.perception);
            perception.begin_tick(world);
        }
        let begin_ns = stage.elapsed().as_nanos() as u64;

        // The CLI goal waits out the start delay (the harness sets the scene);
        // a brain session has no delay and idles with reflexes on from tick 0.
        if self.port.is_none()
            && matches!(self.cli, CliGoal::Waiting(_))
            && view.tick < first_tick.saturating_add(self.options.start_delay_ticks)
        {
            return self.finish_tick(
                view,
                ControlFrame::new(view.tick, view.body.generation(), Held::NONE),
                work_start,
                ts_ns,
                Cue::Continue,
            );
        }
        if matches!(self.cli, CliGoal::Waiting(_))
            && view.tick >= first_tick.saturating_add(self.options.start_delay_ticks)
        {
            self.inject_cli(view.tick, ts_ns, world);
        }
        self.poll_port(view.tick, ts_ns, world);

        // The previous tick's primitive outcome goes to the interpreter, once.
        if !self.outcome_delivered
            && let Some(outcome) = self.runner.outcome().cloned()
        {
            self.bridge.outcome = Some(outcome);
            self.outcome_delivered = true;
        }

        let reflex_view = {
            let mut perception = lock(&self.perception);
            View::capture(
                &mut perception,
                world,
                self.arbiter.engine_mut().state_mut(),
            )
        };
        let needs = Needs::from_view(&reflex_view);
        let stage = Instant::now();

        let mut ticked = {
            let perception = Arc::clone(&self.perception);
            let mut facts = FactsView {
                perception: Arc::clone(&perception),
                world,
            };
            let mut blocks = KnownView { perception, world };
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
        if self.sync_interpreter() {
            // The decision this tick came from the previous goal's frames, so
            // it does not belong to the intent that just got the body back.
            ticked.skill = None;
            ticked.skill_intent = None;
        }
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
            FrameSource::Reflex(_) | FrameSource::Maintenance(_) => ticked.source.frame().clone(),
            FrameSource::Skill(_) | FrameSource::Idle(_) => {
                let mut frame = self.run_primitive(view, world, ticked.skill.as_ref(), ts_ns);
                let wanted = frame.look;
                frame.look = self
                    .look_guard
                    .apply(wanted, view.body.look(), &reflex_view);
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

        // The goal's budget counts only the ticks it owned the body, not the
        // suspended time under a reflex, a meal or a newer command.
        if let Owner::Skill { intent_id } = self.arbiter.owner().clone()
            && let Some(record) = self.commands.get_mut(&intent_id)
        {
            record.run_ticks = record.run_ticks.saturating_add(1);
        }

        // The goal's own end: `done` or a typed failure. The intent that ran
        // the interpreter is the one the decision belongs to.
        let ended = match &ticked.skill {
            Some(Decision::Done) => Some(None),
            Some(Decision::Failed(kind)) => Some(Some(kind.clone())),
            _ => None,
        };
        if let Some(failure) = ended
            && let Some(intent_id) = ticked.skill_intent
        {
            self.settle_goal(intent_id, failure, view.tick, ts_ns, world);
        }

        // A command that ran past its per-goal budget settles `failed(budget)`
        // (proposal §2.3).
        let running = self.arbiter.stack().running().map(|entry| entry.intent_id);
        if let Some(intent_id) = running
            && let Some(record) = self.commands.get(&intent_id)
            && let Some(budget) = record.budget_ticks
            && record.run_ticks > budget
        {
            self.settle_goal(
                intent_id,
                Some("budget".to_owned()),
                view.tick,
                ts_ns,
                world,
            );
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
        // One snapshot per second, on every wake, on the connect edge and
        // whenever the port asks for a fresh one after a drop (proposal §2.5).
        // A disconnected port needs none: rebuilding one every tick was the
        // review's finding 7.
        let connected = self
            .port
            .as_ref()
            .is_some_and(|port| port.connection_state() == ConnectionState::Connected);
        let connected_now = connected && !self.port_connected;
        self.port_connected = connected;
        let snapshot_due = connected
            && (view
                .tick
                .is_multiple_of(SNAPSHOT_INTERVAL_TICKS)
                || ticked.wake_brain
                || connected_now
                || self.port.as_ref().is_some_and(|port| port.needs_snapshot()));
        if snapshot_due {
            self.publish_state(world, view.tick);
        }
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
        matches!(self.cli, CliGoal::Settled)
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
        // carries the request, the gaze carries the camera. A menu means a
        // still gaze; held forward means a walking gaze; otherwise idle.
        let request = frame.look;
        let menu_open = frame.menu.is_some()
            || view
                .world
                .is_some_and(|world| world.open_container().is_some());
        let mode = if menu_open {
            GazeMode::Still
        } else if frame.held.forward {
            GazeMode::Walking
        } else {
            GazeMode::Idle
        };
        let look = self
            .gaze
            .step(request, view.body.look(), cue, view.tick, mode);
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
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

/// Parse a guard text by wrapping it in a one-rule skill, so the rules parser
/// and checker are the only guard language (proposal §4.2: no second
/// evaluator). `None` means the text is not a valid, checked guard.
fn condition_program(text: &str) -> Option<Program> {
    let source = format!("skill {CONDITION_SKILL}()\n  rules:\n    {text} -> done\n");
    praxion_rules::load(&source).ok()
}

/// The facts a snapshot carries (proposal §2.2). Every value is an F3 fact the
/// body already trusts; `pos` and the inventory come from the same store.
fn port_facts(world: &World) -> PortFacts {
    let health = world.health();
    let mut inventory = BTreeMap::new();
    // Slot 0 is the crafting grid's result preview, not an owned item; the
    // facts' `count`/`has` skip it for the same reason.
    for stack in world.inventory().iter().skip(1) {
        if stack.count() > 0 {
            *inventory.entry(item_name(stack)).or_insert(0_u32) += stack.count() as u32;
        }
    }
    let pos = world.player_position().unwrap_or(Vec3::ZERO);
    PortFacts {
        health: health.map_or(20.0, |health| health.health()),
        food: health.map_or(20, |health| health.food()),
        air: world.own_air().unwrap_or(300),
        is_night: praxion_facts::is_night(world.day_time()),
        day_time: world.day_time(),
        dimension: world.dimension().map_or_else(
            || "unknown".to_owned(),
            |dimension| dimension.dimension().to_owned(),
        ),
        pos: [
            pos.x.floor() as i32,
            pos.y.floor() as i32,
            pos.z.floor() as i32,
        ],
        inventory,
    }
}

/// An item id without the `minecraft:` namespace, as the briefing uses it.
fn item_name(stack: &praxion_world::ItemStack) -> String {
    let name = stack.kind().to_str();
    name.strip_prefix("minecraft:").unwrap_or(name).to_owned()
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    use azalea_registry::builtin::ItemKind;
    use praxion_arbiter::NeedKind;
    use praxion_eventlog::EventKind;
    use praxion_reflexes::{ArrowView, FoodChoice, MAX_ARROWS, MAX_ENDERMEN};
    use praxion_world::BlockState;

    use super::*;

    const INSTA: &str = "\
skill insta()
  rules:
    true -> done
skill stall()
  rules:
    true -> wait(100)
";

    static RUNS: AtomicU64 = AtomicU64::new(0);

    /// One `command_receipt` row: command id, intent, generation, outcome.
    type ReceiptRow = (String, Option<u64>, Option<u32>, Option<String>);

    /// One driver over an empty world, plus the log it writes.
    struct Harness {
        driver: BodyDriver,
        world: World,
        body: Body,
        tick: u64,
        run_dir: PathBuf,
    }

    fn run_dir() -> PathBuf {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/test-runs");
        let unique = format!(
            "driver-{}-{}",
            std::process::id(),
            RUNS.fetch_add(1, Ordering::SeqCst)
        );
        let dir = base.join(unique);
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn settings() -> Settings {
        Settings::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config/reflexes.toml"
        ))
        .expect("the reflex settings must load")
    }

    fn harness_with(port: Option<Arc<PortServer>>, silence: Duration) -> Harness {
        let run_dir = run_dir();
        let program = praxion_rules::load(INSTA).expect("the test program must load");
        let shared = Arc::new(Mutex::new(Shared::new("test", "", vec![])));
        let driver = BodyDriver::new(
            BodyOptions {
                skill: None,
                args: Vec::new(),
                start_delay_ticks: 0,
                style_seed: 0,
                brain_silence: silence,
                versions: Versions::default(),
            },
            run_dir.clone(),
            program,
            settings(),
            shared,
            port,
        )
        .expect("the driver must start");
        Harness {
            driver,
            world: World::new(),
            body: Body::new(Vec3::ZERO),
            tick: 1,
            run_dir,
        }
    }

    fn harness() -> Harness {
        harness_with(None, DEFAULT_BRAIN_SILENCE)
    }

    impl Harness {
        fn step(&mut self) -> ControlFrame {
            let view = TickView {
                tick: self.tick,
                body: &self.body,
                world: Some(&self.world),
                packets: &[],
            };
            let frame = self.driver.frame(&view);
            self.tick += 1;
            frame
        }

        fn run(&mut self, ticks: u64) {
            for _ in 0..ticks {
                self.step();
            }
        }

        fn inject(&mut self, mut command: PortCommand) {
            command.based_on_generation = self.driver.arbiter.generation();
            self.driver
                .accept_command(command, self.tick, now_ns(), &self.world);
        }

        fn events(&self) -> Vec<Event> {
            praxion_eventlog::read(self.run_dir.join("events.jsonl"))
                .expect("the event log must be readable")
                .events
        }

        fn receipts(&self, state: &str) -> Vec<ReceiptRow> {
            self.events()
                .into_iter()
                .filter_map(|event| match event.event {
                    EventKind::CommandReceipt {
                        command_id,
                        state: got,
                        intent_id,
                        generation,
                        outcome,
                        ..
                    } if got == state => Some((command_id, intent_id, generation, outcome)),
                    _ => None,
                })
                .collect()
        }

        fn summary(&self) -> serde_json::Value {
            let text = self
                .driver
                .summary_json()
                .expect("the driver has a summary");
            serde_json::from_str(&text).expect("the summary is JSON")
        }
    }

    fn command(id: &str, skill: &str) -> PortCommand {
        PortCommand {
            id: id.to_owned(),
            skill: skill.to_owned(),
            args: Vec::new(),
            tier: "command".to_owned(),
            expires_at_tick: 100_000,
            based_on_generation: 0,
            condition: None,
            budget_ticks: None,
        }
    }

    /// Accept one command as the brain would and return its intent id.
    fn accept(harness: &mut Harness, mut command: PortCommand) -> u64 {
        command.based_on_generation = harness.driver.arbiter.generation();
        harness
            .driver
            .accept_command(command, harness.tick, now_ns(), &harness.world);
        harness
            .driver
            .arbiter
            .stack()
            .entries()
            .last()
            .map(|entry| entry.intent_id)
            .expect("the command was accepted")
    }

    /// A reflex view the test controls by hand (the driver's own view comes
    /// from the world, which a unit test does not build packet by packet).
    fn test_view(feet: Vec3, food: u32) -> View {
        let cell = feet.to_block_pos_floor();
        View {
            tick: 0,
            position: feet,
            eye: feet + Vec3::new(0.0, 1.62, 0.0),
            rotation: (0.0, 0.0),
            health: Some(20.0),
            food: Some(food),
            day_time: 1_000,
            feet: cell,
            eye_cell: cell.up(1),
            feet_state: BlockState::AIR,
            head_state: BlockState::AIR,
            below_state: BlockState::AIR,
            head_in_water: false,
            feet_in_water: false,
            feet_in_lava: false,
            on_fire: false,
            lava: None,
            water: None,
            next_cell: cell.south(1),
            next_cell_lava: false,
            next_drop: 0,
            creeper: None,
            melee: None,
            endermen: [None; MAX_ENDERMEN],
            enderman_count: 0,
            arrows: [None; MAX_ARROWS],
            arrow_count: 0,
            best_food: None,
            best_weapon: None,
        }
    }

    fn arrow_view(mut view: View) -> View {
        // A strafe needs no planner path, so the test works without a loaded
        // world.
        view.arrows[0] = Some(ArrowView {
            id: 3,
            position: view.eye + Vec3::new(0.0, 0.0, 5.0),
            distance: 5.0,
            toward: true,
            strafe_right: true,
        });
        view.arrow_count = 1;
        view
    }

    fn hungry_view(mut view: View) -> View {
        view.best_food = Some(FoodChoice {
            slot: 9,
            kind: ItemKind::Bread,
            nutrition: 5,
            saturation: 6.0,
        });
        view
    }

    /// Run one arbiter tick with a hand-built view, as `frame` does with a
    /// captured one. The caller inspects the returned tick and then calls the
    /// driver method under test.
    fn tick_arbiter(
        driver: &mut BodyDriver,
        world: &World,
        view: &View,
        needs: Needs,
        tick: u64,
    ) -> praxion_arbiter::ArbiterTick {
        let perception = Arc::clone(&driver.perception);
        let mut facts = FactsView {
            perception: Arc::clone(&perception),
            world,
        };
        let mut blocks = KnownView { perception, world };
        driver.arbiter.tick(
            view,
            &mut driver.interpreter,
            &mut facts,
            &mut driver.bridge,
            needs,
            &driver.settings,
            &mut driver.mirror,
            tick,
            &mut blocks,
        )
    }

    fn flush(harness: &mut Harness) {
        harness.driver.log.flush().expect("the event log flushes");
    }

    #[test]
    fn a_command_past_its_expiry_never_starts() {
        let mut harness = harness();
        harness.inject(PortCommand {
            expires_at_tick: 0,
            ..command("c-1", "insta")
        });
        harness.run(2);

        let states: Vec<_> = harness
            .events()
            .into_iter()
            .filter_map(|event| match event.event {
                EventKind::CommandReceipt {
                    command_id, state, ..
                } if command_id == "c-1" => Some(state),
                _ => None,
            })
            .collect();
        assert_eq!(states, vec!["pending", "expired"]);
        assert!(harness.driver.arbiter.stack().entries().is_empty());
        assert!(harness.driver.commands.is_empty());
        assert!(harness.driver.summary_json().is_some());
    }

    #[test]
    fn a_stale_generation_is_rejected() {
        let mut harness = harness();
        harness.driver.accept_command(
            PortCommand {
                based_on_generation: 5,
                ..command("c-1", "insta")
            },
            harness.tick,
            now_ns(),
            &harness.world,
        );
        harness.run(2);

        let rejected = harness.receipts("rejected");
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert_eq!(rejected[0].0, "c-1");
        assert!(harness.driver.arbiter.stack().entries().is_empty());
    }

    #[test]
    fn an_accepted_command_reports_the_arbiter_generation() {
        let mut harness = harness();
        harness.inject(command("c-1", "stall"));
        harness.run(1);

        let accepted = harness.receipts("accepted");
        assert_eq!(accepted.len(), 1, "{accepted:?}");
        assert_eq!(accepted[0].1, Some(1), "the command owns the first intent");
        assert_eq!(
            accepted[0].2,
            Some(harness.driver.arbiter.generation()),
            "the receipt carries the generation Arbiter::command bumped to"
        );
    }

    #[test]
    fn a_settled_command_is_a_goal_and_no_longer_runs() {
        let mut harness = harness();
        harness.inject(command("c-1", "insta"));
        harness.run(3);

        let settled = harness.receipts("settled");
        assert_eq!(settled.len(), 1, "{settled:?}");
        assert_eq!(settled[0].3.as_deref(), Some("done"));
        assert!(
            harness
                .driver
                .arbiter
                .stack()
                .entries()
                .iter()
                .all(|entry| entry.state != praxion_arbiter::IntentState::Running),
            "the settled intent no longer runs"
        );
        let summary = harness.summary();
        assert_eq!(summary["goals"].as_array().map(Vec::len), Some(1));
        assert_eq!(summary["goals"][0]["command_id"], "c-1");
        assert_eq!(summary["goal"]["outcome"], "done");
        assert!(
            !harness.driver.done(),
            "a brain command is not the CLI goal"
        );
    }

    #[test]
    fn settling_writes_the_receipt_then_the_intent_done() {
        let mut harness = harness();
        harness.inject(command("c-1", "insta"));
        harness.run(3);

        let events = harness.events();
        let receipt = events
            .iter()
            .position(|event| {
                matches!(
                    &event.event,
                    EventKind::CommandReceipt { state, .. } if state == "settled"
                )
            })
            .expect("a settled receipt");
        let done = events
            .iter()
            .position(|event| {
                matches!(
                    &event.event,
                    EventKind::IntentState { state, .. } if state == "done"
                )
            })
            .expect("the intent is done");
        assert!(
            receipt < done,
            "the brain must see the receipt before the intent state"
        );
    }

    #[test]
    fn a_goal_that_runs_past_its_budget_settles_failed() {
        let mut harness = harness();
        harness.inject(PortCommand {
            budget_ticks: Some(3),
            ..command("c-1", "stall")
        });
        harness.run(10);

        let accepted = harness.receipts("accepted");
        assert_eq!(accepted.len(), 1, "{accepted:?}");
        let settled = harness.receipts("settled");
        assert_eq!(settled.len(), 1, "{settled:?}");
        assert_eq!(settled[0].3.as_deref(), Some("failed"));
        let receipt = harness
            .events()
            .into_iter()
            .find_map(|event| match event.event {
                EventKind::CommandReceipt {
                    state,
                    failure_kind,
                    ..
                } if state == "settled" => Some(failure_kind),
                _ => None,
            })
            .expect("a settled receipt");
        assert_eq!(receipt.as_deref(), Some("budget"));
        let summary = harness.summary();
        assert_eq!(summary["goal"]["outcome"], "failed(budget)");
        assert!(harness.driver.commands.is_empty());
    }

    #[test]
    fn a_budget_settled_goal_stops_its_primitive_and_idles() {
        let mut harness = harness();
        harness.inject(PortCommand {
            budget_ticks: Some(3),
            ..command("c-1", "stall")
        });
        harness.run(8);

        let settled = harness.receipts("settled");
        assert_eq!(settled.len(), 1, "{settled:?}");
        assert_eq!(settled[0].3.as_deref(), Some("failed"));
        assert!(
            !harness.driver.runner.is_active(),
            "a settled goal must not leave its wait running"
        );
        assert_eq!(
            *harness.driver.arbiter.owner(),
            Owner::Idle,
            "the body is idle after the budget settle"
        );
    }

    #[test]
    fn budget_counts_only_the_ticks_the_goal_owns_the_body() {
        let mut harness = harness();
        harness.inject(PortCommand {
            budget_ticks: Some(3),
            ..command("c-1", "stall")
        });
        // Two owner ticks, then a newer command holds the body for a while.
        harness.run(2);
        harness.inject(PortCommand {
            budget_ticks: Some(2),
            ..command("c-2", "stall")
        });
        for _ in 0..20 {
            harness.step();
            flush(&mut harness);
            if harness
                .receipts("settled")
                .iter()
                .any(|receipt| receipt.0 == "c-2")
            {
                break;
            }
        }
        let settled = harness.receipts("settled");
        assert!(
            settled
                .iter()
                .any(|receipt| receipt.0 == "c-2" && receipt.3.as_deref() == Some("failed")),
            "the newer command spends its budget: {settled:?}"
        );
        assert!(
            settled.iter().all(|receipt| receipt.0 != "c-1"),
            "c-1's suspended ticks are not budget: {settled:?}"
        );

        // c-1 owns the body again: two more owner ticks stay inside budget 3.
        harness.run(1);
        flush(&mut harness);
        assert!(
            harness
                .receipts("settled")
                .iter()
                .all(|receipt| receipt.0 != "c-1"),
            "a resumed goal must not fail the tick it restarts"
        );
        harness.run(1);
        flush(&mut harness);
        let settled = harness.receipts("settled");
        assert!(
            settled
                .iter()
                .any(|receipt| receipt.0 == "c-1" && receipt.3.as_deref() == Some("failed")),
            "the fourth owner tick exceeds budget 3: {settled:?}"
        );
    }

    #[test]
    fn a_goal_condition_is_reported_true_false_or_unknown() {
        for (text, expected) in [
            ("true", "true"),
            ("false", "false"),
            ("near_entity(zombie, 16)", "unknown"),
        ] {
            let mut harness = harness();
            harness.inject(PortCommand {
                condition: Some(text.to_owned()),
                ..command("c-1", "insta")
            });
            harness.run(3);

            let condition = harness.events().into_iter().find_map(|event| match event.event {
                EventKind::CommandReceipt {
                    state, condition, ..
                } if state == "settled" => condition,
                _ => None,
            });
            assert_eq!(condition.as_deref(), Some(expected), "condition {text:?}");
        }
    }

    #[test]
    fn a_popped_goal_settles_popped_once() {
        let mut harness = harness();
        accept(&mut harness, command("c-1", "stall"));
        accept(&mut harness, command("c-2", "stall"));
        // c-3 owns the body; c-2 is suspended and therefore poppable.
        accept(&mut harness, command("c-3", "stall"));

        harness.driver.pop_intent(2, harness.tick, now_ns());
        harness.driver.pop_intent(2, harness.tick, now_ns());
        flush(&mut harness);

        let settled = harness.receipts("settled");
        assert_eq!(settled.len(), 1, "{settled:?}");
        assert_eq!(settled[0].0, "c-2");
        assert_eq!(settled[0].3.as_deref(), Some("popped"));
        let done = harness
            .events()
            .into_iter()
            .filter(|event| {
                matches!(
                    &event.event,
                    EventKind::IntentState { intent_id: 2, state } if state == "done"
                )
            })
            .count();
        assert_eq!(done, 1, "a second pop must not re-emit intent_state done");
        assert_eq!(*harness.driver.arbiter.owner(), Owner::Skill { intent_id: 3 });
        assert_eq!(harness.driver.commands.len(), 2);
    }

    #[test]
    fn a_rejected_cli_goal_settles_failed_and_writes_the_summary() {
        let mut harness = harness();
        harness.driver.cli = CliGoal::Waiting(PortCommand {
            id: CLI_COMMAND_ID.to_owned(),
            skill: "no_such_skill".to_owned(),
            args: Vec::new(),
            tier: "command".to_owned(),
            expires_at_tick: u64::MAX,
            based_on_generation: 0,
            condition: None,
            budget_ticks: None,
        });
        harness.run(1);

        let rejected = harness.receipts("rejected");
        assert_eq!(rejected.len(), 1, "{rejected:?}");
        assert!(harness.driver.done());
        assert_eq!(
            harness.summary()["goal"]["outcome"],
            "failed(start:unknown_skill)"
        );
        assert!(
            harness.run_dir.join("summary.json").exists(),
            "the rejected CLI goal writes the summary"
        );
    }

    #[test]
    fn a_popped_command_under_a_reflex_resumes_the_older_skill() {
        let mut harness = harness();
        accept(&mut harness, command("c-1", "stall"));
        accept(&mut harness, command("c-2", "insta"));
        let feet = Vec3::new(0.5, 64.0, 0.5);
        let needs = Needs {
            health: 20.0,
            food: 20,
        };

        // An incoming arrow preempts the newer command (c-2), suspending it.
        let started = tick_arbiter(
            &mut harness.driver,
            &harness.world,
            &arrow_view(test_view(feet, 20)),
            needs,
            10,
        );
        assert_eq!(
            started.owner,
            Owner::Reflex(ReflexKind::Arrow),
            "{:?}",
            started.events
        );

        // The brain pops c-2 while the reflex owns the body.
        harness.driver.pop_intent(2, 11, now_ns());
        flush(&mut harness);
        assert_eq!(
            harness.receipts("settled")[0].3.as_deref(),
            Some("popped"),
            "the pop is a settled receipt"
        );

        // The reflex ends and c-1 gets the body. The interpreter still holds
        // c-2's frames, so the driver must restart c-1, not run them.
        let mut resumed = false;
        for tick in 12..20 {
            let out = tick_arbiter(
                &mut harness.driver,
                &harness.world,
                &test_view(feet, 20),
                needs,
                tick,
            );
            if out.owner == (Owner::Skill { intent_id: 1 }) {
                resumed = true;
                break;
            }
        }
        assert!(resumed, "the older command gets the body back");
        assert!(harness.driver.sync_interpreter());
        assert_eq!(harness.driver.interpreter.current_skill(), Some("stall"));
        assert_eq!(harness.driver.interpreter_intent, Some(1));
        assert!(
            harness
                .receipts("settled")
                .iter()
                .all(|receipt| receipt.0 != "c-1"),
            "the popped command's frames must not settle the older goal"
        );
    }

    #[test]
    fn a_goal_done_during_maintenance_resumes_the_older_skill() {
        let mut harness = harness();
        accept(&mut harness, command("c-1", "stall"));
        accept(&mut harness, command("c-2", "insta"));
        let feet = Vec3::new(0.5, 64.0, 0.5);
        let needs = Needs {
            health: 20.0,
            food: 10,
        };

        // c-2 decides Done on the tick a soft need starts the meal, so the
        // settle happens while maintenance owns the body.
        let out = tick_arbiter(
            &mut harness.driver,
            &harness.world,
            &hungry_view(test_view(feet, 10)),
            needs,
            10,
        );
        assert_eq!(
            out.owner,
            Owner::Maintenance(NeedKind::Eat),
            "{:?}",
            out.events
        );
        assert_eq!(out.skill, Some(Decision::Done));
        assert_eq!(out.skill_intent, Some(2));
        harness.driver.settle_goal(2, None, 10, now_ns(), &harness.world);
        assert_eq!(
            *harness.driver.arbiter.owner(),
            Owner::Maintenance(NeedKind::Eat),
            "the meal keeps the body while c-2 settles"
        );

        let mut resumed = false;
        let mut food = 10;
        for tick in 11..(11 + praxion_arbiter::EAT_TICKS + 4) {
            let needs = Needs {
                health: 20.0,
                food,
            };
            let out = tick_arbiter(
                &mut harness.driver,
                &harness.world,
                &hungry_view(test_view(feet, food)),
                needs,
                tick,
            );
            if out.ate.is_some() {
                // The meal finished; the next view is fed, so the soft path
                // does not immediately start another one.
                food = 20;
            }
            if out.owner == (Owner::Skill { intent_id: 1 }) {
                resumed = true;
                break;
            }
        }
        assert!(resumed, "the meal ends and the older command resumes");
        assert!(harness.driver.sync_interpreter());
        assert_eq!(harness.driver.interpreter.current_skill(), Some("stall"));
        assert_eq!(harness.driver.interpreter_intent, Some(1));
    }

    #[test]
    fn a_budget_settle_under_a_reflex_resumes_the_older_skill() {
        let mut harness = harness();
        accept(&mut harness, command("c-1", "stall"));
        accept(
            &mut harness,
            PortCommand {
                budget_ticks: Some(3),
                ..command("c-2", "stall")
            },
        );
        let feet = Vec3::new(0.5, 64.0, 0.5);
        let needs = Needs {
            health: 20.0,
            food: 20,
        };

        let started = tick_arbiter(
            &mut harness.driver,
            &harness.world,
            &arrow_view(test_view(feet, 20)),
            needs,
            10,
        );
        assert_eq!(
            started.owner,
            Owner::Reflex(ReflexKind::Arrow),
            "{:?}",
            started.events
        );

        // The budget expires while the reflex owns the body.
        harness
            .driver
            .settle_goal(2, Some("budget".to_owned()), 11, now_ns(), &harness.world);
        assert_eq!(
            *harness.driver.arbiter.owner(),
            Owner::Reflex(ReflexKind::Arrow),
            "a settle under a reflex must not steal the body"
        );
        flush(&mut harness);
        let settled = harness.receipts("settled");
        assert!(
            settled
                .iter()
                .any(|receipt| receipt.0 == "c-2" && receipt.3.as_deref() == Some("failed")),
            "{settled:?}"
        );

        let mut resumed = false;
        for tick in 12..20 {
            let out = tick_arbiter(
                &mut harness.driver,
                &harness.world,
                &test_view(feet, 20),
                needs,
                tick,
            );
            if out.owner == (Owner::Skill { intent_id: 1 }) {
                resumed = true;
                break;
            }
        }
        assert!(resumed, "the reflex ends and the older command resumes");
        assert!(harness.driver.sync_interpreter());
        assert_eq!(harness.driver.interpreter.current_skill(), Some("stall"));
        assert_eq!(harness.driver.interpreter_intent, Some(1));
        assert!(!harness.driver.runner.is_active());
    }

    #[test]
    fn two_goals_run_back_to_back_in_one_driver() {
        let mut harness = harness();
        harness.inject(command("c-1", "insta"));
        harness.run(3);
        harness.inject(command("c-2", "insta"));
        harness.run(3);

        let settled = harness.receipts("settled");
        let ids: Vec<_> = settled.iter().map(|receipt| receipt.0.as_str()).collect();
        assert_eq!(ids, vec!["c-1", "c-2"], "{settled:?}");
        let summary = harness.summary();
        let goals = summary["goals"].as_array().expect("a goals list");
        assert_eq!(goals.len(), 2);
        assert_eq!(goals[1]["command_id"], "c-2");
        assert_eq!(summary["goal"]["skill"], "insta");
        assert_eq!(summary["goal"]["outcome"], "done");
        // The process would have ended on the first settle if this were still
        // a one-goal driver.
        assert!(!harness.driver.done());
        assert_eq!(harness.driver.commands.len(), 0);
    }

    #[test]
    fn a_new_command_suspends_the_running_one_and_its_settle_resumes_it() {
        let mut harness = harness();
        harness.inject(command("c-1", "stall"));
        harness.run(2);
        assert_eq!(
            harness
                .driver
                .arbiter
                .stack()
                .running()
                .map(|entry| entry.intent_id),
            Some(1)
        );

        harness.inject(command("c-2", "insta"));
        harness.run(3);
        let accepted = harness.receipts("accepted");
        assert_eq!(accepted.len(), 2, "{accepted:?}");
        assert!(
            accepted[1].2 > accepted[0].2,
            "a new command bumps the generation: {accepted:?}"
        );
        let states: Vec<_> = harness
            .driver
            .arbiter
            .stack()
            .entries()
            .iter()
            .map(|entry| (entry.intent_id, entry.state.name()))
            .collect();
        assert!(
            states.contains(&(1, "running")),
            "the suspended command gets the body back after c-2 settles: {states:?}"
        );
        assert!(
            states.contains(&(2, "done")),
            "the newer command is settled: {states:?}"
        );
        // The resumed command restarts from the top: the interpreter had been
        // cleared by the newer command's `start_skill`.
        assert_eq!(harness.driver.interpreter.current_skill(), Some("stall"));
        let settled: Vec<_> = harness
            .receipts("settled")
            .into_iter()
            .map(|receipt| receipt.0)
            .collect();
        assert_eq!(settled, vec!["c-2"]);
    }

    #[test]
    fn overflow_makes_the_port_ask_for_a_fresh_snapshot() {
        let server = Arc::new(
            PortServer::bind_with_capacity(
                "127.0.0.1:0".parse().unwrap(),
                praxion_port::PortConfig {
                    body: "test".to_owned(),
                    run_id: "run-1".to_owned(),
                },
                0,
            )
            .unwrap(),
        );
        let brain = connect(&server);
        let mut harness = harness_with(Some(Arc::clone(&server)), DEFAULT_BRAIN_SILENCE);
        harness.run(25);

        // Capacity zero: the driver's publishes overflow and the port asks for
        // a snapshot; it must send `dropped` and then the fresh state.
        let mut saw_dropped = false;
        let mut saw_snapshot = false;
        let mut brain = brain;
        for _ in 0..200 {
            match brain.receive() {
                Some(praxion_port::BodyMessage::Dropped { n, .. }) => {
                    assert!(n >= 1);
                    saw_dropped = true;
                }
                Some(praxion_port::BodyMessage::Snapshot(_)) => {
                    saw_snapshot = true;
                    if saw_dropped {
                        break;
                    }
                }
                Some(_) => {}
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(saw_dropped, "the port must report the dropped events");
        assert!(saw_snapshot, "a fresh snapshot must follow the drop");
    }

    #[test]
    fn brain_silence_is_noted_once_per_silence() {
        let server = Arc::new(
            PortServer::bind(
                "127.0.0.1:0".parse().unwrap(),
                praxion_port::PortConfig {
                    body: "test".to_owned(),
                    run_id: "run-1".to_owned(),
                },
            )
            .unwrap(),
        );
        let mut brain = connect(&server);
        let mut harness = harness_with(Some(Arc::clone(&server)), Duration::from_millis(80));

        let notes = |harness: &Harness| {
            harness
                .events()
                .into_iter()
                .filter(|event| {
                    matches!(&event.event, EventKind::Note { text } if text == "brain_silent")
                })
                .count()
        };
        for _ in 0..60 {
            harness.step();
            if notes(&harness) == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(notes(&harness), 1, "one note per silence");

        // Any message counts as liveness (proposal §2.5), so the next silence
        // is a second note.
        brain.send(&BrainMessage::Ping);
        std::thread::sleep(Duration::from_millis(30));
        for _ in 0..60 {
            harness.step();
            if notes(&harness) == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(notes(&harness), 2, "a later silence is noted again");
    }

    /// A minimal brain: handshake, then frames on demand.
    struct TestBrain {
        writer: std::net::TcpStream,
        reader: praxion_port::FrameReader<std::net::TcpStream>,
    }

    impl TestBrain {
        fn send(&mut self, message: &BrainMessage) {
            praxion_port::write_frame(&mut self.writer, message).unwrap();
        }

        fn receive(&mut self) -> Option<praxion_port::BodyMessage> {
            self.writer
                .set_read_timeout(Some(Duration::from_millis(50)))
                .ok()?;
            match self.reader.read_json::<praxion_port::BodyMessage>() {
                Ok(message) => message,
                // A read timeout means no frame yet; the caller loops.
                Err(error) if error.is_incomplete() => None,
                Err(error) => panic!("frame read failed: {error}"),
            }
        }
    }

    fn connect(server: &PortServer) -> TestBrain {
        use std::net::TcpStream;
        let stream = TcpStream::connect(server.local_addr()).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let reader = praxion_port::FrameReader::new(stream.try_clone().unwrap());
        let mut brain = TestBrain {
            writer: stream,
            reader,
        };
        match brain
            .reader
            .read_json::<praxion_port::BodyMessage>()
            .unwrap()
        {
            Some(praxion_port::BodyMessage::Hello { .. }) => {}
            other => panic!("expected hello, got {other:?}"),
        }
        brain.send(&BrainMessage::Hello {
            port_version: praxion_port::PORT_VERSION,
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while server.connection_state() != ConnectionState::Connected {
            assert!(Instant::now() < deadline, "the handshake did not complete");
            std::thread::sleep(Duration::from_millis(5));
        }
        brain
    }
}
