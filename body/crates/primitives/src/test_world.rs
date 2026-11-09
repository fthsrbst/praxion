//! A minimal real world for primitive tests that need block states: a loaded
//! square of chunks, the player at [`feet`], a `set_block` and a swept
//! perception.
//!
//! The world is built from the same login, chunk and player packets a server
//! sends, so `World::block_state` and `Perception` see real chunks instead of an
//! invalid store.

use std::sync::Arc;

use azalea_buf::AzBuf;
use azalea_core::game_type::{GameMode, OptionalGameType};
use azalea_core::position::{ChunkSectionBlockPos, Vec3};
use azalea_protocol::common::movements::MoveFlags;
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        ClientboundBlockUpdate, ClientboundContainerSetContent, ClientboundGamePacket,
        ClientboundLevelChunkWithLight, ClientboundLogin, ServerboundGamePacket,
        ServerboundMovePlayerPos, c_level_chunk_with_light::ClientboundLevelChunkPacketData,
    },
};
use azalea_registry::{DataRegistry, data::DimensionKind, identifier::Identifier};
use azalea_world::Chunk;
use praxion_perception::{Config, Perception};
use praxion_world::{BlockPos, BlockState, ItemStack, World};

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

fn chunk_packet_with(x: i32, z: i32, chunk: &Chunk) -> ClientboundLevelChunkWithLight {
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

fn chunk_packet(x: i32, z: i32) -> ClientboundLevelChunkWithLight {
    chunk_packet_with(x, z, &Chunk::default())
}

/// A chunk whose sections up to `top` are set by `fill`; every cell above
/// `top` stays air, so the sections above it are not even walked.
fn chunk_with_blocks(
    min_y: i32,
    x: i32,
    z: i32,
    top: i32,
    fill: &impl Fn(BlockPos) -> BlockState,
) -> Chunk {
    let mut chunk = Chunk::default();
    for (index, section) in chunk.sections.iter_mut().enumerate() {
        let base_y = (i32::try_from(index).expect("a chunk has 24 sections") + (min_y >> 4)) * 16;
        if base_y > top {
            continue;
        }
        for ly in 0..16u8 {
            let y = base_y + i32::from(ly);
            if y > top {
                break;
            }
            for lz in 0..16u8 {
                for lx in 0..16u8 {
                    let pos = BlockPos::new(
                        x * 16 + i32::from(lx),
                        y,
                        z * 16 + i32::from(lz),
                    );
                    let state = fill(pos);
                    if !state.is_air() {
                        section.get_and_set_block_state(
                            ChunkSectionBlockPos::new(lx, ly, lz),
                            state,
                        );
                    }
                }
            }
        }
    }
    chunk
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
    move_player(&mut world, BlockPos::new(0, 64, 0));
    world
}

/// A valid world like [`world`] but with a `chunks` by `chunks` square of
/// chunks loaded from (0, 0). Every block at or below `top` is `fill(pos)`,
/// every block above `top` is air.
pub(crate) fn world_with(chunks: i32, top: i32, fill: impl Fn(BlockPos) -> BlockState) -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    let min_y = world.min_y().expect("the login resolves a dimension");
    for x in 0..chunks {
        for z in 0..chunks {
            let chunk = chunk_with_blocks(min_y, x, z, top, &fill);
            world.apply_clientbound_play(
                0,
                &ClientboundGamePacket::LevelChunkWithLight(chunk_packet_with(x, z, &chunk)),
            );
        }
    }
    move_player(&mut world, BlockPos::new(0, 64, 0));
    world
}

/// Move the followed player's feet to `feet`, as a server position packet does.
pub(crate) fn move_player(world: &mut World, feet: BlockPos) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: Vec3::new(
                f64::from(feet.x) + 0.5,
                f64::from(feet.y),
                f64::from(feet.z) + 0.5,
            ),
            flags: MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }),
    );
}

/// Move the followed player's feet to exactly `position`, as the client's own
/// position packet does. Unlike [`move_player`] this keeps sub-block precision,
/// so a stepped body can report every tick's real position to perception.
pub(crate) fn move_body(world: &mut World, position: Vec3) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPos(ServerboundMovePlayerPos {
            pos: position,
            flags: MoveFlags {
                on_ground: true,
                horizontal_collision: false,
            },
        }),
    );
}

/// Give the followed player a full inventory whose hotbar slot 0 holds `item`,
/// so a primitive can hold it without a swap showing up in the world.
pub(crate) fn hold(world: &mut World, item: azalea_registry::builtin::ItemKind) {
    let mut items = vec![ItemStack::Empty; 46];
    items[36] = ItemStack::new(item, 1);
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetContent(ClientboundContainerSetContent {
            container_id: 0,
            state_id: 1,
            items,
            carried_item: ItemStack::Empty,
        }),
    );
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

/// Put `count` of `item` into player-inventory `slot` (window 0 numbering), as
/// a server slot update.
pub(crate) fn give(
    world: &mut World,
    slot: u16,
    item: azalea_registry::builtin::ItemKind,
    count: i32,
) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetSlot(
            azalea_protocol::packets::game::ClientboundContainerSetSlot {
                container_id: 0,
                state_id: 0,
                slot,
                item_stack: azalea_inventory::ItemStack::new(item, count),
            },
        ),
    );
}

/// The server opens menu `id` of `kind` (`open_screen`). Its content arrives
/// separately, see [`set_menu_content`].
pub(crate) fn open_menu(world: &mut World, id: i32, kind: azalea_registry::builtin::MenuKind) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::OpenScreen(azalea_protocol::packets::game::ClientboundOpenScreen {
            container_id: id,
            menu_type: kind,
            title: "Menu".into(),
        }),
    );
}

/// The server sends the full slot list of the open menu `id`
/// (`container_set_content`).
pub(crate) fn set_menu_content(world: &mut World, id: i32, slots: Vec<azalea_inventory::ItemStack>) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetContent(
            azalea_protocol::packets::game::ClientboundContainerSetContent {
                container_id: id,
                state_id: 0,
                items: slots,
                carried_item: azalea_inventory::ItemStack::Empty,
            },
        ),
    );
}
