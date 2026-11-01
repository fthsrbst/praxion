//! Game data shared by the primitive registry and the fake world.
//!
//! * `recipes` is the hand-transcribed 26.3 recipe table (the single source for
//!   `craft` effects and fake-world crafting).
//! * `BLOCKS` and `ENTITIES` are generated at build time from the loot tables
//!   in the pinned server jar (`third_party/minecraft/server-26.3.jar`). Each
//!   entry is one possible drop with its count range and tool requirement.

mod recipes;
pub use recipes::{Ingredient, RECIPES, Recipe, needs_pickaxe, recipe};

/// One possible item drop from a loot table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Drop {
    pub item: &'static str,
    pub min: u32,
    pub max: u32,
    /// The tool the table requires for this drop, if it names one.
    pub tool: Option<&'static str>,
}

/// The loot table of one block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockDrops {
    pub block: &'static str,
    pub drops: &'static [Drop],
}

/// The loot table of one entity kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntityDrops {
    pub entity: &'static str,
    pub drops: &'static [Drop],
}

include!(concat!(env!("OUT_DIR"), "/loot.rs"));

/// The possible drops of a block, or `None` if its table was not loaded.
pub fn block_drops(block: &str) -> Option<&'static [Drop]> {
    BLOCKS
        .iter()
        .find(|table| table.block == block)
        .map(|table| table.drops)
}

/// The possible drops of an entity kind, or `None` if its table was not loaded.
pub fn entity_drops(entity: &str) -> Option<&'static [Drop]> {
    ENTITIES
        .iter()
        .find(|table| table.entity == entity)
        .map(|table| table.drops)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drop<'a>(drops: &'a [Drop], item: &str) -> &'a Drop {
        drops
            .iter()
            .find(|drop| drop.item == item)
            .unwrap_or_else(|| panic!("no drop `{item}` in {drops:?}"))
    }

    #[test]
    fn block_tables_come_from_the_26_3_loot_tables() {
        let stone = block_drops("stone").expect("stone table");
        assert_eq!(stone.len(), 1);
        assert_eq!(stone[0].item, "cobblestone");
        assert_eq!((stone[0].min, stone[0].max), (1, 1));

        let log = block_drops("oak_log").expect("oak log table");
        assert_eq!(drop(log, "oak_log").max, 1);

        let grass = block_drops("grass_block").expect("grass table");
        assert_eq!(drop(grass, "dirt").max, 1);
    }

    #[test]
    fn entity_tables_carry_count_ranges() {
        let cow = entity_drops("cow").expect("cow table");
        assert_eq!((drop(cow, "leather").min, drop(cow, "leather").max), (0, 2));
        assert_eq!((drop(cow, "beef").min, drop(cow, "beef").max), (1, 3));

        let chicken = entity_drops("chicken").expect("chicken table");
        assert_eq!((drop(chicken, "feather").min, drop(chicken, "feather").max), (0, 2));
        assert_eq!((drop(chicken, "chicken").min, drop(chicken, "chicken").max), (1, 1));

        let sheep = entity_drops("sheep").expect("sheep table");
        assert_eq!(drop(sheep, "mutton").min, 1);
        assert_eq!(drop(sheep, "white_wool").max, 1);
    }

    #[test]
    fn unknown_tables_are_none() {
        assert!(block_drops("bedrock").is_none());
        assert!(entity_drops("ender_dragon").is_none());
    }
}
