//! A bounded ring of recent sound packets, for the perception API's hearing.

use azalea_core::position::Vec3;

/// Sounds kept in [`World::sounds`]. A hostile or noisy stream cannot grow the
/// ring; the oldest sound is dropped first.
pub const MAX_SOUNDS: usize = 256;

/// One sound the server sent the client, placed in the world.
#[derive(Clone, Debug, PartialEq)]
pub struct Sound {
    id: String,
    position: Vec3,
    tick: u64,
    entity: Option<i32>,
}

impl Sound {
    pub(crate) fn new(id: String, position: Vec3, tick: u64, entity: Option<i32>) -> Self {
        Self { id, position, tick, entity }
    }

    /// Sound-event id, e.g. `minecraft:block.stone.break`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Where the sound was heard, in world coordinates.
    pub fn position(&self) -> Vec3 {
        self.position
    }

    /// The followed connection's tick at the moment the packet arrived.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// The entity that made the sound, when the packet named one.
    pub fn entity(&self) -> Option<i32> {
        self.entity
    }
}
