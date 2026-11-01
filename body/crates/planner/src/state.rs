//! The planning state (contract §5.5): the hotbar, the best tool, protected
//! blocks, and the blacklisted cells the stuck handler feeds back.
//!
//! Nothing here reads the world. The world is the [`KnownBlocks`] trait; this
//! module only describes what the player brings to it.

use std::collections::HashSet;

use azalea_registry::builtin::BlockKind;
use praxion_world::BlockPos;

/// Placements one plan may use, whatever the stack count. A path never needs
/// more than this many blocks, and keeping it small bounds the A* node key.
pub const MAX_PLACEMENTS: u16 = 64;

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

    /// Blocks the plan may place: the count of the item [`Self::place_item`]
    /// returns, capped at [`MAX_PLACEMENTS`]. The plan uses one item kind, so a
    /// second placeable stack does not raise the count.
    pub fn placements(&self) -> u16 {
        self.place_item()
            .map(|item| self.count(item).min(u32::from(MAX_PLACEMENTS)) as u16)
            .unwrap_or(0)
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
}

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
        assert_eq!(inventory.placements(), 12);
        assert_eq!(Inventory::empty().place_item(), None);
        assert_eq!(Inventory::empty().placements(), 0);
        assert_eq!(Inventory::with(&[("diamond", 1)]).place_item(), None);
        assert_eq!(Inventory::with(&[("cobblestone", 100)]).placements(), 64);
    }
}
