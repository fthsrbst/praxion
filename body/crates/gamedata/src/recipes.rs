//! Crafting recipes and block tool requirements for the first-night items.
//!
//! Transcribed from the 26.3 data pack, which ships as JSON in the server jar
//! (`data/minecraft/recipe/*.json`). The counts below are exactly those files:
//! oak planks 1 log -> 4, sticks 2 planks -> 4, a crafting table 4 planks -> 1,
//! a pickaxe 3 material + 2 sticks -> 1, a sword 2 material + 1 stick -> 1. The
//! real files use tags (`#minecraft:planks`, `#minecraft:stone_tool_materials`)
//! and every log type; the fake world has only oak and cobblestone.
//!
//! This is the single recipe source: the primitive registry derives its craft
//! effects from it and the fake world executes it.

/// One ingredient of a fake-world recipe.
pub struct Ingredient {
    pub item: &'static str,
    pub count: u32,
}

/// One fake-world crafting recipe.
pub struct Recipe {
    pub output: &'static str,
    pub output_count: u32,
    pub inputs: &'static [Ingredient],
    /// 3x3 recipes need a crafting table within reach.
    pub needs_table: bool,
}

/// The recipes the first-night skills use.
pub const RECIPES: &[Recipe] = &[
    Recipe {
        output: "oak_planks",
        output_count: 4,
        inputs: &[Ingredient {
            item: "oak_log",
            count: 1,
        }],
        needs_table: false,
    },
    Recipe {
        output: "stick",
        output_count: 4,
        inputs: &[Ingredient {
            item: "oak_planks",
            count: 2,
        }],
        needs_table: false,
    },
    Recipe {
        output: "crafting_table",
        output_count: 1,
        inputs: &[Ingredient {
            item: "oak_planks",
            count: 4,
        }],
        needs_table: false,
    },
    Recipe {
        output: "wooden_pickaxe",
        output_count: 1,
        inputs: &[
            Ingredient {
                item: "oak_planks",
                count: 3,
            },
            Ingredient {
                item: "stick",
                count: 2,
            },
        ],
        needs_table: true,
    },
    Recipe {
        output: "stone_pickaxe",
        output_count: 1,
        inputs: &[
            Ingredient {
                item: "cobblestone",
                count: 3,
            },
            Ingredient {
                item: "stick",
                count: 2,
            },
        ],
        needs_table: true,
    },
    Recipe {
        output: "stone_sword",
        output_count: 1,
        inputs: &[
            Ingredient {
                item: "cobblestone",
                count: 2,
            },
            Ingredient {
                item: "stick",
                count: 1,
            },
        ],
        needs_table: true,
    },
];

/// The recipe producing `item`, if the fake world knows one.
pub fn recipe(item: &str) -> Option<&'static Recipe> {
    RECIPES.iter().find(|recipe| recipe.output == item)
}

/// Whether mining `block` needs a pickaxe. This is block data, not a loot-table
/// condition; the tables only carry extra tool conditions where a block has
/// one.
pub fn needs_pickaxe(block: &str) -> bool {
    matches!(block, "stone" | "cobblestone" | "iron_ore" | "coal_ore")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_night_recipes_are_complete() {
        for item in [
            "oak_planks",
            "stick",
            "crafting_table",
            "wooden_pickaxe",
            "stone_pickaxe",
            "stone_sword",
        ] {
            assert!(recipe(item).is_some(), "missing recipe for {item}");
        }
        assert_eq!(recipe("oak_planks").unwrap().output_count, 4);
        assert!(recipe("wooden_pickaxe").unwrap().needs_table);
    }

    #[test]
    fn stone_needs_a_pickaxe() {
        assert!(needs_pickaxe("stone"));
        assert!(!needs_pickaxe("oak_log"));
    }
}
