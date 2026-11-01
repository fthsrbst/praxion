//! Remembered block states, bounded and least-recently-used first out.
//!
//! Only blocks that a sight query actually answered are stored. A re-sighting
//! refreshes both the state and its place in the LRU order.

use std::collections::{BTreeMap, HashMap};

use praxion_world::{BlockPos, BlockState};

#[derive(Clone, Copy, Debug)]
struct Entry {
    state: BlockState,
    seen_tick: u64,
    use_seq: u64,
}

/// A remembered block state and the tick it was last seen.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Remembered {
    pub state: BlockState,
    pub seen_tick: u64,
}

/// Bounded LRU memory of seen block positions.
#[derive(Debug)]
pub(crate) struct Memory {
    capacity: usize,
    entries: HashMap<BlockPos, Entry>,
    /// Least recently used first.
    order: BTreeMap<u64, BlockPos>,
    next_seq: u64,
}

impl Memory {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: HashMap::new(),
            order: BTreeMap::new(),
            next_seq: 0,
        }
    }

    pub(crate) fn record(&mut self, pos: BlockPos, state: BlockState, tick: u64) {
        if self.capacity == 0 {
            return;
        }
        let seq = self.next_seq;
        self.next_seq += 1;
        if let Some(old) = self.entries.get_mut(&pos) {
            self.order.remove(&old.use_seq);
            old.state = state;
            old.seen_tick = tick;
            old.use_seq = seq;
            self.order.insert(seq, pos);
            return;
        }
        self.entries.insert(pos, Entry { state, seen_tick: tick, use_seq: seq });
        self.order.insert(seq, pos);
        while self.entries.len() > self.capacity {
            let Some((&oldest, &pos)) = self.order.iter().next() else {
                break;
            };
            self.order.remove(&oldest);
            self.entries.remove(&pos);
        }
    }

    pub(crate) fn get(&self, pos: BlockPos) -> Option<Remembered> {
        self.entries
            .get(&pos)
            .map(|entry| Remembered { state: entry.state, seen_tick: entry.seen_tick })
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(x: i32) -> BlockPos {
        BlockPos::new(x, 64, 0)
    }

    fn state(id: u16) -> BlockState {
        BlockState::try_from(u32::from(id)).expect("valid block state")
    }

    #[test]
    fn the_least_recently_used_entry_is_evicted() {
        let mut memory = Memory::new(2);
        memory.record(pos(1), state(1), 1);
        memory.record(pos(2), state(2), 1);
        // Re-seeing 1 makes 2 the least recently used.
        memory.record(pos(1), state(1), 2);
        memory.record(pos(3), state(3), 2);
        assert_eq!(memory.len(), 2);
        assert!(memory.get(pos(2)).is_none());
        assert!(memory.get(pos(1)).is_some());
        assert_eq!(memory.get(pos(3)).unwrap().state, state(3));
    }
}
