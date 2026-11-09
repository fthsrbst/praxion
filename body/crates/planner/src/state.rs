//! The planning state (contract §5.5): the hotbar, the best tool, protected
//! blocks, and the blacklisted cells the stuck handler feeds back.
//!
//! Nothing here reads the world. The world is the [`KnownBlocks`] trait; this
//! module only describes what the player brings to it.

use std::collections::HashSet;

use azalea_registry::builtin::BlockKind;
use praxion_world::BlockPos;

/// Placements one plan may use in total, whatever the stacks hold. A path
/// never needs more than this many blocks on Demo-0's routes, and keeping it
/// small bounds the A* node key. The plan draws this budget from the first
/// placeable stack that still has blocks, so a second stack of another item is
/// used once the first runs out.
pub const MAX_PLACEMENTS: u16 = 64;

/// The hotbar has nine slots, so a node never names more placeable stacks.
pub const MAX_PLACE_KINDS: usize = 9;

/// The placement budget of one A* node: how many blocks each hotbar slot may
/// still contribute. Entry `i` is hotbar slot `i`; a slot that is not
/// placeable has 0. It is part of the node key, so a route that used more
/// blocks is a different state and can never place more than the hotbar holds.
/// The table is a fixed array of [`MAX_PLACE_KINDS`] counts and the total never
/// exceeds [`MAX_PLACEMENTS`], so a node stays a small `Copy` value and the key
/// stays bounded. Empty slots are always zero, so equal budgets are equal keys.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Placements {
    left: [u16; MAX_PLACE_KINDS],
}

impl Placements {
    /// Nothing can be placed.
    pub const NONE: Self = Self {
        left: [0; MAX_PLACE_KINDS],
    };

    /// Build a budget from `(hotbar slot, blocks)` pairs, in hotbar order.
    /// Entries past [`MAX_PLACE_KINDS`] are dropped and the total is capped.
    fn from_counts(counts: impl IntoIterator<Item = (usize, u16)>) -> Self {
        let mut placements = Self::NONE;
        let mut total = 0;
        for (slot, count) in counts {
            if slot >= MAX_PLACE_KINDS || total >= MAX_PLACEMENTS {
                continue;
            }
            let left = count.min(MAX_PLACEMENTS - total);
            placements.left[slot] = left;
            total += left;
        }
        placements
    }

    /// Blocks the plan may still place, all slots together.
    pub fn total(&self) -> u16 {
        self.left.iter().sum()
    }

    /// The slot to place from next: the first one with blocks left, in hotbar
    /// order. A plan therefore switches to the next stack when one runs out.
    pub fn next_slot(&self) -> Option<u8> {
        self.left
            .iter()
            .position(|left| *left > 0)
            .map(|slot| slot as u8)
    }

    /// The budget after taking one block from `slot`, or `None` when that slot
    /// has nothing left.
    pub fn take(&self, slot: u8) -> Option<Self> {
        let slot = usize::from(slot);
        if *self.left.get(slot)? == 0 {
            return None;
        }
        let mut next = *self;
        next.left[slot] -= 1;
        Some(next)
    }
}

/// One hotbar slot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HotbarItem {
    pub item: String,
    pub count: u32,
}

/// The hotbar the planner may use. A tool makes mining faster, a block item
/// makes bridge and pillar possible.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Inventory {
    pub hotbar: Vec<HotbarItem>,
}

impl Inventory {
    pub fn empty() -> Self {
        Self::default()
    }

    /// `&[("cobblestone", 64), ("stone_pickaxe", 1)]`.
    pub fn with(items: &[(&str, u32)]) -> Self {
        Self {
            hotbar: items
                .iter()
                .map(|(item, count)| HotbarItem {
                    item: (*item).to_owned(),
                    count: *count,
                })
                .collect(),
        }
    }

    pub fn count(&self, item: &str) -> u32 {
        self.hotbar
            .iter()
            .filter(|slot| slot.item == item)
            .map(|slot| slot.count)
            .sum()
    }

    /// The first hotbar block item that can be placed. `None` means the
    /// planner must not plan a bridge or a pillar.
    pub fn place_item(&self) -> Option<&str> {
        self.hotbar
            .iter()
            .find(|slot| slot.count > 0 && place_block_kind(&slot.item).is_some())
            .map(|slot| slot.item.as_str())
    }

    /// The item in hotbar slot `slot`, for resolving a [`PlaceableStack`] back
    /// to the block a `place` primitive names.
    pub fn slot_item(&self, slot: u8) -> Option<&str> {
        self.hotbar
            .get(usize::from(slot))
            .map(|slot| slot.item.as_str())
    }

    /// The placeable slots as an A* placement budget, in hotbar order, capped
    /// so the total never exceeds [`MAX_PLACEMENTS`]. A node starts from this
    /// and draws one block at a time from the first stack that still has any,
    /// so a bridge or a pillar switches stacks when one runs out.
    pub fn place_stacks(&self) -> Placements {
        Placements::from_counts(
            self.hotbar
                .iter()
                .enumerate()
                .filter(|(_, item)| item.count > 0 && place_block_kind(&item.item).is_some())
                .map(|(slot, item)| (slot, item.count.min(u32::from(MAX_PLACEMENTS)) as u16)),
        )
    }

    /// Blocks the plan may place, all placeable slots together, capped at
    /// [`MAX_PLACEMENTS`]. This is the total of [`Self::place_stacks`].
    pub fn placements(&self) -> u16 {
        self.place_stacks().total()
    }
}

/// The block a placeable item puts down. TODO(game data): the full
/// item-to-block table. The list here covers the blocks Demo-0's core skills
/// place; every other item is treated as not placeable, which only loses
/// options, never invents one. Falling blocks (sand, gravel) are deliberately
/// absent: a bridge places its block with side support only, and a falling
/// block would drop through it.
pub fn place_block_kind(item: &str) -> Option<BlockKind> {
    Some(match item {
        "cobblestone" => BlockKind::Cobblestone,
        "stone" => BlockKind::Stone,
        "dirt" => BlockKind::Dirt,
        "grass_block" => BlockKind::GrassBlock,
        "oak_planks" => BlockKind::OakPlanks,
        "oak_log" => BlockKind::OakLog,
        "birch_log" => BlockKind::BirchLog,
        "spruce_log" => BlockKind::SpruceLog,
        "deepslate" => BlockKind::Deepslate,
        _ => return None,
    })
}

/// Everything the planner needs to know about the player, apart from the world.
#[derive(Clone, Debug, Default)]
pub struct PlanningState {
    pub inventory: Inventory,
    /// Positions the ruleset protects: never broken, never placed on, never
    /// occupied. Standing on a protected floor block is allowed.
    pub protected: HashSet<BlockPos>,
    /// Cells the stuck handler blacklisted. Treated like protected cells.
    pub blacklist: HashSet<BlockPos>,
    /// The food level (0 to 20) when the route was planned, or `None` when the
    /// server has not said. The follower refuses to start a sprint at or below
    /// [`SPRINT_FOOD_MIN`]; vanilla sprints only above it.
    pub food: Option<u32>,
}

/// The food level at or below which a sprint cannot start (vanilla: sprinting
/// needs food above 6).
pub const SPRINT_FOOD_MIN: u32 = 6;

impl PlanningState {
    pub fn new(inventory: Inventory) -> Self {
        Self {
            inventory,
            ..Self::default()
        }
    }

    pub fn protect(&mut self, pos: BlockPos) {
        self.protected.insert(pos);
    }

    pub fn is_protected(&self, pos: BlockPos) -> bool {
        self.protected.contains(&pos)
    }

    pub fn is_blacklisted(&self, pos: BlockPos) -> bool {
        self.blacklist.contains(&pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_placeable_item_wins() {
        let inventory = Inventory::with(&[("stone_pickaxe", 1), ("cobblestone", 12), ("dirt", 3)]);
        assert_eq!(inventory.place_item(), Some("cobblestone"));
        assert_eq!(inventory.count("dirt"), 3);
        assert_eq!(inventory.placements(), 15, "both placeable stacks count");
        assert_eq!(inventory.slot_item(1), Some("cobblestone"));
        assert_eq!(Inventory::empty().place_item(), None);
        assert_eq!(Inventory::empty().placements(), 0);
        assert_eq!(Inventory::with(&[("diamond", 1)]).place_item(), None);
        assert_eq!(Inventory::with(&[("cobblestone", 100)]).placements(), 64);
    }

    #[test]
    fn a_node_budget_switches_stacks_and_stays_bounded() {
        let inventory = Inventory::with(&[("cobblestone", 2), ("dirt", 8)]);
        let mut budget = inventory.place_stacks();
        assert_eq!(budget.total(), 10);
        assert_eq!(budget.next_slot(), Some(0));
        budget = budget.take(0).expect("the first stack");
        budget = budget.take(0).expect("the first stack");
        assert_eq!(budget.take(0), None, "an empty stack cannot be drawn on");
        assert_eq!(budget.total(), 8);
        assert_eq!(budget.next_slot(), Some(1), "the plan switches stacks");
        assert_eq!(budget.take(1).map(|budget| budget.total()), Some(7));

        // The total is capped, and the cap is what the five-gap fixture needs.
        let huge = Inventory::with(&[("cobblestone", 64), ("dirt", 64)]);
        assert_eq!(huge.place_stacks().total(), MAX_PLACEMENTS);
        assert_eq!(huge.place_stacks().next_slot(), Some(0));
    }
}
