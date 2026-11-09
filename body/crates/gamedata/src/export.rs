//! The game data as one JSON document for the brain.
//!
//! The brain plans goals against recipes (what a furnace costs, which items a
//! tag holds). Instead of a per-pick port query it reads this file, a pure
//! function of the pinned jar (decision 2026-11-09 "Five next areas", smarter
//! play slice 1). `jar_sha256` lets the reader refuse an export made from
//! another jar.
//!
//! The writer is hand-rolled: the document is small and flat, and `gamedata`
//! has no runtime dependencies.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;

use crate::{ITEM_TAGS, JAR_SHA256, RECIPES, SMELTING};

/// The Minecraft version the pinned jar belongs to.
const VERSION: &str = "26.3";

/// The export, compact and deterministic: keys are sorted, so two builds of
/// the same jar produce the same bytes.
///
/// * `recipes`: one entry per output item, the first recipe the jar lists for
///   it (the one `craft` uses). `inputs` are keyed by item or `#tag`, counted
///   as `craft` consumes them; `table` is whether a crafting table is needed.
/// * `smelting`: per output, the items any `minecraft:smelting` recipe cooks
///   into it and the shortest cooking time in seconds. Fuel is not modelled.
/// * `tags`: every item tag, resolved to concrete item names.
pub fn export_json() -> String {
    let mut recipes: BTreeMap<&str, String> = BTreeMap::new();
    for recipe in RECIPES {
        recipes.entry(recipe.output).or_insert_with(|| {
            let mut inputs: BTreeMap<String, u32> = BTreeMap::new();
            for entry in recipe.inputs() {
                *inputs.entry(entry.item).or_default() += entry.count;
            }
            let mut text = String::new();
            let _ = write!(
                text,
                "{{\"out\":{},\"table\":{},\"inputs\":{{",
                recipe.output_count, recipe.needs_table
            );
            let mut first = true;
            for (item, count) in &inputs {
                if !first {
                    text.push(',');
                }
                first = false;
                let _ = write!(text, "{}:{}", quote(item), count);
            }
            text.push_str("}}");
            text
        });
    }

    let mut smelting: BTreeMap<&str, (BTreeSet<String>, u32)> = BTreeMap::new();
    for recipe in SMELTING {
        let entry = smelting
            .entry(recipe.output)
            .or_insert_with(|| (BTreeSet::new(), u32::MAX));
        entry.0.insert(recipe.ingredient.effect_item());
        entry.1 = entry.1.min(recipe.cooking_time);
    }

    let mut out = String::new();
    let _ = write!(
        out,
        "{{\"version\":{},\"jar_sha256\":{},\"recipes\":{{",
        quote(VERSION),
        quote(JAR_SHA256)
    );
    let mut first = true;
    for (output, body) in &recipes {
        if !first {
            out.push(',');
        }
        first = false;
        let _ = write!(out, "{}:{}", quote(output), body);
    }
    out.push_str("},\"smelting\":{");
    first = true;
    for (output, (inputs, ticks)) in &smelting {
        if !first {
            out.push(',');
        }
        first = false;
        let list: Vec<String> = inputs.iter().map(|item| quote(item)).collect();
        let _ = write!(
            out,
            "{}:{{\"inputs\":[{}],\"seconds\":{}}}",
            quote(output),
            list.join(","),
            ticks / 20
        );
    }
    out.push_str("},\"tags\":{");
    let tags: BTreeMap<&str, &[&str]> = ITEM_TAGS.iter().map(|tag| (tag.name, tag.values)).collect();
    first = true;
    for (name, values) in &tags {
        if !first {
            out.push(',');
        }
        first = false;
        let list: Vec<String> = values.iter().map(|value| quote(value)).collect();
        let _ = write!(out, "{}:[{}]", quote(name), list.join(","));
    }
    out.push_str("}}\n");
    out
}

/// A JSON string literal. Item and tag names are ASCII identifiers, but the
/// quotes and backslash are escaped anyway so the output is valid whatever the
/// jar holds.
fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_escapes_what_json_requires() {
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
        assert_eq!(quote("x\u{1}"), "\"x\\u0001\"");
    }

    #[test]
    fn the_export_is_deterministic_and_carries_the_jar_hash() {
        let first = export_json();
        assert_eq!(first, export_json());
        assert!(first.contains(&format!("\"jar_sha256\":\"{JAR_SHA256}\"")));
        assert_eq!(JAR_SHA256.len(), 64);
        assert!(JAR_SHA256.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_furnace_costs_eight_stone_crafting_materials_at_a_table() {
        // 26.3 writes the furnace ring as the `stone_crafting_materials` tag
        // (cobblestone, blackstone, cobbled deepslate).
        let text = export_json();
        assert!(
            text.contains(
                "\"furnace\":{\"out\":1,\"table\":true,\"inputs\":{\"#stone_crafting_materials\":8}}"
            ),
            "furnace entry missing"
        );
        assert!(
            text.contains("\"stone_crafting_materials\":[\"blackstone\",\"cobbled_deepslate\",\"cobblestone\"]"),
            "the tag that the furnace names must be exported too"
        );
    }

    #[test]
    fn sticks_make_four_from_two_planks_without_a_table() {
        let text = export_json();
        assert!(
            text.contains("\"stick\":{\"out\":4,\"table\":false,\"inputs\":{\"#planks\":2}}"),
            "stick entry missing or different"
        );
    }

    #[test]
    fn smelting_and_tags_are_exported() {
        let text = export_json();
        assert!(text.contains("\"iron_ingot\":{\"inputs\":["), "iron_ingot smelting");
        assert!(text.contains("\"raw_iron\""), "raw_iron cooks into iron ingots");
        assert!(text.contains("\"logs\":["), "the logs tag");
        assert!(text.contains("\"planks\":["), "the planks tag");
    }
}
