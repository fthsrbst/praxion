//! Shared fixtures for the perception integration tests.
//!
//! Everything here uses only the public API of `praxion-perception` and
//! `praxion-world`. The worlds go through the same packet path the body uses:
//! a login, empty chunks, then block updates and entity adds. Nothing is a real
//! chunk; empty chunks plus block updates are enough to pin the rules.

#![allow(dead_code)]

use std::sync::Arc;

use azalea_block::{BlockState, BlockTrait};
use azalea_buf::AzBuf;
use azalea_core::{
    delta::LpVec3, entity_id::MinecraftEntityId, game_type::GameMode, position::Vec3,
};
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        c_level_chunk_with_light::ClientboundLevelChunkPacketData, ClientboundAddEntity,
        ClientboundGamePacket, ClientboundLevelChunkWithLight, ClientboundLogin,
        ServerboundGamePacket, ServerboundMovePlayerPos,
    },
};
use azalea_registry::{
    builtin::EntityKind, data::DimensionKind, identifier::Identifier, DataRegistry,
};
use azalea_world::Chunk;
use praxion_decoder::{Decoder, Event, Packet};
use praxion_perception::DEFAULT_EYE_HEIGHT;
use praxion_tee::Reader;
use praxion_world::{BlockPos, World};
use uuid::Uuid;

pub const EYE_Y: f64 = 65.0;
pub const FEET_Y: f64 = EYE_Y - DEFAULT_EYE_HEIGHT;

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
        player_id: MinecraftEntityId(7),
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

pub fn chunk_packet(x: i32, z: i32) -> ClientboundLevelChunkWithLight {
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

pub fn base_world() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)),
    );
    world
}

pub fn set_player(world: &mut World, feet: Vec3) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: feet,
            flags: azalea_protocol::common::movements::MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }),
    );
}

pub fn set_block(world: &mut World, pos: BlockPos, state: BlockState) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(
            azalea_protocol::packets::game::ClientboundBlockUpdate {
                pos,
                block_state: state,
            },
        ),
    );
}

/// An empty chunk with `blocks` placed and the player's eye at [`EYE_Y`].
pub fn world_with(blocks: &[(BlockPos, BlockState)]) -> World {
    let mut world = base_world();
    set_player(&mut world, Vec3::new(0.5, FEET_Y, 0.5));
    for (pos, state) in blocks {
        set_block(&mut world, *pos, *state);
    }
    world
}

/// Empty chunks `0..chunks` along x with `blocks` placed, the player standing
/// in the first one: long rays that cross more than one chunk need them.
pub fn world_across_chunks(chunks: i32, blocks: &[(BlockPos, BlockState)]) -> World {
    let mut world = base_world();
    for x in 1..chunks {
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(x, 0)),
        );
    }
    set_player(&mut world, Vec3::new(0.5, FEET_Y, 0.5));
    for (pos, state) in blocks {
        set_block(&mut world, *pos, *state);
    }
    world
}

pub fn find_block(name: &str, props: &[(&str, &str)]) -> BlockState {
    for id in 0..=u32::from(u16::MAX) {
        let Ok(state) = BlockState::try_from(id) else {
            break;
        };
        let block = Box::<dyn BlockTrait>::from(state);
        if block.id() != name {
            continue;
        }
        let map = block.property_map();
        if props.iter().all(|(key, value)| map.get(key) == Some(value)) {
            return state;
        }
    }
    panic!("block {name} with {props:?} not found");
}

pub fn stone() -> BlockState {
    find_block("stone", &[])
}

pub fn air() -> BlockState {
    BlockState::AIR
}

pub fn add_zombie(world: &mut World, id: i32, pos: Vec3) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: MinecraftEntityId(id),
            uuid: Uuid::from_u128(id as u128),
            entity_type: EntityKind::Zombie,
            position: pos,
            movement: LpVec3::Zero,
            x_rot: 0,
            y_rot: 0,
            y_head_rot: 0,
            data: 0,
        }),
    );
}

pub fn block_name(state: BlockState) -> String {
    Box::<dyn BlockTrait>::from(state)
        .as_block_kind()
        .to_str()
        .trim_start_matches("minecraft:")
        .to_string()
}

/// Decode a tee capture into a [`World`].
pub fn decode_corpus(name: &str) -> World {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../spikes/captures")
        .join(name);
    let file = std::fs::File::open(&path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    let reader = Reader::new(std::io::BufReader::new(file)).expect("tee header");
    let mut decoder = Decoder::new();
    decoder.set_reorder_window(60_000_000_000);
    let mut world = World::new();
    let mut followed = false;
    for record in reader {
        let record = record.expect("tee record");
        for event in decoder.feed(&record) {
            apply(&mut world, &mut followed, &event);
        }
    }
    for event in decoder.finish() {
        apply(&mut world, &mut followed, &event);
    }
    assert!(
        world.status().is_valid(),
        "corpus desynced: {:?}",
        world.status()
    );
    world
}

/// Apply one decoded tee event to a world, following the first login.
pub fn apply(world: &mut World, followed: &mut bool, event: &Event) {
    match event {
        Event::Opened { conn_id, .. } => world.opened(*conn_id),
        Event::Closed { conn_id, .. } => world.closed(*conn_id),
        Event::Desync(desync) => world.invalidate(desync.conn_id, desync.reason.clone()),
        Event::Decoded(decoded) => match &decoded.packet {
            Packet::ConfigurationClientbound(packet) => {
                world.apply_clientbound_config(decoded.conn_id, packet);
            }
            Packet::PlayClientbound(packet) => {
                if !*followed && decoded.packet.name() == "login" {
                    world.follow(decoded.conn_id);
                    *followed = true;
                }
                world.apply_clientbound_play(decoded.conn_id, packet);
            }
            Packet::PlayServerbound(packet) => {
                world.apply_serverbound_play(decoded.conn_id, packet);
            }
            _ => {}
        },
    }
}
