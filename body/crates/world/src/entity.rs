//! Tracked entities: identity, position, rotation, velocity and raw metadata.

use azalea_core::position::Vec3;
use azalea_entity::{EntityDataItem, EntityDataValue, Pose};
use azalea_inventory::ItemStack;
use azalea_registry::builtin::EntityKind;
use uuid::Uuid;

/// One entity the server told us about.
#[derive(Clone, Debug, PartialEq)]
pub struct Entity {
    id: i32,
    uuid: Uuid,
    kind: EntityKind,
    pos: Vec3,
    yaw: f32,
    pitch: f32,
    head_yaw: f32,
    velocity: Vec3,
    metadata: Vec<EntityDataItem>,
}

impl Entity {
    pub(crate) fn new(
        id: i32,
        uuid: Uuid,
        kind: EntityKind,
        pos: Vec3,
        yaw: f32,
        pitch: f32,
        head_yaw: f32,
    ) -> Self {
        Self {
            id,
            uuid,
            kind,
            pos,
            yaw,
            pitch,
            head_yaw,
            velocity: Vec3::ZERO,
            metadata: Vec::new(),
        }
    }

    pub fn id(&self) -> i32 {
        self.id
    }

    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    pub fn kind(&self) -> EntityKind {
        self.kind
    }

    pub fn position(&self) -> Vec3 {
        self.pos
    }

    pub fn rotation(&self) -> (f32, f32) {
        (self.yaw, self.pitch)
    }

    pub fn head_yaw(&self) -> f32 {
        self.head_yaw
    }

    pub fn velocity(&self) -> Vec3 {
        self.velocity
    }

    /// Every metadata item the server has sent for this entity, in the order
    /// the indices were first seen (an index is replaced in place on update).
    ///
    /// The item stack of an item entity and the pose of a mob are available
    /// through [`Entity::item_stack`] and [`Entity::pose`]. Their metadata
    /// indices depend on the entity type; the raw items keep the mapping
    /// available without baking this version's per-type layout into the store.
    pub fn metadata(&self) -> &[EntityDataItem] {
        &self.metadata
    }

    /// The first item stack in the entity's metadata, if any. For an item
    /// entity this is its stack.
    pub fn item_stack(&self) -> Option<&ItemStack> {
        self.metadata.iter().find_map(|item| match &item.value {
            EntityDataValue::ItemStack(stack) => Some(stack),
            _ => None,
        })
    }

    /// The first pose in the entity's metadata, if any.
    pub fn pose(&self) -> Option<Pose> {
        self.metadata.iter().find_map(|item| match item.value {
            EntityDataValue::Pose(pose) => Some(pose),
            _ => None,
        })
    }

    /// Whether the entity is dead and playing its death animation. A killed
    /// mob stays in the store for about 20 ticks with the `Dying` pose before
    /// the server removes it; it is neither a target nor a threat any more.
    pub fn is_dying(&self) -> bool {
        self.pose() == Some(Pose::Dying)
    }

    pub(crate) fn set_position(&mut self, pos: Vec3) {
        self.pos = pos;
    }

    pub(crate) fn set_rotation(&mut self, yaw: f32, pitch: f32) {
        self.yaw = yaw;
        self.pitch = pitch;
    }

    pub(crate) fn set_velocity(&mut self, velocity: Vec3) {
        self.velocity = velocity;
    }

    pub(crate) fn merge_metadata(&mut self, items: &[EntityDataItem]) {
        for item in items {
            match self.metadata.iter_mut().find(|existing| existing.index == item.index) {
                Some(existing) => *existing = item.clone(),
                None => self.metadata.push(item.clone()),
            }
        }
    }
}
