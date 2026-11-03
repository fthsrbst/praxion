//! The live driver: a decision layer that runs one primitive or one skill
//! against the real body, perception and receipts.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use praxion_eventlog::{Outcome as ReceiptOutcome, Writer};
use praxion_headless::{ControlFrame, Driver, Held, TickView};
use praxion_interpreter::{
    Actuator, Decision, Facts as InterpreterFacts, IntValue, Interpreter, Outcome, StartAction,
    Truth,
};
use praxion_perception::{Answer, Caller, Perception};
use praxion_receipts::{Engine, Intent, ReceiptId};
use praxion_rules::{ArgValue, FactCall};
use praxion_world::World;
use serde::Serialize;

use crate::runner::{Ctx, IntentSpec, Runner, fact_known, near_block};

/// What the live binary should run.
#[derive(Clone, Debug)]
pub enum LiveMode {
    /// One primitive, e.g. `mine` with `oak_log`.
    Primitive { name: String, arg: Option<String> },
    /// One skill from `body/skills/`, e.g. `get_wooden_pickaxe`.
    Skill { name: String },
}

/// A driver run's options.
#[derive(Clone, Debug)]
pub struct LiveOptions {
    pub mode: LiveMode,
    pub run_dir: PathBuf,
    pub look: praxion_planner::LookStyle,
    /// Ticks to wait after login before the first primitive frame, so fixtures
    /// can give items, teleport and build the test scene.
    pub start_delay_ticks: u64,
}

/// The result the binary prints.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub mode: String,
    pub outcome: Option<String>,
    pub receipt: Option<String>,
    pub ticks: u64,
    pub position: Option<[f64; 3]>,
    /// The block the last registered intent named (`dig` and `place`), so the
    /// referee can query the exact cell instead of guessing around the bot.
    pub target: Option<[i32; 3]>,
    pub inventory: BTreeMap<String, i64>,
    pub events: u64,
}

/// The live decision layer.
pub struct LiveDriver {
    options: LiveOptions,
    perception: Perception,
    engine: Engine,
    log: Writer,
    runner: Runner,
    interpreter: Option<Interpreter>,
    bridge: BridgeActuator,
    started: bool,
    done: bool,
    outcome: Option<Outcome>,
    pending: Option<IntentSpec>,
    last_report: Option<(u64, praxion_headless::AppliedReport)>,
    intent_counter: u64,
    receipt_id: Option<ReceiptId>,
    observe_receipt: Option<(ReceiptId, FactCall)>,
    settled_intent: Option<u64>,
    last_tick: u64,
    summary: Summary,
}

impl LiveDriver {
    pub fn new(options: LiveOptions, program: Option<praxion_rules::Program>) -> std::io::Result<Self> {
        let log = Writer::create(&options.run_dir)?;
        let interpreter = match &options.mode {
            LiveMode::Skill { name } => {
                let program = program.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("skill `{name}` needs a loaded ruleset"),
                    )
                })?;
                let mut interpreter = Interpreter::new(program);
                interpreter
                    .start_skill(name, Vec::new())
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
                Some(interpreter)
            }
            LiveMode::Primitive { .. } => None,
        };
        let mode = match &options.mode {
            LiveMode::Primitive { name, arg } => match arg {
                Some(arg) => format!("{name}({arg})"),
                None => format!("{name}()"),
            },
            LiveMode::Skill { name } => format!("skill {name}"),
        };
        Ok(Self {
            options,
            perception: Perception::new(praxion_perception::Config::default()),
            engine: Engine::new(),
            log,
            runner: Runner::new(),
            interpreter,
            bridge: BridgeActuator::default(),
            started: false,
            done: false,
            outcome: None,
            pending: None,
            last_report: None,
            intent_counter: 0,
            receipt_id: None,
            observe_receipt: None,
            settled_intent: None,
            last_tick: 0,
            summary: Summary {
                mode,
                ..Summary::default()
            },
        })
    }

    pub fn done(&self) -> bool {
        self.done
    }

    pub fn summary(&self) -> Summary {
        let mut summary = self.summary.clone();
        summary.outcome = self.outcome.as_ref().map(|outcome| format!("{outcome:?}"));
        summary
    }

    fn finish(&mut self, outcome: Outcome) {
        if self.done {
            return;
        }
        self.done = true;
        self.outcome = Some(outcome);
        let _ = self.log.flush();
        let summary = serde_json::to_string_pretty(&self.summary()).unwrap_or_default();
        let _ = std::fs::write(self.options.run_dir.join("summary.json"), summary);
    }

    fn start_primitive(&mut self, action: &StartAction) {
        if let Err(error) = self.runner.start(action) {
            self.finish(Outcome::Failed(error));
        }
    }

    /// Registers the pending intent from the previous tick's report.
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
                // The actuator's raycast decides the cell, and it can pick a
                // nearer block or another face than the primitive aimed at.
                // The receipt and the referee must watch the cell that was
                // actually used, not the one that was intended.
                let Some((target, before)) = report.block_before else {
                    return;
                };
                self.summary.target = Some([target.x, target.y, target.z]);
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
                self.summary.target = Some([target.x, target.y, target.z]);
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
            IntentSpec::Attack {
                entity_id,
                window,
            } => Some(Intent::attack(intent_id, *entity_id, report_tick).with_window(*window)),
            IntentSpec::Fact { fact, window } => Some(
                Intent::fact(intent_id, fact.to_string(), report_tick).with_window(*window),
            ),
        };
        let Some(intent) = intent else {
            return;
        };
        let ts = now_ns();
        match self
            .engine
            .register(&mut self.log, tick, ts, intent, world)
        {
            Ok(receipt) => {
                self.intent_counter = intent_id;
                self.runner.registered(intent_id);
                self.receipt_id = Some(receipt);
                if let IntentSpec::Fact { fact, .. } = &spec {
                    self.observe_receipt = Some((receipt, fact.clone()));
                }
                let _ = tick;
            }
            Err(error) => {
                eprintln!("live: register intent: {error}");
            }
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
        self.summary.receipt = Some(format!("{outcome:?}"));
        self.runner.settle(praxion_receipts::interpreter_outcome(outcome));
    }
}

impl Driver for LiveDriver {
    fn frame(&mut self, view: &TickView<'_>) -> ControlFrame {
        self.last_tick = view.tick;
        let Some(world) = view.world else {
            return ControlFrame::new(view.tick, view.body.generation(), Held::NONE);
        };
        self.perception.begin_tick(world);
        let ts = now_ns();
        // Register the pending intent before folding this tick's packets into
        // the engine: the server can answer the action of the previous tick in
        // this very batch, and those packets are its evidence, not its
        // baseline.
        self.register_pending(view.tick, world);
        for packet in view.packets {
            if let praxion_decoder::Packet::PlayClientbound(
                azalea_protocol::packets::game::ClientboundGamePacket::ContainerSetSlot(slot),
            ) = &packet.packet
            {
                eprintln!(
                    "container_set_slot id={} slot={} item={:?}",
                    slot.container_id, slot.slot, slot.item_stack
                );
            }
            // The packets a receipt can use as hurt or death evidence. Small
            // volume, and the only way to see which one the server picked.
            if let praxion_decoder::Packet::PlayClientbound(packet) = &packet.packet {
                use azalea_protocol::packets::game::ClientboundGamePacket as C;
                match packet {
                    C::EntityEvent(event) => eprintln!(
                        "entity_event id={} status={}",
                        event.entity_id.0, event.event_id
                    ),
                    C::DamageEvent(event) => {
                        eprintln!("damage_event id={}", event.entity_id.0);
                    }
                    C::HurtAnimation(hurt) => {
                        eprintln!("hurt_animation id={}", hurt.id.0);
                    }
                    C::RemoveEntities(remove) => eprintln!(
                        "remove_entities {:?}",
                        remove.entity_ids.iter().map(|id| id.0).collect::<Vec<_>>()
                    ),
                    _ => {}
                }
            }
            let _ = self
                .engine
                .observe(&mut self.log, view.tick, ts, &packet.packet, world);
        }
        self.poll_receipt();

        if view.tick < self.options.start_delay_ticks {
            return ControlFrame::new(view.tick, view.body.generation(), Held::NONE);
        }
        if view.tick == self.options.start_delay_ticks {
            let nearest = self.perception.nearest_block(
                praxion_perception::Caller::Other,
                world,
                azalea_registry::builtin::BlockKind::OakLog,
                5,
            );
            eprintln!(
                "live: start tick={} status={:?} chunks={} world_player={:?} body={:?} floor={:?} target={:?} nearest_log={:?}",
                view.tick,
                world.status(),
                world.chunk_count(),
                world.player_position(),
                view.body.position(),
                world.block_state(praxion_world::BlockPos::new(0, 69, 0)),
                world.block_state(praxion_world::BlockPos::new(3, 70, 0)),
                nearest,
            );
        }

        // One primitive is the whole run in primitive mode. In skill mode its
        // outcome belongs to the interpreter: `tick_skill` polls it through
        // the bridge and decides what the skill does next.
        if matches!(&self.options.mode, LiveMode::Primitive { .. })
            && let Some(outcome) = self.runner.outcome().cloned()
        {
            self.finish(outcome);
        }
        if self.done {
            return ControlFrame::new(view.tick, view.body.generation(), Held::NONE);
        }

        match self.options.mode.clone() {
            LiveMode::Primitive { name, arg } => {
                if !self.started {
                    self.started = true;
                    let args = arg
                        .map(|arg| vec![argument(&name, arg)])
                        .unwrap_or_default();
                    self.start_primitive(&StartAction {
                        primitive: name,
                        args,
                    });
                }
            }
            LiveMode::Skill { .. } => self.tick_skill(view, world),
        }

        if self.done {
            return ControlFrame::new(view.tick, view.body.generation(), Held::NONE);
        }
        let result = {
            let mut ctx = Ctx {
                tick: view.tick,
                body: view.body,
                world,
                perception: &mut self.perception,
                look: self.options.look,
            };
            self.runner.tick(&mut ctx)
        };
        if let Some(pending) = result.pending {
            self.pending = Some(pending);
        }
        // Same rule as above: in skill mode the primitive's outcome is for the
        // interpreter, which reads it from the runner on the next tick.
        if matches!(&self.options.mode, LiveMode::Primitive { .. })
            && let Some(outcome) = result.outcome
        {
            self.finish(outcome);
        }
        self.summary.ticks = view.tick + 1;
        self.summary.position = Some([
            view.body.position().x,
            view.body.position().y,
            view.body.position().z,
        ]);
        self.summary.inventory = inventory_summary(world);
        let frame = result.frame;
        // Spend the leftover ray budget on the known map after the primitive's
        // own queries, so a sweep never starves the action.
        self.perception.sweep(world);
        frame
    }

    fn applied(&mut self, tick: u64, report: &praxion_headless::AppliedReport) {
        if report.block_hit.is_some() || report.attack_entity.is_some() || report.container_id.is_some()
        {
            eprintln!(
                "applied tick={tick} clicks={} seq={:?} hit={:?} face={:?} attack={:?} container={:?} state={:?}",
                report.clicks,
                report.block_seq,
                report.block_hit,
                report.block_face,
                report.attack_entity,
                report.container_id,
                report.menu_state_id,
            );
        }
        self.last_report = Some((tick, report.clone()));
    }

    fn done(&self) -> bool {
        self.done
    }

    fn summary_json(&self) -> Option<String> {
        serde_json::to_string(&self.summary()).ok()
    }
}

impl LiveDriver {
    fn tick_skill(&mut self, view: &TickView<'_>, world: &World) {
        if let Some(outcome) = self.runner.outcome().cloned() {
            eprintln!("primitive outcome: {outcome:?}");
            self.bridge.outcome = Some(outcome);
        }
        let decision = {
            let mut facts = WorldFacts {
                perception: &mut self.perception,
                world,
            };
            self.interpreter
                .as_mut()
                .expect("skill mode has an interpreter")
                .tick(&mut facts, &mut self.bridge)
        };
        if let Decision::Run { action, .. } = &decision {
            eprintln!(
                "skill run tick={} skill={:?} primitive={} args={:?}",
                view.tick,
                self.interpreter.as_ref().map(|interpreter| interpreter.current_skill()),
                action.primitive,
                action.args
            );
        }
        if matches!(&decision, Decision::Done) {
            let interpreter = self.interpreter.as_ref().expect("skill mode");
            eprintln!(
                "skill done tick={} skill={:?} depth={} logs={} planks={} sticks={} pickaxe={} tables={}",
                view.tick,
                interpreter.current_skill(),
                interpreter.frame_depth(),
                count(world, "#logs"),
                count(world, "#planks"),
                count(world, "stick"),
                count(world, "wooden_pickaxe"),
                count(world, "crafting_table"),
            );
        }
        // The interpreter timed the flight out and asked the actuator to
        // cancel; stop the primitive so it cannot run beside its retry.
        if std::mem::take(&mut self.bridge.cancelled) {
            self.runner.cancel();
        }
        if let Some(action) = self.bridge.start.take()
            && !self.runner.is_active()
        {
            self.start_primitive(&action);
        }
        if let Some(action) = self.bridge.continuous.take()
            && !self.runner.is_active()
        {
            self.start_primitive(&action);
        }
        match decision {
            Decision::Done => self.finish(Outcome::Confirmed),
            Decision::Failed(kind) => self.finish(Outcome::Failed(kind)),
            Decision::Observe(fact) => {
                if !self.runner.is_active() {
                    self.start_primitive(&StartAction {
                        primitive: "observe".to_owned(),
                        args: vec![ArgValue::Fact(fact)],
                    });
                }
            }
            _ => {}
        }
        // An observe receipt is confirmed when perception sees the fact.
        if let Some((receipt, fact)) = self.observe_receipt.clone() {
            let mut ctx = Ctx {
                tick: view.tick,
                body: view.body,
                world,
                perception: &mut self.perception,
                look: self.options.look,
            };
            if fact_known(&mut ctx, &fact) {
                let _ = self
                    .engine
                    .confirm(&mut self.log, receipt, view.tick, now_ns(), "fact_known");
                self.observe_receipt = None;
            }
        }
    }
}

/// The interpreter's `Facts` over perception and the raw inventory.
struct WorldFacts<'a> {
    perception: &'a mut Perception,
    world: &'a World,
}

impl InterpreterFacts for WorldFacts<'_> {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        match call.name.as_str() {
            "has" => {
                let Some(name) = call.args.first().and_then(ArgValue::name) else {
                    return Truth::False;
                };
                Truth::True
                    .take_if_true(count(self.world, name) > 0)
                    .unwrap_or(Truth::False)
            }
            "near" => {
                let Some(name) = call.args.first().and_then(ArgValue::name) else {
                    return Truth::False;
                };
                let radius = call
                    .args
                    .get(1)
                    .and_then(ArgValue::as_int)
                    .unwrap_or(0)
                    .max(0) as u32;
                match near_block(self.perception, self.world, name, radius) {
                    Some(true) => Truth::True,
                    Some(false) => Truth::False,
                    None => Truth::Unknown(call.clone()),
                }
            }
            "near_entity" => {
                let Some(name) = call.args.first().and_then(ArgValue::name) else {
                    return Truth::False;
                };
                let radius = call
                    .args
                    .get(1)
                    .and_then(ArgValue::as_int)
                    .unwrap_or(0)
                    .max(0) as u32;
                match name.parse::<azalea_registry::builtin::EntityKind>() {
                    Ok(kind) => {
                        match self
                            .perception
                            .nearest_entity(Caller::Other, self.world, kind, radius)
                        {
                            Answer::Known { .. } => Truth::True,
                            Answer::Unknown(_) => Truth::Unknown(call.clone()),
                        }
                    }
                    Err(_) => Truth::False,
                }
            }
            _ => Truth::Unknown(call.clone()),
        }
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        match call.name.as_str() {
            "count" => IntValue::Known(
                call.args
                    .first()
                    .and_then(ArgValue::name)
                    .map(|name| i64::from(count(self.world, name)))
                    .unwrap_or(0),
            ),
            _ => IntValue::Unknown(call.clone()),
        }
    }
}

fn count(world: &World, wanted: &str) -> u32 {
    world
        .inventory()
        .iter()
        .filter(|stack| {
            if let Some(tag) = wanted.strip_prefix('#') {
                praxion_gamedata::item_in_tag(stack.kind().to_str(), tag)
            } else {
                stack.kind().to_str().strip_prefix("minecraft:").unwrap_or(stack.kind().to_str())
                    == wanted.strip_prefix("minecraft:").unwrap_or(wanted)
            }
        })
        .map(|stack| stack.count().max(0) as u32)
        .sum()
}

fn argument(primitive: &str, arg: String) -> ArgValue {
    match primitive {
        "mine" | "move_to" | "explore" => ArgValue::Block(arg),
        "attack" => ArgValue::EntityKind(arg),
        _ => ArgValue::Item(arg),
    }
}

fn inventory_summary(world: &World) -> BTreeMap<String, i64> {
    let mut summary = BTreeMap::new();
    for stack in world.inventory() {
        if !stack.is_empty() {
            let name = stack.kind().to_str().to_owned();
            *summary.entry(name).or_insert(0) += i64::from(stack.count());
        }
    }
    summary
}

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as u64)
        .unwrap_or(0)
}

/// A small `Truth::True` helper so `has` reads as one expression.
trait TruthExt {
    fn take_if_true(self, condition: bool) -> Option<Truth>;
}

impl TruthExt for Truth {
    fn take_if_true(self, condition: bool) -> Option<Truth> {
        condition.then_some(self)
    }
}

/// The interpreter's actuator bridge: records the chosen action and the
/// primitive runner's outcome.
#[derive(Debug, Default)]
struct BridgeActuator {
    start: Option<StartAction>,
    outcome: Option<Outcome>,
    continuous: Option<StartAction>,
    /// The interpreter asked to cancel the flight (a deadline fired); the
    /// driver stops the runner before it starts the retry.
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
