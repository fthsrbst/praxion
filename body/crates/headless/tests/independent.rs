//! Independent differential checks for the headless physics step and the
//! `ControlFrame` actuator. Written by the independent tester; not by the
//! author of `physics.rs` / `frame.rs`.
//!
//! Each test pins an exact number taken from azalea's own physics tests or
//! from the vanilla constants the crate claims to reproduce. A test that fails
//! because of a real bug is kept and marked `// FINDING n`.

use std::sync::Arc;

use azalea_block::BlockState;
use azalea_core::game_type::{GameMode, OptionalGameType};
use azalea_core::position::{BlockPos, Vec3};
use azalea_registry::DataRegistry;
use azalea_registry::data::DimensionKind;
use azalea_registry::identifier::Identifier;
use azalea_protocol::packets::game::{
    ClientboundBlockUpdate, ClientboundGamePacket, ClientboundLevelChunkWithLight,
    ClientboundLogin, c_level_chunk_with_light::ClientboundLevelChunkPacketData,
};
use azalea_core::delta::AzBuf;
use azalea_world::Chunk;
use praxion_headless::{Body, ControlFrame, FrameError, Held, Look};
use praxion_world::World;

// ---------------------------------------------------------------- helpers

fn spawn_info() -> azalea_protocol::packets::common::CommonPlayerSpawnInfo {
    azalea_protocol::packets::common::CommonPlayerSpawnInfo {
        dimension_type: DimensionKind::new_raw(0),
        dimension: Identifier::new("minecraft:overworld"),
        seed: 0,
        game_type: GameMode::Survival,
        previous_game_type: OptionalGameType(None),
        is_debug: false,
        is_flat: false,
        last_death_location: None,
        portal_cooldown: 0,
        sea_level: 63,
    }
}

fn login_packet() -> ClientboundLogin {
    ClientboundLogin {
        player_id: azalea_core::entity_id::MinecraftEntityId(7),
        hardcore: false,
        levels: Vec::new(),
        max_players: 20,
        chunk_radius: 8,
        simulation_distance: 8,
        reduced_debug_info: false,
        show_death_screen: true,
        do_limited_crafting: false,
        common: spawn_info(),
        online_mode: false,
        enforces_secure_chat: false,
    }
}

fn chunk_packet(x: i32, z: i32) -> ClientboundLevelChunkWithLight {
    let chunk = Chunk::default();
    let mut buffer = Vec::new();
    for section in &chunk.sections {
        section.azalea_write(&mut buffer).expect("write section");
    }
    ClientboundLevelChunkWithLight {
        x,
        z,
        chunk_data: ClientboundLevelChunkPacketData {
            heightmaps: Vec::new(),
            buffer: Arc::new(buffer.into_boxed_slice()),
            block_entities: Vec::new(),
        },
        light_data: Default::default(),
    }
}

fn active_world() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login_packet()));
    world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)));
    world
}

fn set_block(world: &mut World, pos: BlockPos, state: BlockState) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos,
            block_state: state,
        }),
    );
}

fn stone() -> BlockState {
    BlockState::from(azalea_block::blocks::Stone)
}

// ---------------------------------------------------------------- ported tests

/// Ported from `azalea-physics/tests/physics.rs::test_gravity`.
#[test]
fn ported_gravity_sets_velocity_before_it_moves() {
    let world = World::new(); // no dimension/chunks -> every block is air
    let mut body = Body::new(Vec3::new(0.0, 70.0, 0.0));
    body.step(&Held::NONE, &world);
    assert_eq!(body.position().y, 70.0);
    body.step(&Held::NONE, &world);
    assert!(body.position().y < 70.0);
}

/// Ported from `azalea-physics/tests/physics.rs::test_collision`.
#[test]
fn ported_collision_lands_on_stone() {
    let mut world = active_world();
    set_block(&mut world, BlockPos::new(0, 69, 0), stone());
    let mut body = Body::new(Vec3::new(0.5, 70.0, 0.5));
    // first tick: velocity set, no move
    body.step(&Held::NONE, &world);
    assert_eq!(body.position().y, 70.0);
    // second tick: moves but collides with stone at y=69
    body.step(&Held::NONE, &world);
    assert_eq!(body.position().y, 70.0);
}

/// Ported from `azalea-physics/tests/physics.rs::test_slab_collision`.
#[test]
fn ported_bottom_slab_lands_at_half() {
    let mut world = active_world();
    let slab = BlockState::from(azalea_block::blocks::StoneSlab {
        kind: azalea_block::properties::SlabKind::Bottom,
        waterlogged: false,
    });
    set_block(&mut world, BlockPos::new(0, 69, 0), slab);
    let mut body = Body::new(Vec3::new(0.5, 71.0, 0.5));
    for _ in 0..20 {
        body.step(&Held::NONE, &world);
    }
    assert_eq!(body.position().y, 69.5);
}

/// Ported from `azalea-physics/tests/physics.rs::test_top_slab_collision`.
#[test]
fn ported_top_slab_lands_at_top() {
    let mut world = active_world();
    let slab = BlockState::from(azalea_block::blocks::StoneSlab {
        kind: azalea_block::properties::SlabKind::Top,
        waterlogged: false,
    });
    set_block(&mut world, BlockPos::new(0, 69, 0), slab);
    let mut body = Body::new(Vec3::new(0.5, 71.0, 0.5));
    for _ in 0..20 {
        body.step(&Held::NONE, &world);
    }
    assert_eq!(body.position().y, 70.0);
}

/// Ported from `azalea-physics/tests/physics.rs::test_weird_wall_collision`.
#[test]
fn ported_cobblestone_wall_lands_at_half_plus() {
    let mut world = active_world();
    let wall = BlockState::from(azalea_block::blocks::CobblestoneWall {
        east: azalea_block::properties::WallEast::Low,
        north: azalea_block::properties::WallNorth::Low,
        south: azalea_block::properties::WallSouth::Low,
        west: azalea_block::properties::WallWest::Low,
        up: false,
        waterlogged: false,
    });
    set_block(&mut world, BlockPos::new(0, 69, 0), wall);
    let mut body = Body::new(Vec3::new(0.5, 73.0, 0.5));
    for _ in 0..20 {
        body.step(&Held::NONE, &world);
    }
    assert_eq!(body.position().y, 70.5);
}

// ---------------------------------------------------------------- frame checks

/// Stale generation frames are dropped.
#[test]
fn frame_stale_generation_dropped() {
    let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
    body.bump_generation();
    let frame = ControlFrame::new(0, 0, Held::NONE);
    assert_eq!(frame.apply(&mut body, 0), Err(FrameError::Stale { frame: 0, current: 1 }));
}

/// Aim with pitch beyond +-90: is it clamped? (review probe)
#[test]
fn frame_aim_pitch_beyond_90() {
    let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
    let mut frame = ControlFrame::new(0, body.generation(), Held::NONE);
    frame.look = Look::Aim { yaw: 0.0, pitch: 200.0 };
    frame.apply(&mut body, 0).unwrap();
    // Record what actually happens; the test asserts the documented vanilla
    // clamp. If this fails, it is a FINDING.
    assert!(body.look().x_rot().abs() <= 90.0, "Aim pitch not clamped: {}", body.look().x_rot());
}

/// NaN in a frame: is it refused? (review probe)
// FINDING 1: NaN in a ControlFrame is not validated; apply() accepts it and
// poisons the body's look direction. See review file.
#[test]
fn frame_nan_refused() {
    let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
    let mut frame = ControlFrame::new(0, body.generation(), Held::NONE);
    frame.look = Look::Aim { yaw: f32::NAN, pitch: 0.0 };
    // There is no NaN variant in FrameError, so a refused frame must surface
    // as some error. If apply succeeds, that is a FINDING.
    let result = frame.apply(&mut body, 0);
    assert!(result.is_err(), "NaN frame was accepted: {:?}", result);
}

/// World gap: an unloaded chunk under the player. Does the bot panic or fall?
#[test]
fn world_gap_does_not_panic() {
    let world = World::new(); // dimension None -> no chunks
    let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
    for _ in 0..5 {
        body.step(&Held::NONE, &world);
    }
    // No panic is the pass condition; position may fall (documented difference).
    assert!(body.position().y < 64.0);
}
