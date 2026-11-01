//! Independent tests for the world model (t3b review).

use azalea_block::BlockState;
use azalea_buf::AzBuf;
use azalea_core::position::Vec3;
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        ClientboundAddEntity, ClientboundGamePacket, ClientboundLevelChunkWithLight,
        ClientboundPlayerPosition, ServerboundGamePacket,
        c_level_chunk_with_light::ClientboundLevelChunkPacketData,
    },
};
use azalea_registry::{
    DataRegistry,
    builtin::{EntityKind, ItemKind},
    data::DimensionKind,
    identifier::Identifier,
};
use azalea_world::Chunk;
use uuid::Uuid;

use praxion_world::{BlockPos as WBlockPos, World};

fn active_world() -> World {
    let mut world = World::new();
    world.follow(0);
    let spawn = CommonPlayerSpawnInfo {
        dimension_type: DimensionKind::new_raw(0),
        dimension: Identifier::new("minecraft:overworld"),
        seed: 0,
        game_type: azalea_core::game_type::GameMode::Survival,
        previous_game_type: azalea_core::game_type::OptionalGameType(None),
        is_debug: false,
        is_flat: false,
        last_death_location: None,
        portal_cooldown: 0,
        sea_level: 63,
    };
    // silence unused? spawn used below
    let login = azalea_protocol::packets::game::ClientboundLogin {
        player_id: azalea_core::entity_id::MinecraftEntityId(7),
        hardcore: false,
        levels: Vec::new(),
        max_players: 20,
        chunk_radius: 8,
        simulation_distance: 8,
        reduced_debug_info: false,
        show_death_screen: true,
        do_limited_crafting: false,
        common: spawn,
        online_mode: false,
        enforces_secure_chat: false,
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login));
    world
}

#[test]
fn block_query_out_of_height_unloaded_forget_none() {
    let mut world = active_world();

    // Outside world height (overworld fallback: min_y -64, height 384).
    assert!(world.block_state(WBlockPos::new(0, 10_000, 0)).is_none());
    assert!(world.block_state(WBlockPos::new(0, -10_000, 0)).is_none());

    // Unloaded chunk.
    assert!(world.block_state(WBlockPos::new(100_000, 64, 100_000)).is_none());

    // Load a chunk, set a block, then forget the chunk -> None.
    let chunk = Chunk::default();
    let mut buffer = Vec::new();
    for section in &chunk.sections {
        section.azalea_write(&mut buffer).expect("write section");
    }
    let packet = ClientboundLevelChunkWithLight {
        x: 0,
        z: 0,
        chunk_data: ClientboundLevelChunkPacketData {
            heightmaps: Vec::new(),
            buffer: std::sync::Arc::new(buffer.into_boxed_slice()),
            block_entities: Vec::new(),
        },
        light_data: Default::default(),
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(packet));

    let stone = find_block("minecraft:stone");
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(azalea_protocol::packets::game::ClientboundBlockUpdate {
            pos: WBlockPos::new(0, 64, 0),
            block_state: stone,
        }),
    );
    assert_eq!(world.block_state(WBlockPos::new(0, 64, 0)), Some(stone));

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ForgetLevelChunk(azalea_protocol::packets::game::ClientboundForgetLevelChunk {
            pos: azalea_core::position::ChunkPos::new(0, 0),
        }),
    );
    assert!(world.block_state(WBlockPos::new(0, 64, 0)).is_none());
}

fn find_block(name: &str) -> BlockState {
    for id in 0..4096u32 {
        if let Ok(state) = BlockState::try_from(id) {
            if praxion_world::BlockDescription::of(state).name() == name {
                return state;
            }
        }
    }
    panic!("block {name} not found");
}

#[test]
fn invalid_world_all_queries_none() {
    let mut world = active_world();
    // Load a chunk and a block so there is data to hide.
    let chunk = Chunk::default();
    let mut buffer = Vec::new();
    for section in &chunk.sections {
        section.azalea_write(&mut buffer).expect("write section");
    }
    let packet = ClientboundLevelChunkWithLight {
        x: 0,
        z: 0,
        chunk_data: ClientboundLevelChunkPacketData {
            heightmaps: Vec::new(),
            buffer: std::sync::Arc::new(buffer.into_boxed_slice()),
            block_entities: Vec::new(),
        },
        light_data: Default::default(),
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(packet));
    let stone = find_block("minecraft:stone");
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(azalea_protocol::packets::game::ClientboundBlockUpdate {
            pos: WBlockPos::new(0, 64, 0),
            block_state: stone,
        }),
    );
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: azalea_core::entity_id::MinecraftEntityId(11),
            uuid: Uuid::from_u128(42),
            entity_type: EntityKind::Zombie,
            position: Vec3::new(1.0, 64.0, 1.0),
            movement: azalea_core::delta::LpVec3::Zero,
            x_rot: 0,
            y_rot: 64,
            y_head_rot: 64,
            data: 0,
        }),
    );

    world.invalidate(0, "tee gap");
    assert!(!world.status().is_valid());
    assert!(world.block_state(WBlockPos::new(0, 64, 0)).is_none());
    assert!(world.block_description(WBlockPos::new(0, 64, 0)).is_none());
    assert!(world.entity(11).is_none());
    assert!(world.entities().next().is_none());
}

#[test]
fn teleport_relative_flags_vanilla_result() {
    let mut world = active_world();
    // Base position/rotation via an absolute teleport.
    let base_teleport = ClientboundPlayerPosition {
        id: 1,
        change: azalea_protocol::common::movements::PositionMoveRotation {
            pos: Vec3::new(10.0, 64.0, -3.0),
            delta: Vec3::ZERO,
            look_direction: azalea_entity::LookDirection::new(90.0, 30.0),
        },
        relative: azalea_protocol::common::movements::RelativeMovements::all_absolute(),
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::PlayerPosition(base_teleport));
    assert_eq!(world.player_position(), Some(Vec3::new(10.0, 64.0, -3.0)));

    // All-relative teleport adds to base.
    let relative_all = ClientboundPlayerPosition {
        id: 2,
        change: azalea_protocol::common::movements::PositionMoveRotation {
            pos: Vec3::new(1.0, 2.0, 3.0),
            delta: Vec3::ZERO,
            look_direction: azalea_entity::LookDirection::new(10.0, 5.0),
        },
        relative: azalea_protocol::common::movements::RelativeMovements::all_relative(),
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::PlayerPosition(relative_all));
    assert_eq!(world.player_position(), Some(Vec3::new(11.0, 66.0, 0.0)));
    assert_eq!(world.player_rotation(), (100.0, 35.0));

    // Reset base.
    let base_teleport = ClientboundPlayerPosition {
        id: 3,
        change: azalea_protocol::common::movements::PositionMoveRotation {
            pos: Vec3::new(10.0, 64.0, -3.0),
            delta: Vec3::ZERO,
            look_direction: azalea_entity::LookDirection::new(90.0, 30.0),
        },
        relative: azalea_protocol::common::movements::RelativeMovements::all_absolute(),
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::PlayerPosition(base_teleport));

    // Mixed flags.
    let mut mixed_rel = azalea_protocol::common::movements::RelativeMovements::all_absolute();
    mixed_rel.x = true;
    mixed_rel.z = true;
    mixed_rel.x_rot = true;
    let mixed = ClientboundPlayerPosition {
        id: 4,
        change: azalea_protocol::common::movements::PositionMoveRotation {
            pos: Vec3::new(1.0, 2.0, 3.0),
            delta: Vec3::ZERO,
            look_direction: azalea_entity::LookDirection::new(10.0, 5.0),
        },
        relative: mixed_rel,
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::PlayerPosition(mixed));
    assert_eq!(world.player_position(), Some(Vec3::new(11.0, 2.0, 0.0)));
    assert_eq!(world.player_rotation(), (10.0, 35.0));
}

#[test]
fn inventory_after_container_click_changes_several_slots() {
    let mut world = active_world();
    let items = vec![
        azalea_inventory::ItemStack::new(ItemKind::Stone, 32),
        azalea_inventory::ItemStack::Empty,
        azalea_inventory::ItemStack::new(ItemKind::Dirt, 3),
    ];
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetContent(azalea_protocol::packets::game::ClientboundContainerSetContent {
            container_id: 0,
            state_id: 1,
            items,
            carried_item: azalea_inventory::ItemStack::new(ItemKind::Torch, 2),
        }),
    );

    use azalea_protocol::packets::game::s_container_click::HashedActualItem;
    let hashed = |kind: ItemKind, count: i32| {
        azalea_protocol::packets::game::s_container_click::HashedStack(Some(
            HashedActualItem {
                kind,
                count,
                components: azalea_protocol::packets::game::s_container_click::HashedPatchMap {
                    added_components: Vec::new(),
                    removed_components: Vec::new(),
                },
            },
        ))
    };
    let mut click = azalea_protocol::packets::game::ServerboundContainerClick {
        container_id: 0,
        state_id: 2,
        slot_num: 1,
        button_num: 0,
        click_type: azalea_inventory::operations::ClickType::QuickMove,
        changed_slots: <_ as AzBuf>::azalea_read(&mut std::io::Cursor::new(&[0u8][..])).unwrap(),
        carried_item: azalea_protocol::packets::game::s_container_click::HashedStack(None),
    };
    click.changed_slots.insert(0, hashed(ItemKind::Stone, 16));
    click.changed_slots.insert(1, hashed(ItemKind::Dirt, 5));
    click.changed_slots.insert(2, azalea_protocol::packets::game::s_container_click::HashedStack(None));
    click.carried_item = hashed(ItemKind::Torch, 1);
    world.apply_serverbound_play(0, &ServerboundGamePacket::ContainerClick(click));

    let last = world.last_container_click().expect("click recorded");
    // The implementation's filter_map drops slots whose hashed stack is None,
    // so only the two present slots survive.
    assert_eq!(last.changed_slots().len(), 2);
    assert_eq!(last.changed_slots()[0].slot(), 0);
    assert_eq!(last.changed_slots()[0].kind(), ItemKind::Stone);
    assert_eq!(last.changed_slots()[0].count(), 16);
    assert_eq!(last.changed_slots()[1].slot(), 1);
    assert_eq!(last.changed_slots()[1].kind(), ItemKind::Dirt);
    assert_eq!(last.changed_slots()[1].count(), 5);
    assert_eq!(last.carried(), Some((ItemKind::Torch, 1)));
}

#[test]
fn follow_new_connection_resets_world() {
    let mut world = active_world();

    // Load some state on connection 0.
    let chunk = Chunk::default();
    let mut buffer = Vec::new();
    for section in &chunk.sections {
        section.azalea_write(&mut buffer).expect("write section");
    }
    let packet = ClientboundLevelChunkWithLight {
        x: 0,
        z: 0,
        chunk_data: ClientboundLevelChunkPacketData {
            heightmaps: Vec::new(),
            buffer: std::sync::Arc::new(buffer.into_boxed_slice()),
            block_entities: Vec::new(),
        },
        light_data: Default::default(),
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(packet));
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: azalea_core::entity_id::MinecraftEntityId(11),
            uuid: Uuid::from_u128(42),
            entity_type: EntityKind::Zombie,
            position: Vec3::new(1.0, 64.0, 1.0),
            movement: azalea_core::delta::LpVec3::Zero,
            x_rot: 0,
            y_rot: 64,
            y_head_rot: 64,
            data: 0,
        }),
    );
    assert!(world.chunk_count() > 0);
    assert!(world.entity(11).is_some());

    // A new connection (reconnect /transfer) resets the world.
    world.follow(1);
    assert_eq!(world.chunk_count(), 0);
    assert!(world.entity(11).is_none());
    assert!(world.player_position().is_none());
    assert!(world.status().is_valid());
}