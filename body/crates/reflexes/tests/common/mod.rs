//! Hand-built worlds for the reflex and arbiter tests.
//!
//! Everything goes through the same packet path the real body uses: a login,
//! nine empty chunks, then block updates and entity adds. Perception reads this
//! world; the tests never reach around it. The player is a real
//! `praxion_headless::physics::Body`, stepped once per tick, and its position
//! and look are written back to the world after every tick.

#![allow(dead_code)]

use std::sync::Arc;

use azalea_block::BlockState;
use azalea_buf::AzBuf;
use azalea_core::{
    delta::LpVec3,
    entity_id::MinecraftEntityId,
    game_type::GameMode,
    position::Vec3,
};
use azalea_protocol::packets::{
    common::CommonPlayerSpawnInfo,
    game::{
        ClientboundAddEntity, ClientboundBlockUpdate, ClientboundContainerSetSlot,
        ClientboundGamePacket, ClientboundLevelChunkWithLight, ClientboundLogin,
        ClientboundRemoveEntities, ClientboundSetEntityData, ClientboundSetHealth,
        ClientboundSetTime, ClientboundSound, ClientboundSoundEntity, ServerboundGamePacket,
        ServerboundMovePlayerPosRot,
        c_level_chunk_with_light::ClientboundLevelChunkPacketData, c_set_time::ClockState,
        c_sound::SoundSource,
    },
};
use azalea_registry::{
    Holder,
    builtin::{BlockKind, EntityKind, ItemKind, SoundEvent},
    data::{DimensionKind, WorldClock},
    identifier::Identifier,
    DataRegistry,
};
use azalea_world::Chunk;
use praxion_headless::Body;
use praxion_perception::{Config, Perception};
use praxion_reflexes::{Engine, ReflexBlocks, Settings, View};
use praxion_world::{BlockPos, World};
use uuid::Uuid;

const EYE_HEIGHT: f64 = 1.62;

/// A standard overworld login.
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

/// The nine chunks around the origin, so a radius-12 floor is loaded.
pub fn base_world() -> World {
    let mut world = World::new();
    world.follow(0);
    world.apply_clientbound_play(0, &ClientboundGamePacket::Login(login()));
    for x in -1..=1 {
        for z in -1..=1 {
            world.apply_clientbound_play(
                0,
                &ClientboundGamePacket::LevelChunkWithLight(chunk_packet(x, z)),
            );
        }
    }
    world
}

pub fn set_block(world: &mut World, pos: BlockPos, state: BlockState) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::BlockUpdate(ClientboundBlockUpdate {
            pos,
            block_state: state,
        }),
    );
}

pub fn add_entity(world: &mut World, id: i32, kind: EntityKind, pos: Vec3) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::AddEntity(ClientboundAddEntity {
            id: MinecraftEntityId(id),
            uuid: Uuid::from_u128(id as u128),
            entity_type: kind,
            position: pos,
            movement: LpVec3::Zero,
            x_rot: 0,
            y_rot: 0,
            y_head_rot: 0,
            data: 0,
        }),
    );
}

/// A positional sound packet: what a player hears from a source the server did
/// not name (a block, a fuse, a mob behind a wall). Positions are fixed point
/// with three fractional bits.
pub fn add_sound(world: &mut World, event: SoundEvent, pos: Vec3) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::Sound(ClientboundSound {
            sound: Holder::Reference(event),
            source: SoundSource::Hostile,
            x: (pos.x * 8.0) as i32,
            y: (pos.y * 8.0) as i32,
            z: (pos.z * 8.0) as i32,
            volume: 1.0,
            pitch: 1.0,
            seed: 0,
        }),
    );
}

/// A sound packet that names the entity that made it, as a mob's step or hurt
/// sound does. The world drops the packet when it does not know the entity.
pub fn add_entity_sound(world: &mut World, event: SoundEvent, entity: i32) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SoundEntity(ClientboundSoundEntity {
            sound: Holder::Reference(event),
            source: SoundSource::Hostile,
            id: MinecraftEntityId(entity),
            volume: 1.0,
            pitch: 1.0,
            seed: 0,
        }),
    );
}

pub fn remove_entity(world: &mut World, id: i32) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::RemoveEntities(ClientboundRemoveEntities {
            entity_ids: vec![MinecraftEntityId(id)],
        }),
    );
}

pub fn set_health(world: &mut World, health: f32, food: u32, saturation: f32) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetHealth(ClientboundSetHealth {
            health,
            food,
            saturation,
        }),
    );
}

pub fn set_time(world: &mut World, day_time: u64) {
    let clocks = [(
        WorldClock::new_raw(0),
        ClockState {
            total_ticks: day_time,
            partial_tick: 0.0,
            rate: 1.0,
        },
    )]
    .into_iter()
    .collect();
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetTime(ClientboundSetTime {
            game_time: day_time,
            clock_updates: clocks,
        }),
    );
}

pub fn set_slot(world: &mut World, slot: u16, item: ItemKind, count: i32) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::ContainerSetSlot(ClientboundContainerSetSlot {
            container_id: 0,
            state_id: 0,
            slot,
            item_stack: praxion_world::ItemStack::new(item, count),
        }),
    );
}

pub fn state(kind: BlockKind) -> BlockState {
    BlockState::from(kind)
}

/// A flat stone floor at `y`.
pub fn floor(world: &mut World, y: i32, radius: i32) {
    for x in -radius..=radius {
        for z in -radius..=radius {
            set_block(
                world,
                BlockPos::new(x, y, z),
                state(BlockKind::Stone),
            );
        }
    }
}

/// Write the body's position and look back into the world, the way the
/// serverbound movement packet would.
pub fn sync_player(world: &mut World, body: &Body) {
    world.apply_serverbound_play(
        0,
        &ServerboundGamePacket::MovePlayerPosRot(ServerboundMovePlayerPosRot {
            pos: body.position(),
            look_direction: body.look(),
            flags: azalea_protocol::common::movements::MoveFlags {
                on_ground: body.on_ground(),
                horizontal_collision: body.horizontal_collision(),
            },
        }),
    );
}

/// The body's own entity id in the scripted login.
pub const OWN_ENTITY_ID: i32 = 7;

/// One `set_entity_data` for the body's own entity.
pub fn set_own_entity_data(world: &mut World, items: Vec<azalea_entity::EntityDataItem>) {
    world.apply_clientbound_play(
        0,
        &ClientboundGamePacket::SetEntityData(ClientboundSetEntityData {
            id: MinecraftEntityId(OWN_ENTITY_ID),
            packed_items: azalea_entity::EntityMetadataItems(items),
        }),
    );
}

/// The shared flags byte (index 0): bit 0 is "on fire".
pub fn shared_flags(flags: u8) -> azalea_entity::EntityDataItem {
    azalea_entity::EntityDataItem {
        index: 0,
        value: azalea_entity::EntityDataValue::Byte(flags),
    }
}

/// The air supply (index 1), in ticks.
pub fn air_supply(ticks: i32) -> azalea_entity::EntityDataItem {
    azalea_entity::EntityDataItem {
        index: 1,
        value: azalea_entity::EntityDataValue::Int(ticks),
    }
}

/// One simulated body: world, perception, reflex engine and physics.
pub struct Sim {
    pub world: World,
    pub perception: Perception,
    pub engine: Engine,
    pub body: Body,
    pub settings: Settings,
    pub tick: u64,
    pub last_frame: Option<praxion_headless::ControlFrame>,
    /// Every frame the engine emitted, for predicates.
    pub frames: Vec<praxion_headless::ControlFrame>,
}

impl Sim {
    /// A world with nine chunks and a flat floor at `feet.y - 1`.
    pub fn new(settings: Settings, feet: Vec3) -> Self {
        let mut world = base_world();
        let y = feet.y;
        floor(&mut world, y as i32 - 1, 14);
        let mut body = Body::new(feet);
        body.set_look(azalea_entity::LookDirection::new(0.0, 0.0));
        sync_player(&mut world, &body);
        // One tick so the world's player position and health exist before the
        // first view.
        set_health(&mut world, 20.0, 20, 5.0);
        Self {
            world,
            perception: Perception::new(Config::default()),
            engine: Engine::new(),
            body,
            settings,
            tick: 0,
            last_frame: None,
            frames: Vec::new(),
        }
    }

    pub fn view(&mut self) -> View {
        View::capture(&mut self.perception, &self.world, self.engine.state_mut())
    }

    /// Run one tick and step the body. Returns the frame the reflex emitted.
    pub fn step(&mut self) -> Option<praxion_headless::ControlFrame> {
        self.perception.begin_tick(&self.world);
        let view = View::capture(&mut self.perception, &self.world, self.engine.state_mut());
        let mut blocks = ReflexBlocks {
            perception: &mut self.perception,
            world: &self.world,
        };
        let out = self.engine.poll(
            &view,
            &self.settings,
            &mut self.body,
            self.tick,
            &mut blocks,
        );
        let frame = out.frame().cloned();
        if let Some(frame) = &frame {
            frame
                .apply(&mut self.body, self.tick)
                .expect("the frame is for this tick and generation");
            self.body.step(&frame.held, &self.world);
            self.frames.push(frame.clone());
        }
        sync_player(&mut self.world, &self.body);
        self.last_frame = frame.clone();
        self.tick += 1;
        frame
    }

    /// Run `ticks` ticks, returning the last frame.
    pub fn run(&mut self, ticks: u64) -> Option<praxion_headless::ControlFrame> {
        let mut last = None;
        for _ in 0..ticks {
            last = self.step();
        }
        last
    }

    pub fn run_until(&mut self, ticks: u64, mut done: impl FnMut(&Sim) -> bool) -> bool {
        for _ in 0..ticks {
            self.step();
            if done(self) {
                return true;
            }
        }
        false
    }

    pub fn set_block(&mut self, pos: BlockPos, block: BlockState) {
        set_block(&mut self.world, pos, block);
    }

    pub fn set_health(&mut self, health: f32, food: u32) {
        set_health(&mut self.world, health, food, 0.0);
    }

    pub fn add_mob(&mut self, id: i32, kind: EntityKind, pos: Vec3) {
        add_entity(&mut self.world, id, kind, pos);
    }

    /// A positional sound, as a `Sound` packet carries it.
    pub fn add_sound(&mut self, event: SoundEvent, pos: Vec3) {
        add_sound(&mut self.world, event, pos);
    }

    /// A sound that names the entity that made it (`SoundEntity`).
    pub fn add_entity_sound(&mut self, event: SoundEvent, entity: i32) {
        add_entity_sound(&mut self.world, event, entity);
    }

    pub fn remove_mob(&mut self, id: i32) {
        remove_entity(&mut self.world, id);
    }

    pub fn set_time(&mut self, day_time: u64) {
        set_time(&mut self.world, day_time);
    }

    pub fn add_item(&mut self, slot: u16, item: ItemKind, count: i32) {
        set_slot(&mut self.world, slot, item, count);
    }

    /// The air supply the server reports for the body's own entity.
    pub fn set_own_air(&mut self, ticks: i32) {
        set_own_entity_data(&mut self.world, vec![air_supply(ticks)]);
    }

    /// The shared flags byte the server reports for the body's own entity.
    pub fn set_own_burning(&mut self, burning: bool) {
        set_own_entity_data(&mut self.world, vec![shared_flags(u8::from(burning))]);
    }

    pub fn feet(&self) -> BlockPos {
        self.body.position().to_block_pos_floor()
    }

    pub fn distance_to(&self, pos: Vec3) -> f64 {
        self.body.position().distance_to(pos)
    }
}

/// The settings the matrix uses: fast air, a low hard threshold, no waiting.
pub fn test_settings() -> Settings {
    Settings {
        air_surface_threshold: 10,
        ..Settings::default()
    }
}

/// The centre of a cell at feet height.
pub fn centre(cell: BlockPos) -> Vec3 {
    Vec3::new(f64::from(cell.x) + 0.5, f64::from(cell.y), f64::from(cell.z) + 0.5)
}

/// The eye height the body uses, for building entity positions.
pub fn eye(feet: Vec3) -> Vec3 {
    feet + Vec3::new(0.0, EYE_HEIGHT, 0.0)
}
