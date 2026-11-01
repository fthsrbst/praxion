//! The reflex coverage matrix: nine reflexes x (near/far) x (day/night) x
//! (open/cramped) x ten seeded instances.
//!
//! Worlds are hand-built through the same packet path the body uses, the
//! player is a real `praxion_headless::physics::Body`, and mobs are scripted.
//! Each cell has a pass predicate; the test requires 10/10 in every cell and
//! prints the table.

mod common;

use azalea_entity::LookDirection;
use azalea_registry::builtin::{BlockKind, EntityKind, ItemKind};
use praxion_headless::Click;
use praxion_planner::{aim, wrap_degrees};
use praxion_reflexes::{
    EndermanGuard, HEAD_TOLERANCE_DEG, ReflexAction, Settings, View,
};
use praxion_world::{BlockPos, Vec3};

use common::{
    Sim, centre, set_block, state, test_settings,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Distance {
    Near,
    Far,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Light {
    Day,
    Night,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Terrain {
    Open,
    Cramped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatrixReflex {
    Creeper,
    Melee,
    Arrow,
    Lava,
    Drowning,
    Fire,
    Starvation,
    Enderman,
    Fall,
}

const REFLEXES: [MatrixReflex; 9] = [
    MatrixReflex::Creeper,
    MatrixReflex::Melee,
    MatrixReflex::Arrow,
    MatrixReflex::Lava,
    MatrixReflex::Drowning,
    MatrixReflex::Fire,
    MatrixReflex::Starvation,
    MatrixReflex::Enderman,
    MatrixReflex::Fall,
];

const DISTANCES: [Distance; 2] = [Distance::Near, Distance::Far];
const LIGHTS: [Light; 2] = [Light::Day, Light::Night];
const TERRAINS: [Terrain; 2] = [Terrain::Open, Terrain::Cramped];

fn jitter(seed: u64) -> f64 {
    ((seed as f64) * 0.37).sin() * 0.25
}

/// A scenario's world: nine chunks, a flat floor, then the terrain variant.
fn sim_for(settings: Settings, terrain: Terrain, light: Light) -> Sim {
    let mut sim = Sim::new(settings, Vec3::new(0.5, 64.0, 0.5));
    sim.set_time(match light {
        Light::Day => 1_000,
        Light::Night => 18_000,
    });
    if terrain == Terrain::Cramped {
        // Three blocks wide, two high, running along z. No ceiling: the
        // drowning cells rise out of the water column.
        for z in -12..=12 {
            for y in 64..=65 {
                set_block(&mut sim.world, BlockPos::new(-2, y, z), state(BlockKind::Stone));
                set_block(&mut sim.world, BlockPos::new(2, y, z), state(BlockKind::Stone));
            }
        }
    }
    sim
}

fn cell(kind: MatrixReflex, distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    match kind {
        MatrixReflex::Creeper => creeper(distance, light, terrain, seed),
        MatrixReflex::Melee => melee(distance, light, terrain, seed),
        MatrixReflex::Arrow => arrow(distance, light, terrain, seed),
        MatrixReflex::Lava => lava(distance, light, terrain, seed),
        MatrixReflex::Drowning => drowning(distance, light, terrain, seed),
        MatrixReflex::Fire => fire(distance, light, terrain, seed),
        MatrixReflex::Starvation => starvation(distance, light, terrain, seed),
        MatrixReflex::Enderman => enderman(distance, light, terrain, seed),
        MatrixReflex::Fall => fall(distance, light, terrain, seed),
    }
}

/// Creeper: after the reflex, the creeper is out of five blocks.
fn creeper(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    let z = match distance {
        Distance::Near => -3.0,
        Distance::Far => -7.0,
    };
    let creeper = Vec3::new(0.5 + jitter(seed), 64.0, z);
    sim.add_mob(1, EntityKind::Creeper, creeper);
    sim.run_until(90, |sim| sim.distance_to(creeper) > 5.0)
}

/// Melee: health above `fight_min_health`, so the body attacks the mob.
fn melee(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    let z = match distance {
        Distance::Near => 1.5,
        Distance::Far => 3.0,
    };
    let mob = Vec3::new(0.5 + jitter(seed), 64.0, z);
    sim.add_mob(2, EntityKind::Zombie, mob);
    sim.run_until(40, |sim| {
        sim.frames
            .iter()
            .any(|frame| frame.clicks.contains(&Click::Attack))
    })
}

/// Arrow: the body side-steps while the arrow heads at it.
fn arrow(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    let z = match distance {
        Distance::Near => 8.0,
        Distance::Far => 14.0,
    };
    let mut arrow_z = z;
    let x = 0.5 + jitter(seed);
    sim.add_mob(3, EntityKind::Arrow, Vec3::new(x, 65.0, arrow_z));
    let start_x = sim.body.position().x;
    let mut passed = false;
    for _ in 0..24 {
        sim.step();
        arrow_z -= 0.8;
        sim.add_mob(3, EntityKind::Arrow, Vec3::new(x, 65.0, arrow_z));
        if (sim.body.position().x - start_x).abs() > 0.5 {
            passed = true;
            break;
        }
    }
    passed
}

/// Lava: step back to a safe cell, never stand next to it.
fn lava(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    let lava = match distance {
        Distance::Near => BlockPos::new(0, 64, 1),
        Distance::Far => BlockPos::new(0, 64, -1),
    };
    sim.set_block(lava, state(BlockKind::Lava));
    let lava_centre = centre(lava);
    let _ = seed;
    sim.run_until(30, |sim| {
        let dx = sim.body.position().x - lava_centre.x;
        let dz = sim.body.position().z - lava_centre.z;
        sim.feet() != lava && (dx * dx + dz * dz).sqrt() > 1.0
    })
}

/// Drowning: swim up until the head is out of the water.
fn drowning(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    let depth = match distance {
        Distance::Near => 3,
        Distance::Far => 4,
    };
    let _ = seed;
    for y in 64..64 + depth {
        set_block(&mut sim.world, BlockPos::new(0, y, 0), state(BlockKind::Water));
    }
    // One tick short of the threshold, so the reflex sees it immediately.
    sim.engine.state_mut().air_ticks = test_settings().air_surface_threshold;
    sim.run_until(50, |sim| {
        let eye_cell = common::eye(sim.body.position()).to_block_pos_floor();
        sim.world
            .block_state(eye_cell)
            .is_some_and(|state| BlockKind::from(state) != BlockKind::Water)
    })
}

/// Fire: move to water if there is some within eight blocks.
fn fire(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    let fire_cell = BlockPos::new(0, 64, 0);
    sim.set_block(fire_cell, state(BlockKind::Fire));
    let water_z = match distance {
        Distance::Near => 3,
        Distance::Far => 7,
    };
    let water = BlockPos::new(0, 64, water_z);
    sim.set_block(water, state(BlockKind::Water));
    let _ = seed;
    sim.run_until(60, |sim| sim.feet() != fire_cell)
}

/// Starvation: the hard threshold eats the best food.
fn starvation(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    let _ = (distance, seed);
    sim.set_health(20.0, test_settings().eat_hard_threshold);
    sim.add_item(9, ItemKind::Bread, 3);
    let mut saw_eat = false;
    for _ in 0..16 {
        sim.step();
        if matches!(sim.engine.active_action(), Some(ReflexAction::Eat { .. })) {
            saw_eat = true;
        }
    }
    saw_eat
        && sim
            .frames
            .iter()
            .any(|frame| frame.clicks.contains(&Click::Use))
}

/// Enderman gaze: a look aimed at the head never rests there for two ticks.
fn enderman(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    let z = match distance {
        Distance::Near => 3.0,
        Distance::Far => 20.0,
    };
    sim.add_mob(4, EntityKind::Enderman, Vec3::new(0.5 + jitter(seed), 64.0, z));
    // Start looking away, so the first aim at the head is allowed.
    sim.body.set_look(LookDirection::new(0.0, -30.0));
    let guard = EndermanGuard::new();
    let mut run = 0;
    let mut longest = 0;
    for _ in 0..40 {
        sim.perception.begin_tick(&sim.world);
        let view = View::capture(&mut sim.perception, &sim.world, sim.engine.state_mut());
        let Some(head) = view.enderman_head() else {
            return false;
        };
        let (yaw, pitch) = aim(view.eye, head);
        let desired = praxion_headless::Look::Aim { yaw, pitch };
        let out = guard.apply(desired, sim.body.look(), &view);
        match out {
            praxion_headless::Look::Aim { yaw, pitch } => {
                sim.body.set_look(LookDirection::new(yaw, pitch));
            }
            praxion_headless::Look::Delta { dyaw, dpitch } => {
                let current = sim.body.look();
                sim.body.set_look(LookDirection::new(
                    current.y_rot() + dyaw,
                    current.x_rot() + dpitch,
                ));
            }
            praxion_headless::Look::None => {}
        }
        let current = sim.body.look();
        let on_head = wrap_degrees(yaw - current.y_rot()).abs() <= HEAD_TOLERANCE_DEG
            && (pitch - current.x_rot()).abs() <= HEAD_TOLERANCE_DEG;
        if on_head {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
        sim.tick += 1;
    }
    longest <= 1
}

/// Fall guard: a fleeing reflex never walks off a drop deeper than three.
fn fall(distance: Distance, light: Light, terrain: Terrain, seed: u64) -> bool {
    let mut sim = sim_for(test_settings(), terrain, light);
    // A chasm from z = 3 outward: remove the floor.
    for x in -12..=12 {
        for z in 3..=12 {
            set_block(&mut sim.world, BlockPos::new(x, 63, z), state(BlockKind::Air));
        }
    }
    let z = match distance {
        Distance::Near => -3.0,
        Distance::Far => -7.0,
    };
    sim.add_mob(1, EntityKind::Creeper, Vec3::new(0.5 + jitter(seed), 64.0, z));
    let start_y = sim.body.position().y;
    let mut lowest = start_y;
    for _ in 0..60 {
        sim.step();
        lowest = lowest.min(sim.body.position().y);
        if start_y - lowest > 3.0 {
            return false;
        }
    }
    start_y - lowest <= 3.0
}

#[test]
fn the_coverage_matrix_is_full() {
    let mut failures = Vec::new();
    let mut table = String::new();
    table.push_str("reflex      d/l/t      pass\n");

    for kind in REFLEXES {
        let mut rates = Vec::new();
        for distance in DISTANCES {
            for light in LIGHTS {
                for terrain in TERRAINS {
                    let mut passed = 0;
                    for seed in 0..10 {
                        if cell(kind, distance, light, terrain, seed) {
                            passed += 1;
                        } else {
                            failures.push(format!(
                                "{kind:?} {distance:?}/{light:?}/{terrain:?} seed {seed}"
                            ));
                        }
                    }
                    let label = format!(
                        "{}/{}/{}",
                        if distance == Distance::Near { "near" } else { "far" },
                        if light == Light::Day { "day" } else { "night" },
                        if terrain == Terrain::Open { "open" } else { "cramped" }
                    );
                    rates.push((label, passed));
                }
            }
        }
        table.push_str(&format!("{:<11}", format!("{kind:?}")));
        for (label, passed) in &rates {
            table.push_str(&format!(" {label:<16} {passed:>2}/10 |"));
        }
        table.push('\n');
    }
    println!("{table}");
    assert!(failures.is_empty(), "failed cells: {failures:#?}");
}

#[test]
fn the_priority_order_is_descending() {
    let priorities: Vec<u8> = praxion_reflexes::ReflexKind::ALL
        .iter()
        .map(|kind| kind.priority())
        .collect();
    assert!(
        priorities.windows(2).all(|pair| pair[0] > pair[1]),
        "{priorities:?}"
    );
}
