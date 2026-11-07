//! Entering a pond by dropping into it (FINDING 5), planned and then driven
//! through `praxion_headless::physics::Body`.
//!
//! The world is a real [`World`] (empty chunks plus block updates), so the
//! follower walks against the same blocks the planner read. A stone ledge at
//! y = 63 sits two blocks above a one-block-deep pond: the only way to the far
//! side is the drop into the water, then swimming east.

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
    DirectBlocks, Follower, Goal, MoveKind, PlanStatus, Planner, PlanningState,
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

/// A stone ledge at y = 63 for x <= 0, a pond floor at y = 61 for x >= 1, and
/// one layer of water at y = 62 on top of it: a two-block drop into a pond one
/// block deep.
fn ledge_and_pond() -> World {
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
            pos: Vec3::new(0.5, 64.0, 0.5),
            flags: azalea_protocol::common::movements::MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }),
    );
    for z in -2..=2 {
        for x in -2..=0 {
            set_block(&mut world, BlockPos::new(x, 63, z), BlockKind::Stone);
        }
        for x in 1..=10 {
            set_block(&mut world, BlockPos::new(x, 61, z), BlockKind::Stone);
            set_block(&mut world, BlockPos::new(x, 62, z), BlockKind::Water);
        }
    }
    world
}

#[test]
fn the_body_drops_into_the_pond_and_crosses_it() {
    let world = ledge_and_pond();
    let start = BlockPos::new(0, 64, 0);
    let goal = BlockPos::new(7, 62, 0);

    let mut planner = Planner::new();
    planner.set_max_drop(3);
    let state = PlanningState::new(Default::default());
    let mut source = DirectBlocks { world: &world };
    let plan = planner.plan(&mut source, &state, start, Goal::Block(goal));

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    let entry = plan.steps.first().expect("a first step");
    assert_eq!(entry.kind, MoveKind::EnterWater, "{:?}", plan.steps);
    assert_eq!(entry.from.y - entry.to.y, 2, "the drop is two blocks");
    assert!(
        plan.steps
            .iter()
            .skip(1)
            .all(|step| step.kind == MoveKind::Swim),
        "{:?}",
        plan.steps
    );

    let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
    let mut follower = Follower::new(plan.steps.clone());
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
            assert!(body.is_in_water() || feet.y == goal.y, "{feet:?}");
            return;
        }
    }
    panic!(
        "did not cross the pond in {max_ticks} ticks: {:?} at {:?}",
        follower.progress(&body),
        body.position()
    );
}
