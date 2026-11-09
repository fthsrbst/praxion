//! Fact and primitive registries.
//!
//! The loader type-checks every call against these signatures. Facts answer
//! three-valued: a bool fact is `True | False | Unknown(fact)`, an int fact is
//! `Known(n) | Unknown(fact)`.
//!
//! Primitive records follow architecture §6.2: one versioned record per
//! primitive with typed parameters, discrete or continuous kind, cancellation
//! class, default `max_ticks`, declared effects and the evidence kind that
//! settles a receipt (§5.3). Effects come from game data, not from hand: craft
//! effects from [`praxion_gamedata::RECIPES`], smelt effects from
//! [`praxion_gamedata::SMELTING`] and mine/attack drops from the 26.3 loot
//! tables in the pinned server jar. For a primitive whose effect
//! depends on its first argument, [`PrimitiveRecord::effects_for`] returns the
//! exact effect for that argument; `effects` is the union over all arguments.

use std::collections::BTreeMap;

use praxion_gamedata as gamedata;

use crate::ast::{ArgType, ArgValue, ValueType};

/// The tick budget of `explore`: how long one call may search before it fails
/// with `not_found`.
pub const EXPLORE_MAX_TICKS: u32 = 1200;

/// How close `explore` must bring its target before it succeeds: the radius of
/// the `mine` search (see `block_target`), so the target is in mining reach
/// when the primitive returns. An exploration call still scans the wider
/// perception known map for frontiers.
pub const EXPLORE_NEAR_RADIUS: u32 = 5;

/// How far `explore` looks for its target in the perception known map before
/// it falls back to walking to a frontier. This is the sight range: the view
/// scan records the first blocking cell of each ray, so a log out to 64 blocks
/// reaches the map, and a search shorter than that would walk away from a tree
/// already in plain sight. The 80-block frontier box still reaches past the
/// known ring. The known map is section-ordered with a distance lower bound,
/// so a near target stays cheap.
///
/// This is not the cue radius: leaves are see-through, so the view scan records
/// no canopy. The cue may only use the much shorter sweep radius.
pub const EXPLORE_SEARCH_RADIUS: u32 = 64;

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

/// One possible drop from a loot table, with its count range and tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemDrop {
    pub item: String,
    pub min: u32,
    pub max: u32,
    pub tool: Option<String>,
}

/// One side of a block change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockRef {
    Air,
    Named(&'static str),
    /// The block or item named by argument `index`.
    Param(usize),
    /// Any breakable ground block (`dig_down` and `branch_mine` remove
    /// whatever lies in their way).
    Any,
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
    /// Exact outputs, or the maximum output for a loot-table drop.
    pub produces: Vec<ItemDelta>,
    /// Possible loot-table drops with their full count range and tool.
    pub drops: Vec<ItemDrop>,
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
    /// Effects for a concrete first argument (mine block, attack entity, craft
    /// item). `effects` above is the union over all arguments.
    pub per_argument: BTreeMap<String, Effects>,
    pub evidence: EvidenceKind,
}

impl PrimitiveRecord {
    /// The declared effects for this call, resolving an argument-dependent
    /// primitive (craft, mine, attack) by its first argument.
    pub fn effects_for(&self, args: &[ArgValue]) -> &Effects {
        args.first()
            .and_then(ArgValue::name)
            .and_then(|name| self.per_argument.get(name))
            .unwrap_or(&self.effects)
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
            // Covers the runner's 80-tick wait for an unseen target.
            default_max_ticks: 100,
            effects: Effects::default(),
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::NoCorrection,
        });

        let mine_changes = vec![BlockChange {
            from: BlockRef::Param(0),
            to: BlockRef::Air,
        }];
        let (mine_produces, mine_drops) =
            summary_drops(gamedata::BLOCKS.iter().map(|table| table.drops));
        let dig_down_effects = Effects {
            produces: mine_produces.clone(),
            drops: mine_drops.clone(),
            consumes: Vec::new(),
            changes: vec![BlockChange {
                from: BlockRef::Any,
                to: BlockRef::Air,
            }],
        };
        let mut mine = PrimitiveRecord {
            name: "mine",
            version: 1,
            params: vec![PrimParam {
                name: "block",
                ty: ArgType::Value(ValueType::Block),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            // The dig intent's own window is 160 ticks (slow blocks, no tool),
            // then the post-break walk to the drop may take 60 more; the
            // interpreter must not preempt the pickup earlier.
            default_max_ticks: 240,
            effects: Effects {
                produces: mine_produces,
                drops: mine_drops,
                consumes: Vec::new(),
                changes: mine_changes.clone(),
            },
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::BlockBecomesAir,
        };
        for block in gamedata::BLOCKS {
            mine.per_argument.insert(
                block.block.to_string(),
                loot_effects(block.drops, mine_changes.clone()),
            );
        }
        insert_block_tag_effects(&mut mine);
        primitives.add(mine);

        primitives.add(PrimitiveRecord {
            name: "dig_down",
            version: 1,
            params: vec![PrimParam {
                name: "block",
                ty: ArgType::Value(ValueType::Block),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            // Up to 40 dig receipts (160-tick window each, usually a few ticks
            // for dirt), tool selection and the walk onto each stair; the
            // interpreter must not preempt a staircase that is progressing.
            default_max_ticks: 1500,
            effects: dig_down_effects.clone(),
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::BlockBecomesAir,
        });

        primitives.add(PrimitiveRecord {
            name: "branch_mine",
            version: 1,
            params: vec![PrimParam {
                name: "block",
                ty: ArgType::Value(ValueType::Block),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            // Worst case, from the new start-height budget: a mountain start at
            // y 150 tunnels for the lowest target (y 16) after a descent of
            // 3 * 134 = 402 digs, plus the 160-dig tunnel reserve: 562 digs. At
            // about 26 ticks per dig (the receipt, the walk onto each step, a
            // gravel settle) that is 14,612 ticks, so 15000 covers it with room
            // for a slow pickaxe. The interpreter must not preempt a tunnel
            // that is progressing.
            default_max_ticks: 15000,
            effects: dig_down_effects,
            per_argument: BTreeMap::new(),
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
            // Select the slot, aim, then the 40-tick place receipt window.
            default_max_ticks: 80,
            effects: Effects {
                produces: Vec::new(),
                drops: Vec::new(),
                consumes: vec![ItemDelta {
                    item: "$block".into(),
                    count: 1,
                }],
                changes: vec![BlockChange {
                    from: BlockRef::Air,
                    to: BlockRef::Param(0),
                }],
            },
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::BlockStateChanged,
        });

        let mut craft = PrimitiveRecord {
            name: "craft",
            version: 1,
            params: vec![PrimParam {
                name: "item",
                ty: ArgType::Value(ValueType::Item),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Atomic,
            // Clear the 2x2 grid (<= 21), walk to the table (<= 80, decision
            // 2026-11-09) and open it (<= 60), wait for the content (40), place
            // up to 27 clicks two ticks apart (54), wait for the result (80)
            // and take it (140): about 480 in the worst case, which the
            // primitive owns.
            default_max_ticks: 500,
            effects: craft_summary(),
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::InventoryDelta,
        };
        for recipe in gamedata::RECIPES {
            craft
                .per_argument
                .insert(recipe.output.to_string(), craft_effects(recipe));
        }
        insert_item_tag_effects(&mut craft);
        primitives.add(craft);

        let mut smelt = PrimitiveRecord {
            name: "smelt",
            version: 1,
            params: vec![PrimParam {
                name: "item",
                ty: ArgType::Value(ValueType::Item),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            // Walk to the furnace and open it (<= 100), load the content
            // (<= 40), the clicks (~20), cook one item (200 + 60 for the menu
            // round trip) and take it out (<= 140).
            default_max_ticks: 600,
            effects: smelting_summary(),
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::InventoryDelta,
        };
        for recipe in gamedata::SMELTING {
            merge_effects(
                smelt
                    .per_argument
                    .entry(recipe.output.to_string())
                    .or_default(),
                &smelting_effects(recipe),
            );
        }
        primitives.add(smelt);

        primitives.add(PrimitiveRecord {
            name: "eat",
            version: 1,
            params: vec![PrimParam {
                name: "item",
                ty: ArgType::Value(ValueType::Item),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            // The Use hold is 32 ticks; the primitive's not-hungry bound is
            // 140.
            default_max_ticks: 180,
            effects: Effects {
                produces: Vec::new(),
                drops: Vec::new(),
                consumes: vec![ItemDelta {
                    item: "$item".into(),
                    count: 1,
                }],
                changes: Vec::new(),
            },
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::InventoryDelta,
        });

        let (attack_produces, attack_drops) =
            summary_drops(gamedata::ENTITIES.iter().map(|table| table.drops));
        let mut attack = PrimitiveRecord {
            name: "attack",
            version: 1,
            params: vec![PrimParam {
                name: "target",
                ty: ArgType::Value(ValueType::EntityKind),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Atomic,
            // A long zombie fight is 114 ticks in the live table; the
            // primitive's own target-alive bound is 400.
            default_max_ticks: 420,
            effects: Effects {
                produces: attack_produces,
                drops: attack_drops,
                consumes: Vec::new(),
                changes: Vec::new(),
            },
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::EntityHurt,
        };
        for entity in gamedata::ENTITIES {
            attack.per_argument.insert(
                entity.entity.to_string(),
                loot_effects(entity.drops, Vec::new()),
            );
        }
        primitives.add(attack);

        primitives.add(PrimitiveRecord {
            name: "equip",
            version: 1,
            params: vec![PrimParam {
                name: "item",
                ty: ArgType::Value(ValueType::Item),
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Atomic,
            // A hotbar select is one tick; a menu swap waits up to 40.
            default_max_ticks: 80,
            effects: Effects::default(),
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::InventoryDelta,
        });

        primitives.add(PrimitiveRecord {
            name: "explore",
            version: 1,
            params: vec![PrimParam {
                name: "target",
                ty: ArgType::Value(ValueType::Block),
            }],
            kind: PrimitiveKind::Continuous,
            cancellation: CancellationClass::Interruptible,
            // One exploration call searches until the target is in sight of the
            // known map or this budget runs out.
            default_max_ticks: EXPLORE_MAX_TICKS,
            effects: Effects::default(),
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::NoCorrection,
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
            per_argument: BTreeMap::new(),
            evidence: EvidenceKind::NoCorrection,
        });

        primitives.add(PrimitiveRecord {
            name: "observe",
            version: 1,
            params: vec![PrimParam {
                name: "fact",
                ty: ArgType::Fact,
            }],
            kind: PrimitiveKind::Discrete,
            cancellation: CancellationClass::Interruptible,
            // Covers the runner's 80-tick alignment wait plus the 40-tick fact window.
            default_max_ticks: 120,
            effects: Effects::default(),
            per_argument: BTreeMap::new(),
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

fn delta(item: &str, count: u32) -> ItemDelta {
    ItemDelta {
        item: item.to_string(),
        count,
    }
}

/// The exact effects of one loot table, plus the block change if it has one.
fn loot_effects(drops: &'static [gamedata::Drop], changes: Vec<BlockChange>) -> Effects {
    Effects {
        produces: drops
            .iter()
            .map(|drop| delta(drop.item, drop.max))
            .collect(),
        drops: drops.iter().map(item_drop).collect(),
        consumes: Vec::new(),
        changes,
    }
}

fn item_drop(drop: &gamedata::Drop) -> ItemDrop {
    ItemDrop {
        item: drop.item.to_string(),
        min: drop.min,
        max: drop.max,
        tool: drop.tool.map(str::to_string),
    }
}

/// The union of several loot tables: one entry per item, with the widest range.
fn summary_drops(
    tables: impl Iterator<Item = &'static [gamedata::Drop]>,
) -> (Vec<ItemDelta>, Vec<ItemDrop>) {
    let mut merged: BTreeMap<&'static str, (u32, u32, Option<&'static str>)> = BTreeMap::new();
    for table in tables {
        for drop in table {
            let entry = merged
                .entry(drop.item)
                .or_insert((drop.min, drop.max, drop.tool));
            entry.0 = entry.0.min(drop.min);
            entry.1 = entry.1.max(drop.max);
            if entry.2.is_none() {
                entry.2 = drop.tool;
            }
        }
    }
    let produces = merged
        .iter()
        .map(|(item, (_, max, _))| delta(item, *max))
        .collect();
    let drops = merged
        .into_iter()
        .map(|(item, (min, max, tool))| ItemDrop {
            item: item.to_string(),
            min,
            max,
            tool: tool.map(str::to_string),
        })
        .collect();
    (produces, drops)
}

fn craft_effects(recipe: &gamedata::Recipe) -> Effects {
    Effects {
        produces: vec![delta(recipe.output, recipe.output_count)],
        drops: Vec::new(),
        consumes: recipe
            .inputs()
            .into_iter()
            .map(|input| ItemDelta {
                item: input.item,
                count: input.count,
            })
            .collect(),
        changes: Vec::new(),
    }
}

/// The union of every recipe, used when a `craft` call is inspected without its
/// argument. `effects_for` gives the exact input and output counts.
fn craft_summary() -> Effects {
    let mut produces: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut consumes: BTreeMap<String, u32> = BTreeMap::new();
    for recipe in gamedata::RECIPES {
        let output = produces.entry(recipe.output).or_insert(0);
        *output = (*output).max(recipe.output_count);
        for input in recipe.inputs() {
            let count = consumes.entry(input.item).or_insert(0);
            *count = (*count).max(input.count);
        }
    }
    Effects {
        produces: produces
            .into_iter()
            .map(|(item, count)| delta(item, count))
            .collect(),
        drops: Vec::new(),
        consumes: consumes
            .into_iter()
            .map(|(item, count)| ItemDelta { item, count })
            .collect(),
        changes: Vec::new(),
    }
}

fn smelting_effects(recipe: &gamedata::SmeltingRecipe) -> Effects {
    Effects {
        produces: vec![delta(recipe.output, 1)],
        drops: Vec::new(),
        consumes: vec![ItemDelta {
            item: recipe.ingredient.effect_item(),
            count: 1,
        }],
        changes: Vec::new(),
    }
}

/// The union of every smelting recipe, used when a `smelt` call is inspected
/// without its argument. `effects_for` gives the exact input and output.
fn smelting_summary() -> Effects {
    let mut summary = Effects::default();
    for recipe in gamedata::SMELTING {
        merge_effects(&mut summary, &smelting_effects(recipe));
    }
    summary
}

/// Adds one `#tag` entry to a primitive's per-argument effects: the union of
/// every member the primitive already knows. `mine(#logs)` therefore declares
/// the drops of every log type, and `craft(#planks)` the inputs of every planks
/// recipe.
fn insert_tag_effects(record: &mut PrimitiveRecord, tags: &[gamedata::Tag]) {
    for tag in tags {
        let mut merged = Effects::default();
        let mut any = false;
        for value in tag.values {
            let Some(effects) = record.per_argument.get(*value) else {
                continue;
            };
            any = true;
            merge_effects(&mut merged, effects);
        }
        if any {
            record.per_argument.insert(format!("#{}", tag.name), merged);
        }
    }
}

fn insert_block_tag_effects(record: &mut PrimitiveRecord) {
    insert_tag_effects(record, gamedata::BLOCK_TAGS);
}

fn insert_item_tag_effects(record: &mut PrimitiveRecord) {
    insert_tag_effects(record, gamedata::ITEM_TAGS);
}

fn merge_effects(into: &mut Effects, other: &Effects) {
    for delta in &other.produces {
        match into
            .produces
            .iter_mut()
            .find(|entry| entry.item == delta.item)
        {
            Some(entry) => entry.count = entry.count.max(delta.count),
            None => into.produces.push(delta.clone()),
        }
    }
    for drop in &other.drops {
        match into.drops.iter_mut().find(|entry| entry.item == drop.item) {
            Some(entry) => {
                entry.min = entry.min.min(drop.min);
                entry.max = entry.max.max(drop.max);
                if entry.tool.is_none() {
                    entry.tool = drop.tool.clone();
                }
            }
            None => into.drops.push(drop.clone()),
        }
    }
    for delta in &other.consumes {
        match into
            .consumes
            .iter_mut()
            .find(|entry| entry.item == delta.item)
        {
            Some(entry) => entry.count = entry.count.max(delta.count),
            None => into.consumes.push(delta.clone()),
        }
    }
    for change in &other.changes {
        if !into.changes.contains(change) {
            into.changes.push(change.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_required_record_exists() {
        let primitives = Primitives::standard();
        for name in [
            "move_to",
            "mine",
            "dig_down",
            "branch_mine",
            "place",
            "craft",
            "smelt",
            "eat",
            "explore",
            "attack",
            "equip",
            "wait",
            "observe",
        ] {
            let record = primitives
                .get(name)
                .unwrap_or_else(|| panic!("missing {name}"));
            assert_eq!(record.version, 1);
            assert!(record.default_max_ticks > 0);
        }
    }

    #[test]
    fn kinds_and_evidence() {
        let p = Primitives::standard();
        assert_eq!(p.get("mine").unwrap().kind, PrimitiveKind::Discrete);
        assert_eq!(p.get("move_to").unwrap().kind, PrimitiveKind::Continuous);
        assert_eq!(p.get("explore").unwrap().kind, PrimitiveKind::Continuous);
        assert_eq!(
            p.get("explore").unwrap().default_max_ticks,
            EXPLORE_MAX_TICKS
        );
        assert_eq!(
            p.get("explore").unwrap().params,
            vec![PrimParam {
                name: "target",
                ty: ArgType::Value(ValueType::Block),
            }]
        );
        assert_eq!(
            p.get("explore").unwrap().evidence,
            EvidenceKind::NoCorrection
        );
        assert_eq!(
            p.get("mine").unwrap().evidence,
            EvidenceKind::BlockBecomesAir
        );
        assert_eq!(
            p.get("craft").unwrap().evidence,
            EvidenceKind::InventoryDelta
        );
        assert_eq!(p.get("observe").unwrap().kind, PrimitiveKind::Discrete);
        assert_eq!(p.get("observe").unwrap().evidence, EvidenceKind::FactKnown);
        assert_eq!(p.get("observe").unwrap().default_max_ticks, 120);
    }

    #[test]
    fn dig_down_digs_ground_with_one_receipt_per_block() {
        let p = Primitives::standard();
        let dig_down = p.get("dig_down").unwrap();
        assert_eq!(dig_down.kind, PrimitiveKind::Discrete);
        assert_eq!(dig_down.evidence, EvidenceKind::BlockBecomesAir);
        assert_eq!(
            dig_down.params,
            vec![PrimParam {
                name: "block",
                ty: ArgType::Value(ValueType::Block),
            }]
        );
        // It removes any ground block, so the effect names no single block,
        // and it declares what mining that ground can drop.
        let effects = dig_down.effects_for(&[ArgValue::Block("stone".into())]);
        assert_eq!(
            effects.changes,
            vec![BlockChange {
                from: BlockRef::Any,
                to: BlockRef::Air,
            }]
        );
        assert!(effects.produces.iter().any(|delta| delta.item == "dirt"));
        assert!(
            effects
                .produces
                .iter()
                .any(|delta| delta.item == "cobblestone")
        );
        // The flight deadline outlasts one dig receipt window and a walk.
        assert!(dig_down.default_max_ticks >= 1000);
    }

    #[test]
    fn branch_mine_shares_dig_down_effects_and_outlasts_its_digs() {
        let p = Primitives::standard();
        let branch = p.get("branch_mine").unwrap();
        let dig_down = p.get("dig_down").unwrap();
        assert_eq!(branch.kind, PrimitiveKind::Discrete);
        assert_eq!(branch.evidence, EvidenceKind::BlockBecomesAir);
        assert_eq!(branch.params, dig_down.params);
        assert_eq!(branch.effects, dig_down.effects);
        assert!(branch.default_max_ticks > dig_down.default_max_ticks);
        // The deadline covers the worst start the budget is sized for: a
        // descent from y 150 (402 digs) plus the 160-dig tunnel reserve, at
        // the ~26 ticks per dig the record's comment assumes.
        assert!(branch.default_max_ticks >= 562 * 26);
    }

    #[test]
    fn standard_facts() {
        let f = Facts::standard();
        assert_eq!(f.get("count").unwrap().ret, ValueType::Int);
        assert_eq!(
            f.get("near").unwrap().params,
            vec![ValueType::Block, ValueType::Int]
        );
        assert!(f.get("nope").is_none());
    }

    #[test]
    fn mine_and_attack_effects_come_from_the_loot_tables() {
        let p = Primitives::standard();
        let mine = p.get("mine").unwrap();
        let stone = mine.effects_for(&[ArgValue::Block("stone".into())]);
        assert_eq!(stone.produces, vec![delta("cobblestone", 1)]);
        assert_eq!(stone.drops[0].min, 1);
        let log = mine.effects_for(&[ArgValue::Block("oak_log".into())]);
        assert_eq!(log.produces, vec![delta("oak_log", 1)]);

        let attack = p.get("attack").unwrap();
        let cow = attack.effects_for(&[ArgValue::EntityKind("cow".into())]);
        let beef = cow.drops.iter().find(|drop| drop.item == "beef").unwrap();
        assert_eq!((beef.min, beef.max), (1, 3));
        let leather = cow
            .drops
            .iter()
            .find(|drop| drop.item == "leather")
            .unwrap();
        assert_eq!((leather.min, leather.max), (0, 2));
    }

    #[test]
    fn craft_effects_match_the_recipe() {
        let p = Primitives::standard();
        let craft = p.get("craft").unwrap();
        let planks = craft.effects_for(&[ArgValue::Item("oak_planks".into())]);
        assert_eq!(
            planks.consumes,
            vec![delta("#oak_logs", 1)],
            "one log tag in"
        );
        assert_eq!(
            planks.produces,
            vec![delta("oak_planks", 4)],
            "four planks out"
        );
        let pickaxe = craft.effects_for(&[ArgValue::Item("stone_pickaxe".into())]);
        assert_eq!(
            pickaxe.consumes,
            vec![delta("#stone_tool_materials", 3), delta("stick", 2)]
        );
        assert_eq!(pickaxe.produces, vec![delta("stone_pickaxe", 1)]);
    }

    #[test]
    fn smelt_effects_come_from_the_smelting_recipes() {
        let p = Primitives::standard();
        let smelt = p.get("smelt").unwrap();
        assert_eq!(smelt.kind, PrimitiveKind::Discrete);
        assert_eq!(smelt.evidence, EvidenceKind::InventoryDelta);
        assert_eq!(smelt.default_max_ticks, 600);

        // Raw iron and both iron ores produce one ingot; the merge rule keeps
        // every distinct input once.
        let iron = smelt.effects_for(&[ArgValue::Item("iron_ingot".into())]);
        assert_eq!(iron.produces, vec![delta("iron_ingot", 1)]);
        let mut inputs: Vec<&str> = iron
            .consumes
            .iter()
            .map(|delta| delta.item.as_str())
            .collect();
        inputs.sort();
        assert_eq!(inputs, ["deepslate_iron_ore", "iron_ore", "raw_iron"]);
        assert!(iron.consumes.iter().all(|delta| delta.count == 1));
    }

    #[test]
    fn tag_arguments_resolve_to_their_members() {
        let p = Primitives::standard();
        let mine = p.get("mine").unwrap();
        let logs = mine.effects_for(&[ArgValue::Block("#logs".into())]);
        assert!(
            logs.produces.iter().any(|delta| delta.item == "birch_log"),
            "mine(#logs) declares birch: {:?}",
            logs.produces
        );

        let craft = p.get("craft").unwrap();
        let planks = craft.effects_for(&[ArgValue::Item("#planks".into())]);
        assert!(
            planks
                .produces
                .iter()
                .any(|delta| delta.item == "birch_planks"),
            "craft(#planks) declares birch planks: {:?}",
            planks.produces
        );
    }
}
