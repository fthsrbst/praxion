//! A minimal real world for primitive tests that need block states: one loaded
//! chunk of air, the player at [`feet`], a `set_block` and a swept perception.
//!
//! The world is built from the same login, chunk and player packets a server
//! sends, so `World::block_state` and `Perception` see real chunks instead of an
//! invalid store.

use std::sync::Arc;

use azalea_buf::AzBuf;
use azalea_core::game_type::{GameMode, OptionalGameType};
use azalea_core::position::Vec3;
use azalea_protocol::common::movements::MoveFlags;
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        ClientboundBlockUpdate, ClientboundGamePacket, ClientboundLevelChunkWithLight,
        ClientboundLogin, ServerboundGamePacket, ServerboundMovePlayerPos,
        c_level_chunk_with_light::ClientboundLevelChunkPacketData,
    },
};
use azalea_registry::{DataRegistry, data::DimensionKind, identifier::Identifier};
use azalea_world::Chunk;
use praxion_perception::{Config, Perception};
use praxion_world::{BlockPos, BlockState, World};

/// Ticks of sweeping that fill the eye's section at the per-tick ray slice.
const SWEEP_TICKS: usize = 10;

/// Where every primitive test body stands: the feet cell of the test world.
pub(crate) fn feet() -> Vec3 {
    Vec3::new(0.5, 64.0, 0.5)
}

fn spawn_info() -> CommonPlayerSpawnInfo {
    CommonPlayerSpawnInfo {
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
        online_mode: false,
        common: spawn_info(),
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

/// A valid world: chunk (0, 0) loaded and all air, the player at [`feet`].
pub(crate) fn world() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)),
    );
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: feet(),
            flags: MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }),
    );
    world
}

/// Place `state` at `pos`, as a server block update.
pub(crate) fn set_block(world: &mut World, pos: BlockPos, state: BlockState) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos,
            block_state: state,
        }),
    );
}

/// A perception whose known map has been filled by sweeping `world`.
pub(crate) fn swept(world: &World) -> Perception {
    let mut perception = Perception::new(Config::default());
    for _ in 0..SWEEP_TICKS {
        perception.begin_tick(world);
        perception.sweep(world);
    }
    perception
}
