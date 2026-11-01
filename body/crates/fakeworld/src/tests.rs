//! Fake-world unit tests and the 50-seed first-night runs.

use std::fs;
use std::path::PathBuf;

use praxion_interpreter::{Actuator, Decision, Interpreter, Outcome, StartAction};
use praxion_rules::{ArgValue, CORE_COMPOSITION_DEPTH, FactCall, Program, load_files_with_depth};

use crate::{FakeWorld, Perturbation};

fn skills() -> Vec<(String, String)> {
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
            (name, fs::read_to_string(path).unwrap())
        })
        .collect()
}

fn program() -> Program {
    load_files_with_depth(&skills(), CORE_COMPOSITION_DEPTH).unwrap_or_else(|errors| {
        panic!(
            "skills did not load: {:?}",
            errors.iter().map(|e| e.to_string()).collect::<Vec<_>>()
        )
    })
}

fn settle(world: &mut FakeWorld, action: StartAction) -> Outcome {
    let duration = world.start(&action, 0);
    world
        .poll(u64::from(duration))
        .expect("a discrete action settles at its duration")
}

fn action(primitive: &str, args: Vec<ArgValue>) -> StartAction {
    StartAction {
        primitive: primitive.into(),
        args,
    }
}

#[test]
fn facts_are_three_valued_and_seeded() {
    let world = FakeWorld::new(11);
    assert_eq!(world.count("oak_log"), 0);
    assert!(!world.has("oak_log"));
    world.add_item("oak_log", 3);
    assert_eq!(world.count("oak_log"), 3);
    assert!(world.has("oak_log"));
    world.set_unknown("count", true);
    assert!(world.is_unknown("count"));
    world.set_unknown("count", false);
    assert!(!world.is_unknown("count"));
    // The blocks are seeded but always present in the standard scenario.
    assert!(world.block_distance("stone").is_some());
    assert_eq!(world.seed(), 11);
}

#[test]
fn mining_stone_without_a_pickaxe_fails() {
    let mut world = FakeWorld::empty(1);
    world.add_block("stone", 3);
    let outcome = settle(&mut world, action("mine", vec![ArgValue::Block("stone".into())]));
    assert_eq!(outcome, Outcome::Failed("needs_tool".into()));
    assert_eq!(world.count("cobblestone"), 0);
}

#[test]
fn mining_with_a_pickaxe_drops_cobblestone() {
    let mut world = FakeWorld::empty(2);
    world.add_block("stone", 3);
    world.add_item("wooden_pickaxe", 1);
    let mut drops = 0;
    for _ in 0..20 {
        world.add_block("stone", 3);
        let outcome = settle(&mut world, action("mine", vec![ArgValue::Block("stone".into())]));
        assert_eq!(outcome, Outcome::Confirmed);
        drops += world.count("cobblestone");
    }
    assert!(drops > 0, "some of 20 mines must drop cobblestone");
}

#[test]
fn a_three_by_three_recipe_needs_a_placed_table() {
    let mut world = FakeWorld::empty(3);
    world.add_item("oak_planks", 3);
    world.add_item("stick", 2);
    assert_eq!(
        settle(
            &mut world,
            action("craft", vec![ArgValue::Item("wooden_pickaxe".into())])
        ),
        Outcome::Failed("needs_table".into())
    );
    world.add_block("crafting_table", 3);
    assert_eq!(
        settle(
            &mut world,
            action("craft", vec![ArgValue::Item("wooden_pickaxe".into())])
        ),
        Outcome::Confirmed
    );
    assert_eq!(world.count("wooden_pickaxe"), 1);
}

#[test]
fn duplicate_placements_are_counted() {
    let world = FakeWorld::empty(4);
    world.add_item("crafting_table", 2);
    let mut actuator = world.clone();
    settle(
        &mut actuator,
        action("place", vec![ArgValue::Block("crafting_table".into())]),
    );
    settle(
        &mut actuator,
        action("place", vec![ArgValue::Block("crafting_table".into())]),
    );
    assert_eq!(world.stats().placements, 2);
    assert_eq!(world.stats().duplicate_placements, 1);
}

#[test]
fn observe_clears_the_unknown_mode() {
    let world = FakeWorld::empty(6);
    world.add_item("oak_log", 1);
    world.set_unknown("count", true);
    let mut actuator = world.clone();
    let observe = action(
        "observe",
        vec![ArgValue::Fact(FactCall {
            name: "count".into(),
            args: vec![ArgValue::Item("oak_log".into())],
        })],
    );
    actuator.apply(&observe, 1);
    assert!(!world.is_unknown("count"));
}

#[test]
fn a_perturbation_fires_once_and_is_removed() {
    let world = FakeWorld::empty(5);
    world.add_item("oak_log", 4);
    world.add_perturbation(Perturbation::RemoveItemAfterCraft("oak_planks".into()));
    let mut actuator = world.clone();
    for _ in 0..20 {
        settle(
            &mut actuator,
            action("craft", vec![ArgValue::Item("oak_planks".into())]),
        );
    }
    assert_eq!(world.stats().perturbations_fired, 1);
}

const MAX_TICKS: u64 = 50_000;

fn run(
    program: &Program,
    skill: &str,
    args: Vec<ArgValue>,
    seed: u64,
    perturbations: &[Perturbation],
) -> (FakeWorld, Option<String>) {
    let world = FakeWorld::new(seed);
    for perturbation in perturbations {
        world.add_perturbation(perturbation.clone());
    }
    let mut facts = world.clone();
    let mut actuator = world.clone();
    let mut interpreter = Interpreter::new(program.clone());
    interpreter.start_skill(skill, args).unwrap();
    for _ in 0..MAX_TICKS {
        match interpreter.tick(&mut facts, &mut actuator) {
            Decision::Done => return (world, None),
            Decision::Failed(kind) => return (world, Some(kind)),
            _ => {}
        }
    }
    (world, Some("test_timeout".into()))
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

fn goal_reached(skill: &str, world: &FakeWorld) -> bool {
    match skill {
        "get_wood" => world.count("oak_log") >= 4,
        "get_planks" => world.count("oak_planks") >= 4,
        "get_sticks" => world.count("stick") >= 2,
        "get_crafting_table" => world.count("crafting_table") >= 1,
        "get_crafting_table_near" => world
            .block_distance("crafting_table")
            .is_some_and(|distance| distance <= 4),
        "get_wooden_pickaxe" => world.count("wooden_pickaxe") >= 1,
        "get_cobblestone" => world.count("cobblestone") >= 3,
        "get_stone_pickaxe" => world.count("stone_pickaxe") >= 1,
        "get_stone_sword" => world.count("stone_sword") >= 1,
        other => panic!("no goal for {other}"),
    }
}

fn perturbations(skill: &str) -> Vec<Perturbation> {
    match skill {
        "get_crafting_table" => vec![Perturbation::RemoveItemAfterCraft(
            "crafting_table".into(),
        )],
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

#[test]
fn every_core_skill_reaches_its_goal_on_fifty_seeds() {
    let program = program();
    for (skill, args) in core_skills() {
        for seed in 0..50 {
            let (world, failure) = run(&program, skill, args.clone(), seed, &[]);
            assert!(
                failure.is_none(),
                "{skill} failed on seed {seed}: {failure:?}, inventory {:?}",
                world.inventory()
            );
            assert!(
                goal_reached(skill, &world),
                "{skill} did not reach its goal on seed {seed}: inventory {:?}",
                world.inventory()
            );
        }
    }
}

#[test]
fn undo_perturbations_are_recovered_without_waste() {
    let program = program();
    let mut duplicate_placements = 0;
    let mut wasted_crafts = 0;
    let mut perturbations_fired = 0;

    for (skill, args) in core_skills() {
        let setbacks = perturbations(skill);
        if setbacks.is_empty() {
            continue;
        }
        for seed in 0..50 {
            let (world, failure) = run(&program, skill, args.clone(), seed, &setbacks);
            assert!(
                failure.is_none(),
                "{skill} with perturbations failed on seed {seed}: {failure:?}"
            );
            assert!(
                goal_reached(skill, &world),
                "{skill} with perturbations missed its goal on seed {seed}"
            );
            let stats = world.stats();
            duplicate_placements += stats.duplicate_placements;
            wasted_crafts += stats.wasted_crafts;
            perturbations_fired += stats.perturbations_fired;
        }
    }

    assert_eq!(duplicate_placements, 0, "duplicate placements");
    assert_eq!(wasted_crafts, 0, "wasted crafts");
    assert!(
        perturbations_fired > 0,
        "the perturbations were never injected, the test proves nothing"
    );
    println!(
        "perturbations: fired={perturbations_fired} duplicate_placements={duplicate_placements} wasted_crafts={wasted_crafts}"
    );
}

#[test]
fn a_decision_observe_can_be_executed_by_the_actuator() {
    let world = FakeWorld::empty(7);
    world.add_item("oak_log", 1);
    world.set_unknown("count", true);
    let mut facts = world.clone();
    let mut actuator = world.clone();
    let program = praxion_rules::load(
        "skill watch()\n  rules:\n    count(oak_log) >= 2 -> done\n",
    )
    .unwrap();
    let mut interpreter = Interpreter::new(program);
    interpreter.start_skill("watch", Vec::new()).unwrap();

    // `count` is unknown and no rule is true, so the interpreter asks for an
    // observation instead of failing.
    match interpreter.tick(&mut facts, &mut actuator) {
        Decision::Observe(call) => {
            assert_eq!(call.name, "count");
            actuator.apply(
                &StartAction {
                    primitive: "observe".into(),
                    args: vec![ArgValue::Fact(call)],
                },
                1,
            );
        }
        other => panic!("expected an observe, got {other:?}"),
    }
    assert!(!world.is_unknown("count"));
}
