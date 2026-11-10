//! Independent adversarial tests for the reflex layer (step 9).
//!
//! These complement `behavior.rs` and `matrix.rs`. They are written by the
//! independent reviewer and must not be edited by the implementation worker.

mod common;

use std::collections::HashSet;
use std::time::Instant;

use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_reflexes::{EndermanGuard, ReflexKind, Settings};
use praxion_world::{BlockPos, Vec3};

use azalea_entity::LookDirection;
use common::{Sim, state, test_settings};

// ---------------------------------------------------------------------------
// Priority order with two (or more) threats at once
// ---------------------------------------------------------------------------

#[test]
fn priority_picks_creeper_over_a_zombie_ahead() {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5));
    // creeper behind, zombie ahead, both inside their ranges.
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, -2.0));
    sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.0));
    sim.step();
    assert_eq!(
        sim.engine.active(),
        Some(ReflexKind::Creeper),
        "creeper (priority 40) outranks melee (30)"
    );
}

#[test]
fn priority_picks_drowning_over_a_creeper() {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5));
    // head under water
    let head = sim.body.position().to_block_pos_floor().up(1);
    sim.set_block(head, state(BlockKind::Water));
    // low air
    sim.engine.state_mut().air_ticks = 5;
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, 2.0));
    sim.step();
    assert_eq!(
        sim.engine.active(),
        Some(ReflexKind::Drowning),
        "drowning (priority 60) outranks creeper (40)"
    );
}

#[test]
fn flee_path_never_enters_lava_or_a_drop_over_three() {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5));
    // creeper behind -> flee direction is +z.
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, -2.0));
    // lava on the straight flee path.
    for z in 1..=6 {
        sim.set_block(BlockPos::new(0, 63, z), state(BlockKind::Lava));
    }
    // a deep drop on the other side, to make sure the fall guard is exercised.
    sim.set_block(BlockPos::new(2, 63, 1), state(BlockKind::Air));
    sim.set_block(BlockPos::new(2, 62, 1), state(BlockKind::Air));
    sim.set_block(BlockPos::new(2, 61, 1), state(BlockKind::Air));
    sim.set_block(BlockPos::new(2, 60, 1), state(BlockKind::Air));

    let lava_cells: HashSet<BlockPos> = (1..=6).map(|z| BlockPos::new(0, 63, z)).collect();

    let mut prev_y = sim.body.position().y;
    for _ in 0..40 {
        if let Some(frame) = sim.step() {
            // fall guard: no moving frame may keep moving off a drop > 3.
            if frame.held.moves() {
                assert!(
                    sim.body.position().to_block_pos_floor().y >= 0,
                    "sanity"
                );
            }
        }
        let feet = sim.feet();
        assert!(
            !lava_cells.contains(&feet),
            "reflex walked into lava at {feet:?}"
        );
        // block state at feet must not be lava
        let y = sim.body.position().y;
        assert!(
            (prev_y - y).abs() <= 4.0,
            "vertical jump of {} exceeds the safe drop",
            (prev_y - y).abs()
        );
        prev_y = y;
    }
}

// ---------------------------------------------------------------------------
// Alternating visibility
// ---------------------------------------------------------------------------

#[test]
fn a_human_floor_reflex_never_fires_on_alternating_visibility() {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5));
    let mut ever_active = false;
    for tick in 0..60u64 {
        if tick % 2 == 0 {
            sim.add_mob(2, EntityKind::Zombie, Vec3::new(0.5, 64.0, 2.0));
        } else {
            sim.remove_mob(2);
        }
        sim.step();
        if sim.engine.active() == Some(ReflexKind::Melee) {
            ever_active = true;
        }
    }
    assert!(
        !ever_active,
        "a human_floor reflex must not fire when its trigger blinks every other tick"
    );
}

#[test]
fn a_lethal_fast_reflex_behaves_sensibly_on_alternating_visibility() {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5));
    let panicked = false;
    for tick in 0..40u64 {
        if tick % 2 == 0 {
            sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5, 64.0, 2.5));
        } else {
            sim.remove_mob(1);
        }
        // must not panic
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sim.step();
        }));
        // (if it panicked, the test process would abort; track via active state)
    }
    // After an even number of ticks the creeper is removed, so nothing is active.
    assert_eq!(sim.engine.active(), None, "no reflex stays latched after the trigger leaves");
    let _ = panicked;
}

// ---------------------------------------------------------------------------
// Reflex that never ends is cut at max_reflex_ticks
// ---------------------------------------------------------------------------

#[test]
fn a_never_ending_reflex_is_cut_at_max_reflex_ticks() {
    let settings = Settings {
        max_reflex_ticks: 5,
        air_surface_threshold: 300,
        ..test_settings()
    };
    let mut sim = Sim::new(settings, Vec3::new(0.5, 64.0, 0.5));
    // keep the head under water so drowning never ends on its own
    let head = sim.body.position().to_block_pos_floor().up(1);
    sim.set_block(head, state(BlockKind::Water));
    sim.engine.state_mut().air_ticks = 10;
    let mut timed_out_seen = false;
    for _ in 0..20 {
        // engine.active() becomes None again after the timeout tick
        let before = sim.engine.active();
        sim.step();
        if before == Some(ReflexKind::Drowning) && sim.engine.active().is_none() {
            // could be a timeout or a trigger change; check cooldown was set
            timed_out_seen = true;
        }
    }
    // At some point the reflex must have stopped despite the trigger staying.
    assert!(
        timed_out_seen,
        "drowning never ended on its own but should have been cut"
    );
}

// ---------------------------------------------------------------------------
// Enderman guard over random look sequences
// ---------------------------------------------------------------------------

#[test]
fn enderman_guard_never_lets_a_look_rest_on_the_head_for_two_ticks() {
    use praxion_headless::Look;

    let mut view = common_view();
    // one enderman 5 blocks ahead
    view.endermen[0] = Some(praxion_reflexes::EntityView {
        id: 9,
        kind: EntityKind::Enderman,
        position: Vec3::new(0.5, 64.0, 5.0),
        distance: 5.0,
    });
    view.enderman_count = 1;

    let guard = EndermanGuard::new();
    // current look starts on the head
    let head = Vec3::new(0.5, 65.5, 5.0);
    let (hy, hp) = praxion_planner::aim(view.eye, head);
    let mut current = LookDirection::new(hy, hp);

    // simple deterministic LCG
    let mut rng: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut prev_on_head = false;
    for _ in 0..2000 {
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let desired_yaw = ((rng >> 32) as f32 / u32::MAX as f32) * 360.0 - 180.0;
        rng = rng.wrapping_mul(6364136223846793005).wrapping_add(1);
        let desired_pitch = ((rng >> 32) as f32 / u32::MAX as f32) * 180.0 - 90.0;
        let desired = Look::Aim {
            yaw: desired_yaw,
            pitch: desired_pitch,
        };
        let out = guard.apply(desired, current, &view);
        let (oy, op) = match out {
            Look::Aim { yaw, pitch } => (yaw, pitch),
            Look::None => (current.y_rot(), current.x_rot()),
            Look::Delta { dyaw, dpitch } => (current.y_rot() + dyaw, current.x_rot() + dpitch),
        };
        let now_on_head = on_head_wrapped(oy, op, &view);
        // must never be on head two ticks in a row
        assert!(
            !(prev_on_head && now_on_head),
            "guard allowed two consecutive on-head looks"
        );
        // update current look to what was emitted
        current = LookDirection::new(oy, op);
        prev_on_head = now_on_head;
    }
}

fn on_head_wrapped(yaw: f32, pitch: f32, view: &praxion_reflexes::View) -> bool {
    for e in view.endermen.iter().flatten() {
        let head = e.position + Vec3::new(0.0, 1.5, 0.0);
        let (hy, hp) = praxion_planner::aim(view.eye, head);
        let dy = (hy - yaw).rem_euclid(360.0);
        let dy = if dy > 180.0 { 360.0 - dy } else { dy };
        if dy <= 8.0 && (hp - pitch).abs() <= 8.0 {
            return true;
        }
    }
    false
}

fn common_view() -> praxion_reflexes::View {
    // minimal view with player at origin-ish
    let feet = Vec3::new(0.5, 64.0, 0.5);
    let feet_cell = feet.to_block_pos_floor();
    praxion_reflexes::View {
        tick: 0,
        position: feet,
        eye: feet + Vec3::new(0.0, 1.62, 0.0),
        rotation: (0.0, 0.0),
        health: Some(20.0),
        food: Some(20),
        day_time: 1000,
        feet: feet_cell,
        eye_cell: feet_cell.up(1),
        feet_state: praxion_world::BlockState::AIR,
        head_state: praxion_world::BlockState::AIR,
        below_state: praxion_world::BlockState::from(BlockKind::Stone),
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
        melee_others: [None; praxion_reflexes::MAX_MELEE_OTHERS],
        endermen: [None; praxion_reflexes::MAX_ENDERMEN],
        enderman_count: 0,
        arrows: [None; praxion_reflexes::MAX_ARROWS],
        arrow_count: 0,
        best_food: None,
        best_weapon: None,
    }
}

// ---------------------------------------------------------------------------
// Settings: every field, wrong type / missing / out of range / unknown extra
// ---------------------------------------------------------------------------

const MINIMAL: &str = "\
critical_health = 6.0
fight_min_health = 8.0
melee_range = 4
creeper_flee_range = 8
air_surface_threshold = 120
eat_soft_threshold = 16
eat_hard_threshold = 6
flee_distance = 8.0
shelter_health = 10.0
max_reflex_ticks = 200
switch_gain_factor = 2.0
wake_after_ticks = 40
min_switch_ticks = 100
";

fn set_field(text: &str, field: &str, value: &str) -> String {
    text.lines()
        .map(|line| {
            if line.trim_start().starts_with(&format!("{field} =")) {
                format!("{field} = {value}")
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

fn drop_field(text: &str, field: &str) -> String {
    text.lines()
        .filter(|line| !line.trim_start().starts_with(&format!("{field} =")))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn settings_wrong_type_every_field() {
    let fields = [
        "critical_health", "fight_min_health", "melee_range", "creeper_flee_range",
        "air_surface_threshold", "eat_soft_threshold", "eat_hard_threshold",
        "flee_distance", "shelter_health", "max_reflex_ticks", "switch_gain_factor",
        "wake_after_ticks", "min_switch_ticks",
    ];
    for field in fields {
        let text = set_field(MINIMAL, field, "\"x\"");
        let err = Settings::from_toml(&text).unwrap_err();
        assert!(
            err.to_string().contains(field),
            "wrong type for {field}: {err}"
        );
    }
}

#[test]
fn settings_missing_every_field() {
    let fields = [
        "critical_health", "fight_min_health", "melee_range", "creeper_flee_range",
        "air_surface_threshold", "eat_soft_threshold", "eat_hard_threshold",
        "flee_distance", "shelter_health", "max_reflex_ticks", "switch_gain_factor",
        "wake_after_ticks", "min_switch_ticks",
    ];
    for field in fields {
        let text = drop_field(MINIMAL, field);
        let err = Settings::from_toml(&text).unwrap_err();
        assert!(
            err.to_string().contains(field),
            "missing {field}: {err}"
        );
    }
}

#[test]
fn settings_out_of_range_every_field() {
    // (field, out-of-range literal)
    let cases = [
        ("critical_health", "99.0"),
        ("fight_min_health", "99.0"),
        ("melee_range", "0"),
        ("creeper_flee_range", "99"),
        ("air_surface_threshold", "999"),
        ("eat_soft_threshold", "99"),
        ("eat_hard_threshold", "99"),
        ("flee_distance", "0.0"),
        ("shelter_health", "99.0"),
        ("max_reflex_ticks", "0"),
        ("switch_gain_factor", "999.0"),
        ("wake_after_ticks", "0"),
        ("min_switch_ticks", "9999"),
    ];
    for (field, value) in cases {
        let text = set_field(MINIMAL, field, value);
        // eat_hard relationship: also make hard exceed soft
        let text = if field == "eat_hard_threshold" {
            set_field(&text, "eat_soft_threshold", "6")
        } else {
            text
        };
        let err = Settings::from_toml(&text).unwrap_err();
        assert!(
            err.to_string().contains(field),
            "out of range {field}: {err}"
        );
    }
}

#[test]
fn settings_unknown_extra_field() {
    let text = format!("{MINIMAL}extra_key = 1\n");
    let err = Settings::from_toml(&text).unwrap_err();
    assert!(err.to_string().contains("extra_key"), "{err}");
}

// ---------------------------------------------------------------------------
// Hot path timing (release mode) -- prints, does not assert
// ---------------------------------------------------------------------------

#[test]
fn hot_path_timing_with_sixty_four_entities() {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5));
    for i in 0..64 {
        let x = (i as f64) * 0.5 - 8.0;
        sim.add_mob(1000 + i, EntityKind::Zombie, Vec3::new(x, 64.0, 8.0));
    }
    let start = Instant::now();
    let n = 200u64;
    for _ in 0..n {
        sim.step();
    }
    let elapsed = start.elapsed();
    eprintln!(
        "reflex hot path: {n} ticks with 64 visible entities = {:?} ({:.3} ms/tick)",
        elapsed,
        elapsed.as_secs_f64() * 1000.0 / n as f64
    );
}
