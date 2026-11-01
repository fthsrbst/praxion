//! Fact and primitive registries.
//!
//! The loader type-checks every call against these signatures. Facts answer
//! three-valued: a bool fact is `True | False | Unknown(fact)`, an int fact is
//! `Known(n) | Unknown(fact)`.
//!
//! Primitive records follow architecture §6.2: one versioned record per
//! primitive with typed parameters, discrete or continuous kind, cancellation
//! class, default `max_ticks`, declared effects and the evidence kind that
//! settles a receipt (§5.3). The effects below are hand-written for Demo-0 step
//! 7. Step 8 generates them from the game data report; the TODO on `mine`
//! marks the one table this step does not have.

use std::collections::BTreeMap;

use crate::ast::{ArgType, ValueType};

/// The signature of one fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactSig {
    pub name: &'static str,
    pub params: Vec<ValueType>,
    pub ret: ValueType,
}

/// The facts the hand-written core uses.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Facts {
    map: BTreeMap<&'static str, FactSig>,
}

impl Facts {
    pub fn standard() -> Self {
        let mut facts = Self::default();
        facts.add(FactSig {
            name: "count",
            params: vec![ValueType::Item],
            ret: ValueType::Int,
        });
        facts.add(FactSig {
            name: "has",
            params: vec![ValueType::Item],
            ret: ValueType::Bool,
        });
        facts.add(FactSig {
            name: "near",
            params: vec![ValueType::Block, ValueType::Int],
            ret: ValueType::Bool,
        });
        facts.add(FactSig {
            name: "near_entity",
            params: vec![ValueType::EntityKind, ValueType::Int],
            ret: ValueType::Bool,
        });
        facts
    }

    fn add(&mut self, sig: FactSig) {
        self.map.insert(sig.name, sig);
    }

    pub fn get(&self, name: &str) -> Option<&FactSig> {
        self.map.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &FactSig> {
        self.map.values()
    }
}

/// Whether a primitive finishes once and reports an outcome, or is re-selected
/// every tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrimitiveKind {
    Discrete,
    Continuous,
}

/// How a primitive reacts to a preemption (session 4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CancellationClass {
    /// Abandoning it has no partial effect.
    Atomic,
    /// It can be stopped; partial progress is lost.
    Interruptible,
    /// It must run to its outcome before the body is released.
    MustFinish,
}

/// The evidence that settles a receipt (architecture §5.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EvidenceKind {
    BlockBecomesAir,
    BlockStateChanged,
    InventoryDelta,
    NoCorrection,
    /// The fact an `observe` asked for is no longer unknown.
    FactKnown,
    /// The attacked entity's health drops or it disappears. §5.3's table does
    /// not list attack; this is the proposed reading for step 7.
    EntityHurt,
}

/// An item delta in a primitive's declared effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemDelta {
    pub item: String,
    pub count: u32,
}

/// One side of a block change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockRef {
    Air,
    Named(&'static str),
    /// The block or item named by argument `index`.
    Param(usize),
}

/// A block state change in a primitive's declared effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockChange {
    pub from: BlockRef,
    pub to: BlockRef,
}

/// What a primitive declares it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub produces: Vec<ItemDelta>,
    pub consumes: Vec<ItemDelta>,
    pub changes: Vec<BlockChange>,
}

/// One typed primitive parameter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrimParam {
    pub name: &'static str,
    pub ty: ArgType,
}

/// One versioned primitive record (§6.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimitiveRecord {
    pub name: &'static str,
    pub version: u32,
    pub params: Vec<PrimParam>,
    pub kind: PrimitiveKind,
    pub cancellation: CancellationClass,
    pub default_max_ticks: u32,
    pub effects: Effects,
    pub evidence: EvidenceKind,
}

impl PrimitiveRecord {
    /// The type expected at argument `index`.
    pub fn arg_type(&self, index: usize) -> Option<ArgType> {
        self.params.get(index).map(|p| p.ty)
    }
}

/// All primitive records the body knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Primitives {
    map: BTreeMap<&'static str, PrimitiveRecord>,
}

impl Primitives {
    pub fn standard() -> Self {
        let mut primitives = Self::default();
        primitives.add(PrimitiveRecord {
            name: "move_to",
            version: 1,
            params: vec![PrimParam {
                name: "target",
                ty: ArgType::BlockOrEntity,
            }],
            kind: PrimitiveKind::Continuous,
            cancellation: CancellationClass::Interruptible,
            default_max_ticks: 60,
            effects: Effects::default(),
            evidence: EvidenceKind::NoCorrection,
        });
        primitives.add(PrimitiveRecord {
            name: "mine",
            version: 1,
            params: vec![PrimParam {
                name: "block",
                ty: ArgType::Value(ValueType::Block),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            default_max_ticks: 40,
            // TODO(step 8): the drop table comes from the game data report;
            // the fake world hard-codes stone -> cobblestone and log -> log.
            effects: Effects {
                produces: Vec::new(),
                consumes: Vec::new(),
                changes: vec![BlockChange {
                    from: BlockRef::Param(0),
                    to: BlockRef::Air,
                }],
            },
            evidence: EvidenceKind::BlockBecomesAir,
        });
        primitives.add(PrimitiveRecord {
            name: "place",
            version: 1,
            params: vec![PrimParam {
                name: "block",
                ty: ArgType::Value(ValueType::Block),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            default_max_ticks: 20,
            effects: Effects {
                produces: Vec::new(),
                consumes: vec![ItemDelta {
                    item: "$block".into(),
                    count: 1,
                }],
                changes: vec![BlockChange {
                    from: BlockRef::Air,
                    to: BlockRef::Param(0),
                }],
            },
            evidence: EvidenceKind::BlockStateChanged,
        });
        primitives.add(PrimitiveRecord {
            name: "craft",
            version: 1,
            params: vec![PrimParam {
                name: "item",
                ty: ArgType::Value(ValueType::Item),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Atomic,
            default_max_ticks: 20,
            effects: Effects {
                produces: vec![ItemDelta {
                    item: "$item".into(),
                    count: 1,
                }],
                // TODO(step 8): recipe inputs come from the game data report.
                consumes: Vec::new(),
                changes: Vec::new(),
            },
            evidence: EvidenceKind::InventoryDelta,
        });
        primitives.add(PrimitiveRecord {
            name: "eat",
            version: 1,
            params: vec![PrimParam {
                name: "item",
                ty: ArgType::Value(ValueType::Item),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            default_max_ticks: 10,
            effects: Effects {
                produces: Vec::new(),
                consumes: vec![ItemDelta {
                    item: "$item".into(),
                    count: 1,
                }],
                changes: Vec::new(),
            },
            evidence: EvidenceKind::InventoryDelta,
        });
        primitives.add(PrimitiveRecord {
            name: "attack",
            version: 1,
            params: vec![PrimParam {
                name: "target",
                ty: ArgType::Value(ValueType::EntityKind),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Atomic,
            default_max_ticks: 20,
            effects: Effects::default(),
            evidence: EvidenceKind::EntityHurt,
        });
        primitives.add(PrimitiveRecord {
            name: "equip",
            version: 1,
            params: vec![PrimParam {
                name: "item",
                ty: ArgType::Value(ValueType::Item),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Atomic,
            default_max_ticks: 5,
            effects: Effects::default(),
            evidence: EvidenceKind::InventoryDelta,
        });
        primitives.add(PrimitiveRecord {
            name: "wait",
            version: 1,
            params: vec![PrimParam {
                name: "ticks",
                ty: ArgType::Value(ValueType::Int),
            }],
            kind: PrimitiveKind::Continuous,
            cancellation: CancellationClass::Interruptible,
            default_max_ticks: 100,
            effects: Effects::default(),
            evidence: EvidenceKind::NoCorrection,
        });
        primitives.add(PrimitiveRecord {
            name: "observe",
            version: 1,
            params: vec![PrimParam {
                name: "fact",
                ty: ArgType::Fact,
            }],
            kind: PrimitiveKind::Continuous,
            cancellation: CancellationClass::Interruptible,
            default_max_ticks: 20,
            effects: Effects::default(),
            evidence: EvidenceKind::FactKnown,
        });
        primitives
    }

    fn add(&mut self, record: PrimitiveRecord) {
        self.map.insert(record.name, record);
    }

    pub fn get(&self, name: &str) -> Option<&PrimitiveRecord> {
        self.map.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PrimitiveRecord> {
        self.map.values()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_required_record_exists() {
        let primitives = Primitives::standard();
        for name in [
            "move_to", "mine", "place", "craft", "eat", "attack", "equip", "wait", "observe",
        ] {
            let record = primitives.get(name).unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(record.version, 1);
            assert!(record.default_max_ticks > 0);
        }
    }

    #[test]
    fn kinds_and_evidence() {
        let p = Primitives::standard();
        assert_eq!(p.get("mine").unwrap().kind, PrimitiveKind::Discrete);
        assert_eq!(p.get("move_to").unwrap().kind, PrimitiveKind::Continuous);
        assert_eq!(p.get("mine").unwrap().evidence, EvidenceKind::BlockBecomesAir);
        assert_eq!(
            p.get("craft").unwrap().evidence,
            EvidenceKind::InventoryDelta
        );
    }

    #[test]
    fn standard_facts() {
        let f = Facts::standard();
        assert_eq!(f.get("count").unwrap().ret, ValueType::Int);
        assert_eq!(f.get("near").unwrap().params, vec![ValueType::Block, ValueType::Int]);
        assert!(f.get("nope").is_none());
    }
}
