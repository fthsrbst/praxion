//! `pickup(item)`: walk to a dropped item and collect it (decision 2026-11-09
//! "Five next areas": `hunt` must collect the meat its kill dropped; before,
//! only `mine` walked to its own drops).
//!
//! The receipt is the inventory gaining one `item` (`IntentSpec::Inventory`),
//! so a drop of another kind picked up on the way does not settle it. The walk
//! reuses `mine`'s pickup state: follow the nearest item entity within 8 blocks
//! and treat a seen drop that stays gone for a few ticks as collected.

use praxion_headless::ControlFrame;
use praxion_interpreter::Outcome;
use praxion_perception::{Answer, Caller, EntityFilter};
use azalea_registry::builtin::EntityKind;

use crate::common::{idle, pickup_frame};
use crate::mine::{Pickup, pickup_arrived};
use crate::runner::{Ctx, IntentSpec};

/// Ticks a call may walk before it gives up `no_item`.
const PICKUP_CALL_TICKS: u32 = 80;
/// Ticks to wait for a drop to appear before it counts as absent: a kill's
/// drop spawns a tick or two after the death.
const APPEAR_TICKS: u32 = 10;
/// How far a drop may lie from the eye and still be walked to.
const PICKUP_RADIUS: u32 = 8;

#[derive(Debug)]
pub(crate) struct PickupItem {
    item: String,
    state: Pickup,
    ticks: u32,
    seen: bool,
}

impl PickupItem {
    pub(crate) fn new(item: String) -> Self {
        Self {
            item,
            state: Pickup::default(),
            ticks: 0,
            seen: false,
        }
    }

    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        self.ticks += 1;
        if self.ticks > PICKUP_CALL_TICKS {
            return (frame, None, Some(Outcome::Failed("no_item".into())));
        }
        let items = match ctx.perception.entities_near(
            Caller::Other,
            ctx.world,
            PICKUP_RADIUS,
            EntityFilter::Kind(EntityKind::Item),
        ) {
            Answer::Known { value, .. } => Some(value),
            // An unanswerable entity query is not proof that nothing dropped.
            Answer::Unknown(_) => None,
        };
        let target = items.as_deref().and_then(|items| self.state.target(items));
        self.seen |= target.is_some();
        let Some(target) = target else {
            if self.seen && self.state.gone() {
                // The drop was in sight and is gone: it was collected, but
                // not as `item` (the receipt would have settled): another
                // drop, or a despawn.
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            }
            if !self.seen && self.ticks > APPEAR_TICKS {
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            }
            return (frame, self.spec(started), None);
        };
        if !pickup_arrived(ctx.body.position(), target) {
            let built = pickup_frame(ctx, target);
            frame.look = built.look;
            frame.held = built.held;
        }
        (frame, self.spec(started), None)
    }

    /// The receipt spec, sent until the runner registers it.
    fn spec(&self, started: bool) -> Option<IntentSpec> {
        (!started).then(|| IntentSpec::Inventory {
            primitive: "pickup".to_owned(),
            item: self.item.clone(),
            delta: 1,
            window: 100,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_world::{add_entity, feet, remove_entity, swept, world};
    use azalea_core::position::Vec3;
    use praxion_headless::Body;
    use praxion_planner::Personality;

    fn tick_at(
        pickup: &mut PickupItem,
        world: &praxion_world::World,
        perception: &mut praxion_perception::Perception,
        body: &mut Body,
        tick: u64,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut ctx = Ctx {
            tick,
            body,
            world,
            perception,
            style: Personality::default(),
        };
        let result = pickup.tick(&mut ctx, false);
        // The look takes effect at once here; the body never moves.
        result.0.apply_with_menu(body, tick).expect("frame applies");
        result
    }

    #[test]
    fn a_drop_in_sight_is_walked_to_and_the_receipt_is_asked_for() {
        let mut world = world();
        add_entity(&mut world, 9, EntityKind::Item, Vec3::new(0.5, 64.0, 4.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut pickup = PickupItem::new("beef".into());
        let mut walked = false;
        for tick in 20..30 {
            let (frame, spec, outcome) = tick_at(&mut pickup, &world, &mut perception, &mut body, tick);
            assert!(outcome.is_none(), "tick {tick}");
            assert_eq!(
                spec,
                Some(IntentSpec::Inventory {
                    primitive: "pickup".to_owned(),
                    item: "beef".to_owned(),
                    delta: 1,
                    window: 100,
                })
            );
            walked |= frame.held.forward;
        }
        // The look turns first, so walking may start a few ticks in; the
        // body here never turns, but the target is straight ahead (+z).
        assert!(walked, "the body walks toward the drop");
    }

    #[test]
    fn no_drop_at_all_fails_no_item_after_the_appearance_wait() {
        let world = world();
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut pickup = PickupItem::new("beef".into());
        let mut last = None;
        for tick in 0..30u64 {
            let (_, _, outcome) = tick_at(&mut pickup, &world, &mut perception, &mut body, 20 + tick);
            if outcome.is_some() {
                last = Some((tick, outcome));
                break;
            }
        }
        let (tick, outcome) = last.expect("it gives up");
        assert_eq!(outcome, Some(Outcome::Failed("no_item".into())));
        assert!(tick >= u64::from(APPEAR_TICKS) - 1, "it waited for the drop to appear: {tick}");
    }

    #[test]
    fn a_drop_that_vanishes_without_a_receipt_fails_no_item() {
        let mut world = world();
        add_entity(&mut world, 9, EntityKind::Item, Vec3::new(0.5, 64.0, 4.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut pickup = PickupItem::new("beef".into());
        tick_at(&mut pickup, &world, &mut perception, &mut body, 20);
        remove_entity(&mut world, 9);
        let mut outcome = None;
        for tick in 21..40 {
            outcome = tick_at(&mut pickup, &world, &mut perception, &mut body, tick).2;
            if outcome.is_some() {
                break;
            }
        }
        assert_eq!(outcome, Some(Outcome::Failed("no_item".into())));
    }
}
