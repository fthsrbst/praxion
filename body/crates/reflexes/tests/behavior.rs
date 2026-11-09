//! Reflex behaviour the coverage matrix does not pin: the latency classes, the
//! priority order, the best food, and the water preference.

mod common;

use std::collections::HashSet;

use azalea_registry::builtin::{BlockKind, EntityKind, ItemKind};
use praxion_headless::Click;
use praxion_planner::TestWorld;
use praxion_reflexes::{
    Cell, EndermanGuard, HEAD_TOLERANCE_DEG, LatencyClass, Motion, ReflexAction, ReflexKind,
    Settings,
};
use praxion_world::{BlockPos, Vec3};

use common::{Sim, set_block, state, test_settings};

fn fresh() -> Sim {
    Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5))
}

#[test]
fn human_floor_never_acts_before_three_ticks_of_visibility() {
    let mut sim = fresh();
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    sim.step();
    sim.step();
    assert_eq!(sim.engine.active(), None, "two ticks is 100 ms");
    assert!(
        !sim.frames
            .iter()
            .any(|frame| frame.clicks.contains(&Click::Attack)),
        "no attack before the floor"
    );
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Melee), "third tick");
}

#[test]
fn lethal_fast_acts_on_the_first_tick() {
    let mut sim = fresh();
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, 2.5));
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Creeper));
    assert!(sim.last_frame.is_some());
}

#[test]
fn a_creeper_outside_three_blocks_waits_for_the_floor() {
    let mut sim = fresh();
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, 7.0));
    sim.step();
    assert_eq!(sim.engine.active(), None);
    sim.step();
    assert_eq!(sim.engine.active(), None);
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Creeper));
}

#[test]
fn the_creeper_class_depends_on_distance() {
    let mut sim = fresh();
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, 2.5));
    sim.step();
    assert_eq!(
        ReflexKind::Creeper.latency_class(&sim.view()),
        LatencyClass::LethalFast
    );

    let mut sim = fresh();
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, 7.0));
    sim.step();
    assert_eq!(
        ReflexKind::Creeper.latency_class(&sim.view()),
        LatencyClass::HumanFloor
    );
}

#[test]
fn lava_outranks_a_creeper_behind_the_human_floor() {
    let mut sim = fresh();
    sim.set_block(BlockPos::new(0, 64, 1), state(BlockKind::Lava));
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, -7.0));
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Lava));
}

#[test]
fn an_arrow_is_only_incoming_once_its_heading_is_known() {
    let mut sim = fresh();
    sim.add_mob(3, EntityKind::Arrow, Vec3::new(0.5, 65.0, 8.0));
    sim.step();
    assert_eq!(sim.engine.active(), None, "no heading on the first sighting");
    sim.add_mob(3, EntityKind::Arrow, Vec3::new(0.5, 65.0, 7.2));
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Arrow));
}

#[test]
fn starvation_eats_the_best_food() {
    let mut sim = fresh();
    sim.set_health(20.0, 5);
    sim.add_item(9, ItemKind::Apple, 1);
    sim.add_item(10, ItemKind::CookedBeef, 1);
    sim.add_item(11, ItemKind::Bread, 1);
    sim.step();
    sim.step();
    sim.step();
    match sim.engine.active_action() {
        Some(ReflexAction::Eat { slot, item }) => {
            assert_eq!(item, ItemKind::CookedBeef);
            assert_eq!(slot, 10);
        }
        other => panic!("expected an eat action, got {other:?}"),
    }
}

#[test]
fn fire_prefers_water_within_eight_blocks() {
    let mut sim = fresh();
    sim.set_block(BlockPos::new(0, 64, 0), state(BlockKind::Fire));
    sim.set_block(BlockPos::new(0, 64, 3), state(BlockKind::Water));
    sim.step();
    match sim.engine.active_action() {
        Some(ReflexAction::ToWater { pos }) => {
            assert_eq!(pos, BlockPos::new(0, 64, 3));
        }
        other => panic!("expected water, got {other:?}"),
    }
}

#[test]
fn fire_without_nearby_water_moves_off_the_fire() {
    let mut sim = fresh();
    sim.set_block(BlockPos::new(0, 64, 0), state(BlockKind::Fire));
    sim.step();
    match sim.engine.active_action() {
        Some(ReflexAction::OffFire { .. }) => {}
        other => panic!("expected off-fire, got {other:?}"),
    }
}

#[test]
fn the_melee_reflex_picks_the_best_weapon() {
    let mut sim = fresh();
    sim.add_item(9, ItemKind::IronAxe, 1);
    sim.add_item(10, ItemKind::DiamondSword, 1);
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    sim.step();
    sim.step();
    sim.step();
    match sim.engine.active_action() {
        Some(ReflexAction::Attack { weapon, .. }) => {
            assert_eq!(weapon, Some(10), "the diamond sword is the best weapon");
        }
        other => panic!("expected an attack, got {other:?}"),
    }
}

/// Start the melee attack the engine chose and return its first frame.
fn first_attack_frame(sim: &mut Sim) -> praxion_headless::ControlFrame {
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    sim.perception.begin_tick(&sim.world);
    let view = sim.view();
    let settings = Settings::default();
    let mut blocks = TestWorld::flat(63, 12);
    let action = ReflexAction::Attack {
        target: 2,
        weapon: view.best_weapon.map(|weapon| weapon.slot),
    };
    let mut motion = Motion::start(action, &view, &settings, &sim.body, 0, &mut blocks)
        .expect("an attack always starts");
    motion.step(0, sim.body.generation(), &sim.body, &view, &settings)
}

#[test]
fn the_melee_attack_selects_the_hotbar_weapon() {
    // Decision 2026-11-09 "Five next areas": the reflex computed the best
    // weapon and then fought with whatever was in hand. Slot 38 is hotbar 2.
    let mut sim = fresh();
    sim.add_item(37, ItemKind::WoodenSword, 1);
    sim.add_item(38, ItemKind::DiamondSword, 1);
    let frame = first_attack_frame(&mut sim);
    assert_eq!(frame.select_slot, Some(2), "the diamond sword, not the wooden one");
}

#[test]
fn a_weapon_outside_the_hotbar_is_not_selected_with_a_hotbar_slot() {
    // Inventory slot 20 has no hotbar index: selecting would pick the wrong
    // item, so the frame leaves the hand alone (the swap is a menu action).
    let mut sim = fresh();
    sim.add_item(20, ItemKind::DiamondSword, 1);
    let frame = first_attack_frame(&mut sim);
    assert_eq!(frame.select_slot, None);
}

#[test]
fn an_attack_without_any_weapon_selects_nothing() {
    let mut sim = fresh();
    let frame = first_attack_frame(&mut sim);
    assert_eq!(frame.select_slot, None);
}

#[test]
fn a_death_resets_the_reflex_state_and_drops_the_running_routine() {
    // Decision 2026-11-09 "Five next areas", critique finding 11: nothing used
    // to reset the reflex state when the player respawned.
    let mut sim = fresh();
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 3.0));
    for _ in 0..4 {
        sim.step();
    }
    assert!(sim.engine.active().is_some(), "a reflex is running");
    {
        let state = sim.engine.state_mut();
        state.air_ticks = 5;
        state.last_safe_cell = Some(BlockPos::new(40, 70, 40));
    }
    sim.engine.on_death();
    assert_eq!(sim.engine.active(), None, "the routine of the old life is gone");
    let state = sim.engine.state();
    assert_eq!(state.last_safe_cell, None);
    assert_eq!(state.air_ticks, praxion_reflexes::ReflexState::new().air_ticks);
    assert_eq!(state.visible_since(ReflexKind::Melee), None);
}

#[test]
fn the_fall_guard_refuses_a_deep_step() {
    let mut sim = fresh();
    for x in -8..=8 {
        for z in 1..=8 {
            set_block(&mut sim.world, BlockPos::new(x, 63, z), state(BlockKind::Air));
        }
    }
    // Stand on the edge first: the follower only walks a grounded body, so a
    // body still in its spawn tick would emit no move for the guard to stop.
    // Two ticks: the first only gains downward velocity, the second collides.
    for _ in 0..2 {
        sim.body.step(&praxion_headless::Held::NONE, &sim.world);
    }
    assert!(sim.body.on_ground(), "the body stands on the edge");
    // The follower waits for the walking pitch before it holds forward, so
    // start the eye already there; the guard is what this test checks.
    sim.body
        .set_look(azalea_entity::LookDirection::new(0., 8.));
    // A reflex that wants to go south, straight into the chasm.
    let action = ReflexAction::StepBack {
        to: BlockPos::new(0, 64, 1),
    };
    let view = {
        sim.perception.begin_tick(&sim.world);
        praxion_reflexes::View::capture(&mut sim.perception, &sim.world, sim.engine.state_mut())
    };
    let mut blocks = praxion_reflexes::ReflexBlocks {
        perception: &mut sim.perception,
        world: &sim.world,
    };
    let mut motion = praxion_reflexes::Motion::start(
        action,
        &view,
        &Settings::default(),
        &sim.body,
        0,
        &mut blocks,
    )
    .expect("a single step is always available");
    let frame = motion.step(0, sim.body.generation(), &sim.body, &view, &Settings::default());
    assert!(!frame.held.moves(), "the guard must not walk into the chasm");
    assert!(frame.held.sneak, "it holds the edge instead");
}

// ---------------------------------------------------------------------------
// Unknown is a value, not air (step 9, findings 1 and 7)
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_cell_is_never_a_hazard_and_never_safe() {
    let unknown = Cell::Unknown;
    assert!(!unknown.is_lava(), "an unknown cell is not lava");
    assert!(!unknown.is_fire(), "an unknown cell is not fire");
    assert!(!unknown.is_water());
    assert!(!unknown.is_solid(), "an unknown column is not a floor");
    assert!(!unknown.is_passable(), "an unknown cell is not passable");
    assert!(!unknown.is_safe_to_enter());
    assert_eq!(unknown.state(), None);

    // A known cell answers from its state.
    assert!(Cell::Known(state(BlockKind::Lava)).is_lava());
    assert!(!Cell::Known(state(BlockKind::Stone)).is_passable());
    assert!(Cell::Known(state(BlockKind::Stone)).is_solid());
}

#[test]
fn a_next_cell_outside_the_loaded_chunks_is_unknown_not_lava() {
    // The nine scripted chunks cover x up to 31: the cell past x = 31 is not in
    // the world, so perception cannot answer for it and the view holds no fact.
    let mut sim = Sim::new(test_settings(), Vec3::new(31.5, 64.0, 0.5));
    for x in 20..=31 {
        for z in -2..=2 {
            sim.set_block(BlockPos::new(x, 63, z), state(BlockKind::Stone));
        }
    }
    sim.body.set_look(azalea_entity::LookDirection::new(-90.0, 0.0));
    sim.step();
    let view = sim.view();
    assert_eq!(view.next_cell, BlockPos::new(32, 64, 0), "yaw -90 faces +x");
    assert!(
        !view.next_cell_lava,
        "a cell perception could not answer must not fire the lava trigger"
    );
    assert_eq!(
        view.next_drop,
        5,
        "an unseen column is the deepest drop, not level ground"
    );

    // And no reflex motion steps into it, even as the single-step fallback.
    let settings = Settings::default();
    let unknown = view.next_cell;
    let mut blocks = praxion_reflexes::ReflexBlocks {
        perception: &mut sim.perception,
        world: &sim.world,
    };
    let motion = Motion::start(
        ReflexAction::StepBack { to: unknown },
        &view,
        &settings,
        &sim.body,
        0,
        &mut blocks,
    );
    if let Some(mut motion) = motion {
        for tick in 0..12u64 {
            let frame = motion.step(tick, sim.body.generation(), &sim.body, &view, &settings);
            frame.apply(&mut sim.body, tick).expect("the frame is for this tick");
            sim.body.step(&frame.held, &sim.world);
            assert_ne!(
                sim.body.position().to_block_pos_floor(),
                unknown,
                "an unknown cell is never entered"
            );
            if motion.ended(tick, &view, &sim.body) {
                break;
            }
        }
    }
}

#[test]
fn a_flee_path_never_enters_an_unknown_cell() {
    let mut sim = fresh();
    sim.perception.begin_tick(&sim.world);
    let view = sim.view();
    let mut blocks = TestWorld::flat(63, 12);
    let unknown: HashSet<BlockPos> = (1..=8).map(|z| BlockPos::new(0, 64, z)).collect();
    for pos in &unknown {
        blocks.set_unknown(pos.x, pos.y, pos.z);
    }
    let settings = Settings::default();
    let action = ReflexAction::Flee {
        from: Vec3::new(0.5, 64.0, -2.0),
        distance: settings.flee_distance,
    };
    let mut motion = Motion::start(action, &view, &settings, &sim.body, 0, &mut blocks)
        .expect("there is a way around the unknown cells");
    for tick in 0..200u64 {
        let frame = motion.step(tick, sim.body.generation(), &sim.body, &view, &settings);
        frame.apply(&mut sim.body, tick).expect("the frame is for this tick");
        sim.body.step(&frame.held, &sim.world);
        let feet = sim.body.position().to_block_pos_floor();
        assert!(
            !unknown.contains(&feet),
            "the flee walked into an unknown cell at {feet:?}"
        );
        if motion.ended(tick, &view, &sim.body) {
            return;
        }
    }
    panic!("the flee never ended");
}

#[test]
fn the_single_step_fallback_refuses_lava_and_unknown_cells() {
    let mut sim = fresh();
    sim.perception.begin_tick(&sim.world);
    let settings = Settings::default();
    let view = sim.view();
    let mut blocks = TestWorld::flat(63, 12);

    // Standing on a known floor is safe; the same cell with an unknown floor
    // under it is not.
    assert!(view.is_safe_standing(&mut blocks));
    blocks.set_unknown(view.feet.x, view.feet.y - 1, view.feet.z);
    assert!(
        !view.is_safe_standing(&mut blocks),
        "an unseen floor is not safe ground"
    );

    let mut blocks = TestWorld::flat(63, 12);
    let lava = BlockPos::new(0, 64, 1);
    let neighbours = [
        view.feet.north(1),
        view.feet.south(1),
        view.feet.east(1),
        view.feet.west(1),
    ];
    blocks.set_pos(lava, state(BlockKind::Lava));
    // The preferred cell is lava and every alternative is unknown: stay put.
    for pos in neighbours {
        blocks.set_unknown(pos.x, pos.y, pos.z);
    }
    assert!(
        Motion::start(
            ReflexAction::StepBack { to: lava },
            &view,
            &settings,
            &sim.body,
            0,
            &mut blocks,
        )
        .is_none(),
        "no step into lava, and no step into an unknown cell either"
    );

    // The preferred cell is lava but a neighbour is known safe: the body takes
    // that neighbour and never enters the lava.
    let mut blocks = TestWorld::flat(63, 12);
    blocks.set_pos(lava, state(BlockKind::Lava));
    let mut motion = Motion::start(
        ReflexAction::StepBack { to: lava },
        &view,
        &settings,
        &sim.body,
        0,
        &mut blocks,
    )
    .expect("a known safe alternative exists");
    for tick in 0..12u64 {
        let frame = motion.step(tick, sim.body.generation(), &sim.body, &view, &settings);
        frame.apply(&mut sim.body, tick).expect("the frame is for this tick");
        sim.body.step(&frame.held, &sim.world);
        assert_ne!(
            sim.body.position().to_block_pos_floor(),
            lava,
            "the fallback stepped into lava"
        );
        if motion.ended(tick, &view, &sim.body) {
            break;
        }
    }
    assert_ne!(
        sim.body.position().to_block_pos_floor(),
        view.feet,
        "the body moved to the known safe alternative"
    );
}

#[test]
fn the_fall_guard_refuses_a_lava_next_cell() {
    let mut sim = fresh();
    // Lava one block ahead along +z, so `next_cell_lava` is true. Capture the
    // view before any reflex turns the body away from it.
    sim.set_block(BlockPos::new(0, 64, 1), state(BlockKind::Lava));
    sim.perception.begin_tick(&sim.world);
    let view = sim.view();
    assert!(view.next_cell_lava, "the lava is in the next cell");

    let settings = Settings::default();
    let mut blocks = TestWorld::flat(63, 12);
    let mut motion =
        Motion::start(ReflexAction::Strafe { right: true }, &view, &settings, &sim.body, 0, &mut blocks)
            .expect("a strafe needs no plan");
    let frame = motion.step(0, sim.body.generation(), &sim.body, &view, &settings);
    assert!(
        !frame.held.moves(),
        "the guard must not walk toward lava in the next cell"
    );
    assert!(frame.held.sneak, "it holds instead");

    // The same strafe with no lava ahead moves normally.
    let mut sim = fresh();
    sim.step();
    let view = sim.view();
    let mut motion = Motion::start(
        ReflexAction::Strafe { right: true },
        &view,
        &settings,
        &sim.body,
        0,
        &mut blocks,
    )
    .expect("a strafe needs no plan");
    let frame = motion.step(0, sim.body.generation(), &sim.body, &view, &settings);
    assert!(frame.held.moves(), "no hazard, no hold");
}

// ---------------------------------------------------------------------------
// The own entity facts: air and burning (finding 2)
// ---------------------------------------------------------------------------

#[test]
fn the_drowning_reflex_reads_the_server_air_fact() {
    // The simulated supply is full; only the server's fact is low.
    let mut sim = fresh();
    let head = sim.feet().up(1);
    sim.set_block(head, state(BlockKind::Water));
    sim.engine.state_mut().air_ticks = 300;
    sim.set_own_air(1);
    sim.step();
    assert!(
        sim.engine.state().knows_air(),
        "the server's air supply was recorded"
    );
    assert_eq!(
        sim.engine.active(),
        Some(ReflexKind::Drowning),
        "the air fact, not the simulation, decides"
    );
}

#[test]
fn the_drowning_reflex_ignores_a_stale_simulation_when_the_fact_is_known() {
    let mut sim = fresh();
    let head = sim.feet().up(1);
    sim.set_block(head, state(BlockKind::Water));
    // A simulation that ran out, and a server that says the body has air.
    sim.engine.state_mut().air_ticks = 0;
    sim.set_own_air(300);
    for _ in 0..3 {
        sim.step();
    }
    assert_eq!(sim.engine.active(), None, "the fact wins over the simulation");
}

#[test]
fn the_drowning_reflex_falls_back_to_the_simulation_without_a_fact() {
    let mut sim = fresh();
    let head = sim.feet().up(1);
    sim.set_block(head, state(BlockKind::Water));
    sim.engine.state_mut().air_ticks = test_settings().air_surface_threshold;
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Drowning));
}

#[test]
fn the_fire_reflex_reads_the_server_burning_flag() {
    // No fire block anywhere: only the shared flags byte can trigger this.
    let mut sim = fresh();
    sim.set_own_burning(true);
    sim.step();
    assert_eq!(
        sim.engine.active(),
        Some(ReflexKind::Fire),
        "the server says the body is burning"
    );
}

#[test]
fn the_fire_reflex_does_not_read_a_cleared_flags_byte_as_burning() {
    let mut sim = fresh();
    sim.set_own_burning(true);
    sim.step();
    sim.set_own_burning(false);
    for _ in 0..3 {
        sim.step();
    }
    assert_eq!(sim.engine.active(), None, "the fire went out");
}

// ---------------------------------------------------------------------------
// The Enderman guard's pitch and the nearest Enderman (findings 3 and 5)
// ---------------------------------------------------------------------------

#[test]
fn the_enderman_break_pitch_is_upward_and_leaves_the_head() {
    use azalea_entity::LookDirection;
    use praxion_headless::Look;

    let mut sim = fresh();
    sim.add_mob(4, EntityKind::Enderman, Vec3::new(0.5, 64.0, 5.0));
    sim.perception.begin_tick(&sim.world);
    let mut view = sim.view();
    assert_eq!(view.enderman_count, 1);
    // Put the Enderman's head well above the eye, so "up" is unambiguous.
    let mut enderman = view.endermen[0].expect("one Enderman");
    enderman.position = Vec3::new(0.5, 70.0, 5.0);
    view.endermen[0] = Some(enderman);
    let head = praxion_reflexes::head_of(enderman.position);
    let (head_yaw, head_pitch) = praxion_planner::aim(view.eye, head);
    assert!(head_pitch < -20.0, "a head above the eye is a negative pitch");

    let guard = EndermanGuard::new();
    let current = LookDirection::new(head_yaw, head_pitch);
    let out = guard.apply(Look::Aim { yaw: head_yaw, pitch: head_pitch }, current, &view);
    let Look::Aim { yaw: out_yaw, pitch: out_pitch } = out else {
        panic!("the guard breaks a look that rests on the head");
    };
    assert!(
        out_pitch <= head_pitch - 19.0,
        "the break pitch moves the camera up: {out_pitch} vs {head_pitch}"
    );
    assert!(
        praxion_planner::wrap_degrees(out_yaw - head_yaw).abs() > HEAD_TOLERANCE_DEG,
        "the break also leaves the head sideways"
    );
}

#[test]
fn the_enderman_head_is_the_nearest_one() {
    let mut sim = fresh();
    // The far one is added first, so storage order is not distance order.
    sim.add_mob(5, EntityKind::Enderman, Vec3::new(0.5, 64.0, 20.0));
    sim.add_mob(6, EntityKind::Enderman, Vec3::new(0.5, 64.0, 4.0));
    sim.step();
    let view = sim.view();
    assert!(view.enderman_count >= 2);
    let head = view.enderman_head().expect("Endermen");
    assert_eq!(
        head,
        Vec3::new(0.5, 64.0 + 1.5, 4.0),
        "the nearest head, not the first one seen"
    );
}

// ---------------------------------------------------------------------------
// The arrow strafe side (finding 8)
// ---------------------------------------------------------------------------

/// How far the body moves sideways while it dodges an arrow that flies down
/// `-z` from `x`.
fn strafe_offset(arrow_x: f64) -> f64 {
    let mut sim = fresh();
    let start_x = sim.body.position().x;
    let mut arrow_z = 8.0;
    sim.add_mob(3, EntityKind::Arrow, Vec3::new(arrow_x, 65.0, arrow_z));
    let mut moved = 0.0;
    for _ in 0..24 {
        sim.step();
        arrow_z -= 0.8;
        sim.add_mob(3, EntityKind::Arrow, Vec3::new(arrow_x, 65.0, arrow_z));
        moved = sim.body.position().x - start_x;
        if moved.abs() > 0.3 {
            break;
        }
    }
    moved
}

#[test]
fn the_arrow_strafe_side_comes_from_the_flight_line() {
    // The body faces +z (yaw 0), so its right hand is -x and its left +x.
    // An arrow one block to the right flies past on the right: step left.
    let right_side = strafe_offset(-1.0);
    assert!(
        right_side > 0.1,
        "an arrow passing on the right is dodged to the left, got {right_side}"
    );

    // An arrow on the left: step right.
    let left_side = strafe_offset(2.0);
    assert!(
        left_side < -0.1,
        "an arrow passing on the left is dodged to the right, got {left_side}"
    );
}
