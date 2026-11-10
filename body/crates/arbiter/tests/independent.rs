//! Independent adversarial tests for the arbiter (step 9).

use azalea_block::BlockState;
use azalea_entity::LookDirection;
use azalea_registry::builtin::{BlockKind, EntityKind, ItemKind};
use praxion_eventlog::EventKind;
use praxion_headless::Body;
use praxion_interpreter::{
    Actuator, Facts, IntValue, Interpreter, Outcome, StartAction, Truth,
};
use praxion_planner::{TestWorld, aim};
use praxion_reflexes::{
    EntityView, FoodChoice, MAX_ARROWS, MAX_ENDERMEN, MAX_MELEE_OTHERS, Settings, View, WeaponChoice,
};
use praxion_rules::{ArgValue, FactCall, Program};
use praxion_world::Vec3;

use praxion_arbiter::{Arbiter, ArbiterTick, NeedKind, Needs, Owner, Tier};

// ---------------------------------------------------------------------------
// Mock interpreter world (copied from the crate's own tests; public traits only)
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
}

impl Mock {}

impl Facts for Mock {
    fn bool_fact(&mut self, call: &FactCall) -> Truth {
        self.facts.push(call.name.clone());
        if call.name == "has" {
            Truth::False
        } else {
            Truth::True
        }
    }

    fn int_fact(&mut self, call: &FactCall) -> IntValue {
        self.facts.push(call.name.clone());
        IntValue::Known(1)
    }
}

impl Actuator for Mock {
    fn start(&mut self, action: &StartAction, tick: u64) -> u32 {
        self.starts.push((action.primitive.clone(), tick));
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

fn until_program() -> Program {
    program(
        r#"
skill work()
  rules:
    true -> wait(50) until has(stone)
"#,
    )
}

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
        melee_others: [None; MAX_MELEE_OTHERS],
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
fn soft_threshold_waits_for_safe_point_under_until_hard_preempts() {
    let mut rig = Rig::new(until_program());
    rig.command("work");
    let settings = Settings::default();

    // First tick starts the skill; the until guard is now active.
    let quiet = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 20);
    rig.tick(0, &quiet, Needs { health: 20.0, food: 20 }, &settings);
    assert!(rig.interpreter.until_active(), "the until commitment is active");

    // Soft threshold must NOT fire while the until guard is active.
    let soft_food = settings.eat_soft_threshold;
    let hungry = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, soft_food);
    let out = rig.tick(
        1,
        &hungry,
        Needs {
            health: 20.0,
            food: soft_food,
        },
        &settings,
    );
    assert!(
        !matches!(out.maintenance, Some(NeedKind::Eat)),
        "soft threshold must wait for the safe point"
    );

    // Hard threshold preempts at once, even with the until guard active.
    let hard_food = settings.eat_hard_threshold;
    let starving = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, hard_food);
    let out = rig.tick(
        2,
        &starving,
        Needs {
            health: 20.0,
            food: hard_food,
        },
        &settings,
    );
    assert_eq!(
        out.owner,
        Owner::Maintenance(NeedKind::Eat),
        "hard threshold preempts immediately"
    );
}

#[test]
fn no_a_b_a_within_100_ticks_under_adversarial_switches() {
    let settings = Settings::default();
    let mut arbiter = Arbiter::new(0);
    let mut switches = Vec::new();
    for tick in 0..10_000u64 {
        // adversarially alternating gain: high, low, high, ...
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
            "A-B-A inside 100 ticks: {switches:?}"
        );
    }
}

#[test]
fn generation_increments_exactly_once_per_owner_change_over_random_ticks() {
    let settings = Settings {
        max_reflex_ticks: 4,
        wake_after_ticks: 1,
        air_surface_threshold: 120,
        ..Settings::default()
    };
    let mut rig = Rig::new(continuous_program());
    let mut rng: u64 = 0x1234_5678_9abc_def0;
    let mut prev_gen = rig.arbiter.generation();
    let mut gen_increments = 0u64;
    let mut owner_changed_events = 0u64;
    for tick in 0..10_000u64 {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let creeper_here = (rng >> 63) == 0;
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let food = (rng % 21) as u32;
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let health = (rng % 21) as f32;

        let base = bare_view(Vec3::new(0.5, 64.0, 0.5), health.max(1.0), food);
        let view = if creeper_here {
            with_creeper(base, 1, Vec3::new(0.5, 64.0, 2.5))
        } else {
            base
        };
        let needs = Needs {
            health: health.max(1.0),
            food,
        };
        let out = rig.tick(tick, &view, needs, &settings);
        if out.generation != prev_gen {
            let delta = out.generation - prev_gen;
            gen_increments += u64::from(delta);
        }
        prev_gen = out.generation;
        owner_changed_events += out
            .events
            .iter()
            .filter(|e| matches!(e, EventKind::OwnerChanged { .. }))
            .count() as u64;
    }
    assert_eq!(
        gen_increments, owner_changed_events,
        "every generation increment must be an owner change"
    );
}

#[test]
fn a_never_ending_reflex_is_cut_at_max_reflex_ticks_and_wakes_the_brain() {
    let settings = Settings {
        max_reflex_ticks: 5,
        wake_after_ticks: 1,
        air_surface_threshold: 300,
        ..Settings::default()
    };
    let mut rig = Rig::new(continuous_program());
    let mut wake_seen = false;
    for tick in 0..20u64 {
        let mut view = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 20);
        view.head_in_water = true;
        view.head_state = BlockState::from(BlockKind::Water);
        let out = rig.tick(
            tick,
            &view,
            Needs {
                health: 20.0,
                food: 20,
            },
            &settings,
        );
        if out.wake_brain {
            wake_seen = true;
        }
    }
    assert!(wake_seen, "a reflex cut at max_reflex_ticks must wake the brain");
}

#[test]
fn enderman_guard_random_look_sequences_never_rest_on_head_two_ticks() {
    use praxion_headless::Look;
    use praxion_reflexes::EndermanGuard;

    let feet = Vec3::new(0.5, 64.0, 0.5);
    let feet_cell = feet.to_block_pos_floor();
    let mut view = bare_view(feet, 20.0, 20);
    view.endermen[0] = Some(EntityView {
        id: 9,
        kind: EntityKind::Enderman,
        position: Vec3::new(0.5, 64.0, 5.0),
        distance: 5.0,
    });
    view.enderman_count = 1;

    let guard = EndermanGuard::new();
    let head = Vec3::new(0.5, 65.5, 5.0);
    let (hy, hp) = aim(view.eye, head);
    let mut current = LookDirection::new(hy, hp);

    let mut rng: u64 = 0xfeed_face_cafe_beef;
    let mut prev_on_head = true;
    for _ in 0..2000 {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let dy = ((rng >> 32) as f32 / u32::MAX as f32) * 360.0 - 180.0;
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let dp = ((rng >> 32) as f32 / u32::MAX as f32) * 180.0 - 90.0;
        let desired = Look::Aim { yaw: dy, pitch: dp };
        let out = guard.apply(desired, current, &view);
        let (oy, op) = match out {
            Look::Aim { yaw, pitch } => (yaw, pitch),
            Look::None => (current.y_rot(), current.x_rot()),
            Look::Delta { dyaw, dpitch } => (current.y_rot() + dyaw, current.x_rot() + dpitch),
        };
        // on-head test
        let (hy, hp) = aim(view.eye, head);
        let diff_yaw = {
            let d = (hy - oy).rem_euclid(360.0);
            if d > 180.0 { 360.0 - d } else { d }
        };
        let now_on_head = diff_yaw <= 8.0 && (hp - op).abs() <= 8.0;
        assert!(
            !(prev_on_head && now_on_head),
            "guard allowed two consecutive on-head looks"
        );
        current = LookDirection::new(oy, op);
        prev_on_head = now_on_head;
    }
    let _ = feet_cell;
    let _ = LookDirection::new(0.0, 0.0);
}

// timing
#[test]
fn hot_path_timing_arbiter() {
    let settings = Settings::default();
    let mut rig = Rig::new(continuous_program());
    rig.command("work");
    let start = std::time::Instant::now();
    let n = 500u64;
    for tick in 0..n {
        let view = bare_view(Vec3::new(0.5, 64.0, 0.5), 20.0, 20);
        rig.tick(tick, &view, Needs { health: 20.0, food: 20 }, &settings);
    }
    let elapsed = start.elapsed();
    eprintln!(
        "arbiter hot path: {n} ticks = {:?} ({:.3} ms/tick)",
        elapsed,
        elapsed.as_secs_f64() * 1000.0 / n as f64
    );
}
