//! Every hand-written skill file must parse, load and round-trip through the
//! pretty-printer.

use std::fs;
use std::path::PathBuf;

use praxion_rules::{CORE_COMPOSITION_DEPTH, load_files_with_depth, parse, print};

fn skill_files() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../skills");
    let mut paths: Vec<PathBuf> = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "skill"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let source = fs::read_to_string(&path).unwrap();
            (name, source)
        })
        .collect()
}

#[test]
fn every_skill_file_round_trips() {
    let files = skill_files();
    assert!(!files.is_empty(), "no skill files found");
    for (name, source) in files {
        let first = parse(&source).unwrap_or_else(|e| panic!("{name}: {e}"));
        let printed = print(&first);
        let second = parse(&printed)
            .unwrap_or_else(|e| panic!("{name}: reparse failed: {e}\n{printed}"));
        assert_eq!(first, second, "{name}: parse(print(ast)) != ast");
        assert_eq!(printed, print(&second), "{name}: printing is not stable");
    }
}

#[test]
fn every_skill_file_loads_together() {
    let files = skill_files();
    let program = load_files_with_depth(&files, CORE_COMPOSITION_DEPTH)
        .unwrap_or_else(|errors| panic!("{:?}", errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()));
    for name in [
        "get_wood",
        "get_planks",
        "get_sticks",
        "get_crafting_table",
        "get_crafting_table_near",
        "get_wooden_pickaxe",
        "get_cobblestone",
        "get_stone_pickaxe",
        "get_stone_sword",
        "hunt",
        "get_food",
        "hunt_food",
        "cook_food",
        "take_shelter",
        "night_watch",
        "get_wood",
        "cut_log",
        "get_wooden_axe",
        "get_stone_axe",
    ] {
        assert!(program.skill(name).is_some(), "missing skill {name}");
    }
}

#[test]
fn every_core_skill_has_at_most_six_rules() {
    let program = load_files_with_depth(&skill_files(), CORE_COMPOSITION_DEPTH).unwrap();
    for skill in program.skills() {
        assert!(
            skill.rules.len() <= 6,
            "skill `{}` has {} rules; the core keeps at most 6",
            skill.name,
            skill.rules.len()
        );
        assert!(
            !skill.rules.is_empty(),
            "skill `{}` has no rules",
            skill.name
        );
    }
}
