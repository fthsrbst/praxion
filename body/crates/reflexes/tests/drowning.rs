//! The drowning reflex must reach air, not only rise until the first lid.
//!
//! The live run (2026-11-08, seed 202) mined into a flooded lake bed: the body
//! rose under a solid overhang, the reflex only held jump, the cap ended it and
//! the body drowned. These tests pin the properties that failed:
//!
//! - a flooded 1x1 shaft with a solid lid must be escaped sideways to an open
//!   column and then up;
//! - a flooded hole beside the bank must step out onto the shore;
//! - open water must still be climbed straight up;
//! - the reflex keeps the body after the head clears the water while the air
//!   supply is still low, and only releases when the air recovers;
//! - the cap does not cut a reflex that is still making progress.

mod common;

use azalea_registry::builtin::BlockKind;
use praxion_reflexes::ReflexKind;
use praxion_world::{BlockPos, Vec3};

use common::{Sim, eye, state, test_settings};

fn block(sim: &Sim, pos: BlockPos) -> BlockKind {
    BlockKind::from(sim.world.block_state(pos).expect("the test cell is loaded"))
}

/// A 1x1 flooded shaft at x=0, z=0 under a stone lid, with a flooded tunnel at
/// the bottom that opens into a water column at x=2.
fn flooded_shaft() -> Sim {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 60.0, 0.5));
    // The shaft: three water cells, capped by stone at y=63.
    for y in 60..=62 {
        sim.set_block(BlockPos::new(0, y, 0), state(BlockKind::Water));
    }
    sim.set_block(BlockPos::new(0, 63, 0), state(BlockKind::Stone));
    // Stone walls around the shaft.
    for y in 60..=62 {
        for (x, z) in [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1), (1, -1), (1, 1)] {
            sim.set_block(BlockPos::new(x, y, z), state(BlockKind::Stone));
        }
    }
    // The flooded exit at the bottom and its capped roof.
    for y in 60..=61 {
        sim.set_block(BlockPos::new(1, y, 0), state(BlockKind::Water));
    }
    sim.set_block(BlockPos::new(1, 62, 0), state(BlockKind::Stone));
    sim.set_block(BlockPos::new(1, 63, 0), state(BlockKind::Stone));
    // The open column: water to y=64, air at y=65.
    for y in 60..=64 {
        sim.set_block(BlockPos::new(2, y, 0), state(BlockKind::Water));
        sim.set_block(BlockPos::new(3, y, 0), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(2, y, -1), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(2, y, 1), state(BlockKind::Stone));
    }
    // One tick short of the threshold, so the reflex sees it immediately.
    sim.engine.state_mut().air_ticks = test_settings().air_surface_threshold;
    sim
}

/// A 1x1 flooded hole with a stone lid and a flush bank to the east: the only
/// air the body can stand in is the shore beside the water.
fn flooded_hole_by_the_shore() -> Sim {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 60.0, 0.5));
    // The hole: two water cells, capped by stone at y=62.
    for y in 60..=61 {
        sim.set_block(BlockPos::new(0, y, 0), state(BlockKind::Water));
    }
    sim.set_block(BlockPos::new(0, 62, 0), state(BlockKind::Stone));
    // Stone walls, except the open bank to the east at feet level.
    for y in 60..=61 {
        for (x, z) in [(-1, -1), (-1, 0), (-1, 1), (0, -1), (0, 1)] {
            sim.set_block(BlockPos::new(x, y, z), state(BlockKind::Stone));
        }
        sim.set_block(BlockPos::new(1, y, -1), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(1, y, 1), state(BlockKind::Stone));
        // The wall past the bank, so the only way out is one block east.
        sim.set_block(BlockPos::new(2, y, 0), state(BlockKind::Stone));
    }
    // The bank floor is the flat test floor at y=59; the bank itself stands at
    // the water's own feet level, so its two cells are open air.
    sim.set_block(BlockPos::new(1, 60, 0), state(BlockKind::Air));
    sim.set_block(BlockPos::new(1, 61, 0), state(BlockKind::Air));
    sim.engine.state_mut().air_ticks = test_settings().air_surface_threshold;
    sim
}

/// A walled 1x1 water column from y=64 to y=70 with open air above.
fn open_water() -> Sim {
    let mut sim = Sim::new(test_settings(), Vec3::new(0.5, 64.0, 0.5));
    for y in 64..=70 {
        sim.set_block(BlockPos::new(0, y, 0), state(BlockKind::Water));
        sim.set_block(BlockPos::new(1, y, 0), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(-1, y, 0), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(0, y, 1), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(0, y, -1), state(BlockKind::Stone));
    }
    sim.engine.state_mut().air_ticks = test_settings().air_surface_threshold;
    sim
}

/// The same column, deeper than the body can climb inside a short cap.
fn deep_column() -> Sim {
    let mut settings = test_settings();
    settings.max_reflex_ticks = 20;
    let mut sim = Sim::new(settings, Vec3::new(0.5, 60.0, 0.5));
    for y in 60..=100 {
        sim.set_block(BlockPos::new(0, y, 0), state(BlockKind::Water));
        sim.set_block(BlockPos::new(1, y, 0), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(-1, y, 0), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(0, y, 1), state(BlockKind::Stone));
        sim.set_block(BlockPos::new(0, y, -1), state(BlockKind::Stone));
    }
    sim.engine.state_mut().air_ticks = test_settings().air_surface_threshold;
    sim
}

/// The eye cell is no longer water: the head is out.
fn head_out(sim: &Sim) -> bool {
    block(sim, eye(sim.body.position()).to_block_pos_floor()) != BlockKind::Water
}

#[test]
fn a_body_at_the_bottom_of_a_flooded_shaft_under_an_overhang_reaches_air() {
    let mut sim = flooded_shaft();
    sim.step();
    assert_eq!(
        sim.engine.active(),
        Some(ReflexKind::Drowning),
        "the body starts underwater with low air"
    );

    let reached = sim.run_until(160, head_out);
    assert!(
        reached,
        "the reflex never left the capped shaft; body at {:?}",
        sim.body.position()
    );
    assert!(
        head_out(&sim),
        "the head is still in water at {:?}",
        sim.body.position()
    );
    // The lid at y=63 covers the shaft, so breathing at the open column at
    // x=2 is the only way out: the body had to swim sideways first.
    assert!(
        sim.body.position().x >= 2.0 - 1.0e-6,
        "the body did not take the sideways route to the open column; x={}",
        sim.body.position().x
    );
}

#[test]
fn a_body_in_a_flooded_hole_steps_out_onto_the_shore() {
    let mut sim = flooded_hole_by_the_shore();
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Drowning));

    let reached = sim.run_until(160, |sim| {
        sim.body.position().x >= 1.0 - 1.0e-6 && head_out(sim)
    });
    assert!(
        reached,
        "the reflex never stepped out onto the bank; body at {:?}",
        sim.body.position()
    );
}

#[test]
fn a_body_in_open_water_rises_to_air() {
    let mut sim = open_water();
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Drowning));

    let start_y = sim.body.position().y;
    let reached = sim.run_until(120, head_out);
    assert!(
        reached,
        "the body did not surface; y={}",
        sim.body.position().y
    );
    assert!(
        sim.body.position().y > start_y,
        "the body never rose: {start_y} -> {}",
        sim.body.position().y
    );
}

#[test]
fn the_drowning_reflex_does_not_release_while_the_air_is_low() {
    let mut sim = open_water();
    // A server air fact, so the simulation cannot pretend the air recovered.
    sim.set_own_air(1);
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Drowning));

    let surfaced = sim.run_until(120, head_out);
    assert!(surfaced, "the body did not surface");

    // The head is out, but the air fact is still low and the body has no land
    // under it. Handing control back here is what drowned the live run.
    for _ in 0..5 {
        sim.step();
        assert_eq!(
            sim.engine.active(),
            Some(ReflexKind::Drowning),
            "released while the air was still low"
        );
    }

    // The server says the air recovered: now the reflex may end.
    sim.set_own_air(300);
    sim.step();
    assert_eq!(
        sim.engine.active(),
        None,
        "the air recovered, so the reflex ends"
    );
}

#[test]
fn the_cap_does_not_end_a_reflex_that_is_still_rising() {
    let mut sim = deep_column();
    sim.step();
    assert_eq!(sim.engine.active(), Some(ReflexKind::Drowning));

    // The cap is 20 ticks; the column needs far longer than that to climb.
    // Only the progress clock (the body is closing on its air target every
    // tick) may keep the reflex alive this long.
    for _ in 0..40 {
        sim.step();
    }
    assert_eq!(
        sim.engine.active(),
        Some(ReflexKind::Drowning),
        "the cap ended a reflex that was still rising toward air"
    );
    assert!(
        sim.run_until(400, head_out),
        "the body never reached air; it is at {:?}",
        sim.body.position()
    );
}
