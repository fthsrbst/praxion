//! The ledger of denied attempts and ledgered privileged reads.
//!
//! A ledger entry is recorded at a bounded rate: the first occurrence per
//! (query kind, fact kind) per tick gets an entry, later ones only raise that
//! entry's counter. It records access, not a fact: a query perception denied,
//! and a privileged read perception served, both land here (architecture §5.1).
//! The event log will consume this in step 6; for now the entries are readable
//! in memory.

use std::collections::BTreeMap;
use std::fmt;

/// What was asked.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum QueryKind {
    Block,
    BlocksIn,
    Entity,
    EntitiesNear,
    EntityHealth,
    /// A privileged column read (heightmap plus the blocks above it).
    SkyColumn,
}

impl QueryKind {
    pub fn as_str(self) -> &'static str {
        match self {
            QueryKind::Block => "block",
            QueryKind::BlocksIn => "blocks_in",
            QueryKind::Entity => "entity",
            QueryKind::EntitiesNear => "entities_near",
            QueryKind::EntityHealth => "entity_health",
            QueryKind::SkyColumn => "sky_column",
        }
    }
}

impl fmt::Display for QueryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of fact a ledger entry is about: one the caller was denied, or
/// one a privileged query served.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum FactKind {
    BlockState,
    RegionSize,
    ResultLimit,
    EntityPosition,
    EntityHealth,
    /// The raw contents of a column (surface plus the blocks above it).
    ///
    /// This one is not a denial: [`Perception::privileged_column`] serves it as
    /// a privileged read, and every scan that reads a column ledgers it.
    ///
    /// [`Perception::privileged_column`]: crate::Perception::privileged_column
    Column,
}

impl FactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FactKind::BlockState => "block_state",
            FactKind::RegionSize => "region_size",
            FactKind::ResultLimit => "result_limit",
            FactKind::EntityPosition => "entity_position",
            FactKind::EntityHealth => "entity_health",
            FactKind::Column => "column",
        }
    }
}

impl fmt::Display for FactKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One ledger line: when the current run of ledgered reads started and how many
/// were counted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LedgerEntry {
    first_tick: u64,
    last_tick: u64,
    count: u64,
}

impl LedgerEntry {
    pub fn first_tick(&self) -> u64 {
        self.first_tick
    }

    pub fn last_tick(&self) -> u64 {
        self.last_tick
    }

    pub fn count(&self) -> u64 {
        self.count
    }
}

/// Bounded ledger of denied attempts and privileged reads, keyed by (query
/// kind, fact kind).
#[derive(Debug, Default)]
pub struct Ledger {
    entries: BTreeMap<(QueryKind, FactKind), LedgerEntry>,
}

impl Ledger {
    pub fn record(&mut self, query: QueryKind, fact: FactKind, tick: u64) {
        let entry = self.entries.entry((query, fact)).or_insert(LedgerEntry {
            first_tick: tick,
            last_tick: tick,
            count: 0,
        });
        entry.last_tick = tick;
        entry.count = entry.count.saturating_add(1);
    }

    pub fn entries(&self) -> impl Iterator<Item = ((QueryKind, FactKind), &LedgerEntry)> {
        self.entries.iter().map(|(key, entry)| (*key, entry))
    }

    pub fn get(&self, query: QueryKind, fact: FactKind) -> Option<&LedgerEntry> {
        self.entries.get(&(query, fact))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn total(&self) -> u64 {
        self.entries.values().map(|entry| entry.count).sum()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}
