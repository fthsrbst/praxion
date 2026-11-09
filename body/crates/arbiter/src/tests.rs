//! Arbiter behaviour: tiers, thresholds, preemption, generation and the intent
//! stack. The interpreter runs on a small mock world; the reflex view is built
//! by hand so the tests can place a threat exactly where they need it.

use azalea_block::BlockState;
use azalea_entity::LookDirection;
use azalea_registry::builtin::{BlockKind, EntityKind, ItemKind};
use praxion_eventlog::EventKind;
use praxion_headless::{Body, Look};
use praxion_interpreter::{
    Actuator, Decision, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};
use praxion_planner::{TestWorld, aim};
use praxion_reflexes::{
    EntityView, FoodChoice, MAX_ARROWS, MAX_ENDERMEN, ReflexKind, Settings, View, WeaponChoice,
};
use praxion_rules::{ArgValue, FactCall, Program};
use praxion_world::Vec3;

use crate::{Arbiter, ArbiterTick, CommandError, IntentState, NeedKind, Needs, Owner, Tier};

// ---------------------------------------------------------------------------
// Mock interpreter world
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct Mock {
    duration: u64,
    flight: Option<(u64, Outcome)>,
    starts: Vec<(String, u64)>,
    settlements: Vec<Outcome>,
    cancels: u32,
    applies: u32,
    facts: Vec<String>,
    /// What the `food` fact answers (the tests move it with the meals).
    food: i64,
    /// What `count(...)` answers; 0 makes a meal fail `no_item`.
    count: Option<i64>,
    /// The first argument of every started primitive, as text.
    start_args: Vec<String>,
}

impl Mock {
    fn with_duration(duration: u64) -> Self {
        Self {
            duration,
            ..Self::default()
        }
    }
}

impl Facts for Mock {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        self.facts.push(call.name.clone());
        Truth::True
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        self.facts.push(call.name.clone());
        match call.name.as_str() {
            "food" => IntValue::Known(self.food),
            "count" => IntValue::Known(self.count.unwrap_or(1)),
            _ => IntValue::Known(1),
        }
    }
}

impl Actuator for Mock {
    fn start(&mut self, action: &StartAction, tick: u64) -> u32 {
        self.starts.push((action.primitive.clone(), tick));
        self.start_args
            .push(action.args.first().map(crate::arg_to_string).unwrap_or_default());
        self.flight = Some((tick + self.duration, Outcome::Confirmed));
        self.duration as u32
    }

    fn poll(&mut self, tick: u64) -> Option<Outcome> {
        match &self.flight {
            Some((settle_at, _)) if tick >= *settle_at => {
                let (_, outcome) = self.flight.take().expect("checked above");
                self.settlements.push(outcome.clone());
                Some(outcome)
            }
            _ => None,
        }
    }

    fn cancel(&mut self) -> Option<Outcome> {
        if self.flight.take().is_some() {
            self.cancels += 1;
            self.settlements.push(Outcome::Preempted);
            Some(Outcome::Preempted)
        } else {
            None
        }
    }

    fn apply(&mut self, _action: &StartAction, _tick: u64) {
        self.applies += 1;
    }

    fn release(&mut self) {}
}

/// The real core skill the arbiter pushes for a meal.
const EAT_FOOD: &str = include_str!("../../../skills/eat_food.skill");

/// `source` plus the real `eat_food` skill: a need pushes it by name.
fn program(source: &str) -> Program {
    praxion_rules::load(&format!("{EAT_FOOD}\n{source}"))
        .unwrap_or_else(|errors| panic!("{errors:?}"))
}

fn continuous_program() -> Program {
    program(
        r#"
skill work()
  rules:
    true -> wait(50)
"#,
    )
}

fn discrete_program() -> Program {
    program(
        r#"
skill dig()
  rules:
    true -> mine(stone)
"#,
    )
}

// ---------------------------------------------------------------------------
// Hand-built reflex views
// ---------------------------------------------------------------------------

fn stone() -> BlockState {
    BlockState::from(BlockKind::Stone)
}

fn bread() -> FoodChoice {
    FoodChoice {
        slot: 9,
        kind: ItemKind::Bread,
        nutrition: 5,
        saturation: 6.0,
    }
}

fn bare_view(feet: Vec3, health: f32, food: u32) -> View {
    let feet_cell = feet.to_block_pos_floor();
    View {
        tick: 0,
        position: feet,
        eye: feet + Vec3::new(0.0, 1.62, 0.0),
        rotation: (0.0, 0.0),
        health: Some(health),
        food: Some(food),
        day_time: 1_000,
        feet: feet_cell,
        eye_cell: feet_cell.up(1),
        feet_state: BlockState::AIR,
        head_state: BlockState::AIR,
        below_state: stone(),
        head_in_water: false,
        feet_in_water: false,
        feet_in_lava: false,
        on_fire: false,
        lava: None,
        water: None,
        next_cell: feet_cell.south(1),
        next_cell_lava: false,
        next_drop: 0,
        creeper: None,
        melee: None,
        endermen: [None; MAX_ENDERMEN],
        enderman_count: 0,
        arrows: [None; MAX_ARROWS],
        arrow_count: 0,
        best_food: Some(bread()),
        best_weapon: None::<WeaponChoice>,
    }
}

fn with_creeper(mut view: View, id: i32, position: Vec3) -> View {
    view.creeper = Some(EntityView {
        id,
        kind: EntityKind::Creeper,
        position,
        distance: view.eye.distance_to(position),
    });
    view
}

// ---------------------------------------------------------------------------
// One rig
// ---------------------------------------------------------------------------

struct Rig {
    body: Body,
    interpreter: Interpreter,
    facts: Mock,
    actuator: Mock,
    arbiter: Arbiter,
    blocks: TestWorld,
}

impl Rig {
    fn new(program: Program) -> Self {
        let body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        let arbiter = Arbiter::new(body.generation());
        Self {
            body,
            interpreter: Interpreter::new(program),
            facts: Mock::default(),
            actuator: Mock::default(),
            arbiter,
            blocks: TestWorld::flat(63, 12),
        }
    }

    fn command(&mut self, skill: &str) {
        self.arbiter
            .command(
                &mut self.interpreter,
                &mut self.body,
                0,
                skill,
                Vec::<ArgValue>::new(),
            )
            .expect("the program has the skill");
    }

    fn tick(&mut self, tick: u64, view: &View, needs: Needs, settings: &Settings) -> ArbiterTick {
        self.arbiter.tick(
            view,
            &mut self.interpreter,
            &mut self.facts,
            &mut self.actuator,
            needs,
            settings,
            &mut self.body,
            tick,
            &mut self.blocks,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn tier_one_preemption_happens_within_one_tick() {
    let mut rig = Rig::new(continuous_program());
    rig.command("work");
    let view = with_creeper(
        bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 20),
        1,
        Vec3::new(0.5, 64.0, 2.5),
    );
    let out = rig.tick(0, &view, Needs { health: 20.0, food: 20 }, &Settings::default());
    assert_eq!(out.owner, Owner::Reflex(ReflexKind::Creeper));
    assert_eq!(out.reflex, Some(ReflexKind::Creeper));
    assert_eq!(out.generation, 2, "command 1, reflex preemption 2");
    assert!(rig.interpreter.suspended());
    assert!(
        out.events.iter().any(|event| matches!(
            event,
            EventKind::OwnerChanged { generation: 2, owner } if owner == "reflex:creeper"
        )),
        "{:?}",
        out.events
    );
    assert!(
        out.events
            .iter()
            .any(|event| matches!(event, EventKind::ReplanNeeded { .. })),
        "{:?}",
        out.events
    );
}

#[test]
fn generation_increments_exactly_once_per_owner_change() {
    let mut rig = Rig::new(continuous_program());
    rig.command("work");
    let settings = Settings {
        max_reflex_ticks: 4,
        ..Settings::default()
    };
    let mut generations = vec![rig.arbiter.generation()];
    let view = with_creeper(
        bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 20),
        1,
        Vec3::new(0.5, 64.0, 2.5),
    );
    for tick in 0..6u64 {
        let out = rig.tick(tick, &view, Needs { health: 20.0, food: 20 }, &settings);
        generations.push(out.generation);
        if tick >= 5 {
            assert_eq!(out.owner, Owner::Skill { intent_id: 1 }, "resumed");
        }
    }
    // One bump for the command, one for the reflex, one for the resume.
    assert_eq!(generations[0], 1);
    assert_eq!(generations[1], 2, "reflex start");
    assert_eq!(generations[2], 2);
    assert_eq!(generations[3], 2);
    assert_eq!(generations[4], 2);
    assert_eq!(generations[5], 3, "resume");
    assert_eq!(generations[6], 3);
}

#[test]
fn a_preempted_discrete_action_settles_preempted_once() {
    let mut rig = Rig::new(discrete_program());
    rig.command("dig");
    rig.actuator = Mock::with_duration(20);

    let quiet = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 20);
    let first = rig.tick(0, &quiet, Needs { health: 20.0, food: 20 }, &Settings::default());
    assert!(matches!(first.skill, Some(Decision::Run { .. })));
    assert_eq!(rig.actuator.starts.len(), 1);

    let view = with_creeper(
        bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 20),
        1,
        Vec3::new(0.5, 64.0, 2.5),
    );
    let second = rig.tick(1, &view, Needs { health: 20.0, food: 20 }, &Settings::default());
    assert_eq!(second.owner, Owner::Reflex(ReflexKind::Creeper));
    assert_eq!(rig.actuator.cancels, 1);
    assert_eq!(
        rig.actuator
            .settlements
            .iter()
            .filter(|outcome| **outcome == Outcome::Preempted)
            .count(),
        1
    );
    assert_eq!(
        rig.actuator.starts.len(),
        1,
        "the preempted action is not restarted"
    );

    rig.tick(2, &view, Needs { health: 20.0, food: 20 }, &Settings::default());
    assert_eq!(rig.actuator.cancels, 1);
}

#[test]
fn the_soft_threshold_eats_at_a_safe_point_and_never_starves() {
    let mut rig = Rig::new(continuous_program());
    rig.command("work");
    let settings = Settings::default();

    let mut food = 20u32;
    let mut min_food = food;
    let mut eats = 0;
    let mut maintenance_ticks = 0;
    let mut bitten = 0;
    for tick in 0..36_000u64 {
        if tick % 400 == 399 {
            food = food.saturating_sub(1);
        }
        min_food = min_food.min(food);
        let view = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, food);
        rig.facts.food = i64::from(food);
        let out = rig.tick(tick, &view, Needs { health: 20.0, food }, &settings);
        if out.maintenance == Some(NeedKind::Eat) {
            maintenance_ticks += 1;
        }
        // Each `eat` the meal's skill starts feeds the body, as a bite does.
        let bites = rig
            .actuator
            .starts
            .iter()
            .filter(|(primitive, _)| primitive == "eat")
            .count();
        if bites > bitten {
            bitten = bites;
            food = (food + 6).min(20);
        }
        if out.ate.is_some() {
            eats += 1;
        }
    }
    assert!(eats > 0, "the arbiter ate at least once");
    assert!(maintenance_ticks > 0);
    assert!(
        min_food > settings.eat_hard_threshold,
        "no starvation: min food {min_food}"
    );
    assert!(
        min_food >= settings.eat_soft_threshold - 1,
        "ate at the soft threshold, min food {min_food}"
    );
}

#[test]
fn the_hard_threshold_preempts_an_in_flight_action() {
    let mut rig = Rig::new(discrete_program());
    rig.command("dig");
    rig.actuator = Mock::with_duration(20);
    let settings = Settings::default();

    let quiet = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 20);
    rig.tick(0, &quiet, Needs { health: 20.0, food: 20 }, &settings);
    assert_eq!(rig.actuator.starts.len(), 1);

    let hungry = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, settings.eat_hard_threshold);
    let out = rig.tick(
        1,
        &hungry,
        Needs {
            health: 20.0,
            food: settings.eat_hard_threshold,
        },
        &settings,
    );
    assert_eq!(out.owner, Owner::Maintenance(NeedKind::Eat));
    assert_eq!(rig.actuator.cancels, 1);
    assert_eq!(
        rig.actuator
            .settlements
            .iter()
            .filter(|outcome| **outcome == Outcome::Preempted)
            .count(),
        1
    );
}

#[test]
fn same_tier_switches_never_produce_a_b_a_inside_five_seconds() {
    let settings = Settings::default();
    let mut arbiter = Arbiter::new(0);
    let mut switches = Vec::new();
    for tick in 0..500u64 {
        let gain = if (tick / 10) % 2 == 0 { 10.0 } else { 6.0 };
        if arbiter.switch_allowed(
            Tier::Maintenance,
            Tier::Maintenance,
            gain,
            3.0,
            false,
            &settings,
            tick,
        ) {
            switches.push(tick);
        }
    }
    assert!(!switches.is_empty());
    for pair in switches.windows(2) {
        assert!(
            pair[1] - pair[0] >= u64::from(settings.min_switch_ticks),
            "A-B-A inside five seconds: {switches:?}"
        );
    }
}

#[test]
fn a_same_tier_switch_needs_the_gain_and_no_until() {
    let settings = Settings::default();
    let arbiter = Arbiter::new(0);
    assert!(!arbiter.may_switch(
        Tier::Maintenance,
        Tier::Maintenance,
        100.0,
        1.0,
        false,
        &settings,
        0
    ));
    assert!(!arbiter.may_switch(
        Tier::Maintenance,
        Tier::Maintenance,
        6.0,
        3.0,
        false,
        &settings,
        200
    ));
    assert!(!arbiter.may_switch(
        Tier::Maintenance,
        Tier::Maintenance,
        100.0,
        1.0,
        true,
        &settings,
        200
    ));
    assert!(arbiter.may_switch(
        Tier::Maintenance,
        Tier::Maintenance,
        7.0,
        3.0,
        false,
        &settings,
        200
    ));
}

#[test]
fn a_new_command_suspends_the_old_one() {
    let mut rig = Rig::new(continuous_program());
    rig.command("work");
    let second = rig
        .arbiter
        .command(
            &mut rig.interpreter,
            &mut rig.body,
            10,
            "work",
            Vec::<ArgValue>::new(),
        )
        .expect("same skill, new command");
    assert_eq!(second.intent_id, 2);
    let states: Vec<(u64, IntentState)> = rig
        .arbiter
        .stack()
        .entries()
        .iter()
        .map(|entry| (entry.intent_id, entry.state))
        .collect();
    assert_eq!(states[0], (1, IntentState::Suspended));
    assert_eq!(states[1], (2, IntentState::Running));
}

#[test]
fn end_command_is_idempotent_for_an_already_done_intent() {
    let mut rig = Rig::new(continuous_program());
    rig.command("work");

    let first = rig.arbiter.end_command(1, &mut rig.body, 1);
    assert!(
        first.iter().any(|event| matches!(
            event,
            EventKind::IntentState { intent_id: 1, state } if state == "done"
        )),
        "{first:?}"
    );
    assert_eq!(*rig.arbiter.owner(), Owner::Idle);

    let second = rig.arbiter.end_command(1, &mut rig.body, 2);
    assert!(
        second.is_empty(),
        "a second end_command must not re-emit done: {second:?}"
    );
}

#[test]
fn a_refused_skill_leaves_no_intent_behind() {
    // The push happens before the skill starts, so a refused skill must roll
    // its entry back: an intent without a running skill would own the body.
    let mut rig = Rig::new(continuous_program());
    rig.command("work");
    let depth = rig.arbiter.stack().depth();
    let generation = rig.arbiter.generation();
    let frames = rig.interpreter.frame_depth();

    let refusal = rig
        .arbiter
        .command(
            &mut rig.interpreter,
            &mut rig.body,
            1,
            "no_such_skill",
            Vec::<ArgValue>::new(),
        )
        .unwrap_err();
    assert!(
        matches!(&refusal, CommandError::Skill(message) if message.contains("no_such_skill")),
        "{refusal:?}"
    );
    assert_eq!(rig.arbiter.stack().depth(), depth, "the push was rolled back");
    assert_eq!(
        rig.arbiter.stack().running().map(|entry| entry.skill_ref.as_str()),
        Some("work"),
        "the suspended command runs again"
    );
    assert_eq!(rig.interpreter.frame_depth(), frames, "no skill started");
    assert_eq!(rig.arbiter.generation(), generation, "nothing changed");
}

#[test]
fn a_full_intent_stack_refuses_before_starting_the_skill() {
    let mut rig = Rig::new(continuous_program());
    for _ in 0..8 {
        rig.command("work");
    }
    assert_eq!(rig.arbiter.stack().depth(), 8, "the stack is full");
    let generation = rig.arbiter.generation();

    let refusal = rig
        .arbiter
        .command(
            &mut rig.interpreter,
            &mut rig.body,
            1,
            "work",
            Vec::<ArgValue>::new(),
        )
        .unwrap_err();
    assert_eq!(refusal, CommandError::StackFull);
    assert_eq!(rig.arbiter.stack().depth(), 8, "the stack did not grow");
    assert_eq!(
        rig.arbiter.stack().running().map(|entry| entry.intent_id),
        Some(8),
        "the last accepted command still runs"
    );
    assert_eq!(rig.arbiter.generation(), generation, "no owner change");
}

#[test]
fn the_body_alone_writes_done_when_maintenance_finishes() {
    let mut rig = Rig::new(continuous_program());
    rig.command("work");
    let settings = Settings::default();
    let mut done_at = None;
    let mut food = 10u32;
    for tick in 0..400u64 {
        let view = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, food);
        rig.facts.food = i64::from(food);
        let out = rig.tick(tick, &view, Needs { health: 20.0, food }, &settings);
        if rig.actuator.starts.iter().any(|(primitive, _)| primitive == "eat") {
            food = 20;
        }
        if out.ate.is_some() {
            done_at = Some(tick);
            break;
        }
    }
    assert!(done_at.is_some(), "the eat finished");
    let done = rig
        .arbiter
        .stack()
        .entries()
        .iter()
        .find(|entry| entry.skill_ref == "eat_food")
        .expect("the maintenance entry is kept");
    assert_eq!(done.state, IntentState::Done);
}

fn hungry_rig() -> (Rig, Settings) {
    let mut rig = Rig::new(continuous_program());
    rig.command("work");
    (rig, Settings::default())
}

#[test]
fn a_meal_is_the_eat_food_skill_with_the_item_name_and_no_click_frame() {
    // Critique finding 2: the argument is the food's item name, never the
    // inventory slot (the bread sits in slot 9).
    let (mut rig, settings) = hungry_rig();
    rig.facts.food = 6;
    let hungry = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 6);
    let needs = Needs { health: 20.0, food: 6 };
    let start = rig.tick(0, &hungry, needs, &settings);
    assert_eq!(start.owner, Owner::Maintenance(NeedKind::Eat));
    let id = start.loaded_intent.expect("the arbiter loaded the meal skill");
    let entry = rig.arbiter.stack().get(id).expect("the meal is on the stack");
    assert_eq!((entry.skill_ref.as_str(), entry.args.clone()), ("eat_food", vec!["bread".to_owned()]));
    assert_eq!(entry.tier, Tier::Survival);
    assert_eq!(rig.interpreter.current_skill(), Some("eat_food"));
    // The arbiter builds no click frame of its own on any tick of the meal;
    // the `eat` primitive is what runs.
    let mut frames = vec![start];
    for tick in 1..4 {
        frames.push(rig.tick(tick, &hungry, needs, &settings));
    }
    for out in &frames {
        assert!(out.source.frame().clicks.is_empty(), "{:?}", out.source);
    }
    assert_eq!(frames[1].skill_intent, Some(id));
    let eats: Vec<&String> = rig
        .actuator
        .start_args
        .iter()
        .zip(&rig.actuator.starts)
        .filter(|(_, (primitive, _))| primitive == "eat")
        .map(|(arg, _)| arg)
        .collect();
    assert!(!eats.is_empty(), "the meal runs the eat primitive");
    assert!(eats.iter().all(|arg| *arg == "bread"), "eat gets the item name: {eats:?}");
}

#[test]
fn a_failed_meal_wakes_the_brain_and_is_not_restarted_every_tick() {
    let (mut rig, settings) = hungry_rig();
    rig.facts.food = 6;
    rig.facts.count = Some(0);
    let hungry = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 6);
    let needs = Needs { health: 20.0, food: 6 };
    let mut pushes = 0;
    let mut woke = false;
    for tick in 0..100 {
        let out = rig.tick(tick, &hungry, needs, &settings);
        pushes += out
            .events
            .iter()
            .filter(|event| matches!(event, EventKind::IntentPushed { skill, .. } if skill == "eat_food"))
            .count();
        woke |= out
            .events
            .iter()
            .any(|event| matches!(event, EventKind::WakeBrain { cause, .. } if cause == "hunger"));
    }
    assert_eq!(pushes, 1, "one failed meal, then a cool-down");
    assert!(woke, "the brain hears that the meal failed");
    assert!(
        !matches!(rig.arbiter.owner(), Owner::Maintenance(_)),
        "the failed meal gave the body back: {:?}",
        rig.arbiter.owner()
    );
    assert!(rig.actuator.starts.iter().all(|(primitive, _)| primitive != "eat"));
}

#[test]
fn a_reflex_ends_the_meal_and_leaves_no_suspended_meal_behind() {
    let (mut rig, settings) = hungry_rig();
    rig.facts.food = 6;
    let hungry = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 6);
    let needs = Needs { health: 20.0, food: 6 };
    rig.tick(0, &hungry, needs, &settings);
    rig.tick(1, &hungry, needs, &settings);
    let creeper = with_creeper(hungry.clone(), 1, Vec3::new(0.5, 64.0, 2.5));
    let out = rig.tick(2, &creeper, needs, &settings);
    assert!(matches!(out.owner, Owner::Reflex(_)), "{:?}", out.owner);
    let meal = rig
        .arbiter
        .stack()
        .entries()
        .iter()
        .find(|entry| entry.skill_ref == "eat_food")
        .expect("the meal entry");
    assert_eq!(meal.state, IntentState::Done, "a reflex ends the meal, it is not parked");
}

#[test]
fn a_need_whose_skill_is_missing_is_refused_with_a_wake() {
    // A program without `eat_food`: the push is rolled back, the command keeps
    // the body (critique finding 10: validate the skill name).
    let mut rig = Rig::new(
        praxion_rules::load("skill work()\n  rules:\n    true -> wait(50)\n").unwrap(),
    );
    rig.command("work");
    let settings = Settings::default();
    let hungry = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 6);
    let out = rig.tick(0, &hungry, Needs { health: 20.0, food: 6 }, &settings);
    assert!(
        out.events
            .iter()
            .any(|event| matches!(event, EventKind::WakeBrain { cause, .. } if cause == "need_skill")),
        "{:?}",
        out.events
    );
    assert_eq!(rig.arbiter.stack().depth(), 1, "only the command is on the stack");
    assert!(matches!(rig.arbiter.owner(), Owner::Skill { .. }));
}

#[test]
fn a_death_during_a_meal_ends_the_meal_and_returns_the_command() {
    // Critique finding 11: respawn can land mid-meal.
    let (mut rig, settings) = hungry_rig();
    rig.facts.food = 6;
    let hungry = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 6);
    let needs = Needs { health: 20.0, food: 6 };
    rig.tick(0, &hungry, needs, &settings);
    rig.tick(1, &hungry, needs, &settings);
    assert_eq!(rig.arbiter.owner(), &Owner::Maintenance(NeedKind::Eat));
    let generation = rig.arbiter.generation();
    rig.arbiter.engine_mut().state_mut().air_ticks = 3;

    let events = rig
        .arbiter
        .on_death(&mut rig.interpreter, &mut rig.actuator, &mut rig.body, 2);
    assert!(rig.arbiter.generation() > generation, "a new generation");
    assert_eq!(rig.arbiter.owner(), &Owner::Skill { intent_id: 1 }, "the command has the body back");
    let meal = rig
        .arbiter
        .stack()
        .entries()
        .iter()
        .find(|entry| entry.skill_ref == "eat_food")
        .expect("the meal entry");
    assert_eq!(meal.state, IntentState::Done);
    assert!(
        events.iter().any(|event| matches!(event, EventKind::IntentState { intent_id, state } if *intent_id == meal.intent_id && state == "done")),
        "{events:?}"
    );
    assert_eq!(
        rig.arbiter.engine().state().air_ticks,
        praxion_reflexes::ReflexState::new().air_ticks,
        "the reflex state is the new life's"
    );
}

#[test]
fn a_death_with_no_owner_still_moves_to_a_new_generation() {
    let mut rig = Rig::new(continuous_program());
    let generation = rig.arbiter.generation();
    rig.arbiter
        .on_death(&mut rig.interpreter, &mut rig.actuator, &mut rig.body, 0);
    assert!(rig.arbiter.generation() > generation);
    assert_eq!(rig.arbiter.owner(), &Owner::Idle);
}

#[test]
fn hunger_with_no_food_wakes_the_brain_with_its_own_cause_and_starts_no_meal() {
    let (mut rig, settings) = hungry_rig();
    let mut starving = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 6);
    starving.best_food = None;
    let out = rig.tick(0, &starving, Needs { health: 20.0, food: 6 }, &settings);
    assert!(
        out.events
            .iter()
            .any(|event| matches!(event, EventKind::WakeBrain { cause, .. } if cause == "hunger_no_food")),
        "{:?}",
        out.events
    );
    assert_ne!(out.owner, Owner::Maintenance(NeedKind::Eat));
    assert!(rig.arbiter.stack().entries().iter().all(|entry| entry.skill_ref != "eat_food"));
}

#[test]
fn intent_args_are_preserved_as_strings() {
    assert_eq!(
        crate::arg_to_string(&ArgValue::Item("oak_log".to_owned())),
        "oak_log"
    );
    assert_eq!(crate::arg_to_string(&ArgValue::Int(7)), "7");
    assert_eq!(crate::arg_to_string(&ArgValue::Bool(true)), "true");
}

#[test]
fn the_arbiter_breaks_a_look_that_rests_on_an_enderman_head() {
    let mut rig = Rig::new(continuous_program());
    let feet = Vec3::new(0.5, 64.0, 0.5);
    let head = Vec3::new(0.5, 65.5, 5.0);
    let mut view = bare_view(feet, 20.0, 20);
    view.endermen[0] = Some(EntityView {
        id: 9,
        kind: EntityKind::Enderman,
        position: Vec3::new(0.5, 64.0, 5.0),
        distance: view.eye.distance_to(Vec3::new(0.5, 64.0, 5.0)),
    });
    view.enderman_count = 1;

    // The body already looks at the head, as the previous frame left it.
    let (yaw, pitch) = aim(view.eye, head);
    rig.body.set_look(LookDirection::new(yaw, pitch));

    let out = rig.tick(0, &view, Needs { health: 20.0, food: 20 }, &Settings::default());
    match out.source.frame().look {
        Look::Aim { yaw: broken, .. } => {
            assert!(
                (broken - yaw).abs() > 1.0,
                "the guard must turn the look off the head"
            );
        }
        other => panic!("expected the guard to aim away, got {other:?}"),
    }
}

#[test]
fn tiers_sort_highest_first() {
    assert!(Tier::Survival < Tier::Command);
    assert!(Tier::Command < Tier::Maintenance);
    assert!(Tier::Maintenance < Tier::Projects);
}
