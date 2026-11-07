//! A hand-built pond crossed by the planner and then by the physics body.
//!
//! The world is a real [`World`] (empty chunks plus block updates) so the
//! follower drives `praxion_headless::physics::Body` against the same blocks
//! the planner read. The only route is the water: the floor ends at z = ±2 and
//! a detour has nothing to stand on.

use std::sync::Arc;

use azalea_buf::AzBuf;
use azalea_core::game_type::GameMode;
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        ClientboundBlockUpdate, ClientboundGamePacket, ClientboundLevelChunkWithLight,
        ClientboundLogin, ServerboundGamePacket, ServerboundMovePlayerPos,
        c_level_chunk_with_light::ClientboundLevelChunkPacketData,
    },
};
use azalea_registry::{
    DataRegistry,
    builtin::BlockKind,
    data::DimensionKind,
    identifier::Identifier,
};
use azalea_world::Chunk;
use praxion_headless::Body;
use praxion_planner::{
    DirectBlocks, Follower, Goal, MoveKind, Personality, PlanStatus, Planner, PlanningState,
};
use praxion_world::{BlockPos, Vec3, World};

fn spawn_info() -> CommonPlayerSpawnInfo {
    CommonPlayerSpawnInfo {
        dimension_type: DimensionKind::new_raw(0),
        dimension: Identifier::new("minecraft:overworld"),
        seed: 0,
        game_type: GameMode::Survival,
        previous_game_type: azalea_core::game_type::OptionalGameType(None),
        is_debug: false,
        is_flat: false,
        last_death_location: None,
        portal_cooldown: 0,
        sea_level: 63,
    }
}

fn login() -> ClientboundLogin {
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

fn set_block(world: &mut World, pos: BlockPos, kind: BlockKind) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos,
            block_state: kind.into(),
        }),
    );
}

/// Floor at y = 62 and water at y = 63, both only for z in -2..=2, so the only
/// way from x = 0 to x = 8 is through the pond.
fn pond_world() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    for (x, z) in [(0, 0), (0, -1), (1, 0)] {
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(x, z)),
        );
    }
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: Vec3::new(0.5, 63.0, 0.5),
            flags: azalea_protocol::common::movements::MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }),
    );
    for x in -2..=10 {
        for z in -2..=2 {
            set_block(&mut world, BlockPos::new(x, 62, z), BlockKind::Stone);
            if (2..=6).contains(&x) {
                set_block(&mut world, BlockPos::new(x, 63, z), BlockKind::Water);
            }
        }
    }
    world
}

#[test]
fn the_planner_and_the_body_cross_a_pond() {
    let world = pond_world();
    let start = BlockPos::new(0, 63, 0);
    let goal = BlockPos::new(8, 63, 0);

    let mut planner = Planner::new();
    planner.set_max_drop(3);
    let state = PlanningState::new(Default::default());
    let mut source = DirectBlocks { world: &world };
    let plan = planner.plan(&mut source, &state, start, Goal::Block(goal));
    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    let names: Vec<&str> = plan.steps.iter().map(|step| step.kind.name()).collect();
    assert!(names.contains(&"enter_water"), "{names:?}");
    assert!(names.contains(&"swim"), "{names:?}");
    assert!(names.contains(&"exit_water"), "{names:?}");
    assert!(plan
        .steps
        .iter()
        .all(|step| !matches!(step.kind, MoveKind::Dig { .. })));

    let mut body = Body::new(Vec3::new(0.5, 63.0, 0.5));
    let mut route_world = DirectBlocks { world: &world };
    let mut follower = Follower::new(
        plan.steps.clone(),
        &mut route_world,
        &state,
        Personality::default(),
    );
    let max_ticks = (plan.steps.len() as u64) * 60 + 200;
    for tick in 0..max_ticks {
        let frame = follower.step(tick, body.generation(), &body);
        frame
            .apply(&mut body, tick)
            .unwrap_or_else(|error| panic!("frame at tick {tick}: {error}"));
        body.step(&frame.held, &world);
        assert!(!follower.is_stalled(), "stalled at tick {tick}");
        if follower.is_done() {
            let feet = BlockPos::from(body.position());
            assert_eq!(feet, goal, "the body crossed the pond at {feet:?}");
            return;
        }
    }
    panic!(
        "did not cross the pond in {max_ticks} ticks: {:?} at {:?}",
        follower.progress(&body),
        body.position()
    );
}
