//! The followed player's own state and inventory.

use azalea_core::position::Vec3;
use azalea_entity::{EntityDataItem, EntityDataValue};
use azalea_inventory::ItemStack;
use azalea_registry::builtin::{ItemKind, MenuKind};

/// The metadata index of the shared flags byte, which every entity has.
///
/// The layout is azalea's `AbstractEntity::apply_metadata`: index 0 is the
/// flags byte (bit 0 on fire), index 1 the air supply in ticks. The player is a
/// living entity, so its own `set_entity_data` uses the same indices.
const SHARED_FLAGS_INDEX: u8 = 0;

/// Bit 0 of the shared flags byte: the entity is on fire.
const ON_FIRE_BIT: u8 = 0x1;

/// The metadata index of the air supply, in ticks. Respiration raises the
/// maximum above vanilla's 300, so only the floor is clamped.
const AIR_SUPPLY_INDEX: u8 = 1;

/// The player's own inventory slots: 27 storage slots plus the 9-slot hotbar.
/// A menu lists them after its own slots, so they are the menu's last 36.
pub const PLAYER_STORAGE_SLOTS: usize = 36;

/// Health, food and saturation from `set_health`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Health {
    health: f32,
    food: u32,
    saturation: f32,
}

impl Health {
    pub fn health(&self) -> f32 {
        self.health
    }

    pub fn food(&self) -> u32 {
        self.food
    }

    pub fn saturation(&self) -> f32 {
        self.saturation
    }
}

/// Progress towards the next level, from `set_experience`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Experience {
    progress: f32,
    level: u32,
    total: u32,
}

impl Experience {
    pub fn progress(&self) -> f32 {
        self.progress
    }

    pub fn level(&self) -> u32 {
        self.level
    }

    pub fn total(&self) -> u32 {
        self.total
    }
}

/// One slot a serverbound click says it changed. The protocol sends a hash of
/// the data components, not the components themselves, so the raw store keeps
/// the kind and count and leaves the full stack to the authoritative
/// `container_set_slot` that follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChangedSlot {
    slot: u16,
    kind: ItemKind,
    count: i32,
}

impl ChangedSlot {
    pub fn slot(&self) -> u16 {
        self.slot
    }

    pub fn kind(&self) -> ItemKind {
        self.kind
    }

    pub fn count(&self) -> i32 {
        self.count
    }
}

/// The latest serverbound container click.
#[derive(Clone, Debug, PartialEq)]
pub struct ContainerClick {
    container_id: i32,
    slot: i16,
    button: u8,
    changed_slots: Vec<ChangedSlot>,
    carried: Option<(ItemKind, i32)>,
}

impl ContainerClick {
    pub fn container_id(&self) -> i32 {
        self.container_id
    }

    pub fn slot(&self) -> i16 {
        self.slot
    }

    pub fn button(&self) -> u8 {
        self.button
    }

    pub fn changed_slots(&self) -> &[ChangedSlot] {
        &self.changed_slots
    }

    pub fn carried(&self) -> Option<(ItemKind, i32)> {
        self.carried
    }
}

/// A container window the server opened, live only while it is open.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenContainer {
    id: i32,
    kind: MenuKind,
    title: String,
    slots: Vec<ItemStack>,
}

impl OpenContainer {
    pub(crate) fn new(id: i32, kind: MenuKind, title: String) -> Self {
        Self { id, kind, title, slots: Vec::new() }
    }

    pub fn id(&self) -> i32 {
        self.id
    }

    pub fn kind(&self) -> MenuKind {
        self.kind
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn slots(&self) -> &[ItemStack] {
        &self.slots
    }

    pub(crate) fn slots_mut(&mut self) -> &mut Vec<ItemStack> {
        &mut self.slots
    }

    pub(crate) fn set_content(&mut self, slots: Vec<ItemStack>) {
        self.slots = slots;
    }
}

#[derive(Debug, Default)]
pub(crate) struct Player {
    pos: Option<Vec3>,
    yaw: f32,
    pitch: f32,
    /// The on-ground flag of the client's last serverbound movement packet.
    /// `None` until one was seen in this life, so "not reported" never reads as
    /// "in the air".
    on_ground: Option<bool>,
    health: Option<Health>,
    experience: Option<Experience>,
    selected_slot: Option<u32>,
    inventory: Vec<ItemStack>,
    carried: ItemStack,
    last_click: Option<ContainerClick>,
    /// The shared flags byte says the player is on fire. `None` until the
    /// server sends it.
    burning: Option<bool>,
    /// Air supply in ticks. `None` until the server sends it.
    air: Option<u32>,
}

impl Player {
    pub(crate) fn position(&self) -> Option<Vec3> {
        self.pos
    }

    pub(crate) fn rotation(&self) -> (f32, f32) {
        (self.yaw, self.pitch)
    }

    pub(crate) fn set_position(&mut self, pos: Vec3) {
        self.pos = Some(pos);
    }

    pub(crate) fn set_rotation(&mut self, yaw: f32, pitch: f32) {
        self.yaw = yaw;
        self.pitch = pitch;
    }

    pub(crate) fn clear_position(&mut self) {
        self.pos = None;
    }

    pub(crate) fn on_ground(&self) -> Option<bool> {
        self.on_ground
    }

    pub(crate) fn set_on_ground(&mut self, on_ground: bool) {
        self.on_ground = Some(on_ground);
    }

    /// Forget the grounded state with the position: a new life is a new entity.
    pub(crate) fn clear_on_ground(&mut self) {
        self.on_ground = None;
    }

    pub(crate) fn health(&self) -> Option<Health> {
        self.health
    }

    pub(crate) fn set_health(&mut self, health: Health) {
        self.health = Some(health);
    }

    pub(crate) fn experience(&self) -> Option<Experience> {
        self.experience
    }

    pub(crate) fn set_experience(&mut self, experience: Experience) {
        self.experience = Some(experience);
    }

    pub(crate) fn selected_slot(&self) -> Option<u32> {
        self.selected_slot
    }

    pub(crate) fn set_selected_slot(&mut self, slot: u32) {
        self.selected_slot = Some(slot);
    }

    pub(crate) fn inventory(&self) -> &[ItemStack] {
        &self.inventory
    }

    pub(crate) fn carried(&self) -> &ItemStack {
        &self.carried
    }

    pub(crate) fn set_content(&mut self, items: Vec<ItemStack>, carried: ItemStack) {
        self.inventory = items;
        self.carried = carried;
    }

    pub(crate) fn set_slot(&mut self, slot: u32, item: ItemStack) -> bool {
        // The slot comes straight off the network. `SetPlayerInventory` sends it
        // as a VarInt and `ContainerSetSlot` as a u16, so only this check keeps
        // a wrapped value from allocating a 65536 element inventory.
        let Some(index) = usize::try_from(slot).ok().filter(|index| *index < PLAYER_CONTAINER_SIZE)
        else {
            return false;
        };
        if self.inventory.len() <= index {
            self.inventory.resize(index + 1, ItemStack::Empty);
        }
        self.inventory[index] = item;
        true
    }

    pub(crate) fn last_click(&self) -> Option<&ContainerClick> {
        self.last_click.as_ref()
    }

    pub(crate) fn set_last_click(&mut self, click: ContainerClick) {
        self.last_click = Some(click);
    }

    pub(crate) fn burning(&self) -> Option<bool> {
        self.burning
    }

    pub(crate) fn air(&self) -> Option<u32> {
        self.air
    }

    /// Forget the own-entity facts. A respawn hands the player a new entity, so
    /// the old flags and air would be a stale fact.
    pub(crate) fn clear_own_facts(&mut self) {
        self.burning = None;
        self.air = None;
    }

    /// Fold the player's own `set_entity_data` items in. Only the two indices
    /// the body reads as HUD facts are interpreted; everything else stays in
    /// the packet, because the store keeps no untyped metadata for the player.
    pub(crate) fn merge_metadata(&mut self, items: &[EntityDataItem]) {
        for item in items {
            match (item.index, &item.value) {
                (SHARED_FLAGS_INDEX, EntityDataValue::Byte(flags)) => {
                    self.burning = Some(flags & ON_FIRE_BIT != 0);
                }
                (AIR_SUPPLY_INDEX, EntityDataValue::Int(ticks)) => {
                    // Vanilla lets air go negative while drowning.
                    self.air = Some(u32::try_from(*ticks).unwrap_or(0));
                }
                _ => {}
            }
        }
    }
}

/// Slots in the player's own container (`container_id` 0). Vanilla's
/// `InventoryMenu` has 5 crafting result/grid slots, 4 armour slots, 27
/// inventory slots, 9 hotbar slots and 1 offhand slot.
pub(crate) const PLAYER_CONTAINER_SIZE: usize = 46;

/// Write a slot, growing the slot list when a packet names a slot past its
/// current end (the first packet may be a single slot update). Callers that
/// know the container size must check the slot first; used for open containers,
/// whose size the server only reveals through `set_content`.
pub(crate) fn set_slot(slots: &mut Vec<ItemStack>, slot: u16, item: ItemStack) {
    let index = usize::from(slot);
    if slots.len() <= index {
        slots.resize(index + 1, ItemStack::Empty);
    }
    slots[index] = item;
}

impl Health {
    pub(crate) fn new(health: f32, food: u32, saturation: f32) -> Self {
        Self { health, food, saturation }
    }
}

impl Experience {
    pub(crate) fn new(progress: f32, level: u32, total: u32) -> Self {
        Self { progress, level, total }
    }
}

impl ContainerClick {
    pub(crate) fn new(
        container_id: i32,
        slot: i16,
        button: u8,
        changed_slots: Vec<ChangedSlot>,
        carried: Option<(ItemKind, i32)>,
    ) -> Self {
        Self { container_id, slot, button, changed_slots, carried }
    }
}

impl ChangedSlot {
    pub(crate) fn new(slot: u16, kind: ItemKind, count: i32) -> Self {
        Self { slot, kind, count }
    }
}
