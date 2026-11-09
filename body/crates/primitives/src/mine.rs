//! `mine`: break a block, then walk to its drop.

use azalea_core::position::{BlockPos, Vec3};
use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_gamedata as gamedata;
use praxion_headless::ControlFrame;
use praxion_interpreter::Outcome;
use praxion_perception::{Answer, Caller, EntityFilter, exposes_fluid, workable};
use praxion_planner::center;
use praxion_world::BlockState;

use crate::common::{
    HandSelect, ToolStep, aim_precise, block_target_where, hold_tool, idle, pickup_frame,
    walk_point,
};
use crate::frontier::nearest_known_block;
use crate::runner::{Ctx, IntentSpec};

/// How far `mine` looks in the known map for a target it cannot see yet. One
/// block wider than the skills' `near(T, 5)`: the fact measures from the eye
/// position and this search from the eye cell, so a block at the edge was near
/// for the skill and missing here, and `mine` failed `no_block` one tick after
/// `find_stone` ended (autonomous run 3, 2026-11-08). Out-of-reach targets are
/// walked to anyway.
const KNOWN_SEARCH_RADIUS: u32 = 6;

/// How many soft occluders one `mine` call may dig through before it gives up.
const MAX_OCCLUDERS: u32 = 3;

/// A bare hand breaks a soft occluder in this many ticks (one second) at most.
const HAND_BREAK_TICKS: f32 = 20.0;

/// What the sight ray to a known target says about the first cell it meets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DigThrough {
    /// Nothing to dig: keep the walk-then-fail behaviour.
    Nothing,
    /// The first blocking cell is the wanted kind after all.
    Target(BlockPos),
    /// A soft block hides the wanted cell; dig it and select again.
    Occluder(BlockPos),
}

/// Dig-through bookkeeping for one `mine` call. `Mine::dig_through` is `Some`
/// while the call is breaking soft occluders to reach a known wanted cell.
#[derive(Debug, Default)]
pub(crate) struct DigState {
    /// Soft occluders dug in this call.
    occluders: u32,
    /// The current target is an occluder, not the wanted block.
    digging_occluder: bool,
    /// The runner registered a `Dig` receipt for the current target.
    ///
    /// The runner's `started` flag covers only the first target of a call and
    /// stays true afterwards, so a target selected after an occluder broke
    /// needs its own flag. It is set by [`Mine::note_registered`], never when
    /// the intent is merely emitted: the driver drops a spec it cannot
    /// register, and the primitive must send it again.
    target_started: bool,
}

/// What the start of a tick does with the current dig target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetCheck {
    /// The target is still there (or its cell is unloaded): keep digging.
    Keep,
    /// The target vanished before the runner registered it: select again.
    Forget,
    /// A dig-through target broke after its receipt: walk to the drop.
    Pickup,
    /// A receipt settled but the cell is occupied again (falling sand or
    /// gravel): dig it again under its own receipt.
    Retry,
}

/// Decide the current target's fate before the tick acts. `settled` is true
/// once the runner settled a receipt and started the pickup marker. A target
/// that vanished before it started is forgotten even while a pickup marker is
/// active, so a dropped `Dig` spec cannot leave the body clicking air.
fn target_check(
    gone: bool,
    settled: bool,
    dig_through: bool,
    digging_occluder: bool,
    target_started: bool,
) -> TargetCheck {
    if gone {
        if !target_started {
            return TargetCheck::Forget;
        }
        if !dig_through || !settled {
            return TargetCheck::Keep;
        }
        return if digging_occluder {
            TargetCheck::Forget
        } else {
            TargetCheck::Pickup
        };
    }
    if settled && target_started && dig_through {
        return TargetCheck::Retry;
    }
    TargetCheck::Keep
}

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
    /// `Some` while the call digs soft blocks that hide a known target.
    pub(crate) dig_through: Option<DigState>,
}

impl Mine {
    pub(crate) fn begin_pickup(&mut self) {
        if self.pickup.is_none() {
            self.pickup = Some(Pickup::default());
        }
    }

    /// Select a new dig target for this call.
    fn set_target(&mut self, target: BlockPos, digging_occluder: bool) {
        self.target = Some(target);
        self.waited = 0;
        self.tool_waited = 0;
        if let Some(dig) = &mut self.dig_through {
            dig.digging_occluder = digging_occluder;
            dig.target_started = false;
        }
    }

    /// The target vanished before the runner registered a `Dig` for it: forget
    /// it and select again. The pickup marker is kept: while it is `Some`,
    /// `Runner` keeps the primitive active for the dig-through loop.
    fn forget_target(&mut self) {
        self.target = None;
        self.waited = 0;
        self.tool_waited = 0;
        if let Some(dig) = &mut self.dig_through {
            dig.digging_occluder = false;
            dig.target_started = false;
        }
    }

    /// The receipt settled but the cell holds a block again (a falling sand or
    /// gravel block refilled it). Dig it as a new target: a fresh receipt and
    /// a fresh tool choice, never an unreceipted second click.
    fn retry_target(&mut self) {
        if let Some(dig) = &mut self.dig_through {
            dig.target_started = false;
        }
        self.tool_waited = 0;
    }

    /// The runner registered the pending intent. The driver drops a spec it
    /// cannot register, so a target counts as started only after this call.
    pub(crate) fn note_registered(&mut self) {
        if let Some(dig) = &mut self.dig_through {
            dig.target_started = true;
        }
    }

    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        // The runner's `started` covers only the first target of a call; a
        // dig-through target after an occluder broke has its own flag.
        let target_started = self
            .dig_through
            .as_ref()
            .map_or(started, |dig| dig.target_started);
        let gone = self
            .target
            .is_some_and(|target| block_gone(ctx.world.block_state(target)));
        let dig_through = self.dig_through.is_some();
        let digging_occluder = self
            .dig_through
            .as_ref()
            .is_some_and(|dig| dig.digging_occluder);
        match target_check(
            gone,
            self.pickup.is_some(),
            dig_through,
            digging_occluder,
            target_started,
        ) {
            TargetCheck::Forget => self.forget_target(),
            TargetCheck::Retry => self.retry_target(),
            TargetCheck::Pickup => {
                let last = self.target.map(center);
                self.dig_through = None;
                self.pickup = Some(Pickup {
                    last,
                    ..Pickup::default()
                });
                return self.tick_pickup(ctx);
            }
            TargetCheck::Keep => {}
        }
        // Not digging through: a pending pickup is the post-break walk.
        if self.pickup.is_some() && self.dig_through.is_none() {
            return self.tick_pickup(ctx);
        }
        let mut frame = idle(ctx);
        if self.target.is_none() {
            // Never break a block that holds back water or lava: digging the
            // stone of a lake bed flooded the hole and the body drowned
            // (seed 202 live run 2026-11-08). `near`, `explore` and this search
            // ask the same `workable` rule, so a search cannot end on a cell
            // this call would then refuse.
            let world = ctx.world;
            let accept = |pos: BlockPos, state: BlockState| {
                workable(pos, state, |cell| world.block_state(cell))
            };
            if let Some(target) = block_target_where(ctx, &self.name, 5, accept) {
                self.set_target(target, false);
            } else {
                if self.waited == 0
                    && let Ok(kind) = self.name.parse::<BlockKind>()
                {
                    let answer = ctx
                        .perception
                        .nearest_block(Caller::Other, ctx.world, kind, 5);
                    log::debug!("mine: nearest {:?} -> {:?}", self.name, answer);
                }
                // The wanted cell is known but not in sight. Ask perception
                // where the sight ray meets first: the wanted block itself or
                // a soft block hiding it.
                if let Some((cell, _)) =
                    nearest_known_block(ctx.perception, ctx.world, &self.name, KNOWN_SEARCH_RADIUS, &[])
                {
                    // No reach check on the hidden cell itself: the block to
                    // dig is the first one the ray meets, which is nearer, and
                    // a chosen target out of reach is walked to below (seed 101:
                    // stone 4.4 blocks away under the floor was never tried).
                    let eye = ctx.body.eye_position();
                    if ctx.body.on_ground() {
                        let feet = ctx.body.position().to_block_pos_floor();
                        let blocking =
                            ctx.perception
                                .first_blocking_cell(Caller::Other, ctx.world, eye, cell);
                        let decision = through_decision(&self.name, feet, blocking, |pos| {
                            ctx.world.block_state(pos)
                        });
                        match decision {
                            DigThrough::Target(target) => {
                                log::debug!(
                                    "mine: the ray meets {:?} at {:?} before the known {:?} at {:?}; body at {:?}",
                                    kind_at(ctx.world, target),
                                    target,
                                    self.name,
                                    cell,
                                    ctx.body.position()
                                );
                                self.set_target(target, false);
                            }
                            DigThrough::Occluder(occluder) => {
                                let dug = self.dig_through.as_ref().map_or(0, |dig| dig.occluders);
                                if dug >= MAX_OCCLUDERS {
                                    log::debug!(
                                        "mine: {:?} occluders already dug, {:?} at {:?} still hides {:?}; giving up",
                                        dug,
                                        kind_at(ctx.world, occluder),
                                        occluder,
                                        self.name
                                    );
                                    return (frame, None, Some(Outcome::Failed("no_block".into())));
                                }
                                log::debug!(
                                    "mine: dig through {:?} at {:?} to reach the known {:?} at {:?}; body at {:?}",
                                    kind_at(ctx.world, occluder),
                                    occluder,
                                    self.name,
                                    cell,
                                    ctx.body.position()
                                );
                                let dig = self.dig_through.get_or_insert_with(DigState::default);
                                dig.occluders += 1;
                                self.set_target(occluder, true);
                            }
                            DigThrough::Nothing => {}
                        }
                    }
                }
                if self.target.is_none() {
                    self.waited += 1;
                    // Walk toward a known cell until a ray reaches it, as
                    // `craft` does for its table (`Station`); only a block the
                    // known map lost too is gone.
                    if let Some((cell, _)) =
                        nearest_known_block(ctx.perception, ctx.world, &self.name, KNOWN_SEARCH_RADIUS, &[])
                    {
                        if self.waited > 80 {
                            log::debug!(
                                "mine: known {:?} at {:?} never came into sight; body at {:?}",
                                self.name,
                                cell,
                                ctx.body.position()
                            );
                            return (frame, None, Some(Outcome::Failed("no_block".into())));
                        }
                        let feet = ctx.body.position().to_block_pos_floor();
                        return (pickup_frame(ctx, walk_point(feet, cell)), None, None);
                    }
                    if self.waited > 40 {
                        return (frame, None, Some(Outcome::Failed("no_block".into())));
                    }
                    return (frame, None, None);
                }
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
        // The runner's `started` covers only the first target of a call; a
        // dig-through selects later targets after it is already true, so the
        // dig state carries the per-target flag. The flag moves only when the
        // runner reports the intent registered (`Mine::note_registered`), so
        // a dropped spec is sent again instead of being lost.
        let target_started = match &self.dig_through {
            Some(dig) => dig.target_started,
            None => started,
        };
        if !target_started && let Some(state) = ctx.world.block_state(target) {
            match hold_tool(ctx, state, &mut self.hand, &mut self.tool_waited, &mut frame) {
                ToolStep::Ready => {}
                ToolStep::Pending => return (frame, None, None),
                ToolStep::Missing => {
                    return (frame, None, Some(Outcome::Failed("needs_tool".into())));
                }
            }
        }
        let (look, aligned) = aim_precise(ctx, center(target));
        frame.look = look;
        if !aligned {
            return (frame, None, None);
        }
        frame.clicks.push(praxion_headless::Click::Attack);
        // Keep sending the spec until the runner registers it; a target that
        // is already registered never asks for a second receipt.
        let pending = (!target_started).then_some(IntentSpec::Dig {
            pos: target,
            window: 160,
        });
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
    pub(crate) fn target(&mut self, items: &[praxion_perception::EntityFact]) -> Option<Vec3> {
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

    pub(crate) fn gone(&self) -> bool {
        self.target_id.is_some() && self.missing >= PICKUP_GONE_TICKS
    }
}

/// Decide whether a known-but-unseen target can be reached by digging the
/// first cell the sight ray meets. The ray itself is cast by perception;
/// `lookup` is the world's block access, so the decision is pure and testable
/// without a body or a loaded chunk.
fn through_decision(
    wanted: &str,
    feet: BlockPos,
    blocking: Option<BlockPos>,
    lookup: impl Fn(BlockPos) -> Option<BlockState>,
) -> DigThrough {
    let Some(cell) = blocking else {
        return DigThrough::Nothing;
    };
    let Some(state) = lookup(cell) else {
        return DigThrough::Nothing;
    };
    if matches_wanted(wanted, state) {
        // A wanted block that holds back water or lava is not a target even
        // when it is the nearest match: breaking the lake-bed stone floods the
        // hole and drowns the body. Workstations always count, as everywhere
        // else the rule is applied.
        return if workable(cell, state, &lookup) {
            DigThrough::Target(cell)
        } else {
            DigThrough::Nothing
        };
    }
    let support = BlockPos::new(feet.x, feet.y.saturating_sub(1), feet.z);
    if cell == support || drop_too_deep(feet, cell, &lookup) {
        return DigThrough::Nothing;
    }
    if is_soft_occluder(state) && !exposes_fluid(cell, &lookup) {
        DigThrough::Occluder(cell)
    } else {
        DigThrough::Nothing
    }
}

/// Whether `state` is a block `wanted` names: a block kind or a `#tag`.
pub(crate) fn matches_wanted(wanted: &str, state: BlockState) -> bool {
    let kind = BlockKind::from(state);
    match wanted.strip_prefix('#') {
        Some(tag) => gamedata::block_in_tag(kind.to_str(), tag),
        None => wanted
            .parse::<BlockKind>()
            .is_ok_and(|wanted| wanted == kind),
    }
}

/// Ticks a bare hand needs for `state`, from the same `hardness` data the
/// planner's dig cost uses. A block that needs a pickaxe is slower by hand.
fn hand_break_ticks(state: BlockState) -> f32 {
    let hardness = praxion_planner::blocks::hardness(state);
    if hardness < 0.0 {
        return f32::INFINITY;
    }
    let divisor = if gamedata::needs_pickaxe(praxion_planner::blocks::name(state)) {
        100.0
    } else {
        30.0
    };
    (hardness * divisor).max(1.0)
}

/// A block worth digging through: breakable by hand in about a second, not a
/// hazard, and not a fluid itself.
fn is_soft_occluder(state: BlockState) -> bool {
    praxion_planner::blocks::is_breakable(state)
        && !praxion_planner::blocks::is_hazard(state)
        && hand_break_ticks(state) <= HAND_BREAK_TICKS
}

/// Whether removing `cell` leaves the body's own column over a drop deeper
/// than two blocks. Count the cells below the feet as if `removed` were air,
/// and stop at the first solid cell; an unknown cell is not evidence of a drop.
fn drop_too_deep(
    feet: BlockPos,
    removed: BlockPos,
    lookup: &impl Fn(BlockPos) -> Option<BlockState>,
) -> bool {
    let mut empty = 0;
    // Three empty cells are already a drop deeper than two.
    for depth in 1..=3 {
        let pos = BlockPos::new(feet.x, feet.y.saturating_sub(depth), feet.z);
        if pos != removed {
            match lookup(pos) {
                Some(state) if praxion_planner::blocks::is_solid(state) => break,
                Some(_) => {}
                None => break,
            }
        }
        empty += 1;
        if empty > 2 {
            return true;
        }
    }
    false
}

/// Whether a target cell no longer holds a block (air, cave air or void air).
/// An unloaded cell is not proof that the block broke.
pub(crate) fn block_gone(state: Option<BlockState>) -> bool {
    state.is_some_and(|state| {
        matches!(
            BlockKind::from(state),
            BlockKind::Air | BlockKind::CaveAir | BlockKind::VoidAir
        )
    })
}

/// The kind of a cell, for the dig-through log line.
fn kind_at(world: &praxion_world::World, pos: BlockPos) -> Option<BlockKind> {
    world.block_state(pos).map(BlockKind::from)
}

/// Whether the body is close enough that the server should collect the drop.
pub(crate) fn pickup_arrived(position: Vec3, target: Vec3) -> bool {
    let dx = position.x - target.x;
    let dz = position.z - target.z;
    dx * dx + dz * dz < 0.49 && (position.y - target.y).abs() < 1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use praxion_headless::Body;
    use praxion_perception::{EntityFact, Perception};
    use praxion_world::World;

    #[test]
    fn a_cell_under_the_feet_is_looked_at_from_the_side() {
        let feet = BlockPos::new(-102, 63, 106);
        let below = BlockPos::new(-102, 61, 106);
        let point = walk_point(feet, below);
        assert_eq!(point.to_block_pos_floor(), BlockPos::new(-100, 63, 106));
        // Diagonally under: step away from it, not onto it.
        let diagonal = BlockPos::new(-103, 61, 105);
        assert_eq!(
            walk_point(feet, diagonal).to_block_pos_floor(),
            BlockPos::new(-101, 63, 107)
        );
        // A cell beside or above is walked to directly.
        let beside = BlockPos::new(-99, 63, 106);
        assert_eq!(walk_point(feet, beside), center(beside));
    }

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

    fn known(entries: &[(BlockPos, BlockKind)]) -> std::collections::HashMap<BlockPos, BlockState> {
        entries
            .iter()
            .map(|(pos, kind)| (*pos, BlockState::from(*kind)))
            .collect()
    }

    fn decide(
        map: &std::collections::HashMap<BlockPos, BlockState>,
        wanted: &str,
        feet: BlockPos,
        blocking: Option<BlockPos>,
    ) -> DigThrough {
        through_decision(wanted, feet, blocking, |pos| map.get(&pos).copied())
    }

    #[test]
    fn a_soft_occluder_before_the_target_is_dug() {
        // The sight ray meets dirt at (1, 64) before the remembered stone.
        let feet = BlockPos::new(0, 64, 0);
        let dirt = BlockPos::new(1, 64, 0);
        let map = known(&[(dirt, BlockKind::Dirt)]);
        assert_eq!(
            decide(&map, "stone", feet, Some(dirt)),
            DigThrough::Occluder(dirt)
        );
    }

    #[test]
    fn dirt_next_to_water_is_not_dug_through() {
        let feet = BlockPos::new(0, 64, 0);
        let dirt = BlockPos::new(1, 64, 0);
        let map = known(&[
            (dirt, BlockKind::Dirt),
            (BlockPos::new(1, 65, 0), BlockKind::Water),
        ]);
        assert_eq!(
            decide(&map, "stone", feet, Some(dirt)),
            DigThrough::Nothing,
            "removing the dirt would expose water"
        );
    }

    #[test]
    fn a_target_kind_occluder_is_mined_as_the_target() {
        let feet = BlockPos::new(0, 64, 0);
        let nearer = BlockPos::new(1, 64, 0);
        let map = known(&[(nearer, BlockKind::Stone)]);
        assert_eq!(
            decide(&map, "stone", feet, Some(nearer)),
            DigThrough::Target(nearer),
            "the ray meets stone before the remembered cell"
        );
    }

    #[test]
    fn a_wanted_block_next_to_water_is_not_a_target() {
        let feet = BlockPos::new(0, 64, 0);
        let stone = BlockPos::new(1, 64, 0);
        let map = known(&[
            (stone, BlockKind::Stone),
            (BlockPos::new(1, 65, 0), BlockKind::Water),
        ]);
        assert_eq!(
            decide(&map, "stone", feet, Some(stone)),
            DigThrough::Nothing,
            "breaking the lake-bed stone would expose water"
        );
    }

    #[test]
    fn obsidian_is_too_hard_to_dig_through() {
        let feet = BlockPos::new(0, 64, 0);
        let obsidian = BlockPos::new(1, 64, 0);
        let map = known(&[(obsidian, BlockKind::Obsidian)]);
        assert_eq!(
            decide(&map, "stone", feet, Some(obsidian)),
            DigThrough::Nothing
        );
    }

    #[test]
    fn the_feet_support_is_never_dug() {
        // Looking straight down: the dirt under the feet hides stone below.
        let feet = BlockPos::new(0, 64, 0);
        let support = BlockPos::new(0, 63, 0);
        let map = known(&[(support, BlockKind::Dirt)]);
        assert_eq!(
            decide(&map, "stone", feet, Some(support)),
            DigThrough::Nothing,
            "the supporting dirt is never chosen"
        );
    }

    #[test]
    fn a_removed_floor_cell_drops_only_as_far_as_the_next_solid_cell() {
        let feet = BlockPos::new(0, 64, 0);
        let removed = BlockPos::new(0, 63, 0);

        let short = known(&[(BlockPos::new(0, 62, 0), BlockKind::Stone)]);
        assert!(
            !drop_too_deep(feet, removed, &|pos| short.get(&pos).copied()),
            "one empty cell then stone is a safe drop"
        );

        let deep = known(&[
            (BlockPos::new(0, 62, 0), BlockKind::Air),
            (BlockPos::new(0, 61, 0), BlockKind::Air),
            (BlockPos::new(0, 60, 0), BlockKind::Stone),
        ]);
        assert!(
            drop_too_deep(feet, removed, &|pos| deep.get(&pos).copied()),
            "three empty cells are deeper than two"
        );

        let unknown = known(&[]);
        assert!(
            !drop_too_deep(feet, removed, &|pos| unknown.get(&pos).copied()),
            "an unknown cell is not evidence of a drop"
        );
    }

    fn state_machine_mine(target: BlockPos, dig_through: Option<DigState>) -> Mine {
        Mine {
            name: "stone".to_owned(),
            target: Some(target),
            waited: 0,
            tool_waited: 0,
            hand: HandSelect::default(),
            pickup: None,
            dig_through,
        }
    }

    fn run_mine_tick(
        mine: &mut Mine,
        tick: u64,
        started: bool,
        body: &mut Body,
        world: &World,
        perception: &mut Perception,
    ) -> (Option<IntentSpec>, Option<Outcome>) {
        let mut ctx = Ctx {
            tick,
            body,
            world,
            perception,
            style: praxion_planner::Personality::default(),
        };
        let (frame, pending, outcome) = mine.tick(&mut ctx, started);
        frame.apply(body, tick).expect("the mine frame applies");
        (pending, outcome)
    }

    fn run_until_dig(
        mine: &mut Mine,
        started: bool,
        body: &mut Body,
        world: &World,
        perception: &mut Perception,
    ) -> Option<IntentSpec> {
        for tick in 0..20 {
            let (pending, _) = run_mine_tick(mine, tick, started, body, world, perception);
            if pending.is_some() {
                return pending;
            }
        }
        None
    }

    #[test]
    fn a_dig_is_repeated_until_the_runner_registers_it() {
        let cell = BlockPos::new(0, 64, 2);
        let mut mine = state_machine_mine(cell, Some(DigState::default()));
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));

        let first = run_until_dig(&mut mine, false, &mut body, &world, &mut perception);
        assert_eq!(
            first,
            Some(IntentSpec::Dig {
                pos: cell,
                window: 160
            })
        );
        assert!(
            !mine.dig_through.as_ref().expect("dig state").target_started,
            "emitting the spec is not a receipt"
        );
        let again = run_mine_tick(&mut mine, 20, false, &mut body, &world, &mut perception).0;
        assert_eq!(
            again,
            Some(IntentSpec::Dig {
                pos: cell,
                window: 160
            }),
            "an unregistered spec is sent again"
        );

        mine.note_registered();
        let after = run_mine_tick(&mut mine, 21, false, &mut body, &world, &mut perception).0;
        assert_eq!(after, None, "once registered, the spec is not repeated");
        assert!(
            mine.dig_through.as_ref().expect("dig state").target_started,
            "the runner reported the registration"
        );
    }

    #[test]
    fn the_first_target_follows_the_runner_started_flag() {
        let cell = BlockPos::new(0, 64, 2);
        let mut mine = state_machine_mine(cell, None);
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));

        assert!(run_until_dig(&mut mine, false, &mut body, &world, &mut perception).is_some());
        let again = run_mine_tick(&mut mine, 20, false, &mut body, &world, &mut perception).0;
        assert!(
            again.is_some(),
            "the first target stays unregistered until the runner says so"
        );
        let started = run_mine_tick(&mut mine, 21, true, &mut body, &world, &mut perception).0;
        assert_eq!(started, None);
    }

    #[test]
    fn a_target_gone_before_it_started_is_forgotten_regardless_of_pickup() {
        for settled in [false, true] {
            assert_eq!(
                target_check(true, settled, true, true, false),
                TargetCheck::Forget,
                "a vanished occluder with no receipt is dropped (settled={settled})"
            );
            assert_eq!(
                target_check(true, settled, false, false, false),
                TargetCheck::Forget,
                "a vanished wanted target with no receipt is dropped (settled={settled})"
            );
        }

        let mut mine = state_machine_mine(
            BlockPos::new(0, 64, 2),
            Some(DigState {
                occluders: 1,
                digging_occluder: true,
                target_started: false,
            }),
        );
        mine.forget_target();
        assert!(mine.target.is_none(), "the target is selected again");
        let dig = mine.dig_through.as_ref().expect("dig state");
        assert!(!dig.digging_occluder, "the occluder flag is reset");
        assert!(!dig.target_started);
    }

    #[test]
    fn a_refilled_occluder_is_dug_under_a_new_receipt() {
        let cell = BlockPos::new(0, 64, 2);
        let mut mine = state_machine_mine(
            cell,
            Some(DigState {
                occluders: 1,
                digging_occluder: true,
                target_started: true,
            }),
        );
        // The occluder's receipt settled and the cell is not gone in this
        // world (an unloaded cell is never proof of air): falling sand refilled
        // it. It must be dug again under a new receipt.
        mine.pickup = Some(Pickup::default());
        mine.tool_waited = 9;
        let world = World::new();
        let mut perception = Perception::new(praxion_perception::Config::default());
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));

        let pending = run_until_dig(&mut mine, true, &mut body, &world, &mut perception);
        assert_eq!(
            pending,
            Some(IntentSpec::Dig {
                pos: cell,
                window: 160
            }),
            "the refilled cell is a new target with its own receipt"
        );
        let dig = mine.dig_through.as_ref().expect("dig state");
        assert!(dig.digging_occluder, "it is still the occluder cell");
        assert!(
            !dig.target_started,
            "the new target waits for its own registration"
        );
        assert_eq!(mine.tool_waited, 0, "the tool is chosen again");
    }

    #[test]
    fn mine_chooses_a_dry_stone_over_the_nearer_wet_one() {
        use crate::test_world::{feet, set_block, world};

        // The nearer stone holds back water; the farther one is dry. A lake-bed
        // stone is exactly what `near(stone, 5)` used to accept and `mine` then
        // refused with `no_block` (seed 202 live run 2026-11-08).
        let mut world = world();
        let wet = BlockPos::new(0, 64, 2);
        let dry = BlockPos::new(2, 64, 2);
        set_block(&mut world, wet, BlockState::from(BlockKind::Stone));
        set_block(&mut world, dry, BlockState::from(BlockKind::Stone));
        set_block(
            &mut world,
            BlockPos::new(0, 65, 2),
            BlockState::from(BlockKind::Water),
        );

        let mut perception = Perception::new(praxion_perception::Config::default());
        perception.begin_tick(&world);
        let body = Body::new(feet());
        let mut mine = Mine {
            name: "stone".to_owned(),
            target: None,
            waited: 0,
            tool_waited: 0,
            hand: HandSelect::default(),
            pickup: None,
            dig_through: None,
        };
        let mut ctx = Ctx {
            tick: 0,
            body: &body,
            world: &world,
            perception: &mut perception,
            style: praxion_planner::Personality::default(),
        };
        let _ = mine.tick(&mut ctx, false);
        assert_eq!(
            mine.target,
            Some(dry),
            "the wet stone is nearer but cannot be worked"
        );
    }

    #[test]
    fn mining_stone_with_only_planks_fails_needs_tool() {
        use crate::test_world::{feet, hold, set_block, world};
        use azalea_registry::builtin::ItemKind;

        let mut world = world();
        let cell = BlockPos::new(0, 64, 2);
        set_block(&mut world, cell, BlockState::from(BlockKind::Stone));
        hold(&mut world, ItemKind::OakPlanks);
        let mut perception = Perception::new(praxion_perception::Config::default());
        perception.begin_tick(&world);
        let body = Body::new(feet());
        let mut mine = state_machine_mine(cell, None);

        let mut ctx = Ctx {
            tick: 0,
            body: &body,
            world: &world,
            perception: &mut perception,
            style: praxion_planner::Personality::default(),
        };
        let (_, pending, outcome) = mine.tick(&mut ctx, false);
        assert_eq!(
            outcome,
            Some(Outcome::Failed("needs_tool".into())),
            "stone mined with planks breaks and drops nothing"
        );
        assert!(pending.is_none());
    }

    #[test]
    fn a_dig_already_under_way_never_swaps_the_tool() {
        use crate::test_world::{feet, give, hold, set_block, world};
        use azalea_registry::builtin::ItemKind;
        use praxion_headless::held_item;
        use crate::common::{ToolChoice, choose_tool};

        let mut world = world();
        let cell = BlockPos::new(0, 64, 2);
        set_block(&mut world, cell, BlockState::from(BlockKind::Stone));
        hold(&mut world, ItemKind::OakPlanks);
        // A pickaxe sits in the main inventory: a fresh target would swap to it.
        give(&mut world, 20, ItemKind::WoodenPickaxe, 1);
        assert_eq!(
            choose_tool(
                BlockState::from(BlockKind::Stone),
                world.inventory(),
                &held_item(&world),
            ),
            ToolChoice::Select("wooden_pickaxe".into())
        );

        let mut perception = Perception::new(praxion_perception::Config::default());
        perception.begin_tick(&world);
        let body = Body::new(feet());
        let mut mine = state_machine_mine(cell, None);
        let mut ctx = Ctx {
            tick: 0,
            body: &body,
            world: &world,
            perception: &mut perception,
            style: praxion_planner::Personality::default(),
        };
        // The runner already registered this target: the dig is under way.
        let (frame, pending, outcome) = mine.tick(&mut ctx, true);
        assert!(
            frame.menu.is_none(),
            "a dig under way never swaps the tool"
        );
        assert!(frame.select_slot.is_none(), "no hotbar key either");
        assert!(pending.is_none(), "the registered target needs no new receipt");
        assert!(outcome.is_none());
    }
}
