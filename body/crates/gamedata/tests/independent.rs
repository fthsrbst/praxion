//! Independent tests for gamedata (step 8b review).

use praxion_gamedata::*;

#[test]
fn every_vanilla_wood_family_resolves_in_logs_and_planks() {
    for log in [
        "oak_log",
        "birch_log",
        "spruce_log",
        "jungle_log",
        "acacia_log",
        "dark_oak_log",
        "mangrove_log",
        "cherry_log",
        "pale_oak_log",
        "poplar_log",
        "crimson_stem",
        "warped_stem",
    ] {
        assert!(
            item_in_tag(log, "logs"),
            "missing {log} in #logs"
        );
    }
    for plank in [
        "oak_planks",
        "birch_planks",
        "spruce_planks",
        "jungle_planks",
        "acacia_planks",
        "dark_oak_planks",
        "mangrove_planks",
        "cherry_planks",
        "pale_oak_planks",
        "poplar_planks",
        "bamboo_planks",
        "crimson_planks",
        "warped_planks",
    ] {
        assert!(
            item_in_tag(plank, "planks"),
            "missing {plank} in #planks"
        );
    }
}

#[test]
fn shaped_recipes_round_trip() {
    let mut count = 0usize;
    for recipe in RECIPES {
        let Some((width, height, grid)) = recipe.grid() else {
            continue;
        };
        count += 1;
        assert!(
            width > 0 && height > 0,
            "bad dimensions for {}: {}x{}",
            recipe.output,
            width,
            height
        );
        assert_eq!(
            grid.len(),
            usize::from(width) * usize::from(height),
            "pattern size mismatch for {}",
            recipe.output
        );
        assert!(recipe.output_count > 0, "zero result count for {}", recipe.output);
        for ingredient in grid.iter().flatten() {
            assert!(
                !ingredient.items.is_empty(),
                "empty keys for {}",
                recipe.output
            );
        }
    }
    assert!(count > 0, "no shaped recipes");
}

#[test]
fn composite_tag_resolves_to_its_members() {
    let included = item_tag("logs_that_burn").expect("logs_that_burn tag");
    let composite = item_tag("logs").expect("logs tag");
    for value in included {
        assert!(
            composite.contains(value),
            "composite #logs missing member {value} of #logs_that_burn"
        );
    }
    assert!(composite.len() > included.len());
}
