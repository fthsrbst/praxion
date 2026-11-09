//! Independent tests for primitives (step 8b review).

use azalea_core::position::Vec3;
use praxion_headless::{Body, ControlFrame, Held};
use praxion_interpreter::Outcome;
use praxion_perception::Perception;
use praxion_planner::Goal;
use praxion_primitives::{Ctx, Runner};
use praxion_interpreter::StartAction;
use praxion_rules::{ArgValue, Primitives};
use praxion_world::{BlockPos, World};

fn make_ctx<'a>(
    body: &'a Body,
    world: &'a World,
    perception: &'a mut Perception,
) -> Ctx<'a> {
    Ctx {
        tick: 0,
        body,
        world,
        perception,
        style: praxion_planner::Personality::default(),
    }
}

#[test]
fn runner_cancel_mid_flight_leaves_no_action_in_flight() {
    let mut runner = Runner::new();
    runner
        .start(&StartAction {
            primitive: "wait".to_owned(),
            args: vec![ArgValue::Int(5)],
        })
        .unwrap();
    // Tick once so the primitive is actually in flight.
    let body = Body::new(Vec3::new(0.0, 64.0, 0.0));
    let world = World::new();
    let mut perception = Perception::new(praxion_perception::Config::default());
    let mut ctx = make_ctx(&body, &world, &mut perception);
    let first = runner.tick(&mut ctx);
    assert!(first.pending.is_some() || runner.is_active());

    let cancelled = runner.cancel();
    assert_eq!(cancelled, Some(Outcome::Preempted));
    assert!(!runner.is_active());
    assert_eq!(runner.outcome(), Some(&Outcome::Preempted));

    // A further tick must not resurrect an action or emit a pending intent.
    let mut ctx = make_ctx(&body, &world, &mut perception);
    let after = runner.tick(&mut ctx);
    assert!(after.pending.is_none());
    assert_eq!(after.outcome, Some(Outcome::Preempted));
    let _ = ControlFrame::new(0, body.generation(), Held::NONE);
}

// FINDING 1: cancel() finishes the primitive but leaves `intent_id` set, so a
// later poll can attribute a stale receipt to a new run.
#[test]
fn runner_cancel_clears_stale_intent_id() {
    let mut runner = Runner::new();
    runner.registered(42);
    assert_eq!(runner.intent_id(), Some(42));
    runner
        .start(&StartAction {
            primitive: "wait".to_owned(),
            args: vec![ArgValue::Int(5)],
        })
        .unwrap();
    runner.cancel();
    assert!(runner.intent_id().is_none());
}

#[test]
fn flight_deadlines_cover_internal_waits() {
    let p = Primitives::standard();
    assert!(p.get("mine").unwrap().default_max_ticks >= 180);
    assert!(p.get("place").unwrap().default_max_ticks >= 80);
    assert!(p.get("craft").unwrap().default_max_ticks >= 500);
    assert!(p.get("smelt").unwrap().default_max_ticks >= 700);
    assert!(p.get("eat").unwrap().default_max_ticks >= 180);
    assert!(p.get("attack").unwrap().default_max_ticks >= 420);
    assert!(p.get("equip").unwrap().default_max_ticks >= 80);
    assert!(p.get("wait").unwrap().default_max_ticks >= 100);
}

// FINDING 2: observe waits up to 80 ticks internally but its flight deadline
// is only 20, so the interpreter preempts it before its own bound.
#[test]
fn flight_deadline_observe_covers_internal_wait() {
    let p = Primitives::standard();
    assert!(p.get("observe").unwrap().default_max_ticks >= 80);
}

// FINDING 3: move_to waits up to 80 ticks for a target before reporting
// `no_target`, but its flight deadline is only 60.
#[test]
fn flight_deadline_move_to_covers_internal_wait() {
    let p = Primitives::standard();
    assert!(p.get("move_to").unwrap().default_max_ticks >= 80);
}

#[test]
fn move_to_radius_does_not_report_reached_two_below() {
    let goal = Goal::Near {
        pos: BlockPos::new(0, 0, 0),
        radius: 1.5,
    };
    assert!(!goal.success(BlockPos::new(0, -2, 0)));
}
