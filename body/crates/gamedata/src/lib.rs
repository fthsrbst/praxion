//! Game data shared by the primitive registry and the body.
//!
//! Everything here is generated at build time from the pinned 26.3 server jar
//! (`third_party/minecraft/server-26.3.jar`):
//!
//! * `RECIPES` holds every shaped and shapeless crafting recipe, with its
//!   pattern, its keys and its tag-aware ingredients;
//! * `ITEM_TAGS` and `BLOCK_TAGS` hold every item and block tag with tag
//!   references resolved to concrete names;
//! * `BLOCKS` and `ENTITIES` hold the loot tables of the blocks and mobs the
//!   core skills use.
//!
//! The generated files live in `OUT_DIR`; `build.rs` is the only reader of the
//! jar. Nothing is hand-copied, so a recipe or tag the jar does not have cannot
//! be used by a skill.

include!(concat!(env!("OUT_DIR"), "/loot.rs"));
include!(concat!(env!("OUT_DIR"), "/tags.rs"));
include!(concat!(env!("OUT_DIR"), "/recipes.rs"));

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

/// One resolved item or block tag. `name` has no namespace and no `#`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    pub name: &'static str,
    pub values: &'static [&'static str],
}

/// One recipe ingredient. `tag` is the tag it came from (`None` for a plain
/// item); `items` are the concrete items it accepts, sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ingredient {
    pub tag: Option<&'static str>,
    pub items: &'static [&'static str],
}

impl Ingredient {
    /// Whether `item` (with or without the `minecraft:` namespace) satisfies
    /// this ingredient.
    pub fn matches(&self, item: &str) -> bool {
        let item = normalize_tag(item);
        self.items.iter().any(|candidate| *candidate == item)
    }

    /// The name a craft effect should consume: `#tag` for a tag, the item for a
    /// plain ingredient.
    pub fn effect_item(&self) -> String {
        match self.tag {
            Some(tag) => format!("#{tag}"),
            None => self.items.first().copied().unwrap_or_default().to_owned(),
        }
    }
}

/// The shape of a crafting recipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// Row-major `width * height` grid; `None` is an empty cell.
    Shaped {
        width: u8,
        height: u8,
        grid: &'static [Option<Ingredient>],
    },
    Shapeless {
        ingredients: &'static [Ingredient],
    },
}

/// One crafting recipe from the 26.3 data pack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recipe {
    pub output: &'static str,
    pub output_count: u32,
    /// 3x3 recipes (or shapeless recipes with more than four ingredients) need
    /// a crafting table within reach.
    pub needs_table: bool,
    pub shape: Shape,
}

/// One ingredient count in a recipe's flat input list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngredientCount {
    /// `#tag` for a tag ingredient, the item otherwise.
    pub item: String,
    pub count: u32,
}

impl Recipe {
    /// The recipe's inputs, aggregated by effect name: a tag ingredient becomes
    /// one `#tag` entry, so three `#planks` cells consume three planks.
    pub fn inputs(&self) -> Vec<IngredientCount> {
        let mut counts: Vec<IngredientCount> = Vec::new();
        let mut add = |ingredient: &Ingredient| {
            let item = ingredient.effect_item();
            match counts.iter_mut().find(|entry| entry.item == item) {
                Some(entry) => entry.count += 1,
                None => counts.push(IngredientCount { item, count: 1 }),
            }
        };
        match &self.shape {
            Shape::Shaped { grid, .. } => {
                for cell in *grid {
                    if let Some(ingredient) = cell {
                        add(ingredient);
                    }
                }
            }
            Shape::Shapeless { ingredients } => {
                for ingredient in *ingredients {
                    add(ingredient);
                }
            }
        }
        counts
    }

    /// The grid cells of a shaped recipe, or `None` for a shapeless one.
    pub fn grid(&self) -> Option<(u8, u8, &'static [Option<Ingredient>])> {
        match &self.shape {
            Shape::Shaped {
                width,
                height,
                grid,
            } => Some((*width, *height, grid)),
            Shape::Shapeless { .. } => None,
        }
    }
}

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

/// The resolved values of an item tag, without the namespace or `#`.
pub fn item_tag(name: &str) -> Option<&'static [&'static str]> {
    let name = normalize_tag(name);
    ITEM_TAGS
        .iter()
        .find(|tag| tag.name == name)
        .map(|tag| tag.values)
}

/// The resolved values of a block tag, without the namespace or `#`.
pub fn block_tag(name: &str) -> Option<&'static [&'static str]> {
    let name = normalize_tag(name);
    BLOCK_TAGS
        .iter()
        .find(|tag| tag.name == name)
        .map(|tag| tag.values)
}

/// Whether `item` is in the item tag `name`.
pub fn item_in_tag(item: &str, name: &str) -> bool {
    let item = normalize_tag(item);
    item_tag(name).is_some_and(|values| values.iter().any(|value| *value == item))
}

/// Whether `block` is in the block tag `name`.
pub fn block_in_tag(block: &str, name: &str) -> bool {
    let block = normalize_tag(block);
    block_tag(name).is_some_and(|values| values.iter().any(|value| *value == block))
}

/// The first recipe producing `item` (normalized).
pub fn recipe(item: &str) -> Option<&'static Recipe> {
    let item = normalize_tag(item);
    RECIPES.iter().find(|recipe| recipe.output == item)
}

/// Every recipe producing `item` (normalized).
pub fn recipes_for(item: &str) -> impl Iterator<Item = &'static Recipe> {
    let item = normalize_tag(item);
    RECIPES.iter().filter(move |recipe| recipe.output == item)
}

/// The recipes whose output is in the item tag `name`.
pub fn recipes_for_tag(name: &str) -> impl Iterator<Item = &'static Recipe> {
    let values = item_tag(name).unwrap_or(&[]);
    RECIPES
        .iter()
        .filter(move |recipe| values.iter().any(|value| *value == recipe.output))
}

/// Whether mining `block` needs a pickaxe. This is block data, not a loot-table
/// condition; the tables only carry extra tool conditions where a block has
/// one.
pub fn needs_pickaxe(block: &str) -> bool {
    matches!(
        normalize_tag(block).as_str(),
        "stone" | "cobblestone" | "iron_ore" | "coal_ore" | "deepslate"
    )
}

/// Strips an optional `minecraft:` namespace and a leading `#`.
pub fn normalize_tag(name: &str) -> String {
    let name = name.strip_prefix('#').unwrap_or(name);
    name.strip_prefix("minecraft:").unwrap_or(name).to_string()
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

        let birch = block_drops("birch_log").expect("birch log table");
        assert_eq!(drop(birch, "birch_log").max, 1);

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

    #[test]
    fn tags_resolve_through_tag_references() {
        let logs = item_tag("logs").expect("logs tag");
        for log in ["oak_log", "birch_log", "spruce_log", "jungle_log", "acacia_log"] {
            assert!(logs.contains(&log), "logs is missing {log}: {logs:?}");
        }
        assert!(item_in_tag("oak_log", "#logs"));
        assert!(!item_in_tag("stone", "logs"));

        let planks = item_tag("planks").expect("planks tag");
        assert!(planks.contains(&"oak_planks"));
        assert!(planks.contains(&"birch_planks"));
    }

    #[test]
    fn every_recipe_the_core_skills_need_is_generated() {
        for item in [
            "oak_planks",
            "birch_planks",
            "stick",
            "crafting_table",
            "wooden_pickaxe",
            "stone_pickaxe",
            "stone_sword",
        ] {
            assert!(recipe(item).is_some(), "missing recipe for {item}");
        }
        assert_eq!(recipe("oak_planks").unwrap().output_count, 4);
        assert_eq!(recipe("stick").unwrap().output_count, 4);
        assert!(!recipe("oak_planks").unwrap().needs_table);
        assert!(!recipe("stick").unwrap().needs_table);
        assert!(recipe("wooden_pickaxe").unwrap().needs_table);
        assert!(recipe("stone_pickaxe").unwrap().needs_table);
    }

    #[test]
    fn recipe_inputs_come_from_the_data() {
        let planks = recipe("oak_planks").unwrap();
        let inputs = planks.inputs();
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].count, 1);
        assert!(inputs[0].item.starts_with('#'), "log recipes use a tag: {inputs:?}");

        let pickaxe = recipe("wooden_pickaxe").unwrap();
        let inputs = pickaxe.inputs();
        let sticks = inputs.iter().find(|input| input.item == "stick").unwrap();
        assert_eq!(sticks.count, 2);
        let material = inputs
            .iter()
            .find(|input| input.item.starts_with('#'))
            .expect("tool material is a tag");
        assert_eq!(material.count, 3);

        let grid = pickaxe.grid().expect("pickaxe is shaped");
        assert_eq!((grid.0, grid.1), (3, 3));
    }

    #[test]
    fn recipes_for_a_tag_cover_every_wood_type() {
        let outputs: Vec<&str> = recipes_for_tag("#planks").map(|recipe| recipe.output).collect();
        assert!(outputs.contains(&"oak_planks"));
        assert!(outputs.contains(&"birch_planks"));
        assert!(outputs.len() >= 10, "every planks recipe: {outputs:?}");
    }

    #[test]
    fn stone_needs_a_pickaxe() {
        assert!(needs_pickaxe("stone"));
        assert!(needs_pickaxe("minecraft:stone"));
        assert!(!needs_pickaxe("oak_log"));
    }
}
