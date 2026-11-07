//! The cross-tick visibility cache, shared by the block and entity queries.
//!
//! An entry remembers whether one key was visible. It stays valid while the
//! block state is the same (for blocks), the eye is in the same cell, and no
//! chunk the rays crossed changed (architecture §5.1). `checked` is the world
//! revision the entry was last verified against, so a tick with no block change
//! validates every entry with one integer compare; otherwise `crossed` names
//! the chunks whose revisions must still match.

use std::collections::HashMap;

use praxion_world::{BlockPos, BlockState, World};

use crate::hash::FxBuildHasher;
use crate::sight::CrossedChunks;

/// Cache entries kept. Moving the eye to another cell drops the old cell's
/// entries; a query that fills the cache without moving clears it, which only
/// costs rays the next time the same blocks are asked for.
const MAX_ENTRIES: usize = 8_192;

/// The key of a cached block answer: the eye cell and the block cell.
pub(crate) type BlockCacheKey = (BlockPos, BlockPos);

/// The key of a cached entity answer: the eye cell, the entity id and the
/// entity cell, so a moved entity is a miss.
pub(crate) type EntityCacheKey = (BlockPos, i32, BlockPos);

/// A key that carries the eye cell, which is what ties a cache to one eye cell:
/// `begin_tick` keeps only the current one.
pub(crate) trait EyeKey: Eq + std::hash::Hash {
    fn eye_cell(&self) -> BlockPos;
}

impl EyeKey for BlockCacheKey {
    fn eye_cell(&self) -> BlockPos {
        self.0
    }
}

impl EyeKey for EntityCacheKey {
    fn eye_cell(&self) -> BlockPos {
        self.0
    }
}

/// One cached visibility answer.
#[derive(Clone, Copy, Debug)]
struct Entry {
    /// The block state the answer was for. Entities have no state, so their
    /// entries carry `None` and the compare in [`VisibilityCache::cached`] is
    /// always equal.
    state: Option<BlockState>,
    visible: bool,
    /// The world revision the answer was last verified against.
    checked: u64,
    crossed: CrossedChunks,
}

/// The visibility cache for one kind of query.
pub(crate) struct VisibilityCache<K> {
    entries: HashMap<K, Entry, FxBuildHasher>,
}

impl<K: EyeKey> VisibilityCache<K> {
    pub(crate) fn new() -> Self {
        Self {
            entries: HashMap::default(),
        }
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }

    /// Keep only the entries of `eye_cell`; the entries of the cell the eye
    /// left can never be used again.
    pub(crate) fn retain_eye_cell(&mut self, eye_cell: BlockPos) {
        self.entries.retain(|key, _| key.eye_cell() == eye_cell);
    }

    /// The cached answer for `key`, when it is still valid, refreshed against
    /// the current world revision so the next lookup is one compare.
    pub(crate) fn cached(
        &mut self,
        world: &World,
        key: K,
        state: Option<BlockState>,
        revision: u64,
    ) -> Option<bool> {
        let entry = self.entries.get_mut(&key)?;
        if entry.state != state {
            // The block itself changed; a fresh cast is needed.
            self.entries.remove(&key);
            return None;
        }
        if entry.checked != revision {
            if !entry.crossed.is_current(world) {
                self.entries.remove(&key);
                return None;
            }
            entry.checked = revision;
        }
        Some(entry.visible)
    }

    /// Store an answer, keeping the cache bounded.
    pub(crate) fn remember(
        &mut self,
        key: K,
        state: Option<BlockState>,
        visible: bool,
        revision: u64,
        crossed: CrossedChunks,
    ) {
        if self.entries.len() >= MAX_ENTRIES {
            self.retain_eye_cell(key.eye_cell());
            if self.entries.len() >= MAX_ENTRIES {
                self.entries.clear();
            }
        }
        self.entries.insert(
            key,
            Entry {
                state,
                visible,
                checked: revision,
                crossed,
            },
        );
    }
}
