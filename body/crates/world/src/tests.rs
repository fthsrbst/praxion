use std::sync::Arc;

use azalea_block::BlockState;
use azalea_buf::AzBuf;
use azalea_core::{
    delta::{LpVec3, PositionDelta8},
    entity_id::MinecraftEntityId,
    game_type::{GameMode, OptionalGameType},
    heightmap_kind::HeightmapKind,
    position::{BlockPos, ChunkPos, ChunkSectionBlockPos, ChunkSectionPos, Vec3},
};
use azalea_entity::{EntityDataItem, EntityDataValue, LookDirection, Pose};
use azalea_protocol::{
    common::movements::{PositionMoveRotation, RelativeMovements},
    packets::{
        ProtocolPacket,
        common::CommonPlayerSpawnInfo,
        game::{
            ClientboundAddEntity, ClientboundBlockChangedAck, ClientboundBlockUpdate,
            ClientboundChunkBatchFinished, ClientboundChunkBatchStart, ClientboundForgetLevelChunk,
            ClientboundContainerSetContent, ClientboundContainerSetSlot, ClientboundGameEvent,
            ClientboundGamePacket, ClientboundLevelChunkWithLight, ClientboundLogin,
            ClientboundMoveEntityPos, ClientboundPlayerPosition, ClientboundRemoveEntities,
            ClientboundRespawn, ClientboundSectionBlocksUpdate, ClientboundSetEntityData,
            ClientboundSetHealth, ClientboundSetPlayerInventory, ClientboundSetTime,
            ClientboundTeleportEntity, ServerboundClientTickEnd, ServerboundGamePacket,
            ServerboundMovePlayerPos,
            c_game_event::EventType,
            c_level_chunk_with_light::ClientboundLevelChunkPacketData,
            c_move_entity_pos_rot::{Properties, VecDelta},
            c_section_blocks_update::BlockStateWithPosition,
            c_set_time::ClockState,
        },
    },
};
use azalea_registry::{
    DataRegistry,
    builtin::{BlockKind, EntityKind, ItemKind},
    data::{DimensionKind, WorldClock},
    identifier::Identifier,
};
use azalea_world::Chunk;
use azalea_world::heightmap::Heightmap;
use uuid::Uuid;

use super::*;

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

fn active_world() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    world
}

#[test]
fn login_sets_dimension_mode_and_entity_id() {
    let world = active_world();
    assert_eq!(world.own_entity_id(), Some(7));
    assert_eq!(world.game_mode(), Some(GameMode::Survival));
    let dimension = world.dimension().expect("dimension");
    assert_eq!(dimension.dimension(), "minecraft:overworld");
    assert_eq!(dimension.height(), 384);
    assert_eq!(dimension.min_y(), -64);
    assert_eq!(dimension.source(), DimensionSource::Fallback);
    assert!(world.status().is_valid());
}

#[test]
fn teleport_then_moves_update_position_and_rotation() {
    let mut world = active_world();
    let teleport = ClientboundPlayerPosition {
        id: 1,
        change: PositionMoveRotation {
            pos: Vec3::new(10.0, 64.0, -3.0),
            delta: Vec3::ZERO,
            look_direction: LookDirection::new(90.0, 0.0),
        },
        relative: RelativeMovements::default(),
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::PlayerPosition(teleport));
    assert_eq!(world.player_position(), Some(Vec3::new(10.0, 64.0, -3.0)));

    // Relative teleport: x adds, z is absolute.
    let relative = ClientboundPlayerPosition {
        id: 2,
        change: PositionMoveRotation {
            pos: Vec3::new(1.0, 0.5, 40.0),
            delta: Vec3::ZERO,
            look_direction: LookDirection::new(0.0, 0.0),
        },
        relative: RelativeMovements { x: true, y: true, ..RelativeMovements::default() },
    };
    world.apply_clientbound_play(0, &ClientboundGamePacket::PlayerPosition(relative));
    assert_eq!(world.player_position(), Some(Vec3::new(11.0, 64.5, 40.0)));

    let moved = ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
        pos: Vec3::new(12.0, 65.0, 41.0),
        flags: Default::default(),
    });
    world.apply_serverbound_play(0, &moved);
    assert_eq!(world.player_position(), Some(Vec3::new(12.0, 65.0, 41.0)));
}

#[test]
fn health_experience_and_hotbar_slot() {
    let mut world = active_world();
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetHealth(ClientboundSetHealth {
            health: 18.5,
            food: 17,
            saturation: 4.25,
        }),
    );
    let health = world.health().expect("health");
    assert_eq!(health.health(), 18.5);
    assert_eq!(health.food(), 17);
    assert_eq!(health.saturation(), 4.25);

    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::SetCarriedItem(
            azalea_protocol::packets::game::ServerboundSetCarriedItem { slot: 4 },
        ),
    );
    assert_eq!(world.selected_hotbar_slot(), Some(4));
}

#[test]
fn inventory_follows_set_content_and_set_slot() {
    let mut world = active_world();
    let items = vec![
        ItemStack::new(ItemKind::Stone, 32),
        ItemStack::Empty,
        ItemStack::new(ItemKind::Dirt, 3),
    ];
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
            container_id: 0,
            state_id: 1,
            items,
            carried_item: ItemStack::new(ItemKind::Torch, 2),
        }),
    );
    assert_eq!(world.inventory().len(), 3);
    assert_eq!(world.inventory()[0].kind(), ItemKind::Stone);
    assert_eq!(world.inventory()[0].count(), 32);
    assert_eq!(world.carried_item().kind(), ItemKind::Torch);

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetSlot(ClientboundContainerSetSlot {
            container_id: 0,
            state_id: 2,
            slot: 5,
            item_stack: ItemStack::new(ItemKind::IronIngot, 7),
        }),
    );
    assert_eq!(world.inventory().len(), 6);
    assert_eq!(world.inventory()[5].kind(), ItemKind::IronIngot);
    assert_eq!(world.inventory()[5].count(), 7);
}

#[test]
fn inventory_slots_follow_the_player_container_layout() {
    // One test pins the 46-slot layout the port and the console panel both
    // treat as true (critique finding 10): 0 result, 1-4 grid, 5-8 armour,
    // 9-35 storage, 36-44 hotbar, 45 offhand.
    let mut world = active_world();
    let mut items = vec![ItemStack::Empty; 47];
    items[0] = ItemStack::new(ItemKind::CraftingTable, 1); // result preview, never owned
    items[5] = ItemStack::new(ItemKind::IronHelmet, 1);
    items[9] = ItemStack::new(ItemKind::OakLog, 12);
    items[36] = ItemStack::new(ItemKind::StonePickaxe, 1);
    items[45] = ItemStack::new(ItemKind::Torch, 8);
    items[46] = ItemStack::new(ItemKind::Dirt, 1); // past the container: never sent
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
            container_id: 0,
            state_id: 1,
            items,
            carried_item: ItemStack::Empty,
        }),
    );

    let slots = world.inventory_slots();
    assert_eq!(
        slots.iter().map(|slot| slot.slot).collect::<Vec<_>>(),
        vec![5, 9, 36, 45],
        "slot 0 is the result preview and empty slots are skipped"
    );
    assert_eq!(slots[0].item, "iron_helmet");
    assert_eq!(slots[1].item, "oak_log");
    assert_eq!(slots[1].count, 12);
    assert_eq!(slots[2].item, "stone_pickaxe");
    assert_eq!(slots[3].item, "torch");

    // The hand is a hotbar index 0..=8, straight from the server's packet; the
    // respawn reset to 0 has its own test (critique finding 8).
    assert_eq!(world.selected_hotbar_slot(), None, "no held-slot fact yet");
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetHeldSlot(azalea_protocol::packets::game::ClientboundSetHeldSlot {
            slot: 2,
        }),
    );
    assert_eq!(world.selected_hotbar_slot(), Some(2));
}

#[test]
fn entities_are_added_moved_and_removed() {
    let mut world = active_world();
    let uuid = Uuid::from_u128(42);
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: MinecraftEntityId(11),
            uuid,
            entity_type: EntityKind::Zombie,
            position: Vec3::new(1.0, 64.0, 1.0),
            movement: LpVec3::Zero,
            x_rot: 0,
            y_rot: 64,
            y_head_rot: 64,
            data: 0,
        }),
    );
    let entity = world.entity(11).expect("entity");
    assert_eq!(entity.uuid(), uuid);
    assert_eq!(entity.position(), Vec3::new(1.0, 64.0, 1.0));
    assert!((entity.rotation().0 - 90.0).abs() < 0.001);

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::MoveEntityPos(ClientboundMoveEntityPos {
            entity_id: MinecraftEntityId(11),
            properties: Properties::new(),
            delta: VecDelta::Linear(PositionDelta8 { xa: 4096, ya: 0, za: -2048 }),
        }),
    );
    assert_eq!(world.entity(11).unwrap().position(), Vec3::new(2.0, 64.0, 0.5));

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::TeleportEntity(ClientboundTeleportEntity {
            id: MinecraftEntityId(11),
            change: PositionMoveRotation {
                pos: Vec3::new(5.0, 70.0, 5.0),
                delta: Vec3::ZERO,
                look_direction: LookDirection::new(180.0, 45.0),
            },
            relative: RelativeMovements::default(),
            on_ground: true,
        }),
    );
    assert_eq!(world.entity(11).unwrap().position(), Vec3::new(5.0, 70.0, 5.0));

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::RemoveEntities(ClientboundRemoveEntities {
            entity_ids: vec![MinecraftEntityId(11)],
        }),
    );
    assert!(world.entity(11).is_none());
}

#[test]
fn invalid_world_answers_none() {
    let mut world = active_world();
    world.invalidate(0, "tee gap");
    assert!(!world.status().is_valid());
    assert_eq!(world.status().reason(), Some("tee gap"));
    assert_eq!(world.block_state(BlockPos::new(0, 64, 0)), None);
    assert!(world.entities().next().is_none());
}

fn find_block(name: &str) -> BlockState {
    for id in 0..4096u32 {
        if let Ok(state) = BlockState::try_from(id)
            && BlockDescription::of(state).name() == name
        {
            return state;
        }
    }
    panic!("block {name} not found");
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

#[test]
fn chunks_and_block_updates_are_applied() {
    let mut world = active_world();
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)),
    );
    assert_eq!(world.chunk_count(), 1);
    let stone = find_block("minecraft:stone");
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos: BlockPos::new(0, 64, 0),
            block_state: stone,
        }),
    );
    assert_eq!(world.block_state(BlockPos::new(0, 64, 0)), Some(stone));
    assert_eq!(
        world.block_description(BlockPos::new(0, 64, 0)).unwrap().name(),
        "minecraft:stone"
    );
}

#[test]
fn surface_y_reads_and_tracks_the_world_surface_heightmap() {
    // 256 columns at ceil(log2(384 + 1)) = 9 bits each pack into 36 u64 words.
    fn surface_heightmap() -> Heightmap {
        Heightmap::new(
            HeightmapKind::WorldSurface,
            384,
            -64,
            vec![0u64; 36].into_boxed_slice(),
        )
    }

    let mut world = active_world();
    assert_eq!(world.surface_y(0, 0), None, "no chunk is loaded");

    // A chunk holding only a `WorldSurface` heightmap. Column (0, 0) says its
    // first free y is 65, every other column is untouched (min_y).
    let mut chunk = Chunk::default();
    let mut heightmap = surface_heightmap();
    heightmap.set_height(0, 0, 65);
    chunk
        .heightmaps
        .insert(HeightmapKind::WorldSurface, heightmap);
    world.chunks.insert(ChunkPos::new(0, 0), chunk);

    assert_eq!(world.surface_y(0, 0), Some(65));
    assert_eq!(world.surface_y(15, 15), Some(-64), "an untouched column");
    assert_eq!(world.surface_y(16, 0), None, "a missing chunk");

    // x = -1 is local column 15 of chunk -1, not chunk 0.
    assert_eq!(world.surface_y(-1, 0), None, "chunk (-1, 0) is not loaded");
    let mut left = Chunk::default();
    let mut left_heightmap = surface_heightmap();
    left_heightmap.set_height(15, 0, 70);
    left.heightmaps
        .insert(HeightmapKind::WorldSurface, left_heightmap);
    world.chunks.insert(ChunkPos::new(-1, 0), left);
    assert_eq!(world.surface_y(-1, 0), Some(70));

    // A block placed above the recorded surface raises the heightmap, which is
    // what makes the answer current after every block update.
    let stone = find_block("minecraft:stone");
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos: BlockPos::new(0, 70, 0),
            block_state: stone,
        }),
    );
    assert_eq!(world.surface_y(0, 0), Some(71));
}

#[test]
fn time_weather_batches_and_entity_metadata() {
    let mut world = active_world();
    let clocks = [(
        WorldClock::new_raw(0),
        ClockState { total_ticks: 6000, partial_tick: 0.0, rate: 1.0 },
    )]
    .into_iter()
    .collect();
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetTime(ClientboundSetTime {
            game_time: 123_456,
            clock_updates: clocks,
        }),
    );
    assert_eq!(world.game_time(), 123_456);
    assert_eq!(world.day_time(), 6000);

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::GameEvent(ClientboundGameEvent {
            event: EventType::StartRaining,
            param: 0.0,
        }),
    );
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::GameEvent(ClientboundGameEvent {
            event: EventType::RainLevelChange,
            param: 0.75,
        }),
    );
    assert!(world.weather().raining());
    assert_eq!(world.weather().rain_level(), 0.75);

    world.apply_clientbound_play(0, &ClientboundGamePacket::ChunkBatchStart(ClientboundChunkBatchStart));
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ChunkBatchFinished(ClientboundChunkBatchFinished { batch_size: 7 }),
    );
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockChangedAck(ClientboundBlockChangedAck { seq: 42 }),
    );
    assert_eq!(world.chunk_batches_started(), 1);
    assert_eq!(world.chunk_batches_finished(), 1);
    assert_eq!(world.last_chunk_batch_size(), Some(7));
    assert_eq!(world.last_block_acked(), Some(42));

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: MinecraftEntityId(21),
            uuid: Uuid::from_u128(7),
            entity_type: EntityKind::Item,
            position: Vec3::new(2.0, 64.0, 2.0),
            movement: LpVec3::Zero,
            x_rot: 0,
            y_rot: 0,
            y_head_rot: 0,
            data: 0,
        }),
    );
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
            id: MinecraftEntityId(21),
            packed_items: azalea_entity::EntityMetadataItems(vec![
                EntityDataItem {
                    index: 8,
                    value: EntityDataValue::ItemStack(ItemStack::new(ItemKind::Dirt, 3)),
                },
                EntityDataItem {
                    index: 6,
                    value: EntityDataValue::Pose(Pose::Crouching),
                },
            ]),
        }),
    );
    let entity = world.entity(21).expect("item entity");
    assert_eq!(entity.item_stack().unwrap().kind(), ItemKind::Dirt);
    assert_eq!(entity.item_stack().unwrap().count(), 3);
    assert_eq!(entity.pose(), Some(Pose::Crouching));
}

#[test]
fn packet_name_is_reported_for_each_variant() {
    // A tiny guard against an enum variant being added without a name arm.
    let login = ClientboundGamePacket::Login(login());
    assert_eq!(login.name(), "login");
}

// --- Review findings 2, 5, 6, 7 and 8 --------------------------------------

#[test]
fn section_blocks_update_coordinates_are_checked() {
    let mut world = active_world();
    world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)));
    let stone = find_block("minecraft:stone");
    let update = |section: ChunkSectionPos| {
        ClientboundGamePacket::SectionBlocksUpdate(ClientboundSectionBlocksUpdate {
            section_pos: section,
            states: vec![BlockStateWithPosition {
                pos: ChunkSectionBlockPos { x: 0, y: 0, z: 0 },
                state: stone,
            }],
        })
    };

    // Finding 2: section.x * 16 leaves i32 for i32::MAX / 8. Before the fix this
    // wrapped in release and panicked in debug; now the update is dropped.
    world.apply_clientbound_play(0, &update(ChunkSectionPos { x: i32::MAX / 8, y: 4, z: 0 }));
    assert_eq!(world.warning_counts()[&WarningKind::SectionOutOfBounds], 1);
    // A section below and above the world height is dropped too (-64..320).
    world.apply_clientbound_play(0, &update(ChunkSectionPos { x: 0, y: -1000, z: 0 }));
    world.apply_clientbound_play(0, &update(ChunkSectionPos { x: 0, y: 1000, z: 0 }));
    assert_eq!(world.warning_counts()[&WarningKind::SectionOutOfBounds], 3);

    // The same packet with a sane position still applies at y = 4 * 16.
    world.apply_clientbound_play(0, &update(ChunkSectionPos { x: 0, y: 4, z: 0 }));
    assert_eq!(world.block_state(BlockPos::new(0, 64, 0)), Some(stone));
    assert_eq!(world.warning_counts()[&WarningKind::SectionOutOfBounds], 3);
}

#[test]
fn player_inventory_slot_out_of_range_is_ignored() {
    // Finding 5: `set_player_inventory` sends the slot as a VarInt. `slot as u16`
    // turned 100_000 into 34_464 and could allocate a 65_536 slot inventory.
    let mut world = active_world();
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetPlayerInventory(ClientboundSetPlayerInventory {
            slot: 100_000,
            contents: ItemStack::new(ItemKind::Stone, 1),
        }),
    );
    assert!(world.inventory().is_empty());
    assert_eq!(world.warning_counts()[&WarningKind::SlotOutOfRange], 1);

    // Slot 45 is the last real one (the offhand).
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetPlayerInventory(ClientboundSetPlayerInventory {
            slot: 45,
            contents: ItemStack::new(ItemKind::Dirt, 2),
        }),
    );
    assert_eq!(world.inventory().len(), 46);
    assert_eq!(world.inventory()[45].kind(), ItemKind::Dirt);

    // container_set_slot carries a u16 and has the same bound.
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetSlot(ClientboundContainerSetSlot {
            container_id: 0,
            state_id: 3,
            slot: 46,
            item_stack: ItemStack::new(ItemKind::Diamond, 1),
        }),
    );
    assert_eq!(world.inventory().len(), 46);
    assert_eq!(world.warning_counts()[&WarningKind::SlotOutOfRange], 2);
}

#[test]
fn world_height_bounds_do_not_overflow() {
    // Finding 6: `min_y + height` overflows i32 for a hostile dimension type;
    // the interval is computed in i64 and compares the same way.
    assert!(y_in_world(i32::MAX, u32::MAX, i32::MAX));
    assert!(!y_in_world(i32::MAX, u32::MAX, i32::MIN));
    assert!(y_in_world(i32::MIN, u32::MAX, i32::MAX - 1));
    assert!(!y_in_world(i32::MIN, u32::MAX, i32::MAX));

    // The vanilla overworld fallback still answers as before.
    assert!(y_in_world(-64, 384, -64));
    assert!(y_in_world(-64, 384, 319));
    assert!(!y_in_world(-64, 384, 320));
    assert!(!y_in_world(-64, 384, -65));
}

#[test]
fn warnings_are_counted_and_bounded() {
    // Finding 7: the message list is capped, the counts are not.
    let mut world = active_world();
    let stone = find_block("minecraft:stone");
    let total = MAX_WARNING_MESSAGES as u64 + 50;
    for x in 0..total as i32 {
        world.apply_clientbound_play(
            0,
            &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
                pos: BlockPos::new(x, 64, 0),
                block_state: stone,
            }),
        );
    }
    assert_eq!(world.warnings().len(), MAX_WARNING_MESSAGES);
    assert_eq!(world.warning_counts()[&WarningKind::ChunkNotLoaded], total);
}

#[test]
fn closed_followed_connection_stops_updating() {
    // Review note: `World::closed` used to be a no-op, so packets that arrived
    // for the followed connection after `Closed` still changed the world.
    let health = |value: f32| {
        ClientboundGamePacket::SetHealth(ClientboundSetHealth {
            health: value,
            food: 20,
            saturation: 5.0,
        })
    };
    let mut world = active_world();
    world.apply_clientbound_play(0, &health(19.0));

    // Closing a connection we do not follow changes nothing.
    world.closed(1);
    assert!(!world.is_ended());

    // Closing the followed one keeps its state and ignores later packets.
    world.closed(0);
    assert!(world.is_ended());
    world.apply_clientbound_play(0, &health(3.0));
    assert_eq!(world.health().expect("health").health(), 19.0);
    world.apply_serverbound_play(0, &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd));
    assert_eq!(world.tick(), 0);

    // A new session on the same connection id starts clean and accepts again.
    world.opened(0);
    assert!(!world.is_ended());
    world.apply_clientbound_play(0, &health(3.0));
    assert_eq!(world.health().expect("health").health(), 3.0);
}

#[test]
fn closed_is_ignored_for_a_connection_that_is_followed_later() {
    let mut world = World::new();
    world.follow(0);
    world.closed(5);
    assert!(!world.is_ended());
    world.follow(5);
    assert!(!world.is_ended());
    assert!(world.status().is_valid());
}

// --- Chunk revisions and section views ---------------------------------------

#[test]
fn revisions_follow_every_block_change() {
    let mut world = active_world();
    let start = world.revision();
    assert_eq!(world.chunk_revision(ChunkPos::new(0, 0)), None);

    world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)));
    let loaded = world.revision();
    assert!(loaded > start);
    assert_eq!(world.chunk_revision(ChunkPos::new(0, 0)), Some(loaded));

    let stone = find_block("minecraft:stone");
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos: BlockPos::new(0, 64, 0),
            block_state: stone,
        }),
    );
    let updated = world.revision();
    assert!(updated > loaded);
    assert_eq!(world.chunk_revision(ChunkPos::new(0, 0)), Some(updated));

    // A section update bumps the revision of the chunk it is in, once.
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SectionBlocksUpdate(ClientboundSectionBlocksUpdate {
            section_pos: ChunkSectionPos { x: 0, y: 4, z: 0 },
            states: vec![BlockStateWithPosition {
                pos: ChunkSectionBlockPos { x: 1, y: 1, z: 1 },
                state: stone,
            }],
        }),
    );
    let section = world.revision();
    assert!(section > updated);
    assert_eq!(world.chunk_revision(ChunkPos::new(0, 0)), Some(section));

    // Unloading the chunk drops its revision and moves the counter, so a
    // reload can never bring the old one back.
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ForgetLevelChunk(ClientboundForgetLevelChunk {
            pos: ChunkPos::new(0, 0),
        }),
    );
    assert_eq!(world.chunk_revision(ChunkPos::new(0, 0)), None);
    assert!(world.revision() > section);
    world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)));
    let reloaded = world.chunk_revision(ChunkPos::new(0, 0)).expect("reloaded");
    assert!(reloaded > section, "a reloaded chunk gets a fresh revision");
}

#[test]
fn a_section_view_reads_states_and_answers_the_palette_question() {
    let mut world = active_world();
    world.apply_clientbound_play(0, &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(0, 0)));
    let stone = find_block("minecraft:stone");
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos: BlockPos::new(1, 65, 2),
            block_state: stone,
        }),
    );

    let section_pos = ChunkSectionPos { x: 0, y: 4, z: 0 };
    let view = world.section(section_pos).expect("section of a loaded chunk");
    assert_eq!(view.state(1, 1, 2), stone);
    assert_eq!(view.state(0, 0, 0), BlockState::AIR);
    assert!(view.may_contain(|state| state == stone));
    let dirt = BlockState::from(BlockKind::Dirt);
    assert!(!view.may_contain(|state| state == dirt));

    // A section that only holds air cannot contain stone.
    let air = world.section(ChunkSectionPos { x: 0, y: 10, z: 0 }).expect("air section");
    assert!(!air.may_contain(|state| state == stone));

    // Outside the world height and outside the loaded chunks there is none.
    assert!(world.section(ChunkSectionPos { x: 0, y: 100, z: 0 }).is_none());
    assert!(world.section(ChunkSectionPos { x: 5, y: 4, z: 0 }).is_none());

    // A desynchronised world answers nothing.
    world.invalidate(0, "test");
    assert!(world.section(section_pos).is_none());
    assert_eq!(world.chunk_revision(ChunkPos::new(0, 0)), None);
}

// --- The player's own entity data: burning and air (step 9, finding 2) -----

/// One `set_entity_data` for `entity` with exactly these items.
fn set_entity_data(world: &mut World, entity: i32, items: Vec<EntityDataItem>) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
            id: MinecraftEntityId(entity),
            packed_items: azalea_entity::EntityMetadataItems(items),
        }),
    );
}

fn shared_flags(flags: u8) -> EntityDataItem {
    EntityDataItem {
        index: 0,
        value: EntityDataValue::Byte(flags),
    }
}

fn air_supply(ticks: i32) -> EntityDataItem {
    EntityDataItem {
        index: 1,
        value: EntityDataValue::Int(ticks),
    }
}

#[test]
fn own_entity_data_carries_burning_and_air() {
    let mut world = active_world();
    // No packet yet: unknown, not "safe".
    assert_eq!(world.own_burning(), None);
    assert_eq!(world.own_air(), None);

    // Bit 0 is "on fire"; bit 1 is shift-down and must not set it.
    set_entity_data(&mut world, 7, vec![shared_flags(0b0000_0010)]);
    assert_eq!(world.own_burning(), Some(false));
    assert_eq!(world.own_air(), None, "no air packet yet");

    set_entity_data(&mut world, 7, vec![shared_flags(0b0000_0011), air_supply(120)]);
    assert_eq!(world.own_burning(), Some(true));
    assert_eq!(world.own_air(), Some(120));

    // A later packet replaces only the index it names.
    set_entity_data(&mut world, 7, vec![air_supply(300)]);
    assert_eq!(world.own_air(), Some(300));
    assert_eq!(world.own_burning(), Some(true), "the flags byte was not resent");
}

#[test]
fn own_entity_data_ignores_other_entities() {
    let mut world = active_world();
    set_entity_data(&mut world, 21, vec![shared_flags(0b1), air_supply(10)]);
    assert_eq!(world.own_burning(), None);
    assert_eq!(world.own_air(), None);
}

#[test]
fn own_air_is_clamped_at_zero_and_unknown_after_respawn() {
    let mut world = active_world();
    // Vanilla drives the air supply to -20 while drowning.
    set_entity_data(&mut world, 7, vec![air_supply(-20)]);
    assert_eq!(world.own_air(), Some(0));

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::Respawn(ClientboundRespawn {
            common: spawn_info(),
            data_to_keep: 0,
        }),
    );
    assert_eq!(world.own_air(), None, "a new entity has no facts yet");
    assert_eq!(world.own_burning(), None);
}

#[test]
fn respawn_resets_the_selected_hotbar_slot() {
    let mut world = active_world();
    // Our own selection packet writes the mirror, as it does on the wire.
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::SetCarriedItem(
            azalea_protocol::packets::game::ServerboundSetCarriedItem { slot: 6 },
        ),
    );
    assert_eq!(world.selected_hotbar_slot(), Some(6));

    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::Respawn(ClientboundRespawn {
            common: spawn_info(),
            data_to_keep: 0,
        }),
    );
    assert_eq!(
        world.selected_hotbar_slot(),
        Some(0),
        "the server's new player starts on slot 0 and sends no held-slot packet"
    );
}

#[test]
fn a_desynchronised_world_answers_no_own_fact() {
    let mut world = active_world();
    set_entity_data(&mut world, 7, vec![shared_flags(0b1), air_supply(5)]);
    assert_eq!(world.own_air(), Some(5));
    world.invalidate(0, "test");
    assert_eq!(world.own_air(), None);
    assert_eq!(world.own_burning(), None);
}

// --- 26.3 world clock (run 8: day_time stayed 0 the whole run) ---------------
//
// The server sends a clock's state only on join, on an `advance_time` change
// and when a command moves it; the 20-tick `set_time` carries no clocks. The
// client extrapolates per tick by `rate` (`ClientClockManager.tick`).

fn set_time(world: &mut World, game_time: u64, clocks: &[(u32, u64, f32, f32)]) {
    let clock_updates = clocks
        .iter()
        .map(|&(id, total_ticks, partial_tick, rate)| {
            (WorldClock::new_raw(id), ClockState { total_ticks, partial_tick, rate })
        })
        .collect();
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetTime(ClientboundSetTime { game_time, clock_updates }),
    );
}

fn client_ticks(world: &mut World, n: u32) {
    for _ in 0..n {
        world.apply_serverbound_play(
            0,
            &ServerboundGamePacket::ClientTickEnd(ServerboundClientTickEnd),
        );
    }
}

#[test]
fn a_running_clock_advances_with_client_ticks() {
    let mut world = active_world();
    // `time set 0` with advance_time on: the only clock update of the run.
    set_time(&mut world, 1_000, &[(0, 0, 0.0, 1.0)]);
    assert_eq!(world.day_time(), 0);
    client_ticks(&mut world, 4_660);
    assert_eq!(world.day_time(), 4_660);
    assert_eq!(world.game_time(), 5_660);

    // The 20-tick sync carries only the game time; the clock catches up to
    // it (the server ran 10 ticks further than the client here).
    set_time(&mut world, 5_670, &[]);
    assert_eq!(world.day_time(), 4_670);
    assert_eq!(world.game_time(), 5_670);
    // ...and back when the server is behind the client's own count.
    set_time(&mut world, 5_665, &[]);
    assert_eq!(world.day_time(), 4_665);
}

#[test]
fn a_fractional_rate_carries_the_partial_tick() {
    let mut world = active_world();
    set_time(&mut world, 0, &[(0, 100, 0.0, 0.5)]);
    client_ticks(&mut world, 5);
    assert_eq!(world.day_time(), 102, "2.5 ticks: two whole, half carried");
    client_ticks(&mut world, 1);
    assert_eq!(world.day_time(), 103);
    set_time(&mut world, 10, &[(0, 1_000, 0.0, 3.0)]);
    client_ticks(&mut world, 4);
    assert_eq!(world.day_time(), 1_012);
}

#[test]
fn a_frozen_or_paused_clock_stays_put() {
    let mut world = active_world();
    // `advance_time false` or a paused clock arrives as rate 0.
    set_time(&mut world, 0, &[(0, 13_000, 0.0, 0.0)]);
    client_ticks(&mut world, 500);
    set_time(&mut world, 520, &[]);
    assert_eq!(world.day_time(), 13_000);
    assert_eq!(world.game_time(), 520, "the game time still runs");

    // A frozen tick rate stops the client's own advance as well.
    set_time(&mut world, 520, &[(0, 2_000, 0.0, 1.0)]);
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::TickingState(
            azalea_protocol::packets::game::ClientboundTickingState {
                tick_rate: 20.0,
                is_frozen: true,
            },
        ),
    );
    client_ticks(&mut world, 50);
    assert_eq!(world.day_time(), 2_000);
    assert_eq!(world.game_time(), 520);
}

#[test]
fn time_set_replaces_the_base() {
    let mut world = active_world();
    set_time(&mut world, 0, &[(0, 6_000, 0.0, 1.0)]);
    client_ticks(&mut world, 300);
    assert_eq!(world.day_time(), 6_300);
    // `time set night` jumps; ticking continues from the new base.
    set_time(&mut world, 300, &[(0, 13_000, 0.0, 1.0)]);
    assert_eq!(world.day_time(), 13_000);
    client_ticks(&mut world, 10);
    assert_eq!(world.day_time(), 13_010);
    // An update for another clock (the_end, id 1) leaves the overworld alone.
    set_time(&mut world, 310, &[(1, 99, 0.0, 1.0)]);
    assert_eq!(world.day_time(), 13_010);
}

#[test]
fn the_overworld_clock_id_comes_from_the_registry_order() {
    let mut world = World::new();
    world.apply_clientbound_config(
        0,
        &ClientboundConfigPacket::RegistryData(
            azalea_protocol::packets::config::ClientboundRegistryData {
                registry_id: Identifier::new("minecraft:world_clock"),
                entries: vec![
                    (Identifier::new("minecraft:the_end"), None),
                    (Identifier::new("minecraft:overworld"), None),
                ],
            },
        ),
    );
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    set_time(&mut world, 0, &[(0, 99, 0.0, 1.0), (1, 18_000, 0.0, 1.0)]);
    assert_eq!(world.day_time(), 18_000);
}

#[test]
fn a_new_login_starts_a_fresh_clock() {
    let mut world = active_world();
    set_time(&mut world, 0, &[(0, 6_000, 0.0, 1.0)]);
    client_ticks(&mut world, 10);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    assert_eq!(world.day_time(), 0, "no clock until the join sync");
    set_time(&mut world, 10, &[(0, 7_000, 0.0, 1.0)]);
    client_ticks(&mut world, 1);
    assert_eq!(world.day_time(), 7_001);
}
