//! The raw store: everything the decoder receives, kept behind typed queries.
//!
//! The store only applies the packets of the one connection being followed
//! ([`World::follow`]). It answers block and entity queries with `None` once the
//! connection desynchronises, never with stale data.
//!
//! All fields are private. Later the perception API is the only reader; the
//! accessors below are the seam that API will narrow (architecture §4, §5.1).

mod block;
mod dimension;
mod entity;
mod player;
mod section;
mod sound;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Cursor;

use azalea_core::{
    game_type::GameMode,
    position::{ChunkBlockPos, ChunkSectionBlockPos, ChunkSectionPos},
    sound::CustomSound,
};
use azalea_protocol::packets::{
    config::ClientboundConfigPacket,
    game::{
        ClientboundGamePacket, ServerboundGamePacket,
        c_game_event::EventType,
        c_move_entity_pos_rot::VecDelta,
    },
};
use azalea_registry::{DataRegistry, Holder, builtin::SoundEvent, data::WorldClock};
use azalea_world::Chunk;

pub use azalea_block::BlockState;
pub use azalea_core::position::{BlockPos, ChunkPos, Vec3};
pub use azalea_inventory::ItemStack;
pub use azalea_registry::builtin::{EntityKind, MenuKind};
pub use block::BlockDescription;
pub use dimension::{Dimension, DimensionSource};
pub use entity::Entity;
pub use player::{
    ChangedSlot, ContainerClick, Experience, Health, OpenContainer, PLAYER_STORAGE_SLOTS,
};
pub use section::SectionView;
pub use sound::{MAX_SOUNDS, Sound};

use player::{Player, set_slot};

/// Whether the followed world can still be trusted.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Status {
    #[default]
    Valid,
    Invalid { reason: String },
}

impl Status {
    pub fn is_valid(&self) -> bool {
        matches!(self, Status::Valid)
    }

    /// `Some(reason)` after a desync.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Status::Valid => None,
            Status::Invalid { reason } => Some(reason),
        }
    }
}

/// What a warning is about. Counts are kept per kind so a hostile or broken
/// stream cannot grow the warning list: the counts always stay exact, the
/// messages are capped at [`MAX_WARNING_MESSAGES`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum WarningKind {
    /// A chunk packet arrived before the Play login packet that names the
    /// dimension.
    ChunkBeforeLogin,
    /// A block update named a chunk that is not loaded.
    ChunkNotLoaded,
    /// A level chunk packet did not parse.
    ChunkParseFailed,
    /// A section block position was outside `i32` or outside the world height.
    SectionOutOfBounds,
    /// A slot was outside the size of the container it names.
    SlotOutOfRange,
}

impl WarningKind {
    pub fn as_str(self) -> &'static str {
        match self {
            WarningKind::ChunkBeforeLogin => "chunk_before_login",
            WarningKind::ChunkNotLoaded => "chunk_not_loaded",
            WarningKind::ChunkParseFailed => "chunk_parse_failed",
            WarningKind::SectionOutOfBounds => "section_out_of_bounds",
            WarningKind::SlotOutOfRange => "slot_out_of_range",
        }
    }
}

/// Messages kept in [`World::warnings`]. Older ones are dropped; the per-kind
/// counts in [`World::warning_counts`] stay exact.
pub const MAX_WARNING_MESSAGES: usize = 100;

/// Rain and thunder level from game-event packets.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Weather {
    raining: bool,
    rain_level: f32,
    thunder_level: f32,
}

impl Weather {
    pub fn raining(&self) -> bool {
        self.raining
    }

    /// 0 to 1.
    pub fn rain_level(&self) -> f32 {
        self.rain_level
    }

    /// 0 to 1.
    pub fn thunder_level(&self) -> f32 {
        self.thunder_level
    }
}

/// Everything the decoder received for one followed connection.
pub struct World {
    followed: Option<u32>,
    /// The followed connection sent `Closed`; later packets for it are ignored
    /// even if the decoder still hands them over.
    ended: bool,
    status: Status,
    registries: azalea_core::registry_holder::RegistryHolder,
    dimension: Option<Dimension>,
    game_mode: Option<GameMode>,
    own_entity_id: Option<i32>,
    chunks: HashMap<ChunkPos, Chunk>,
    /// One revision per loaded chunk, bumped whenever a block in it changes.
    /// A chunk that is not loaded has no entry, so unloading is visible as a
    /// missing revision and reloading always produces a fresh number.
    chunk_revisions: HashMap<ChunkPos, u64>,
    /// Bumped on every block change anywhere; lets a reader skip all further
    /// work when nothing changed since it last looked.
    revision: u64,
    player: Player,
    entities: BTreeMap<i32, Entity>,
    sounds: VecDeque<Sound>,
    open_container: Option<OpenContainer>,
    game_time: u64,
    day_time: u64,
    weather: Weather,
    tick: u64,
    chunk_batches_started: u64,
    chunk_batches_finished: u64,
    last_chunk_batch_size: Option<u32>,
    last_block_ack: Option<u32>,
    warnings: Vec<String>,
    warning_counts: BTreeMap<WarningKind, u64>,
}

impl World {
    pub fn new() -> Self {
        Self {
            followed: None,
            ended: false,
            status: Status::Valid,
            registries: azalea_core::registry_holder::RegistryHolder::default(),
            dimension: None,
            game_mode: None,
            own_entity_id: None,
            chunks: HashMap::new(),
            chunk_revisions: HashMap::new(),
            revision: 0,
            player: Player::default(),
            entities: BTreeMap::new(),
            sounds: VecDeque::new(),
            open_container: None,
            game_time: 0,
            day_time: 0,
            weather: Weather::default(),
            tick: 0,
            chunk_batches_started: 0,
            chunk_batches_finished: 0,
            last_chunk_batch_size: None,
            last_block_ack: None,
            warnings: Vec::new(),
            warning_counts: BTreeMap::new(),
        }
    }

    /// Follow `conn_id` from now on and forget the previous connection's world.
    /// Registries are server-wide and are kept, because a new connection's
    /// configuration data usually arrives before this switch.
    pub fn follow(&mut self, conn_id: u32) {
        self.followed = Some(conn_id);
        self.reset();
    }

    pub fn followed(&self) -> Option<u32> {
        self.followed
    }

    /// A fresh connection state for `conn_id`, if it is the followed one.
    pub fn opened(&mut self, conn_id: u32) {
        if self.followed() == Some(conn_id) {
            self.reset();
        }
    }

    /// The followed connection closed: its last valid state is kept, but no
    /// later packet for it is applied. A fresh [`World::follow`] or
    /// [`World::opened`] for the same connection starts it again.
    pub fn closed(&mut self, conn_id: u32) {
        if self.followed() == Some(conn_id) {
            self.ended = true;
        }
    }

    /// Whether the followed connection has ended.
    pub fn is_ended(&self) -> bool {
        self.ended
    }

    /// The followed connection lost framing or decoding.
    pub fn invalidate(&mut self, conn_id: u32, reason: impl Into<String>) {
        if self.followed() == Some(conn_id) && self.status.is_valid() {
            self.status = Status::Invalid { reason: reason.into() };
        }
    }

    pub fn status(&self) -> &Status {
        &self.status
    }

    pub fn dimension(&self) -> Option<&Dimension> {
        self.dimension.as_ref()
    }

    pub fn height(&self) -> Option<u32> {
        self.dimension.as_ref().map(Dimension::height)
    }

    pub fn min_y(&self) -> Option<i32> {
        self.dimension.as_ref().map(Dimension::min_y)
    }

    pub fn game_mode(&self) -> Option<GameMode> {
        self.game_mode
    }

    pub fn own_entity_id(&self) -> Option<i32> {
        self.own_entity_id
    }

    pub fn player_position(&self) -> Option<Vec3> {
        self.player.position()
    }

    pub fn player_rotation(&self) -> (f32, f32) {
        self.player.rotation()
    }

    pub fn health(&self) -> Option<Health> {
        self.player.health()
    }

    pub fn experience(&self) -> Option<Experience> {
        self.player.experience()
    }

    pub fn selected_hotbar_slot(&self) -> Option<u32> {
        self.player.selected_slot()
    }

    /// Whether the player's own shared flags say it is on fire, from its own
    /// entity data. `None` until the server sends the flags byte, and after a
    /// new login: an unanswered fact is unknown, never "not burning".
    pub fn own_burning(&self) -> Option<bool> {
        if !self.status.is_valid() {
            return None;
        }
        self.player.burning()
    }

    /// The player's own air supply in ticks (300 at the surface, Respiration
    /// can raise the maximum). `None` until the server says, so a caller can
    /// tell an absent fact from full air.
    pub fn own_air(&self) -> Option<u32> {
        if !self.status.is_valid() {
            return None;
        }
        self.player.air()
    }

    pub fn inventory(&self) -> &[ItemStack] {
        self.player.inventory()
    }

    pub fn carried_item(&self) -> &ItemStack {
        self.player.carried()
    }

    pub fn last_container_click(&self) -> Option<&ContainerClick> {
        self.player.last_click()
    }

    pub fn open_container(&self) -> Option<&OpenContainer> {
        self.open_container.as_ref()
    }

    pub fn entity(&self, id: i32) -> Option<&Entity> {
        if !self.status.is_valid() {
            return None;
        }
        self.entities.get(&id)
    }

    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        let valid = self.status.is_valid();
        self.entities.values().filter(move |_| valid)
    }

    /// Recent sounds, oldest first. Bounded by [`MAX_SOUNDS`].
    pub fn sounds(&self) -> &VecDeque<Sound> {
        &self.sounds
    }

    pub fn game_time(&self) -> u64 {
        self.game_time
    }

    /// `world_clock` overworld ticks (the day time).
    pub fn day_time(&self) -> u64 {
        self.day_time
    }

    pub fn weather(&self) -> Weather {
        self.weather
    }

    /// `client_tick_end` packets seen from the followed connection.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    pub fn chunk_batches_started(&self) -> u64 {
        self.chunk_batches_started
    }

    pub fn chunk_batches_finished(&self) -> u64 {
        self.chunk_batches_finished
    }

    pub fn last_chunk_batch_size(&self) -> Option<u32> {
        self.last_chunk_batch_size
    }

    /// Latest `block_changed_ack` sequence number.
    pub fn last_block_acked(&self) -> Option<u32> {
        self.last_block_ack
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn has_chunk(&self, pos: ChunkPos) -> bool {
        self.chunks.contains_key(&pos)
    }

    /// `None` when invalid, outside the world, or the chunk is not loaded.
    pub fn block_state(&self, pos: BlockPos) -> Option<BlockState> {
        if !self.status.is_valid() {
            return None;
        }
        let dimension = self.dimension.as_ref()?;
        if !y_in_world(dimension.min_y(), dimension.height(), pos.y) {
            return None;
        }
        let chunk = self.chunks.get(&ChunkPos::from(pos))?;
        chunk.get_block_state(&ChunkBlockPos::from(pos), dimension.min_y())
    }

    pub fn block_description(&self, pos: BlockPos) -> Option<BlockDescription> {
        self.block_state(pos).map(BlockDescription::of)
    }

    /// A counter that changes whenever the loaded blocks change: a block
    /// update, a section update, a chunk load or a chunk unload.
    ///
    /// A reader that remembers the number can skip re-checking anything while
    /// it is unchanged; [`World::chunk_revision`] then names which chunk moved.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The revision of the chunk at `pos`, or `None` when it is not loaded.
    ///
    /// Every change inside a chunk bumps its revision, so a cached fact about
    /// a block stays valid exactly while the revisions of the chunks it
    /// depended on are unchanged.
    pub fn chunk_revision(&self, pos: ChunkPos) -> Option<u64> {
        if !self.status.is_valid() {
            return None;
        }
        self.chunk_revisions.get(&pos).copied()
    }

    /// The section at `section` (its `y` is in sections, not blocks), if the
    /// chunk is loaded and the section is inside the world height.
    pub fn section(&self, section: ChunkSectionPos) -> Option<SectionView<'_>> {
        if !self.status.is_valid() {
            return None;
        }
        let chunk = self.chunks.get(&ChunkPos::new(section.x, section.z))?;
        let min_y = self.dimension.as_ref()?.min_y();
        let index = usize::try_from(section.y - (min_y >> 4)).ok()?;
        chunk.sections.get(index).map(SectionView::new)
    }

    /// The first [`MAX_WARNING_MESSAGES`] non-fatal problems (a chunk that
    /// failed to parse, a block update for an unloaded chunk). Kept so a replay
    /// can explain a surprising answer; [`World::warning_counts`] has the exact
    /// numbers per kind.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// How many warnings of each kind were dropped or kept.
    pub fn warning_counts(&self) -> &BTreeMap<WarningKind, u64> {
        &self.warning_counts
    }

    /// Registry data is server-wide and is accepted from every connection, so
    /// a follow switch made at the Play login packet still resolves dimensions.
    pub fn apply_clientbound_config(&mut self, _conn_id: u32, packet: &ClientboundConfigPacket) {
        if let ClientboundConfigPacket::RegistryData(packet) = packet {
            self.registries.append(
                packet.registry_id.clone(),
                packet.entries.iter().cloned().map(|(ident, tag)| (ident, tag.map(Into::into))).collect(),
            );
        }
    }

    pub fn apply_clientbound_play(&mut self, conn_id: u32, packet: &ClientboundGamePacket) {
        if !self.is_active(conn_id) {
            return;
        }
        match packet {
            ClientboundGamePacket::Login(packet) => {
                self.forget_all_chunks();
                self.entities.clear();
                self.sounds.clear();
                self.player = Player::default();
                self.open_container = None;
                self.weather = Weather::default();
                self.set_dimension_and_mode(&packet.common);
                self.own_entity_id = Some(packet.player_id.0);
            }
            ClientboundGamePacket::Respawn(packet) => {
                let changed = self
                    .dimension
                    .as_ref()
                    .is_some_and(|dimension| dimension.dimension() != packet.common.dimension.to_string());
                if changed {
                    self.forget_all_chunks();
                }
                self.entities.clear();
                self.sounds.clear();
                self.player.clear_position();
                self.player.clear_own_facts();
                self.open_container = None;
                self.set_dimension_and_mode(&packet.common);
            }
            ClientboundGamePacket::LevelChunkWithLight(packet) => {
                let Some(dimension) = &self.dimension else {
                    self.warn(
                        WarningKind::ChunkBeforeLogin,
                        "level_chunk_with_light arrived before the Play login packet",
                    );
                    return;
                };
                let pos = ChunkPos::new(packet.x, packet.z);
                let read = Chunk::read_with_dimension_height(
                    &mut Cursor::new(&packet.chunk_data.buffer[..]),
                    dimension.height(),
                    dimension.min_y(),
                    &packet.chunk_data.heightmaps,
                );
                match read {
                    Ok(chunk) => {
                        self.chunks.insert(pos, chunk);
                        self.touch_chunk(pos);
                    }
                    Err(e) => self.warn(
                        WarningKind::ChunkParseFailed,
                        format!("chunk {pos:?} failed to parse: {e}"),
                    ),
                }
            }
            ClientboundGamePacket::ForgetLevelChunk(packet) => {
                self.forget_chunk(packet.pos);
            }
            ClientboundGamePacket::BlockUpdate(packet) => {
                if !self.set_block(packet.pos, packet.block_state) {
                    self.warn(
                        WarningKind::ChunkNotLoaded,
                        format!(
                            "block_update at {:?}: chunk not loaded or position outside the world",
                            packet.pos
                        ),
                    );
                }
            }
            ClientboundGamePacket::SectionBlocksUpdate(packet) => {
                // The section position comes from the network, so every step
                // from it to a world coordinate is checked: a coordinate that
                // overflows `i32` or falls outside the world height is dropped
                // rather than wrapped into a block somewhere else.
                let section = packet.section_pos;
                let Some((min_y, height)) =
                    self.dimension.as_ref().map(|dimension| (dimension.min_y(), dimension.height()))
                else {
                    self.warn(
                        WarningKind::ChunkBeforeLogin,
                        "section_blocks_update arrived before the Play login packet",
                    );
                    return;
                };
                let mut missing = 0u64;
                let mut out_of_bounds = 0u64;
                let mut changed = false;
                for update in &packet.states {
                    let Some(pos) =
                        section_block_pos_in_world(section, update.pos, min_y, height)
                    else {
                        out_of_bounds += 1;
                        continue;
                    };
                    if self.set_block(pos, update.state) {
                        changed = true;
                    } else {
                        missing += 1;
                    }
                }
                // One revision per packet is enough: the section is in one
                // chunk and its blocks all changed at once.
                if changed {
                    self.touch_chunk(ChunkPos::new(section.x, section.z));
                }
                if out_of_bounds > 0 {
                    self.warn_many(
                        WarningKind::SectionOutOfBounds,
                        out_of_bounds,
                        format!(
                            "section_blocks_update in {section:?}: {out_of_bounds} block(s) \
                             outside the world, ignored"
                        ),
                    );
                }
                if missing > 0 {
                    self.warn_many(
                        WarningKind::ChunkNotLoaded,
                        missing,
                        format!(
                            "section_blocks_update in {section:?}: {missing} block(s) in \
                             chunks that are not loaded"
                        ),
                    );
                }
            }
            ClientboundGamePacket::ChunkBatchStart(_) => {
                self.chunk_batches_started += 1;
            }
            ClientboundGamePacket::ChunkBatchFinished(packet) => {
                self.chunk_batches_finished += 1;
                self.last_chunk_batch_size = Some(packet.batch_size);
            }
            ClientboundGamePacket::BlockChangedAck(packet) => {
                self.last_block_ack = Some(packet.seq);
            }
            ClientboundGamePacket::PlayerPosition(packet) => {
                let change = &packet.change;
                let relative = &packet.relative;
                let base = self.player.position().unwrap_or_default();
                self.player.set_position(Vec3::new(
                    apply_change(base.x, relative.x, change.pos.x),
                    apply_change(base.y, relative.y, change.pos.y),
                    apply_change(base.z, relative.z, change.pos.z),
                ));
                let (yaw, pitch) = self.player.rotation();
                self.player.set_rotation(
                    apply_change(yaw, relative.y_rot, change.look_direction.y_rot()),
                    apply_change(pitch, relative.x_rot, change.look_direction.x_rot()),
                );
            }
            ClientboundGamePacket::SetHealth(packet) => {
                self.player
                    .set_health(Health::new(packet.health, packet.food, packet.saturation));
            }
            ClientboundGamePacket::SetExperience(packet) => {
                self.player.set_experience(Experience::new(
                    packet.experience_progress,
                    packet.experience_level,
                    packet.total_experience,
                ));
            }
            ClientboundGamePacket::SetHeldSlot(packet) => {
                self.player.set_selected_slot(packet.slot);
            }
            ClientboundGamePacket::ContainerSetContent(packet) => {
                if packet.container_id == 0 {
                    self.player
                        .set_content(packet.items.clone(), packet.carried_item.clone());
                } else if let Some(container) = &mut self.open_container
                    && container.id() == packet.container_id
                {
                    container.set_content(packet.items.clone());
                }
            }
            ClientboundGamePacket::ContainerSetSlot(packet) => {
                if packet.container_id == 0 {
                    if !self.player.set_slot(u32::from(packet.slot), packet.item_stack.clone()) {
                        self.warn(
                            WarningKind::SlotOutOfRange,
                            format!(
                                "container_set_slot for slot {} outside the {} slot player \
                                 container, ignored",
                                packet.slot,
                                player::PLAYER_CONTAINER_SIZE
                            ),
                        );
                    }
                } else if let Some(container) = &mut self.open_container
                    && container.id() == packet.container_id
                {
                    set_slot(container.slots_mut(), packet.slot, packet.item_stack.clone());
                }
            }
            ClientboundGamePacket::SetPlayerInventory(packet) => {
                if !self.player.set_slot(packet.slot, packet.contents.clone()) {
                    self.warn(
                        WarningKind::SlotOutOfRange,
                        format!(
                            "set_player_inventory for slot {} outside the {} slot player \
                             container, ignored",
                            packet.slot,
                            player::PLAYER_CONTAINER_SIZE
                        ),
                    );
                }
            }
            ClientboundGamePacket::OpenScreen(packet) => {
                self.open_container = Some(OpenContainer::new(
                    packet.container_id,
                    packet.menu_type,
                    packet.title.to_string(),
                ));
            }
            ClientboundGamePacket::ContainerClose(packet) => {
                if self.open_container.as_ref().is_some_and(|c| c.id() == packet.container_id) {
                    self.open_container = None;
                }
            }
            ClientboundGamePacket::AddEntity(packet) => {
                self.entities.insert(
                    packet.id.0,
                    Entity::new(
                        packet.id.0,
                        packet.uuid,
                        packet.entity_type,
                        packet.position,
                        compact_angle(packet.y_rot),
                        compact_angle(packet.x_rot),
                        compact_angle(packet.y_head_rot),
                    ),
                );
            }
            ClientboundGamePacket::MoveEntityPos(packet) => {
                if let Some(entity) = self.entities.get_mut(&packet.entity_id.0) {
                    let pos = apply_delta(entity.position(), &packet.delta);
                    entity.set_position(pos);
                }
            }
            ClientboundGamePacket::MoveEntityPosRot(packet) => {
                if let Some(entity) = self.entities.get_mut(&packet.entity_id.0) {
                    let pos = apply_delta(entity.position(), &packet.delta);
                    entity.set_position(pos);
                    entity.set_rotation(
                        compact_angle(packet.look_direction.y_rot),
                        compact_angle(packet.look_direction.x_rot),
                    );
                }
            }
            ClientboundGamePacket::MoveEntityRot(packet) => {
                if let Some(entity) = self.entities.get_mut(&packet.entity_id.0) {
                    entity.set_rotation(
                        compact_angle(packet.look_direction.y_rot),
                        compact_angle(packet.look_direction.x_rot),
                    );
                }
            }
            ClientboundGamePacket::TeleportEntity(packet) => {
                if let Some(entity) = self.entities.get_mut(&packet.id.0) {
                    let change = &packet.change;
                    let relative = &packet.relative;
                    let pos = entity.position();
                    entity.set_position(Vec3::new(
                        apply_change(pos.x, relative.x, change.pos.x),
                        apply_change(pos.y, relative.y, change.pos.y),
                        apply_change(pos.z, relative.z, change.pos.z),
                    ));
                    let (yaw, pitch) = entity.rotation();
                    entity.set_rotation(
                        apply_change(yaw, relative.y_rot, change.look_direction.y_rot()),
                        apply_change(pitch, relative.x_rot, change.look_direction.x_rot()),
                    );
                    let velocity = entity.velocity();
                    entity.set_velocity(Vec3::new(
                        apply_change(velocity.x, relative.delta_x, change.delta.x),
                        apply_change(velocity.y, relative.delta_y, change.delta.y),
                        apply_change(velocity.z, relative.delta_z, change.delta.z),
                    ));
                }
            }
            ClientboundGamePacket::EntityPositionSync(packet) => {
                if let Some(entity) = self.entities.get_mut(&packet.id.0) {
                    entity.set_position(packet.position.end_position());
                    entity.set_rotation(
                        packet.look_direction.y_rot(),
                        packet.look_direction.x_rot(),
                    );
                }
            }
            ClientboundGamePacket::SetEntityMotion(packet) => {
                if let Some(entity) = self.entities.get_mut(&packet.id.0) {
                    entity.set_velocity(packet.delta.to_vec3());
                }
            }
            ClientboundGamePacket::RemoveEntities(packet) => {
                for id in &packet.entity_ids {
                    self.entities.remove(&id.0);
                }
            }
            ClientboundGamePacket::SetEntityData(packet) => {
                // The player's own entity never arrives as an `AddEntity`, so
                // its data is kept on the player instead of in `entities`.
                if Some(packet.id.0) == self.own_entity_id {
                    self.player.merge_metadata(&packet.packed_items);
                } else if let Some(entity) = self.entities.get_mut(&packet.id.0) {
                    entity.merge_metadata(&packet.packed_items);
                }
            }
            ClientboundGamePacket::Sound(packet) => {
                // Sound positions are fixed point with three fractional bits.
                let position = Vec3::new(
                    f64::from(packet.x) / 8.0,
                    f64::from(packet.y) / 8.0,
                    f64::from(packet.z) / 8.0,
                );
                self.push_sound(sound_id(&packet.sound), position, None);
            }
            ClientboundGamePacket::SoundEntity(packet) => {
                if let Some(entity) = self.entities.get(&packet.id.0) {
                    let position = entity.position();
                    let id = entity.id();
                    self.push_sound(sound_id(&packet.sound), position, Some(id));
                }
            }
            ClientboundGamePacket::SetTime(packet) => {
                self.game_time = packet.game_time;
                // The overworld clock is registry entry 0 in every vanilla
                // data set; the packet keys the map by that registry id.
                if let Some(clock) = packet.clock_updates.get(&WorldClock::new_raw(0)) {
                    self.day_time = clock.total_ticks;
                }
            }
            ClientboundGamePacket::GameEvent(packet) => match packet.event {
                EventType::StartRaining => self.weather.raining = true,
                EventType::StopRaining => self.weather.raining = false,
                EventType::RainLevelChange => self.weather.rain_level = packet.param,
                EventType::ThunderLevelChange => self.weather.thunder_level = packet.param,
                EventType::ChangeGameMode => {
                    self.game_mode = GameMode::from_id(packet.param as u8);
                }
                _ => {}
            },
            _ => {}
        }
    }

    pub fn apply_serverbound_play(&mut self, conn_id: u32, packet: &ServerboundGamePacket) {
        if !self.is_active(conn_id) {
            return;
        }
        match packet {
            ServerboundGamePacket::MovePlayerPos(packet) => {
                self.player.set_position(packet.pos);
            }
            ServerboundGamePacket::MovePlayerPosRot(packet) => {
                self.player.set_position(packet.pos);
                self.player.set_rotation(
                    packet.look_direction.y_rot(),
                    packet.look_direction.x_rot(),
                );
            }
            ServerboundGamePacket::MovePlayerRot(packet) => {
                self.player.set_rotation(
                    packet.look_direction.y_rot(),
                    packet.look_direction.x_rot(),
                );
            }
            ServerboundGamePacket::MovePlayerStatusOnly(_) => {}
            ServerboundGamePacket::SetCarriedItem(packet) => {
                self.player.set_selected_slot(u32::from(packet.slot));
            }
            ServerboundGamePacket::ContainerClick(packet) => {
                let changed: Vec<ChangedSlot> = packet
                    .changed_slots
                    .iter()
                    .filter_map(|(slot, stack)| {
                        stack.0.as_ref().map(|item| ChangedSlot::new(*slot, item.kind, item.count))
                    })
                    .collect();
                let carried = packet.carried_item.0.as_ref().map(|item| (item.kind, item.count));
                self.player.set_last_click(ContainerClick::new(
                    packet.container_id,
                    packet.slot_num,
                    packet.button_num,
                    changed,
                    carried,
                ));
            }
            ServerboundGamePacket::ContainerClose(packet) => {
                if self.open_container.as_ref().is_some_and(|c| c.id() == packet.container_id) {
                    self.open_container = None;
                }
            }
            ServerboundGamePacket::ClientTickEnd(_) => {
                self.tick += 1;
            }
            _ => {}
        }
    }

    fn is_active(&self, conn_id: u32) -> bool {
        self.followed() == Some(conn_id) && !self.ended && self.status.is_valid()
    }

    fn reset(&mut self) {
        self.ended = false;
        self.status = Status::Valid;
        self.dimension = None;
        self.game_mode = None;
        self.own_entity_id = None;
        self.forget_all_chunks();
        self.player = Player::default();
        self.entities.clear();
        self.sounds.clear();
        self.open_container = None;
        self.game_time = 0;
        self.day_time = 0;
        self.weather = Weather::default();
        self.tick = 0;
        self.chunk_batches_started = 0;
        self.chunk_batches_finished = 0;
        self.last_chunk_batch_size = None;
        self.last_block_ack = None;
        self.warnings.clear();
        self.warning_counts.clear();
    }

    fn set_dimension_and_mode(
        &mut self,
        common: &azalea_protocol::packets::common::CommonPlayerSpawnInfo,
    ) {
        self.dimension = Some(dimension::resolve(common, &self.registries));
        self.game_mode = Some(common.game_type);
    }

    fn push_sound(&mut self, id: String, position: Vec3, entity: Option<i32>) {
        if self.sounds.len() >= MAX_SOUNDS {
            self.sounds.pop_front();
        }
        self.sounds.push_back(Sound::new(id, position, self.tick, entity));
    }

    fn set_block(&mut self, pos: BlockPos, state: BlockState) -> bool {
        let Some(dimension) = &self.dimension else {
            return false;
        };
        if !y_in_world(dimension.min_y(), dimension.height(), pos.y) {
            return false;
        }
        let Some(chunk) = self.chunks.get_mut(&ChunkPos::from(pos)) else {
            return false;
        };
        chunk.set_block_state(&ChunkBlockPos::from(pos), state, dimension.min_y());
        self.touch_chunk(ChunkPos::from(pos));
        true
    }

    /// The content of a chunk changed: it gets a revision no other chunk load
    /// ever had, and the world counter moves so readers notice.
    fn touch_chunk(&mut self, pos: ChunkPos) {
        self.revision = self.revision.wrapping_add(1);
        self.chunk_revisions.insert(pos, self.revision);
    }

    /// A chunk was unloaded: its revision disappears with it.
    fn forget_chunk(&mut self, pos: ChunkPos) {
        self.chunks.remove(&pos);
        self.chunk_revisions.remove(&pos);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Every loaded chunk was dropped at once (login, respawn, reset).
    fn forget_all_chunks(&mut self) {
        self.chunks.clear();
        self.chunk_revisions.clear();
        self.revision = self.revision.wrapping_add(1);
    }

    fn warn(&mut self, kind: WarningKind, message: impl Into<String>) {
        self.warn_many(kind, 1, message);
    }

    /// Count `count` warnings of `kind` and keep the message only while the
    /// list has room, so a stream of malformed packets costs O(1) memory.
    fn warn_many(&mut self, kind: WarningKind, count: u64, message: impl Into<String>) {
        if count == 0 {
            return;
        }
        *self.warning_counts.entry(kind).or_insert(0) += count;
        if self.warnings.len() < MAX_WARNING_MESSAGES {
            self.warnings.push(message.into());
        }
    }
}

impl Default for World {
    fn default() -> Self {
        Self::new()
    }
}

/// The world height as an `i64` interval, `min_y .. min_y + height`.
///
/// Both numbers come from server registry data (or the vanilla fallback), so
/// neither the sum nor the comparison may happen in `i32`: `min_y + height`
/// overflows for a hostile dimension type.
fn world_bounds(min_y: i32, height: u32) -> (i64, i64) {
    let bottom = i64::from(min_y);
    (bottom, bottom + i64::from(height))
}

fn y_in_world(min_y: i32, height: u32, y: i32) -> bool {
    let (bottom, top) = world_bounds(min_y, height);
    let y = i64::from(y);
    bottom <= y && y < top
}

/// World position of a block inside a section update.
///
/// `None` when a section coordinate times 16 overflows `i32`. Callers that
/// know the world height must also check it; see the crate's section update
/// handling.
pub fn section_block_pos(
    section: ChunkSectionPos,
    pos: ChunkSectionBlockPos,
) -> Option<BlockPos> {
    let x = section.x.checked_mul(16)?.checked_add(i32::from(pos.x))?;
    let y = section.y.checked_mul(16)?.checked_add(i32::from(pos.y))?;
    let z = section.z.checked_mul(16)?.checked_add(i32::from(pos.z))?;
    Some(BlockPos::new(x, y, z))
}

/// [`section_block_pos`] plus the world-height check. `None` when a coordinate
/// overflows `i32` or the block is outside the world height. `section.x * 16`
/// is what overflows: a section x of `i32::MAX / 8` is already past the limit,
/// which the tests use.
fn section_block_pos_in_world(
    section: ChunkSectionPos,
    pos: ChunkSectionBlockPos,
    min_y: i32,
    height: u32,
) -> Option<BlockPos> {
    let pos = section_block_pos(section, pos)?;
    if !y_in_world(min_y, height, pos.y) {
        return None;
    }
    Some(pos)
}

fn apply_change<T: std::ops::Add<Output = T>>(base: T, relative: bool, change: T) -> T {
    if relative { base + change } else { change }
}

fn apply_delta(pos: Vec3, delta: &VecDelta) -> Vec3 {
    match delta {
        VecDelta::Linear(delta) => pos.with_delta(delta),
        VecDelta::Stepped(steps) => steps.iter().fold(pos, |pos, step| pos.with_delta(&step.pos)),
    }
}

/// A packet rotation byte is 1/256 of a full turn.
fn compact_angle(angle: i8) -> f32 {
    f32::from(angle) * 360.0 / 256.0
}

/// The sound-event identifier of a sound packet, whether it names a registry
/// entry or carries a custom sound.
fn sound_id(sound: &Holder<SoundEvent, CustomSound>) -> String {
    match sound {
        Holder::Reference(event) => event.to_str().to_owned(),
        Holder::Direct(custom) => custom.sound_id.to_string(),
    }
}

#[cfg(test)]
mod tests;
