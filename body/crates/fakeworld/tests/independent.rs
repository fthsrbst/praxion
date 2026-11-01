//! Independent tests for the fake world (step 7 review).

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use praxion_interpreter::{Decision, Interpreter};
use praxion_rules::{ArgValue, CORE_COMPOSITION_DEPTH, PrimitiveRecord, Primitives, Program, load_files_with_depth};

use praxion_fakeworld::{FakeWorld, Perturbation};

fn skills() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "skill"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|p| (p.file_name().unwrap().to_string_lossy().into_owned(), fs::read_to_string(p).unwrap()))
        .collect()
}

fn program() -> Program {
    load_files_with_depth(&skills(), CORE_COMPOSITION_DEPTH).unwrap()
}

fn core_skills() -> Vec<(&'static str, Vec<ArgValue>)> {
    vec![
        ("get_wood", vec![ArgValue::Int(4)]),
        ("get_planks", vec![ArgValue::Int(4)]),
        ("get_sticks", vec![ArgValue::Int(2)]),
        ("get_crafting_table", Vec::new()),
        ("get_crafting_table_near", Vec::new()),
        ("get_wooden_pickaxe", Vec::new()),
        ("get_cobblestone", vec![ArgValue::Int(3)]),
        ("get_stone_pickaxe", Vec::new()),
        ("get_stone_sword", Vec::new()),
    ]
}

fn perturbations_for(skill: &str) -> Vec<Perturbation> {
    match skill {
        "get_crafting_table" => vec![Perturbation::RemoveItemAfterCraft("crafting_table".into())],
        "get_crafting_table_near" => vec![
            Perturbation::RemoveItemAfterCraft("crafting_table".into()),
            Perturbation::BreakBlockAfterPlace("crafting_table".into()),
        ],
        "get_planks" => vec![Perturbation::RemoveItemAfterCraft("oak_planks".into())],
        "get_sticks" => vec![Perturbation::RemoveItemAfterCraft("stick".into())],
        "get_wooden_pickaxe" | "get_cobblestone" => vec![
            Perturbation::RemoveItemAfterCraft("wooden_pickaxe".into()),
            Perturbation::BreakBlockAfterPlace("crafting_table".into()),
        ],
        "get_stone_pickaxe" => vec![
            Perturbation::RemoveItemAfterCraft("wooden_pickaxe".into()),
            Perturbation::RemoveItemAfterCraft("stone_pickaxe".into()),
            Perturbation::BreakBlockAfterPlace("crafting_table".into()),
        ],
        "get_stone_sword" => vec![
            Perturbation::RemoveItemAfterCraft("wooden_pickaxe".into()),
            Perturbation::RemoveItemAfterCraft("stone_sword".into()),
            Perturbation::BreakBlockAfterPlace("crafting_table".into()),
        ],
        _ => Vec::new(),
    }
}

fn declared_produces(prim: &PrimitiveRecord, args: &[praxion_rules::ArgValue]) -> Vec<String> {
    let mut out = Vec::new();
    for delta in &prim.effects.produces {
        let name = if delta.item.starts_with('$') {
            // $item / $block refer to arg 0 by convention in these records.
            args.first().and_then(praxion_rules::ArgValue::name).unwrap_or("").to_string()
        } else if delta.item == "$item" {
            args.first().and_then(praxion_rules::ArgValue::name).unwrap_or("").to_string()
        } else {
            delta.item.clone()
        };
        // Handle the literal "$item"/"$block" spellings used in registry.rs.
        let resolved = match delta.item.as_str() {
            "$item" | "$block" => args.first().and_then(praxion_rules::ArgValue::name).unwrap_or("").to_string(),
            other => other.to_string(),
        };
        let _ = name;
        for _ in 0..delta.count {
            out.push(resolved.clone());
        }
    }
    out
}

// FINDING 1: inventory history plausibility over 200 seeds with perturbations.
// The fake world grants items (cobblestone/oak_log from mine, beef/leather/etc.
// from attack) that the primitive records do not declare as `produces`, so
// those gains are "from nowhere" relative to the declared effects.
#[test]
fn fake_world_inventory_history_plausible() {
    let program = program();
    let primitives = Primitives::standard();
    let mut violations: Vec<String> = Vec::new();
    let mut seeds_run = 0u32;

    for (skill, args) in core_skills() {
        let setbacks = perturbations_for(skill);
        if setbacks.is_empty() {
            continue;
        }
        for seed in 0..200u64 {
            seeds_run += 1;
            let world = FakeWorld::new(seed);
            for p in &setbacks {
                world.add_perturbation(p.clone());
            }
            let mut facts = world.clone();
            let mut actuator = world.clone();
            let mut interpreter = Interpreter::new(program.clone());
            interpreter.start_skill(skill, args.clone()).unwrap();

            let mut prev_inventory: BTreeMap<String, u32> = world.inventory();
            let mut tick = 0u64;
            while tick < 50_000 {
                tick += 1;
                let events_before = world.events().len();
                match interpreter.tick(&mut facts, &mut actuator) {
                    Decision::Done => break,
                    Decision::Failed(k) => {
                        violations.push(format!("{skill} seed {seed}: failed {k}"));
                        break;
                    }
                    _ => {}
                }
                let now_inventory = world.inventory();
                // Detect item gains.
                for (item, &now) in &now_inventory {
                    let before = prev_inventory.get(item).copied().unwrap_or(0);
                    if now > before {
                        let gain = now - before;
                        // Find a primitive that settled this tick.
                        let events = world.events();
                        let settled = if events.len() > events_before {
                            events[events_before..]
                                .iter()
                                .rev()
                                .find(|e| e.outcome.is_some())
                        } else {
                            None
                        };
                        let mut explained = false;
                        if let Some(ev) = settled {
                            if let Some(outcome) = &ev.outcome {
                                if *outcome == praxion_interpreter::Outcome::Confirmed {
                                    if let Some(prim) = primitives.get(&ev.action.primitive) {
                                        let produces = declared_produces(prim, &ev.action.args);
                                        // Count how many times `item` appears in produces.
                                        let declared_count = produces.iter().filter(|x| *x == item).count();
                                        if declared_count > 0 {
                                            explained = true;
                                        }
                                    }
                                }
                            }
                        }
                        if !explained {
                            violations.push(format!(
                                "{skill} seed {seed} tick {tick}: gained {gain} {item} not explained by declared produces (last settled: {:?})",
                                settled.map(|e| e.action.primitive.clone())
                            ));
                            if violations.len() > 20 {
                                break;
                            }
                        }
                    }
                }
                prev_inventory = now_inventory;
                if violations.len() > 20 {
                    break;
                }
            }
            if violations.len() > 20 {
                break;
            }
        }
        if violations.len() > 20 {
            break;
        }
    }

    assert!(
        seeds_run > 0,
        "no seeds were run"
    );
    // This assertion is expected to fail: see FINDING 1.
    assert!(
        violations.is_empty(),
        "inventory-history violations ({} total, showing first 5): {:?}",
        violations.len(),
        &violations[..violations.len().min(5)]
    );
}
