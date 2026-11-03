//! Physics tests against hand-built worlds.
//!
//! The first group ports azalea's own physics and movement tests
//! (`azalea-physics/tests/physics.rs`, `azalea-client/tests/simulation/
//! correct_movement.rs`, `.../correct_sneak_movement.rs`,
//! `.../correct_sprint_sneak_movement.rs`) to the lifted step, so their exact
//! vanilla-tick numbers hold here too. The rest check the behaviours Demo-0
//! needs: a 10-block fall, a 1-block step, a wall, a ledge while sneaking and
//! swimming up.

use std::collections::HashMap;

use azalea_block::{BlockState, properties::WaterLevel};
use azalea_core::position::{BlockPos, Vec3};

use super::{Blocks, Body};
use crate::frame::Held;

/// A world made of the blocks a test names; everything else is air.
struct TestWorld {
    blocks: HashMap<BlockPos, BlockState>,
}

impl TestWorld {
    fn new() -> Self {
        Self {
            blocks: HashMap::new(),
        }
    }

    fn set(&mut self, x: i32, y: i32, z: i32, block: BlockState) {
        self.blocks.insert(BlockPos::new(x, y, z), block);
    }

    /// Stone at `y` for every x and z within `radius`.
    fn flat(y: i32, radius: i32) -> Self {
        let mut world = Self::new();
        for x in -radius..=radius {
            for z in -radius..=radius {
                world.set(x, y, z, stone());
            }
        }
        world
    }

    /// Stone at `y` only for z <= `z_max`.
    fn ledge(y: i32, z_max: i32, radius: i32) -> Self {
        let mut world = Self::new();
        for x in -radius..=radius {
            for z in -radius..=z_max {
                world.set(x, y, z, stone());
            }
        }
        world
    }
}

impl Blocks for TestWorld {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        self.blocks.get(&pos).copied()
    }
}

fn stone() -> BlockState {
    BlockState::from(azalea_block::blocks::Stone)
}

fn water() -> BlockState {
    // Level 0 is a source block (amount 8, a full block of water).
    BlockState::from(azalea_block::blocks::Water {
        level: WaterLevel::from(0),
    })
}

fn body_at(x: f64, y: f64, z: f64) -> Body {
    Body::new(Vec3::new(x, y, z))
}

/// Step with no controls for as long as it takes to settle on the ground.
fn settle(body: &mut Body, world: &TestWorld, ticks: usize) {
    for _ in 0..ticks {
        body.step(&Held::NONE, world);
    }
}

fn assert_close(actual: f64, expected: f64, tolerance: f64, what: &str) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "{what}: expected {expected}, got {actual} (difference {})",
        actual - expected
    );
}

// ---------------------------------------------------------------------------
// Ported from azalea
// ---------------------------------------------------------------------------

/// `azalea-physics/tests/physics.rs::test_gravity`: the first tick only sets the
/// velocity, the second tick moves.
#[test]
fn gravity_sets_velocity_before_it_moves_the_body() {
    let world = TestWorld::new();
    let mut body = body_at(0.0, 70.0, 0.0);

    body.step(&Held::NONE, &world);
    assert_eq!(body.position().y, 70.0);
    assert!(body.physics.velocity.y < 0.0);

    body.step(&Held::NONE, &world);
    assert!(body.position().y < 70.0);
}

/// `azalea-client/tests/simulation/correct_movement.rs`: one tick of walking,
/// byte for byte the position azalea sends.
#[test]
fn ported_azalea_walk_one_tick() {
    let mut world = TestWorld::new();
    world.set(31, 63, 370, stone());
    let mut body = body_at(31.5, 64.0, 370.5);
    settle(&mut body, &world, 2);

    body.step(
        &Held {
            forward: true,
            ..Held::NONE
        },
        &world,
    );

    assert_close(
        body.position().z,
        370.59800000336764,
        1e-9,
        "one tick of walking",
    );
    assert_eq!(body.position().x, 31.5);
    assert_eq!(body.position().y, 64.0);
    assert!(body.on_ground());
}

/// `azalea-client/tests/simulation/correct_sneak_movement.rs`.
#[test]
fn ported_azalea_sneak_forward() {
    let mut world = TestWorld::new();
    world.set(0, 119, 0, stone());
    world.set(0, 119, 1, stone());
    let mut body = body_at(0.5, 120.0, 0.5);
    settle(&mut body, &world, 3);
    // Crouching follows the previous tick's shift key, as in azalea: hold shift
    // for one tick before walking, like the simulation test does.
    body.step(
        &Held {
            sneak: true,
            ..Held::NONE
        },
        &world,
    );

    let held = Held {
        forward: true,
        sneak: true,
        ..Held::NONE
    };
    let expected = [0.5294000033944846, 0.5748524105068866, 0.6290694310673044];
    for (tick, expected) in expected.into_iter().enumerate() {
        body.step(&held, &world);
        assert_close(
            body.position().z,
            expected,
            1e-9,
            &format!("sneak tick {}", tick + 1),
        );
    }
}

/// `azalea-client/tests/simulation/correct_sprint_sneak_movement.rs`.
///
/// The sneak values rely on azalea's one-tick crouching lag: the pose and the
/// sneaking speed follow the *previous* tick's shift key.
#[test]
fn ported_azalea_sprint_then_sneak() {
    let mut world = TestWorld::new();
    world.set(0, 119, 0, stone());
    world.set(0, 119, 1, stone());
    let mut body = body_at(0.5, 120.0, 0.5);
    settle(&mut body, &world, 3);

    let mut held = Held {
        forward: true,
        sprint: true,
        ..Held::NONE
    };
    body.step(&held, &world);
    assert_close(body.position().z, 0.6274000124096872, 1e-9, "sprint tick 1");
    body.step(&held, &world);
    assert_close(body.position().z, 0.8243604396746886, 1e-9, "sprint tick 2");

    held.sneak = true;
    let expected = [
        1.0593008578621674,
        1.2257983479146455,
        1.3549259948648078,
    ];
    for (tick, expected) in expected.into_iter().enumerate() {
        body.step(&held, &world);
        assert_close(
            body.position().z,
            expected,
            1e-9,
            &format!("sprint and sneak tick {}", tick + 1),
        );
    }
}

// ---------------------------------------------------------------------------
// The behaviours Demo-0 needs
// ---------------------------------------------------------------------------

/// A body dropped 10 blocks above a stone floor lands exactly on it, in the
/// vanilla number of ticks (18: the first tick only builds velocity).
#[test]
fn falling_ten_blocks_lands_in_eighteen_ticks() {
    let world = TestWorld::flat(-1, 2);
    let mut body = body_at(0.5, 10.0, 0.5);

    let mut ticks = 0;
    while !body.on_ground() && ticks < 40 {
        body.step(&Held::NONE, &world);
        ticks += 1;
    }

    assert_eq!(ticks, 18, "ticks to land from 10 blocks up");
    assert_eq!(body.position().y, 0.0);
    assert!(body.on_ground());
}

/// Walking 20 ticks from rest covers the vanilla distance.
///
/// The formula is vanilla's: each tick the input adds `0.1 * 0.98` (speed times
/// the input multiplier) to the velocity, the body moves by it, then the
/// velocity is multiplied by the ground inertia `0.6 * 0.91`.
#[test]
fn walking_twenty_ticks_covers_the_vanilla_distance() {
    let world = TestWorld::flat(-1, 4);
    let mut body = body_at(0.5, 0.0, 0.5);
    settle(&mut body, &world, 2);

    let held = Held {
        forward: true,
        ..Held::NONE
    };
    for _ in 0..20 {
        body.step(&held, &world);
    }

    // Closed form, computed with vanilla's constants and f32 input maths.
    let mut velocity = 0.0f64;
    let mut distance = 0.0f64;
    let acceleration = 0.09800000336766246f64; // f32(0.1f32 * 0.98f32) as f64
    let inertia = 0.546000063419342f64; // f32(0.6f32 * 0.91f32) as f64
    for _ in 0..20 {
        velocity += acceleration;
        distance += velocity;
        velocity *= inertia;
    }

    assert_close(
        body.position().z - 0.5,
        distance,
        1e-9,
        "20 ticks of walking",
    );
    assert!(distance > 4.0 && distance < 4.1, "vanilla distance {distance}");
    assert_eq!(body.position().y, 0.0);
}

/// A 1-block step needs a jump (the 0.6 step height cannot climb it), and after
/// jumping the body stands on top.
#[test]
fn jumping_onto_a_one_block_step() {
    let mut world = TestWorld::flat(-1, 4);
    // The step: one block high, from z = 2 onwards.
    for x in -1..=1 {
        for z in 2..=4 {
            world.set(x, 0, z, stone());
        }
    }
    let mut body = body_at(0.5, 0.0, 0.5);
    settle(&mut body, &world, 2);

    let jump = Held {
        forward: true,
        jump: true,
        ..Held::NONE
    };
    for _ in 0..20 {
        body.step(&jump, &world);
        assert!(body.position().y >= 0.0);
    }
    let walk = Held {
        forward: true,
        ..Held::NONE
    };
    for _ in 0..10 {
        body.step(&walk, &world);
    }

    assert_eq!(body.position().y, 1.0, "standing on the step");
    assert!(body.position().z > 2.0, "past the step edge");
    assert!(body.on_ground());
}

/// A two-block wall stops the body: it never passes the wall's face.
#[test]
fn cannot_walk_through_a_wall() {
    let mut world = TestWorld::flat(-1, 4);
    for x in -1..=1 {
        world.set(x, 0, 2, stone());
        world.set(x, 1, 2, stone());
    }
    let mut body = body_at(0.5, 0.0, 0.5);
    settle(&mut body, &world, 2);

    let held = Held {
        forward: true,
        ..Held::NONE
    };
    for _ in 0..40 {
        body.step(&held, &world);
    }

    // Half a player is 0.3 wide, so the feet can reach z = 1.7 at most.
    assert_close(body.position().z, 1.7, 1e-6, "blocked by the wall");
    assert_eq!(body.position().y, 0.0);
    assert!(body.horizontal_collision());
}

/// Sneaking stops at the edge of a ledge instead of walking off it.
///
/// Vanilla lets the footprint lean over the void: the guard stops the body once
/// no part of its box (half a block wide) is over the block any more, which is
/// `z = 2.3` for an edge at `z = 2`.
#[test]
fn sneaking_does_not_fall_off_an_edge() {
    let world = TestWorld::ledge(-1, 1, 4);
    let mut body = body_at(0.5, 0.0, 0.5);
    settle(&mut body, &world, 2);

    let held = Held {
        forward: true,
        sneak: true,
        ..Held::NONE
    };
    for _ in 0..60 {
        body.step(&held, &world);
        assert_eq!(body.position().y, 0.0, "never left the ledge");
    }

    assert!(
        body.position().z > 1.9 && body.position().z <= 2.3 + 1e-9,
        "stopped at the edge, got z = {}",
        body.position().z
    );
    assert!(body.on_ground());
}

/// Holding jump in deep water swims upwards.
#[test]
fn swimming_up_in_water() {
    let mut world = TestWorld::flat(-1, 2);
    for y in 0..10 {
        for x in -2..=2 {
            for z in -2..=2 {
                world.set(x, y, z, water());
            }
        }
    }
    let mut body = body_at(0.5, 0.5, 0.5);
    body.step(&Held::NONE, &world);
    assert!(body.is_in_water(), "the body is in the water column");

    let held = Held {
        jump: true,
        ..Held::NONE
    };
    for _ in 0..30 {
        body.step(&held, &world);
    }

    assert!(
        body.position().y > 1.5,
        "swam up, got y = {}",
        body.position().y
    );
}

/// Without any world data every block reads as air: the body falls, and nothing
/// panics.
#[test]
fn an_empty_world_reads_as_air() {
    let world = TestWorld::new();
    let mut body = body_at(0.5, 64.0, 0.5);
    for _ in 0..20 {
        body.step(&Held::NONE, &world);
    }
    assert!(body.position().y < 60.0);
    assert!(!body.on_ground());
}

/// A world that reports its chunks as not loaded, like a player who walked
/// past the loaded edge (review m1).
struct NotReady(TestWorld);

impl Blocks for NotReady {
    fn block(&self, pos: BlockPos) -> Option<BlockState> {
        self.0.block(pos)
    }

    fn ready(&self, _position: Vec3) -> bool {
        false
    }
}

#[test]
fn an_unloaded_chunk_freezes_the_body() {
    let world = NotReady(TestWorld::flat(63, 4));
    let mut body = body_at(0.5, 80.0, 0.5);
    let start = body.position();
    for _ in 0..40 {
        body.step(&Held::NONE, &world);
    }
    assert_eq!(body.position(), start, "no movement and no gravity while frozen");
    assert!(!body.on_ground());

    // Once the chunk is loaded the same body falls normally.
    let mut body = body_at(0.5, 80.0, 0.5);
    for _ in 0..40 {
        body.step(&Held::NONE, &TestWorld::flat(63, 4));
    }
    assert!(body.position().y < 80.0);
}

#[test]
fn the_bounding_box_follows_the_position_after_a_step() {
    // A placement check reads this box between steps; a box from the start of
    // the last step lags a walking body by its last move (review, 2026-11-03).
    let world = TestWorld::flat(-1, 4);
    let mut body = body_at(0.5, 0.0, 0.5);
    settle(&mut body, &world, 2);
    let held = Held {
        forward: true,
        ..Held::NONE
    };
    for _ in 0..5 {
        body.step(&held, &world);
    }
    let position = body.position();
    let bounding_box = body.bounding_box();
    assert_close((bounding_box.min.z + bounding_box.max.z) / 2., position.z, 1e-9, "box centre z");
    assert_close((bounding_box.min.x + bounding_box.max.x) / 2., position.x, 1e-9, "box centre x");
    assert_close(bounding_box.min.y, position.y, 1e-9, "box bottom");
}
