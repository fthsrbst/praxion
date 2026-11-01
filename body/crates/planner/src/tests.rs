//! Hand-built worlds for the planner contract.
//!
//! Each test builds the smallest world that proves one rule: a corridor, a
//! wall to detour, a step, a drop, a gap, a pit, a protected block, lava, an
//! unknown frontier, and a budget stop. The real-terrain and follower tests
//! live in `tests/terrain.rs`.

use std::time::Duration;

use azalea_block::BlockState;
use azalea_registry::builtin::BlockKind;
use praxion_rules::{EvidenceKind, Primitives};
use praxion_world::BlockPos;

use crate::*;

fn state(kind: BlockKind) -> BlockState {
    BlockState::from(kind)
}

fn stone() -> BlockState {
    state(BlockKind::Stone)
}

fn bedrock() -> BlockState {
    state(BlockKind::Bedrock)
}

fn lava() -> BlockState {
    state(BlockKind::Lava)
}

fn dirt() -> BlockState {
    state(BlockKind::Dirt)
}

fn pos(x: i32, y: i32, z: i32) -> BlockPos {
    BlockPos::new(x, y, z)
}

fn planner() -> Planner {
    let mut planner = Planner::new();
    planner.set_max_drop(3);
    planner
}

fn state_with(items: &[(&str, u32)]) -> PlanningState {
    PlanningState::new(Inventory::with(items))
}

fn plan_once(world: &mut TestWorld, state: &PlanningState, start: BlockPos, goal: Goal) -> Plan {
    planner().plan(world, state, start, goal)
}

fn kinds(plan: &Plan) -> Vec<&'static str> {
    plan.steps.iter().map(|step| step.kind.name()).collect()
}

fn assert_valid_steps(plan: &Plan) {
    for step in &plan.steps {
        let dx = (step.to.x - step.from.x).abs();
        let dz = (step.to.z - step.from.z).abs();
        let dy = step.to.y - step.from.y;
        let horizontal = dx.max(dz);
        match step.kind {
            MoveKind::Bridge { .. } => {
                assert_eq!(horizontal, 2, "a bridge crosses one gap cell: {step:?}");
                assert_eq!(dy, 0, "{step:?}");
            }
            MoveKind::Pillar { .. } => {
                assert_eq!(horizontal, 0, "{step:?}");
                assert_eq!(dy, 1, "{step:?}");
            }
            MoveKind::EnterWater | MoveKind::Swim => {
                assert_eq!(horizontal, 1, "{step:?}");
                assert_eq!(dy, 0, "{step:?}");
            }
            MoveKind::SwimUp => {
                assert_eq!(horizontal, 0, "{step:?}");
                assert_eq!(dy, 1, "{step:?}");
            }
            MoveKind::ExitWater { rise } => {
                assert_eq!(horizontal, 1, "{step:?}");
                assert_eq!(dy, i32::from(rise), "{step:?}");
            }
            _ => assert!(
                horizontal <= 1 && (-3..=1).contains(&dy),
                "a move must go to a neighbouring cell: {step:?}"
            ),
        }
    }
}

#[test]
fn a_straight_corridor_is_five_walks() {
    let mut world = TestWorld::flat(63, 8);
    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 64, 5)));

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    assert_eq!(plan.steps.len(), 5);
    assert!(plan.steps.iter().all(|step| step.kind == MoveKind::Walk));
    assert!(plan.steps.iter().all(|step| step.primitives.is_empty()));
    assert!(plan.path_cells().contains(&pos(0, 64, 5)));
    assert_valid_steps(&plan);
}

#[test]
fn a_bedrock_wall_forces_a_detour() {
    let mut world = TestWorld::flat(63, 10);
    for x in -2..=2 {
        for y in [64, 65] {
            world.set(x, y, 2, bedrock());
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 64, 5)));

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    assert!(
        plan.steps.iter().any(|step| step.to.x.abs() >= 3),
        "the path must go around the wall: {:?}",
        kinds(&plan)
    );
    assert!(plan.steps.iter().all(|step| !step.kind.changes_world()));
    assert_valid_steps(&plan);
}

#[test]
fn a_one_block_step_uses_a_jump() {
    let mut world = TestWorld::flat(63, 6);
    for x in 2..=6 {
        for z in -6..=6 {
            world.set(x, 64, z, stone());
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(4, 65, 0)));

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    assert!(
        plan.steps.iter().any(|step| step.kind == MoveKind::StepUp),
        "{:?}",
        kinds(&plan)
    );
    assert!(plan.steps.iter().all(|step| !step.kind.changes_world()));
    assert_valid_steps(&plan);
}

#[test]
fn a_three_block_drop_is_allowed() {
    let mut world = TestWorld::flat(63, 6);
    for x in 1..=6 {
        for z in -6..=6 {
            world.remove(x, 63, z);
            world.set(x, 60, z, stone());
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(2, 61, 0)));

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    assert!(
        plan.steps
            .iter()
            .any(|step| step.kind == MoveKind::Descend { drop: 3 }),
        "{:?}",
        kinds(&plan)
    );
    assert_valid_steps(&plan);
}

#[test]
fn a_four_block_drop_is_refused() {
    let mut world = TestWorld::flat(63, 6);
    for x in 1..=6 {
        for z in -6..=6 {
            world.remove(x, 63, z);
            world.set(x, 59, z, stone());
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(2, 60, 0)));

    assert!(
        matches!(plan.status, PlanStatus::NoPath { .. }),
        "a 4-block drop must be refused: {:?}",
        plan.status
    );
    assert!(plan.steps.is_empty());
}

#[test]
fn a_one_wide_gap_needs_a_bridge_and_a_block() {
    // The floor is removed in a full-width line, so no detour exists.
    let mut world = TestWorld::flat(63, 6);
    for z in -6..=6 {
        world.remove(2, 63, z);
    }

    let with_blocks = state_with(&[("cobblestone", 16)]);
    let plan = plan_once(&mut world, &with_blocks, pos(0, 64, 0), Goal::Block(pos(4, 64, 0)));
    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    let bridge = plan
        .steps
        .iter()
        .find(|step| matches!(step.kind, MoveKind::Bridge { .. }))
        .expect("the path must bridge the gap");
    assert_eq!(bridge.primitives.len(), 1);
    assert_eq!(bridge.primitives[0].name, "place");
    assert_eq!(bridge.primitives[0].target, pos(2, 63, bridge.to.z));
    assert_valid_steps(&plan);

    let without_blocks = state_with(&[]);
    let no_path = plan_once(
        &mut world,
        &without_blocks,
        pos(0, 64, 0),
        Goal::Block(pos(4, 64, 0)),
    );
    assert!(
        matches!(no_path.status, PlanStatus::NoPath { .. }),
        "{:?}",
        no_path.status
    );
}

#[test]
fn a_path_needing_more_blocks_than_the_stack_is_refused() {
    // Five one-wide gaps, each crossed by one bridge: 2, 5, 8, 11 and 14.
    let mut world = TestWorld::flat(63, 20);
    for z in -20..=20 {
        for x in [2, 5, 8, 11, 14] {
            world.remove(x, 63, z);
        }
    }

    let mut planner = Planner::with_budget(Budget::new(Duration::from_secs(5), 200_000));
    planner.set_max_drop(3);

    let three = state_with(&[("cobblestone", 3)]);
    let short = planner.plan(
        &mut world,
        &three,
        pos(0, 64, 0),
        Goal::Block(pos(16, 64, 0)),
    );
    assert_ne!(
        short.status,
        PlanStatus::Success,
        "3 blocks cannot cross 5 gaps: {:?}",
        short.status
    );
    let short_bridges = short
        .steps
        .iter()
        .filter(|step| matches!(step.kind, MoveKind::Bridge { .. }))
        .count();
    assert!(
        short_bridges <= 3,
        "a plan may never need more placements than the stack: {short_bridges}"
    );

    let five = state_with(&[("cobblestone", 5)]);
    let plan = planner.plan(
        &mut world,
        &five,
        pos(0, 64, 0),
        Goal::Block(pos(16, 64, 0)),
    );
    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    let bridges = plan
        .steps
        .iter()
        .filter(|step| matches!(step.kind, MoveKind::Bridge { .. }))
        .count();
    assert_eq!(bridges, 5, "{:?}", kinds(&plan));
    assert_valid_steps(&plan);
}

#[test]
fn a_pond_is_crossed_by_swimming() {
    let mut world = TestWorld::flat(62, 12);
    for x in 2..=6 {
        for z in -12..=12 {
            world.set(x, 63, z, state(BlockKind::Water));
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(
        &mut world,
        &state,
        pos(0, 63, 0),
        Goal::Block(pos(8, 63, 0)),
    );

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    let names = kinds(&plan);
    assert!(names.contains(&"enter_water"), "{names:?}");
    assert!(names.contains(&"swim"), "{names:?}");
    assert!(names.contains(&"exit_water"), "{names:?}");
    assert!(plan.steps.iter().all(|step| !step.kind.changes_world()));
    assert_valid_steps(&plan);
}

#[test]
fn a_water_column_is_climbed_with_swim_up() {
    let mut world = TestWorld::flat(62, 12);
    for x in 2..=4 {
        for y in 63..=65 {
            for z in -12..=12 {
                world.set(x, y, z, state(BlockKind::Water));
            }
        }
    }
    // A ledge one block above the start; the only way up is the water column.
    for x in 5..=12 {
        for z in -12..=12 {
            world.set(x, 64, z, stone());
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(
        &mut world,
        &state,
        pos(0, 63, 0),
        Goal::Block(pos(8, 65, 0)),
    );

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    let names = kinds(&plan);
    assert!(names.contains(&"swim_up"), "{names:?}");
    assert!(names.contains(&"exit_water"), "{names:?}");
    assert_valid_steps(&plan);
}

#[test]
fn lava_is_still_impassable() {
    let mut world = TestWorld::flat(62, 12);
    for x in 2..=6 {
        for z in -12..=12 {
            world.set(x, 63, z, lava());
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(
        &mut world,
        &state,
        pos(0, 63, 0),
        Goal::Block(pos(8, 63, 0)),
    );
    assert!(
        matches!(plan.status, PlanStatus::NoPath { .. }),
        "lava must never be a path: {:?}",
        plan.status
    );
}

#[test]
fn a_pit_needs_a_pillar() {
    let mut world = TestWorld::flat(63, 3);
    // A 1x1 shaft with two-block walls; the only way out is up.
    for (x, z) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
        for y in [64, 65] {
            world.set(x, y, z, stone());
        }
    }
    let state = state_with(&[("cobblestone", 16)]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 66, 0)));

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    let pillars: Vec<_> = plan
        .steps
        .iter()
        .filter(|step| matches!(step.kind, MoveKind::Pillar { .. }))
        .collect();
    assert_eq!(pillars.len(), 2, "{:?}", kinds(&plan));
    for pillar in pillars {
        assert_eq!(pillar.primitives.len(), 1);
        assert_eq!(pillar.primitives[0].name, "place");
        assert_eq!(pillar.primitives[0].target, pillar.from);
    }
    assert_valid_steps(&plan);
}

#[test]
fn a_protected_block_on_the_only_path_is_no_path() {
    let mut world = TestWorld::flat(63, 8);
    for x in -8..=8 {
        for y in [64, 65] {
            world.set(x, y, 2, bedrock());
        }
    }
    // The one breakable cell, protected: no route may break or enter it.
    for y in [64, 65] {
        world.set(0, y, 2, stone());
    }
    let mut state = state_with(&[]);
    state.protect(pos(0, 64, 2));
    state.protect(pos(0, 65, 2));

    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 64, 4)));
    assert!(
        matches!(plan.status, PlanStatus::NoPath { .. }),
        "{:?}",
        plan.status
    );
    assert!(plan.steps.iter().all(|step| step
        .primitives
        .iter()
        .all(|call| !state.is_protected(call.target))));
}

#[test]
fn a_protected_block_with_a_detour_is_walked_around() {
    let mut world = TestWorld::flat(63, 8);
    for x in -1..=1 {
        for y in [64, 65] {
            world.set(x, y, 2, stone());
        }
    }
    let mut state = state_with(&[]);
    state.protect(pos(0, 64, 2));
    state.protect(pos(0, 65, 2));

    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 64, 4)));
    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    assert!(plan.steps.iter().any(|step| step.to.x.abs() >= 2));
    assert!(plan.steps.iter().all(|step| step
        .primitives
        .iter()
        .all(|call| !state.is_protected(call.target))));
    assert_valid_steps(&plan);
}

#[test]
fn a_protected_air_cell_cannot_be_digged_into() {
    let mut world = TestWorld::flat(63, 6);
    for x in -6..=6 {
        for y in [64, 65] {
            world.set(x, y, 1, bedrock());
        }
    }
    // Air at feet height, stone at head height: a dig would break the head and
    // step into the protected feet cell. It must be refused as a whole.
    world.remove(0, 64, 1);
    world.set(0, 65, 1, stone());
    let mut state = state_with(&[]);
    state.protect(pos(0, 64, 1));

    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 64, 2)));
    assert!(
        matches!(plan.status, PlanStatus::NoPath { .. }),
        "{:?}",
        plan.status
    );
    assert!(
        plan.steps
            .iter()
            .all(|step| step.primitives.iter().all(|call| call.target != pos(0, 65, 1)))
    );
}

#[test]
fn lava_next_to_a_wall_is_never_dug_into() {
    let mut world = TestWorld::flat(63, 8);
    for x in -8..=8 {
        for y in [64, 65] {
            world.set(x, y, 2, bedrock());
        }
    }
    for y in [64, 65] {
        world.set(0, y, 2, stone());
    }
    // The stone is the only way through, and removing it would let the lava
    // at its east face flow into the corridor.
    world.set(1, 64, 2, lava());
    world.set(1, 65, 2, lava());

    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 64, 4)));
    assert!(
        matches!(plan.status, PlanStatus::NoPath { .. }),
        "{:?}",
        plan.status
    );
    assert!(
        plan.steps
            .iter()
            .all(|step| step.primitives.iter().all(|call| call.name != "mine")),
        "no block next to lava may be mined"
    );
}

#[test]
fn an_unknown_corridor_ends_at_the_frontier() {
    let mut world = TestWorld::flat(63, 10);
    for x in 6..=10 {
        for y in 63..=65 {
            for z in -2..=2 {
                world.set_unknown(x, y, z);
            }
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(10, 64, 0)));

    match plan.status {
        PlanStatus::Partial { cell } => {
            assert_eq!(cell, pos(6, 64, 0), "the frontier cell that ended it");
        }
        other => panic!("expected partial, got {other:?}"),
    }
    assert_eq!(plan.steps.last().map(|step| step.to), Some(pos(5, 64, 0)));
    assert!(plan.steps.iter().all(|step| !step.kind.changes_world()));
    assert_valid_steps(&plan);
}

#[test]
fn an_unknown_cell_next_to_a_dig_is_a_frontier_not_a_dig() {
    let mut world = TestWorld::flat(63, 4);
    // A bedrock wall with one breakable pair; the breakable pair's only route
    // is a dig, and one of its neighbours is unknown.
    for x in -4..=4 {
        for y in [64, 65] {
            world.set(x, y, 1, bedrock());
        }
    }
    for y in [64, 65] {
        world.set(0, y, 1, stone());
    }
    world.set_unknown(1, 64, 1);

    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 64, 2)));
    match plan.status {
        PlanStatus::Partial { cell } => assert_eq!(cell, pos(1, 64, 1)),
        other => panic!("expected partial, got {other:?}"),
    }
    assert!(plan.steps.is_empty());
}

#[test]
fn a_tiny_budget_times_out() {
    let mut world = TestWorld::flat(63, 20);
    let state = state_with(&[]);
    let mut planner = Planner::with_budget(Budget::nodes(5));
    planner.set_max_drop(3);
    let plan = planner.plan(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(20, 64, 20)));
    assert_eq!(plan.status, PlanStatus::Timeout);
    assert!(plan.nodes_expanded <= 5);
}

#[test]
fn digging_two_blocks_ahead_carries_two_mine_calls() {
    let mut world = TestWorld::flat(63, 6);
    // A bedrock wall with one breakable pair, so the dig is the only route.
    for x in -6..=6 {
        for y in [64, 65] {
            world.set(x, y, 2, bedrock());
        }
    }
    for y in [64, 65] {
        world.set(0, y, 2, stone());
    }
    let state = state_with(&[("iron_pickaxe", 1)]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 64, 3)));

    assert_eq!(plan.status, PlanStatus::Success, "{plan:?}");
    let dig = plan
        .steps
        .iter()
        .find(|step| matches!(step.kind, MoveKind::Dig { .. }))
        .expect("the wall must be dug");
    assert_eq!(dig.primitives.len(), 2);
    assert_eq!(dig.primitives[0].name, "mine");
    assert_eq!(dig.primitives[0].args, vec![praxion_rules::ArgValue::Block("stone".into())]);

    let primitives = Primitives::standard();
    for call in &dig.primitives {
        let record = call.record(&primitives).expect("a registry record");
        assert_eq!(record.evidence, EvidenceKind::BlockBecomesAir);
        assert!(
            call.effects(&primitives)
                .is_some_and(|effects| !effects.changes.is_empty())
        );
    }
    assert_valid_steps(&plan);
}

#[test]
fn the_planner_never_digs_straight_down() {
    let mut world = TestWorld::flat(63, 4);
    // A pure dirt floor, so a downward dig would be possible if a move existed.
    for x in -4..=4 {
        for z in -4..=4 {
            world.set(x, 63, z, dirt());
        }
    }
    let state = state_with(&[]);
    let plan = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Block(pos(0, 60, 0)));
    assert!(matches!(plan.status, PlanStatus::NoPath { .. }));
    assert!(
        plan.steps
            .iter()
            .all(|step| step.primitives.iter().all(|call| call.name != "mine"))
    );
}

#[test]
fn a_better_tool_makes_the_same_dig_cheaper() {
    let mut world = TestWorld::flat(63, 6);
    for x in -6..=6 {
        for y in [64, 65] {
            world.set(x, y, 2, bedrock());
        }
    }
    for y in [64, 65] {
        world.set(0, y, 2, stone());
    }
    let hand = plan_once(
        &mut world,
        &state_with(&[]),
        pos(0, 64, 0),
        Goal::Block(pos(0, 64, 3)),
    );
    let pick = plan_once(
        &mut world,
        &state_with(&[("iron_pickaxe", 1)]),
        pos(0, 64, 0),
        Goal::Block(pos(0, 64, 3)),
    );
    let hand_dig: f32 = hand
        .steps
        .iter()
        .filter(|step| matches!(step.kind, MoveKind::Dig { .. }))
        .map(|step| step.cost)
        .sum();
    let pick_dig: f32 = pick
        .steps
        .iter()
        .filter(|step| matches!(step.kind, MoveKind::Dig { .. }))
        .map(|step| step.cost)
        .sum();
    assert!(
        pick_dig < hand_dig,
        "iron pickaxe {pick_dig} must beat hand {hand_dig}"
    );
}

#[test]
fn region_near_and_entity_goals_resolve() {
    let mut world = TestWorld::flat(63, 8);
    world.add_entity(42, pos(3, 64, 3));
    let state = state_with(&[]);

    let region = plan_once(
        &mut world,
        &state,
        pos(0, 64, 0),
        Goal::Region {
            min: pos(2, 64, 2),
            max: pos(4, 64, 4),
        },
    );
    assert_eq!(region.status, PlanStatus::Success);
    assert!(region
        .steps
        .last()
        .is_some_and(|step| step.to.x >= 2 && step.to.z >= 2));

    let near = plan_once(
        &mut world,
        &state,
        pos(0, 64, 0),
        Goal::Near {
            pos: pos(5, 64, 0),
            radius: 2.,
        },
    );
    assert_eq!(near.status, PlanStatus::Success);
    let end = near.steps.last().expect("a path").to;
    assert!((end.x - 5).abs() <= 2);

    let entity = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Entity(42));
    assert_eq!(entity.status, PlanStatus::Success);
    assert_eq!(entity.steps.last().map(|step| step.to), Some(pos(3, 64, 3)));

    let unknown_entity = plan_once(&mut world, &state, pos(0, 64, 0), Goal::Entity(99));
    assert_eq!(
        unknown_entity.status,
        PlanStatus::NoPath {
            reason: Unreachable::GoalUnknown
        }
    );
}

#[test]
fn the_default_budget_is_a_time_limit() {
    assert_eq!(Planner::new().max_drop(), 3);
    assert!(Planner::new().budget().time <= Duration::from_secs(1));
    assert!(Planner::new().budget().nodes >= 1_000);
}
