//! `mine`: break a block, then walk to its drop.

use azalea_core::position::{BlockPos, Vec3};
use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_headless::ControlFrame;
use praxion_interpreter::Outcome;
use praxion_perception::{Answer, Caller, EntityFilter};
use praxion_planner::{WALK_TOLERANCE_DEG, center, look_towards};

use crate::common::{
    HandSelect, Selecting, ToolChoice, aim_precise, block_target, choose_tool, idle,
};
use crate::frontier::nearest_known_block;
use crate::runner::{Ctx, IntentSpec};

/// How long `mine` tries to bring its tool into the main hand before it mines
/// with what it holds (a swap waits for the server's slot update).
const TOOL_SELECT_TICKS: u32 = 40;

#[derive(Debug)]
pub(crate) struct Mine {
    pub(crate) name: String,
    pub(crate) target: Option<BlockPos>,
    pub(crate) waited: u32,
    /// Ticks spent selecting the tool before the first attack click.
    pub(crate) tool_waited: u32,
    pub(crate) hand: HandSelect,
    /// After the break is confirmed the runner walks to the drop before it
    /// settles, so the item is picked up. The mine receipt itself stays the
    /// block-break receipt; no inventory receipt is invented here.
    pub(crate) pickup: Option<Pickup>,
}

impl Mine {
    pub(crate) fn begin_pickup(&mut self) {
        if self.pickup.is_none() {
            self.pickup = Some(Pickup::default());
        }
    }

    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        if self.pickup.is_some() {
            return self.tick_pickup(ctx);
        }
        let mut frame = idle(ctx);
        if self.target.is_none() {
            self.target = block_target(ctx, &self.name, 5);
            if self.target.is_none() {
                if self.waited == 0
                    && let Ok(kind) = self.name.parse::<BlockKind>()
                {
                    let answer = ctx
                        .perception
                        .nearest_block(Caller::Other, ctx.world, kind, 5);
                    log::debug!("mine: nearest {:?} -> {:?}", self.name, answer);
                }
                self.waited += 1;
                // The skill's `near(T, r)` reads the known map, which also
                // holds cells out of sight (behind a corner, on a ledge). Walk
                // toward such a cell until a ray reaches it, as `craft` does
                // for its table; only a block the known map lost too is gone.
                if let Some((cell, _)) =
                    nearest_known_block(ctx.perception, ctx.world, &self.name, 5, &[])
                {
                    if self.waited > 80 {
                        return (frame, None, Some(Outcome::Failed("no_block".into())));
                    }
                    return (pickup_frame(ctx, center(cell)), None, None);
                }
                if self.waited > 40 {
                    return (frame, None, Some(Outcome::Failed("no_block".into())));
                }
                return (frame, None, None);
            }
        }
        let target = self.target.expect("checked above");
        // A known log can sit just outside the interaction range (the search
        // radius is five, vanilla reach is about 4.5). Walk toward it instead
        // of aiming forever at a cell the client will never pick.
        let eye = ctx.body.eye_position();
        let reach = ctx.body.block_interaction_range() - 0.25;
        if eye.distance_to(center(target)) > reach {
            self.waited = self.waited.saturating_add(1);
            if self.waited > 80 {
                return (frame, None, Some(Outcome::Failed("no_block".into())));
            }
            return (pickup_frame(ctx, center(target)), None, None);
        }
        self.waited = 0;
        // Hold the best tool before the first attack click; a mine that is
        // already under way keeps its item, because switching restarts the
        // break. Stone mined by hand breaks and drops nothing, which is how a
        // fresh-spawn body mined 21 stone and collected no cobblestone.
        if !started && let Some(state) = ctx.world.block_state(target) {
            let held = praxion_headless::held_item(ctx.world);
            match choose_tool(state, ctx.world.inventory(), &held) {
                ToolChoice::Keep => {}
                ToolChoice::Missing => {
                    return (frame, None, Some(Outcome::Failed("needs_tool".into())));
                }
                ToolChoice::Select(item) => {
                    self.tool_waited = self.tool_waited.saturating_add(1);
                    let selecting = self.hand.tick(ctx.world, &item, &mut frame);
                    let give_up =
                        selecting == Selecting::Unavailable || self.tool_waited > TOOL_SELECT_TICKS;
                    if !give_up {
                        return (frame, None, None);
                    }
                    // The better tool cannot be brought into the hand: mine
                    // with the held item only if the block still drops with it.
                    if !praxion_headless::tool_fit(state, &held).drops {
                        return (frame, None, Some(Outcome::Failed("needs_tool".into())));
                    }
                    frame = idle(ctx);
                }
            }
        }
        let (look, aligned) = aim_precise(ctx, center(target));
        frame.look = look;
        if !aligned {
            return (frame, None, None);
        }
        frame.clicks.push(praxion_headless::Click::Attack);
        let pending = if started {
            None
        } else {
            Some(IntentSpec::Dig {
                pos: target,
                window: 160,
            })
        };
        (frame, pending, None)
    }

    /// Walk to the dropped item for at most [`PICKUP_TICKS`] ticks after the
    /// break. A missing item entity means it was collected (or never spawned),
    /// which also ends the phase. The outcome is always `Confirmed`: the block
    /// break is what the mine receipt settled, and a failed pickup is not a
    /// failed dig.
    fn tick_pickup(
        &mut self,
        ctx: &mut Ctx<'_>,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        let pickup = self.pickup.as_mut().expect("checked by the caller");
        pickup.ticks += 1;
        if pickup.ticks > PICKUP_TICKS {
            return (frame, None, Some(Outcome::Confirmed));
        }
        let items = match ctx.perception.entities_near(
            Caller::Other,
            ctx.world,
            8,
            EntityFilter::Kind(EntityKind::Item),
        ) {
            Answer::Known { value, .. } => Some(value),
            // An unanswerable entity query is not proof the item is gone.
            Answer::Unknown(_) => None,
        };
        let target = match items.as_deref() {
            Some(items) => match pickup.target(items) {
                Some(position) => position,
                None if pickup.gone() => {
                    // It was visible and has been gone for a few ticks:
                    // picked up.
                    return (frame, None, Some(Outcome::Confirmed));
                }
                None => pickup
                    .last
                    .or_else(|| self.target.map(center))
                    .unwrap_or_else(|| ctx.body.position()),
            },
            None => pickup
                .last
                .or_else(|| self.target.map(center))
                .unwrap_or_else(|| ctx.body.position()),
        };
        if pickup_arrived(ctx.body.position(), target) {
            return (frame, None, None);
        }
        let built = pickup_frame(ctx, target);
        frame.look = built.look;
        frame.held = built.held;
        (frame, None, None)
    }
}

/// How long the post-break walk to the drop may take.
pub const PICKUP_TICKS: u32 = 60;

/// How many ticks a seen drop must be missing before the phase treats it as
/// collected. One or two missed sightings can be an occlusion.
const PICKUP_GONE_TICKS: u32 = 5;

/// The pickup phase state: the item entity it is following and how long it has
/// been walking.
#[derive(Debug, Default)]
pub(crate) struct Pickup {
    ticks: u32,
    target_id: Option<i32>,
    last: Option<Vec3>,
    missing: u32,
}

impl Pickup {
    /// The position to walk to, from this tick's item entities. The first
    /// visible item becomes the target; if it is missing for a few ticks the
    /// drop has been collected.
    fn target(&mut self, items: &[praxion_perception::EntityFact]) -> Option<Vec3> {
        if let Some(id) = self.target_id {
            if let Some(fact) = items.iter().find(|fact| fact.id == id) {
                self.missing = 0;
                self.last = Some(fact.position);
                return Some(fact.position);
            }
            self.missing = self.missing.saturating_add(1);
            return None;
        }
        let nearest = items
            .iter()
            .min_by(|a, b| a.distance.total_cmp(&b.distance))?;
        self.target_id = Some(nearest.id);
        self.missing = 0;
        self.last = Some(nearest.position);
        Some(nearest.position)
    }

    fn gone(&self) -> bool {
        self.target_id.is_some() && self.missing >= PICKUP_GONE_TICKS
    }
}

/// Whether the body is close enough that the server should collect the drop.
fn pickup_arrived(position: Vec3, target: Vec3) -> bool {
    let dx = position.x - target.x;
    let dz = position.z - target.z;
    dx * dx + dz * dz < 0.49 && (position.y - target.y).abs() < 1.0
}

/// Steer toward a drop: aim first, then walk, and jump when it is above.
fn pickup_frame(ctx: &Ctx<'_>, target: Vec3) -> ControlFrame {
    let mut frame = idle(ctx);
    let eye = ctx.body.eye_position();
    let aim_point = Vec3::new(target.x, target.y + 0.25, target.z);
    let (look, aligned) = look_towards(ctx.body.look(), eye, aim_point, WALK_TOLERANCE_DEG);
    frame.look = look;
    if aligned {
        frame.held.forward = true;
        if target.y > ctx.body.position().y + 0.5 && ctx.body.on_ground() {
            frame.held.jump = true;
        }
    }
    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use praxion_headless::Body;
    use praxion_perception::{EntityFact, Perception};
    use praxion_world::World;

    #[test]
    fn pickup_steers_toward_the_drop() {
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        // South is the body's initial yaw; the look still has to pitch down to
        // the drop, so the walk starts once the aim is inside the tolerance.
        let mut walks = false;
        for _ in 0..30 {
            let frame = {
                let ctx = Ctx {
                    tick: 0,
                    body: &body,
                    world: &world,
                    perception: &mut perception,
                    style: praxion_planner::Personality::default(),
                };
                pickup_frame(&ctx, Vec3::new(0.5, 64.0, 1.5))
            };
            if frame.held.forward {
                walks = true;
                break;
            }
            frame.apply(&mut body, 0).unwrap();
        }
        assert!(walks, "walk toward the drop once the aim settles");
        assert!(pickup_arrived(
            Vec3::new(0.5, 64.0, 0.9),
            Vec3::new(0.5, 64.0, 1.2)
        ));
        assert!(!pickup_arrived(
            Vec3::new(0.5, 64.0, 0.5),
            Vec3::new(0.5, 64.0, 2.0)
        ));
    }

    fn item(id: i32, x: f64, y: f64, z: f64, distance: f64) -> EntityFact {
        EntityFact {
            id,
            kind: EntityKind::Item,
            position: Vec3::new(x, y, z),
            distance,
        }
    }

    #[test]
    fn pickup_follows_one_drop_until_it_is_gone() {
        let mut pickup = Pickup::default();
        let drop = item(7, 0.5, 64.0, 1.5, 1.0);
        assert_eq!(
            pickup.target(std::slice::from_ref(&drop)),
            Some(Vec3::new(0.5, 64.0, 1.5))
        );
        assert!(!pickup.gone());
        for _ in 0..(PICKUP_GONE_TICKS - 1) {
            assert_eq!(pickup.target(&[]), None);
            assert!(!pickup.gone(), "one missed sighting is not a pickup");
        }
        assert_eq!(pickup.target(&[]), None);
        assert!(pickup.gone(), "the drop was collected");
    }
}
