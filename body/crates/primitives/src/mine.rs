//! `mine`: break a block, then walk to its drop.

use azalea_core::position::{BlockPos, Vec3};
use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_facts::item_count;
use praxion_gamedata as gamedata;
use praxion_headless::ControlFrame;
use praxion_interpreter::Outcome;
use praxion_perception::{Answer, Caller, EntityFilter, Perception, exposes_fluid, workable};
use praxion_planner::{blocks, center};
use praxion_world::BlockState;

use crate::common::{
    HandSelect, ToolStep, aim_precise, block_target_where, cell_centre, choose_aim, hold_tool,
    idle, pickup_frame, walk_point,
};
use crate::frontier::nearest_known_block;
use crate::runner::{Ctx, IntentSpec};

/// The typed failure of a dig the body cannot finish inside its remaining air:
/// the eyes are under water and the block outlasts the breath. A drowned dig
/// never completes, and every restart dug the same cell again until the goal's
/// budget ran out (live run A, 2026-11-12: eight 185-tick `mine` calls on
/// `-96, 58, 10`, each preempted by the drowning reflex).
const UNDERWATER: &str = "underwater";

/// Air ticks a submerged dig must leave for the drowning reflex and its walk
/// to the surface: the break estimate is approximate, and the reflex needs a
/// turn before the air runs out.
const AIR_MARGIN_TICKS: u32 = 40;

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
/// once the runner settled a receipt for the current target. A target
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
    /// Aligned ticks on which the crosshair was not on the target. Past
    /// [`BLOCKED_TICKS`] the call fails `out_of_reach` or `no_sight` instead of
    /// clicking something else for 160 ticks (decision 2026-11-09 "Five next
    /// areas": run 6 spent 2,358 ticks on logs it could not hit).
    pub(crate) blocked: u32,
    /// Ticks spent walking toward a target higher than reach.
    pub(crate) uphill: u32,
    /// Targets this call found no crosshair point on and could not dig
    /// through: every later search of the call skips them (run 9: each retry
    /// picked the same hidden log again).
    pub(crate) rejected: Vec<BlockPos>,
    /// The failure kind of the last rejected target: `no_sight`, or
    /// `out_of_reach` for one two or more blocks above the feet. The call
    /// fails with it once no candidate is left.
    pub(crate) rejected_kind: Option<&'static str>,
    /// The one item breaking the current target always drops (see
    /// [`sure_drop`]), or `None` when it drops nothing certain. The pickup walk
    /// ends only once the inventory holds more of it than before the break.
    pub(crate) sure_drop: Option<&'static str>,
    /// A dig receipt settled `confirmed` for the current target. The pickup
    /// marker cannot say it: it stays set across targets of a dig-through so
    /// the runner keeps the primitive alive.
    pub(crate) settled: bool,
    /// How many of [`Mine::sure_drop`] the body held when the pickup walk
    /// began: the break's own drop cannot be collected before it spawns, so
    /// the count at that moment is the "before".
    pub(crate) held_before: Option<u32>,
}

/// Aligned ticks without the crosshair on the target before the call gives up.
const BLOCKED_TICKS: u32 = 10;

/// Horizontal distance (eye to target centre) inside which a target higher than
/// reach is "right overhead": no walk helps, only a climb.
const UNDER_TARGET_RADIUS: f64 = 1.5;

/// Ticks `mine` walks toward a target higher than reach, hoping the ground rises
/// (about three seconds), before it fails `out_of_reach`.
const UPHILL_WALK_TICKS: u32 = 60;

impl Mine {
    pub(crate) fn begin_pickup(&mut self) {
        self.settled = true;
        if self.pickup.is_none() {
            self.pickup = Some(Pickup::default());
        }
    }

    /// Select a new dig target for this call.
    fn set_target(&mut self, target: BlockPos, digging_occluder: bool) {
        self.target = Some(target);
        self.waited = 0;
        self.tool_waited = 0;
        self.blocked = 0;
        // The receipt that settled belonged to the block before. Left set,
        // `target_check` read it as "a receipt settled for THIS target" and
        // dug the new target again under a new receipt, tick after tick, the
        // whole time it was being dug (run B, 2026-11-12: nine intents on one
        // log, every one expiring unconfirmed). The pickup marker itself must
        // stay: it also keeps the runner's primitive alive between targets.
        self.settled = false;
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
        self.settled = false;
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
        // The settled receipt is spent: the new one has to settle before the
        // cell counts as broken again.
        self.settled = false;
    }

    /// The runner registered the pending intent. The driver drops a spec it
    /// cannot register, so a target counts as started only after this call.
    pub(crate) fn note_registered(&mut self) {
        if let Some(dig) = &mut self.dig_through {
            dig.target_started = true;
        }
    }

    /// No crosshair point lands on `target` from here. `centre_hit` is the
    /// cell the pick toward its centre lands on instead: dig it when it is a
    /// soft occluder [`through_decision`] allows (not the support, no drop, no
    /// fluid) and the call is under [`MAX_OCCLUDERS`], take it when it is a
    /// wanted block itself, else reject the target for the rest of the call.
    ///
    /// The cells the decision reads come through perception, not the raw
    /// store (architecture §5.1): the hit cell by sight (the pick met it, so
    /// it is in view), its neighbours and the cells under the feet from
    /// perception's memory. A cell perception cannot answer is no fluid and no
    /// drop, as the shared [`exposes_fluid`] rule reads an unanswered cell.
    fn unseen_target(&mut self, ctx: &mut Ctx<'_>, target: BlockPos, centre_hit: Option<BlockPos>) {
        let feet = ctx.body.position().to_block_pos_floor();
        let occluders = self.dig_through.as_ref().map_or(0, |dig| dig.occluders);
        let hit = centre_hit.and_then(|cell| {
            ctx.perception
                .block(Caller::Other, ctx.world, cell)
                .value()
                .map(|fact| (cell, fact.state))
        });
        let perception = &*ctx.perception;
        let lookup = |pos: BlockPos| match hit {
            Some((cell, state)) if cell == pos => Some(state),
            _ => perception.known(pos).map(|seen| seen.state),
        };
        match through_decision(&self.name, feet, centre_hit, lookup) {
            DigThrough::Target(cell) if !self.rejected.contains(&cell) => {
                log::debug!(
                    "mine: {:?} at {:?} is hidden; the crosshair meets the wanted {:?} first",
                    self.name,
                    target,
                    cell
                );
                self.set_target(cell, false);
            }
            DigThrough::Occluder(cell) if occluders < MAX_OCCLUDERS => {
                log::debug!(
                    "mine: dig through {:?} at {:?} to reach {:?} at {:?}",
                    hit.map(|(_, state)| BlockKind::from(state)),
                    cell,
                    self.name,
                    target
                );
                let dig = self.dig_through.get_or_insert_with(DigState::default);
                dig.occluders += 1;
                self.set_target(cell, true);
            }
            _ => self.reject(feet, target),
        }
    }

    /// Give up on `target` for the rest of the call and select again; the
    /// call fails with this kind only once no candidate is left.
    fn reject(&mut self, feet: BlockPos, target: BlockPos) {
        let kind = if target.y >= feet.y + 2 {
            "out_of_reach"
        } else {
            "no_sight"
        };
        log::debug!("mine: rejected {:?} at {:?} ({kind})", self.name, target);
        self.rejected.push(target);
        self.rejected_kind = Some(kind);
        self.blocked = 0;
        self.forget_target();
    }

    /// One tick of walking toward a target above reach: counts it and says
    /// whether the call must give up now. It does when the body is right under
    /// the target (no step helps) or has walked [`UPHILL_WALK_TICKS`] without
    /// the ground rising to it.
    fn give_up_uphill(&mut self, eye: Vec3, aim: Vec3) -> bool {
        let horizontal = ((aim.x - eye.x).powi(2) + (aim.z - eye.z).powi(2)).sqrt();
        self.uphill = self.uphill.saturating_add(1);
        horizontal <= UNDER_TARGET_RADIUS || self.uphill > UPHILL_WALK_TICKS
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
            self.settled,
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
            //
            // A target this call already rejected (no crosshair point lands on
            // it and nothing soft hides it) is skipped by every search below.
            let world = ctx.world;
            let rejected = &self.rejected;
            let accept = |pos: BlockPos, state: BlockState| {
                !rejected.contains(&pos) && workable(pos, state, |cell| world.block_state(cell))
            };
            // A block inside a pool (a lake bed, a shore under the water line)
            // is mined with the eyes under water: slow, and over in one breath.
            // A dry alternative in the same reach wins, so a body at the edge
            // of a pool walks ashore instead of drowning beside it. When no dry
            // cell can stand beside any candidate, the underwater check below
            // decides, and the dig is refused rather than restarted forever.
            //
            // The stand cells are read from what perception remembers: a cell
            // it has not seen is no evidence of a dry stand.
            let dry = |perception: &Perception, pos: BlockPos, state: BlockState| {
                accept(pos, state)
                    && dry_stand(pos, |cell| perception.known(cell).map(|seen| seen.state))
            };
            if let Some(target) = block_target_where(ctx, &self.name, 5, dry).or_else(|| {
                block_target_where(ctx, &self.name, 5, |_, pos, state| accept(pos, state))
            }) {
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
                    nearest_known_block(ctx.perception, ctx.world, &self.name, KNOWN_SEARCH_RADIUS, &self.rejected)
                {
                    // No reach check on the hidden cell itself: the block to
                    // dig is the first one the ray meets, which is nearer, and
                    // a chosen target out of reach is walked to below (seed 101:
                    // stone 4.4 blocks away under the floor was never tried).
                    let eye = ctx.body.eye_position();
                    // A hidden target above reach is not dug toward: the soft
                    // blocks between are the roof of the body's own hole, and
                    // tunnelling up through them brings a log six blocks over
                    // the head no nearer (run B, 2026-11-12: the body at the
                    // bottom of its dig broke a wall block and the next, then
                    // clicked a log it could never hit). It is walked toward
                    // or failed `out_of_reach` below, like a visible one.
                    let overhead = above_reach(eye.y, cell, ctx.body.block_interaction_range() - 0.25);
                    if ctx.body.on_ground() && !overhead {
                        let feet = ctx.body.position().to_block_pos_floor();
                        let blocking =
                            ctx.perception
                                .first_blocking_cell(Caller::Other, ctx.world, eye, cell);
                        let decision =
                            through_decision(&self.name, feet, blocking, |pos| ctx.world.block_state(pos));
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
                        nearest_known_block(ctx.perception, ctx.world, &self.name, KNOWN_SEARCH_RADIUS, &self.rejected)
                    {
                        let eye = ctx.body.eye_position();
                        let reach = ctx.body.block_interaction_range() - 0.25;
                        if above_reach(eye.y, cell, reach) && self.give_up_uphill(eye, center(cell)) {
                            log::debug!(
                                "mine: known {:?} at {:?} is higher than reach above the eye {:?}",
                                self.name,
                                cell,
                                eye
                            );
                            return (frame, None, Some(Outcome::Failed("out_of_reach".into())));
                        }
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
                    // Every candidate left was rejected in this call: fail
                    // with the rejection's own kind at once, not `no_block`
                    // after a wait (no candidate is coming back).
                    if let Some(kind) = self.rejected_kind {
                        return (frame, None, Some(Outcome::Failed(kind.into())));
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
        // A target whose centre lies higher above the eye than the reach: from
        // right under it no step helps, and climbing (`pillar`) is the skill's
        // job, so the typed failure comes at once. From off to the side the
        // ground may rise toward it (a tree on a slope above a pit, run 8,
        // 2026-11-10): walk toward it for a bounded time first, jumping the
        // steps, and fail `out_of_reach` only if it never comes within reach
        // (user decision 2026-11-10).
        if above_reach(eye.y, target, reach) {
            let aim = center(target);
            if self.give_up_uphill(eye, aim) {
                log::debug!(
                    "mine: {:?} at {:?} is higher than reach above the eye {:?}",
                    self.name,
                    target,
                    eye
                );
                return (frame, None, Some(Outcome::Failed("out_of_reach".into())));
            }
            return (pickup_frame(ctx, aim), None, None);
        }
        if eye.distance_to(center(target)) > reach {
            self.waited = self.waited.saturating_add(1);
            if self.waited > 80 {
                return (frame, None, Some(Outcome::Failed("no_block".into())));
            }
            return (pickup_frame(ctx, center(target)), None, None);
        }
        self.waited = 0;
        // In reach: the stand position of this dig is the body's own cell. A
        // block the breath cannot outlast is not started and not continued:
        // the drowning reflex would preempt it mid-dig tick after tick while
        // every restart began the same block from zero (run A, 2026-11-12).
        let target_state = ctx.world.block_state(target);
        // What breaking it certainly drops, so the pickup walk afterwards can
        // check the inventory instead of trusting that a drop which left the
        // body's view was collected. A soft block dug only to get at the
        // wanted one is never watched.
        let digging_occluder = self
            .dig_through
            .as_ref()
            .is_some_and(|dig| dig.digging_occluder);
        self.sure_drop = target_state
            .filter(|_| !digging_occluder)
            .and_then(sure_drop);
        if target_state.is_some_and(|state| drowned_before_break(ctx, state)) {
            log::debug!(
                "mine: {:?} at {:?} outlasts the breath with the eyes under water; body at {:?}",
                self.name,
                target,
                ctx.body.position()
            );
            return (frame, None, Some(Outcome::Failed(UNDERWATER.into())));
        }
        // The runner's `started` covers only the first target of a call; a
        // dig-through selects later targets after it is already true, so the
        // dig state carries the per-target flag. The flag moves only when the
        // runner reports the intent registered (`Mine::note_registered`), so
        // a dropped spec is sent again instead of being lost.
        let target_started = match &self.dig_through {
            Some(dig) => dig.target_started,
            None => started,
        };
        // Aim where the crosshair lands on the target: its centre, else the
        // nearest inset face point whose pick lands on it (run 9: perception
        // saw a log through one of its seven points while the click aimed at a
        // point another block hid). With no such point the call digs through
        // what hides it, or rejects the target and picks another; a dig under
        // way keeps going.
        let aim = choose_aim(ctx, target);
        let point = match aim.point {
            Some(point) => point,
            // A pick that meets nothing (the chunk not loaded, a ray just
            // missing) is no proof of a hiding block: aim at the centre and
            // let the crosshair check below decide, as before.
            None if target_started || aim.centre_hit.is_none() => cell_centre(target),
            None => {
                if ctx.body.on_ground() {
                    self.unseen_target(ctx, target, aim.centre_hit);
                }
                return (frame, None, None);
            }
        };
        if !target_started && let Some(state) = target_state {
            match hold_tool(ctx, state, &mut self.hand, &mut self.tool_waited, &mut frame) {
                ToolStep::Ready => {}
                ToolStep::Pending => return (frame, None, None),
                ToolStep::Missing => {
                    return (frame, None, Some(Outcome::Failed("needs_tool".into())));
                }
            }
        }
        let (look, aligned) = aim_precise(ctx, point);
        frame.look = look;
        if !aligned {
            return (frame, None, None);
        }
        // The crosshair must be on the target. A canopy log behind leaves or
        // another log clicked something else until the dig window ran out
        // (run 6): fail fast and typed instead.
        let pick = praxion_headless::pick(ctx.world, ctx.body);
        // Only another block under the crosshair counts: an empty pick (the
        // chunk not loaded, a ray just missing) keeps the old behaviour and
        // clicks.
        let on_other = pick
            .block
            .as_ref()
            .is_some_and(|hit| !hit.miss && hit.block_pos != target);
        if on_other && !target_started {
            self.blocked += 1;
            if self.blocked > BLOCKED_TICKS {
                log::debug!(
                    "mine: crosshair never reached {:?} at {:?}; pick {:?}",
                    self.name,
                    target,
                    pick.block.as_ref().map(|hit| (hit.block_pos, hit.miss))
                );
                let feet = ctx.body.position().to_block_pos_floor();
                self.reject(feet, target);
            }
            return (frame, None, None);
        }
        if !on_other {
            self.blocked = 0;
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
    /// break. The outcome is always `Confirmed`: the block break is what the
    /// mine receipt settled, and a failed pickup is not a failed dig.
    ///
    /// When breaking the block certainly drops one item ([`sure_drop`]) the
    /// phase ends when the inventory holds more of it than it did at the
    /// start, and not before: a drop that left the body's view, or another
    /// item picked up on the way, is no proof that this one was collected (run
    /// B, 2026-11-12: the placed crafting table was mined "to take it along"
    /// and the body never held it). The walk follows an item entity of the
    /// wanted kind, looks for another one when the tracked one is gone without
    /// the inventory growing, and keeps to the cell the block stood in while
    /// none is in sight. Without a certain drop the old rule stands: a drop
    /// seen and then missing for a few ticks was collected.
    fn tick_pickup(
        &mut self,
        ctx: &mut Ctx<'_>,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        let wanted = self.sure_drop;
        let held = wanted.and_then(|item| item_count(ctx.perception, ctx.world, item));
        // The drop cannot be collected before it has spawned, and a block's
        // drop has a pickup delay: the count on the first tick of the walk is
        // the "before".
        if self.held_before.is_none() {
            self.held_before = held;
        }
        let collected = matches!(
            (self.held_before, held),
            (Some(before), Some(now)) if now > before
        );
        let pickup = self.pickup.as_mut().expect("checked by the caller");
        pickup.ticks += 1;
        if pickup.ticks > PICKUP_TICKS || collected {
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
        // An item entity whose stack the server has not sent yet may still be
        // the one: it is followed, and dropped again once its kind is known.
        let is_wanted = |fact: &praxion_perception::EntityFact| match (wanted, fact.item) {
            (Some(item), Some(kind)) => gamedata::normalize(kind.to_str()) == item,
            _ => true,
        };
        let last = pickup
            .last
            .or_else(|| self.target.map(center))
            .unwrap_or_else(|| ctx.body.position());
        let target = match items.as_deref() {
            Some(items) => match pickup.target_where(items, is_wanted) {
                Some(position) => position,
                None if pickup.gone() && wanted.is_none() => {
                    // It was visible and has been gone for a few ticks:
                    // picked up.
                    return (frame, None, Some(Outcome::Confirmed));
                }
                None => {
                    if pickup.gone() {
                        // Gone, yet the inventory did not grow: it was not
                        // collected. Follow the nearest one of the kind again.
                        pickup.retarget();
                    }
                    last
                }
            },
            None => last,
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
        self.target_where(items, |_| true)
    }

    /// [`Pickup::target`] restricted to the items `accept` takes. The tracked
    /// item counts as missing once it no longer passes `accept` either (its
    /// stack turned out to be another kind).
    pub(crate) fn target_where(
        &mut self,
        items: &[praxion_perception::EntityFact],
        accept: impl Fn(&praxion_perception::EntityFact) -> bool,
    ) -> Option<Vec3> {
        if let Some(id) = self.target_id {
            if let Some(fact) = items.iter().find(|fact| fact.id == id && accept(fact)) {
                self.missing = 0;
                self.last = Some(fact.position);
                return Some(fact.position);
            }
            self.missing = self.missing.saturating_add(1);
            return None;
        }
        let nearest = items
            .iter()
            .filter(|fact| accept(fact))
            .min_by(|a, b| a.distance.total_cmp(&b.distance))?;
        self.target_id = Some(nearest.id);
        self.missing = 0;
        self.last = Some(nearest.position);
        Some(nearest.position)
    }

    pub(crate) fn gone(&self) -> bool {
        self.target_id.is_some() && self.missing >= PICKUP_GONE_TICKS
    }

    /// Forget the tracked item so the next [`Pickup::target`] call picks the
    /// nearest one again. `pickup` uses it when the tracked drop vanished
    /// while the wanted item was not collected: the drop it followed was
    /// another item (or despawned), and a farther drop of the wanted item may
    /// still be on the ground (review finding 6).
    pub(crate) fn retarget(&mut self) {
        self.target_id = None;
        self.missing = 0;
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

/// The one item breaking `state` always drops, when its loot table has exactly
/// one entry that cannot roll zero (a log, a crafting table, stone, dirt). A
/// table with chances (leaves: sticks, saplings, apples) or several entries
/// names no certain item, so no pickup is verified for it.
fn sure_drop(state: BlockState) -> Option<&'static str> {
    match gamedata::block_drops(blocks::name(state))? {
        [only] if only.min >= 1 => Some(only.item),
        _ => None,
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

/// Whether the centre of `target` lies higher above the eye (`eye_y`) than
/// `reach` allows: no horizontal approach can bring it within range.
fn above_reach(eye_y: f64, target: BlockPos, reach: f64) -> bool {
    f64::from(target.y) + 0.5 - eye_y > reach
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

/// The four cells a body can stand in beside a block, at the block's own
/// height.
const STAND_SIDES: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];

/// Whether `target` can be mined with the eyes out of the water: one of the
/// four cells beside it is a dry cell the body fits in, meaning the feet and
/// the eye cell are both passable (no fluid, no hazard, no collision shape).
/// A lake-bed block has water beside it and over its stand cells, so every
/// place a body could stand to mine it puts the eyes under water.
///
/// `lookup` is the block access: the world in `mine`, a map in the tests. An
/// unloaded cell is not evidence of a dry stand, and a cell the body cannot
/// stand in (stone beside the target) is not a stand cell either.
fn dry_stand(target: BlockPos, lookup: impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    STAND_SIDES.iter().any(|(dx, dz)| {
        let feet = BlockPos::new(target.x + dx, target.y, target.z + dz);
        let eye = BlockPos::new(feet.x, feet.y + 1, feet.z);
        lookup(feet).is_some_and(praxion_planner::blocks::is_passable)
            && lookup(eye).is_some_and(praxion_planner::blocks::is_passable)
    })
}

/// Whether the body's own eyes are under water: the cell the eye position
/// falls in holds a swimmable water block (perception's own-state `eye_block`).
fn eyes_in_water(ctx: &Ctx<'_>) -> bool {
    ctx.perception
        .eye_block(ctx.world)
        .value()
        .is_some_and(|state| blocks::is_water(*state))
}

/// Ticks the best item the body carries needs to break `state` from where it
/// stands, or `u32::MAX` when nothing carried can break it. `submerged` adds
/// the vanilla eye-in-water penalty, destroy speed divided by five
/// (`Player.getDestroySpeed`), which the physics port leaves out: lake-bed
/// deepslate takes about 22 ticks above water and about 112 with the eyes
/// under, and no breath outlasts the second.
fn break_ticks(ctx: &Ctx<'_>, state: BlockState, submerged: bool) -> u32 {
    let progress = |item: &azalea_inventory::ItemStack| {
        let progress = ctx.body.mine_progress(state, item);
        if submerged { progress / 5.0 } else { progress }
    };
    let held = praxion_headless::held_item(ctx.world);
    let best = ctx
        .world
        .inventory()
        .iter()
        .enumerate()
        .filter(|(index, stack)| (9..=44).contains(index) && stack.count() > 0)
        .map(|(_, stack)| progress(stack))
        .fold(progress(&held), f32::max);
    if best <= 0.0 {
        return u32::MAX;
    }
    (1.0 / best).ceil().max(1.0) as u32
}

/// Whether a dig of `state` from where the body stands cannot finish inside the
/// breath it has left: the eyes are under water and the block outlasts the air
/// with [`AIR_MARGIN_TICKS`] to spare. Only a submerged eye is a risk; an
/// unknown air count is not a reason to refuse a dig (the drowning reflex still
/// guards the body).
fn drowned_before_break(ctx: &mut Ctx<'_>, state: BlockState) -> bool {
    if !eyes_in_water(ctx) {
        return false;
    }
    let Some(air) = ctx.perception.air(ctx.world).value().copied() else {
        return false;
    };
    break_ticks(ctx, state, true).saturating_add(AIR_MARGIN_TICKS) > air
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
            item: None,
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
            blocked: 0,
            uphill: 0,
            rejected: Vec::new(),
            rejected_kind: None,
            sure_drop: None,
            settled: false,
            held_before: None,
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
        mine.settled = true;
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
            blocked: 0,
            uphill: 0,
            rejected: Vec::new(),
            rejected_kind: None,
            sure_drop: None,
            settled: false,
            held_before: None,
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
    fn mine_skips_a_stone_whose_stand_cells_are_under_water() {
        use crate::test_world::{feet, set_block, swept, world};

        // The nearer stone is beside a pond whose surface reaches the eye level
        // of every cell a body could stand in to mine it. `workable` accepts it
        // (no face of the block touches water), yet mining it would put the
        // eyes under water; the farther stone has dry stand cells and wins.
        let mut world = world();
        let wet = BlockPos::new(0, 64, 2);
        let dry = BlockPos::new(3, 64, 0);
        set_block(&mut world, wet, BlockState::from(BlockKind::Stone));
        set_block(&mut world, dry, BlockState::from(BlockKind::Stone));
        for (dx, dz) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
            set_block(
                &mut world,
                BlockPos::new(wet.x + dx, wet.y + 1, wet.z + dz),
                BlockState::from(BlockKind::Water),
            );
        }

        let mut perception = swept(&world);
        let body = Body::new(feet());
        let mut mine = fresh_mine("stone", None);
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
            "the nearer stone would be mined with the eyes under water"
        );
    }

    #[test]
    fn mine_fails_underwater_instead_of_digging_past_the_breath() {
        use crate::test_world::{feet, hold, set_air, set_block, swept, world};
        use azalea_registry::builtin::ItemKind;

        // Live run A (2026-11-12): the body stood in a pool, `mine` dug the
        // same stone for 185 ticks, the drowning reflex preempted it, and the
        // skill restarted the same cell from zero. A block the remaining air
        // cannot outlast is a typed failure, at once.
        let mut world = world();
        let cell = BlockPos::new(2, 64, 0);
        set_block(&mut world, cell, BlockState::from(BlockKind::Stone));
        for pos in [BlockPos::new(0, 64, 0), BlockPos::new(0, 65, 0)] {
            set_block(&mut world, pos, BlockState::from(BlockKind::Water));
        }
        hold(&mut world, ItemKind::WoodenPickaxe);
        set_air(&mut world, 60);
        let mut perception = swept(&world);
        let body = Body::new(feet());
        let eye = body.eye_position().to_block_pos_floor();
        assert!(
            world
                .block_state(eye)
                .is_some_and(praxion_planner::blocks::is_water),
            "the test body's eyes are in the water column at {eye:?}"
        );
        let mut mine = fresh_mine("stone", Some(cell));
        let mut ctx = Ctx {
            tick: 0,
            body: &body,
            world: &world,
            perception: &mut perception,
            style: praxion_planner::Personality::default(),
        };
        let (_, pending, outcome) = mine.tick(&mut ctx, false);
        assert_eq!(outcome, Some(Outcome::Failed(UNDERWATER.into())));
        assert!(pending.is_none(), "no dig intent for a block out of breath");
    }

    #[test]
    fn mine_digs_a_soft_block_under_water_when_the_breath_outlasts_it() {
        use crate::test_world::{feet, set_air, set_block, swept, world};

        // The rule is not "never dig with wet eyes": leaves break in about 150
        // ticks with both water penalties, which a full breath covers.
        let mut world = world();
        let cell = BlockPos::new(2, 64, 0);
        set_block(&mut world, cell, BlockState::from(BlockKind::OakLeaves));
        for pos in [BlockPos::new(0, 64, 0), BlockPos::new(0, 65, 0)] {
            set_block(&mut world, pos, BlockState::from(BlockKind::Water));
        }
        set_air(&mut world, 300);
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut mine = fresh_mine("#leaves", Some(cell));
        let (dug, outcome) = first_dig(&mut mine, &world, &mut perception, &mut body, 20);
        assert_eq!(outcome, None, "a breakable block is not refused");
        assert_eq!(dug, Some(cell), "the leaves are dug");
    }

    #[test]
    fn dry_stand_needs_a_cell_beside_the_block_with_air_at_the_eye() {
        let target = BlockPos::new(0, 64, 0);
        let mut cells = known(&[(target, BlockKind::Stone)]);
        // Open on all four sides: a body can stand there.
        assert!(
            !dry_stand(target, |pos| cells.get(&pos).copied()),
            "an unloaded ring is no stand"
        );
        for (dx, dz) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
            cells.insert(BlockPos::new(dx, 64, dz), BlockState::from(BlockKind::Air));
            cells.insert(BlockPos::new(dx, 65, dz), BlockState::from(BlockKind::Air));
        }
        assert!(dry_stand(target, |pos| cells.get(&pos).copied()));
        // A stand cell the body does not fit in is no stand either: the eye
        // cell is a full block here, so the body cannot occupy the column.
        for (dx, dz) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
            cells.insert(BlockPos::new(dx, 65, dz), BlockState::from(BlockKind::Stone));
        }
        assert!(
            !dry_stand(target, |pos| cells.get(&pos).copied()),
            "a solid cell at the eye is not a stand"
        );
        // A pond over the whole ring: only water to stand in.
        for (dx, dz) in [(1, 0), (0, 1), (-1, 0), (0, -1)] {
            cells.insert(BlockPos::new(dx, 64, dz), BlockState::from(BlockKind::Water));
            cells.insert(BlockPos::new(dx, 65, dz), BlockState::from(BlockKind::Water));
        }
        assert!(!dry_stand(target, |pos| cells.get(&pos).copied()));
    }

    #[test]
    fn mine_never_targets_the_log_the_body_placed_to_climb() {
        use crate::test_world::{feet, set_block, swept, world};
        use praxion_perception::scaffold;

        // Review finding 1: a scaffold log is real `#logs`, and `mine` took it
        // for a target while `near` still counted it. Both sides skip the cells
        // the body filled to climb, through one predicate.
        let placed = BlockPos::new(1, 65, 0);
        let real = BlockPos::new(2, 65, 2);
        let mut world = world();
        set_block(&mut world, placed, BlockState::from(BlockKind::OakLog));
        set_block(&mut world, real, BlockState::from(BlockKind::OakLog));
        let mut perception = swept(&world);
        scaffold::note(placed);

        let body = Body::new(feet());
        let mut mine = fresh_mine("#logs", None);
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
            Some(real),
            "the nearer log is the body's own scaffolding"
        );
        scaffold::reset();
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

    #[test]
    fn above_reach_is_the_vertical_gap_only() {
        // Eye at 65.62, reach 4.25: a centre at 70.5 is 4.88 above, one at
        // 69.5 is 3.88 above.
        assert!(above_reach(65.62, BlockPos::new(0, 70, 0), 4.25));
        assert!(!above_reach(65.62, BlockPos::new(0, 69, 0), 4.25));
        assert!(!above_reach(65.62, BlockPos::new(40, 64, 40), 4.25), "far away is not above");
    }

    fn fresh_mine(name: &str, target: Option<BlockPos>) -> Mine {
        Mine {
            name: name.to_owned(),
            target,
            waited: 0,
            tool_waited: 0,
            hand: HandSelect::default(),
            pickup: None,
            dig_through: None,
            blocked: 0,
            uphill: 0,
            rejected: Vec::new(),
            rejected_kind: None,
            sure_drop: None,
            settled: false,
            held_before: None,
        }
    }

    /// Tick `mine` until it ends, applying its look each tick; returns the
    /// outcome and the tick it came on.
    fn run_to_outcome(
        mine: &mut Mine,
        world: &World,
        perception: &mut Perception,
        body: &mut Body,
        max: u64,
    ) -> Option<(u64, Outcome)> {
        for tick in 0..max {
            let (_, outcome) = run_mine_tick(mine, tick, false, body, world, perception);
            if let Some(outcome) = outcome {
                return Some((tick, outcome));
            }
        }
        None
    }

    #[test]
    fn a_log_higher_than_reach_fails_out_of_reach_at_once() {
        use crate::test_world::{feet, set_block, swept, world};
        // Decision 2026-11-09 "Five next areas": a log five blocks above the
        // feet was walked toward for 80 ticks. It is in view and under five
        // blocks away, but no step brings it within reach.
        let mut world = world();
        set_block(&mut world, BlockPos::new(0, 70, 0), BlockState::from(BlockKind::OakLog));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut mine = fresh_mine("#logs", None);
        let (tick, outcome) = run_to_outcome(&mut mine, &world, &mut perception, &mut body, 40)
            .expect("it fails");
        assert_eq!(outcome, Outcome::Failed("out_of_reach".into()));
        assert!(tick <= 3, "fast, not 80 ticks: {tick}");
    }

    #[test]
    fn a_high_log_off_to_the_side_is_walked_toward_before_out_of_reach() {
        use crate::test_world::{feet, set_block, swept, world};
        // Run 8, 2026-11-10: the body spawned in a pit and every log stood on
        // the ground above it, five or more blocks over the feet. Off to the
        // side, the call walks toward the log (jumping the steps) instead of
        // failing at once. On this flat floor the ground never rises, so it
        // ends `out_of_reach` once it stands under the log, where `pillar`
        // can climb straight to it.
        let mut world = world();
        let target = BlockPos::new(0, 70, 4);
        set_block(&mut world, target, BlockState::from(BlockKind::OakLog));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut mine = fresh_mine("#logs", Some(target));
        let mut walked = false;
        let mut result = None;
        for tick in 0..120u64 {
            let mut ctx = Ctx {
                tick,
                body: &body,
                world: &world,
                perception: &mut perception,
                style: praxion_planner::Personality::default(),
            };
            let (frame, _, outcome) = mine.tick(&mut ctx, false);
            walked |= frame.held.forward;
            frame.apply(&mut body, tick).expect("the mine frame applies");
            if let Some(outcome) = outcome {
                result = Some((tick, outcome));
                break;
            }
            body.step(&frame.held, &world);
        }
        let (tick, outcome) = result.expect("it fails in the end");
        assert!(walked, "the body walked toward the log");
        assert_eq!(outcome, Outcome::Failed("out_of_reach".into()));
        assert!(tick > 3, "after a walk, not at once: {tick}");
        let eye = body.eye_position();
        let horizontal = ((eye.x - 0.5).powi(2) + (eye.z - 4.5).powi(2)).sqrt();
        assert!(horizontal <= UNDER_TARGET_RADIUS, "it ends under the log: {horizontal}");
    }

    /// A stone floor at y 63 over chunk (0, 0); the test sets the rest.
    fn floor_world() -> World {
        crate::test_world::world_with(1, 63, |_| BlockState::from(BlockKind::Stone))
    }

    /// A body standing on the floor at the test feet cell.
    fn standing_body(world: &World) -> Body {
        let mut body = Body::new(crate::test_world::feet());
        for _ in 0..5 {
            body.step(&praxion_headless::Held::NONE, world);
        }
        assert!(body.on_ground(), "the body stands on the floor");
        body
    }

    /// Tick `mine` until it emits a dig spec or ends; the look is applied, the
    /// body does not move.
    fn first_dig(
        mine: &mut Mine,
        world: &World,
        perception: &mut Perception,
        body: &mut Body,
        max: u64,
    ) -> (Option<BlockPos>, Option<Outcome>) {
        for tick in 0..max {
            let (pending, outcome) = run_mine_tick(mine, tick, false, body, world, perception);
            if let Some(IntentSpec::Dig { pos, .. }) = pending {
                return (Some(pos), None);
            }
            if outcome.is_some() {
                return (None, outcome);
            }
        }
        (None, None)
    }

    #[test]
    fn a_log_whose_floor_centre_is_hidden_is_dug_where_the_crosshair_lands() {
        use crate::test_world::{set_block, swept};
        // Run 9 diagnosis: `mine` aimed only at the floor centre of its target
        // and failed `no_sight` when that point was hidden, although the log's
        // top was in plain view. Here a stone in front hides the floor centre;
        // the cell centre is still reachable over it.
        let target = BlockPos::new(0, 64, 2);
        let wall = BlockPos::new(0, 64, 1);
        let mut world = floor_world();
        set_block(&mut world, target, BlockState::from(BlockKind::OakLog));
        set_block(&mut world, wall, BlockState::from(BlockKind::Stone));
        let mut perception = swept(&world);
        let mut body = standing_body(&world);
        let eye = body.eye_position();
        let (yaw, pitch) = praxion_planner::aim(eye, center(target));
        let old_point = praxion_headless::pick_block_looking(&world, &body, yaw, pitch)
            .map(|hit| hit.block_pos);
        assert_eq!(old_point, Some(wall), "the old aim point is hidden by the stone");

        let mut mine = fresh_mine("#logs", Some(target));
        let (dug, outcome) = first_dig(&mut mine, &world, &mut perception, &mut body, 40);
        assert_eq!(outcome, None);
        assert_eq!(dug, Some(target), "the log is dug through the point the crosshair reaches");
        assert!(mine.rejected.is_empty());
    }

    #[test]
    fn leaves_hiding_every_point_of_a_log_are_dug_through_first() {
        use crate::test_world::{set_block, swept};
        // Run 9: a canopy log behind leaves. No crosshair point lands on the
        // log; the pick toward its centre meets a leaf block, which is soft,
        // not the support, opens no drop and holds back no fluid: it is dug as
        // an occluder (dig-through used to run only when no target was found).
        let target = BlockPos::new(0, 64, 2);
        let mut world = floor_world();
        set_block(&mut world, target, BlockState::from(BlockKind::OakLog));
        for y in [64, 65] {
            set_block(&mut world, BlockPos::new(0, y, 1), BlockState::from(BlockKind::OakLeaves));
        }
        let mut perception = swept(&world);
        let mut body = standing_body(&world);
        let mut mine = fresh_mine("#logs", Some(target));
        let (dug, outcome) = first_dig(&mut mine, &world, &mut perception, &mut body, 40);
        assert_eq!(outcome, None);
        assert_eq!(dug, Some(BlockPos::new(0, 65, 1)), "the leaf block is dug first");
        let dig = mine.dig_through.as_ref().expect("a dig-through");
        assert_eq!(dig.occluders, 1);
        assert!(dig.digging_occluder);
        assert!(mine.rejected.is_empty(), "an occluder is not a rejection");
    }

    #[test]
    fn a_log_no_crosshair_point_reaches_is_rejected_and_another_is_mined() {
        use crate::test_world::{set_block, swept};
        // Stone hides every point of the nearer log; nothing soft to dig. The
        // call rejects it for the rest of the call and mines the other log
        // instead of retrying the same cell (run 9).
        let hidden = BlockPos::new(0, 64, 2);
        let other = BlockPos::new(2, 64, 0);
        let mut world = floor_world();
        set_block(&mut world, hidden, BlockState::from(BlockKind::OakLog));
        set_block(&mut world, other, BlockState::from(BlockKind::OakLog));
        for y in [64, 65] {
            set_block(&mut world, BlockPos::new(0, y, 1), BlockState::from(BlockKind::Stone));
        }
        let mut perception = swept(&world);
        let mut body = standing_body(&world);
        let mut mine = fresh_mine("#logs", Some(hidden));
        let (dug, outcome) = first_dig(&mut mine, &world, &mut perception, &mut body, 40);
        assert_eq!(outcome, None);
        assert_eq!(dug, Some(other), "the other log is mined");
        assert_eq!(mine.rejected, vec![hidden]);
    }

    #[test]
    fn with_every_candidate_rejected_the_call_fails_typed_at_once() {
        use crate::test_world::{set_block, swept};
        for (target, walls, kind) in [
            // Level with the head: nothing to climb to, the view is blocked.
            (
                BlockPos::new(0, 64, 2),
                vec![BlockPos::new(0, 64, 1), BlockPos::new(0, 65, 1)],
                "no_sight",
            ),
            // Overhead (feet + 3): climbing could change the angle.
            (BlockPos::new(0, 67, 2), vec![BlockPos::new(0, 66, 1)], "out_of_reach"),
        ] {
            let mut world = floor_world();
            set_block(&mut world, target, BlockState::from(BlockKind::OakLog));
            for wall in &walls {
                set_block(&mut world, *wall, BlockState::from(BlockKind::Stone));
            }
            let mut perception = swept(&world);
            let mut body = standing_body(&world);
            let mut mine = fresh_mine("#logs", Some(target));
            let (tick, outcome) =
                run_to_outcome(&mut mine, &world, &mut perception, &mut body, 60)
                    .unwrap_or_else(|| panic!("{kind}: it never failed"));
            assert_eq!(outcome, Outcome::Failed(kind.into()), "{target:?} behind {walls:?}");
            assert!(tick <= 3, "{kind}: no candidate is left, so no wait: tick {tick}");
            assert_eq!(mine.rejected, vec![target]);
        }
    }

    #[test]
    fn a_clear_view_still_digs() {
        use crate::test_world::{feet, set_block, swept, world};
        let target = BlockPos::new(0, 64, 2);
        let mut world = world();
        set_block(&mut world, target, BlockState::from(BlockKind::OakLog));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut mine = fresh_mine("#logs", Some(target));
        let pending = run_until_dig(&mut mine, false, &mut body, &world, &mut perception);
        assert_eq!(
            pending,
            Some(IntentSpec::Dig {
                pos: target,
                window: 160
            })
        );
    }
    #[test]
    fn a_new_target_does_not_inherit_the_settled_receipt_of_the_block_before() {
        // Run B, 2026-11-12: after the first soft block of a dig-through broke,
        // the settled flag stayed set, so every later target counted as
        // "settled" the moment the runner registered it, was dug again under a
        // new receipt and re-registered, nine times on one log.
        let cell = BlockPos::new(0, 64, 2);
        let mut mine = state_machine_mine(
            cell,
            Some(DigState {
                occluders: 1,
                digging_occluder: true,
                target_started: true,
            }),
        );
        mine.begin_pickup();
        assert!(mine.settled, "the occluder's receipt settled");
        mine.forget_target();
        assert!(!mine.settled, "the broken occluder's receipt is spent");
        mine.begin_pickup();
        mine.set_target(BlockPos::new(0, 64, 3), false);
        assert!(!mine.settled, "a selected target has no receipt yet");
        assert!(mine.pickup.is_some(), "the marker keeps the runner's primitive alive");
        assert_eq!(
            target_check(false, mine.settled, true, false, true),
            TargetCheck::Keep,
            "a registered target is dug on, not registered again"
        );
    }

    #[test]
    fn a_hidden_log_above_reach_is_not_tunnelled_toward() {
        use crate::test_world::{set_block, swept};
        // Run B, 2026-11-12: the body at the bottom of its own dig dug the wall
        // blocks over its head toward a log six blocks up, then clicked a log it
        // could never hit, restarting for 1,400 ticks. A known log that no step
        // brings within reach fails `out_of_reach` at once and digs nothing.
        let log = BlockPos::new(0, 70, 0);
        let mut world = floor_world();
        set_block(&mut world, log, BlockState::from(BlockKind::OakLog));
        let mut perception = swept(&world);
        for y in [68, 69] {
            set_block(&mut world, BlockPos::new(0, y, 0), BlockState::from(BlockKind::Dirt));
        }
        let mut body = standing_body(&world);
        let mut mine = fresh_mine("#logs", None);
        let mut specs = Vec::new();
        let mut result = None;
        for tick in 0..60 {
            let (pending, outcome) =
                run_mine_tick(&mut mine, tick, false, &mut body, &world, &mut perception);
            specs.extend(pending);
            if let Some(outcome) = outcome {
                result = Some((tick, outcome));
                break;
            }
        }
        let (tick, outcome) = result.expect("it fails");
        assert_eq!(outcome, Outcome::Failed("out_of_reach".into()));
        assert!(tick <= 3, "at once, not after a wait or a tunnel: tick {tick}");
        assert!(specs.is_empty(), "no block was dug on the way: {specs:?}");
    }

    #[test]
    fn a_hidden_log_within_reach_is_still_dug_through() {
        use crate::test_world::{set_block, swept};
        // The overhead rule must not switch off the dig-through for a log a
        // person can reach: two leaf blocks on the way, the log at head height.
        let log = BlockPos::new(0, 65, 3);
        let mut world = floor_world();
        set_block(&mut world, log, BlockState::from(BlockKind::OakLog));
        let mut perception = swept(&world);
        for z in [1, 2] {
            for y in [64, 65] {
                set_block(&mut world, BlockPos::new(0, y, z), BlockState::from(BlockKind::Dirt));
            }
        }
        let mut body = standing_body(&world);
        let mut mine = fresh_mine("#logs", None);
        let (dug, outcome) = first_dig(&mut mine, &world, &mut perception, &mut body, 40);
        assert_eq!(outcome, None);
        assert!(dug.is_some(), "a leaf block on the way is dug");
    }

    #[test]
    fn only_a_certain_single_drop_is_watched() {
        let state = |kind| BlockState::from(kind);
        assert_eq!(sure_drop(state(BlockKind::CraftingTable)), Some("crafting_table"));
        assert_eq!(sure_drop(state(BlockKind::OakLog)), Some("oak_log"));
        assert_eq!(sure_drop(state(BlockKind::Stone)), Some("cobblestone"));
        assert_eq!(
            sure_drop(state(BlockKind::OakLeaves)),
            None,
            "leaves have no loot table here and drop by chance"
        );
        assert_eq!(sure_drop(state(BlockKind::Air)), None);
    }

    /// A mine whose break just settled on `cell`, walking to its drop.
    fn mine_collecting(cell: BlockPos, item: &'static str) -> Mine {
        let mut mine = fresh_mine("crafting_table", Some(cell));
        mine.sure_drop = Some(item);
        mine.begin_pickup();
        mine
    }

    #[test]
    fn a_vanished_drop_is_not_a_collected_one_while_the_inventory_has_not_grown() {
        use crate::test_world::{add_item_entity, feet, give, remove_entity, swept, world};
        use azalea_registry::builtin::ItemKind;
        // Run B, 2026-11-12: the placed crafting table was mined "to take it
        // along" and the body never held it. The pickup phase counted a drop
        // that was seen and then missing for five ticks as collected, and
        // followed whichever item entity was nearest.
        let cell = BlockPos::new(2, 64, 0);
        let mut world = world();
        add_item_entity(&mut world, 30, ItemKind::CraftingTable, Vec3::new(2.5, 64.2, 0.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut mine = mine_collecting(cell, "crafting_table");
        for tick in 0..3 {
            let (_, outcome) = run_mine_tick(&mut mine, tick, true, &mut body, &world, &mut perception);
            assert_eq!(outcome, None);
        }
        assert_eq!(mine.pickup.as_ref().and_then(|p| p.target_id), Some(30));

        // The entity leaves the body's view (despawned, or hidden).
        remove_entity(&mut world, 30);
        for tick in 3..30 {
            let (_, outcome) = run_mine_tick(&mut mine, tick, true, &mut body, &world, &mut perception);
            assert_eq!(
                outcome, None,
                "tick {tick}: the inventory did not grow, so the walk is not over"
            );
        }
        // A second table drop shows up farther away: it is followed.
        add_item_entity(&mut world, 31, ItemKind::CraftingTable, Vec3::new(3.5, 64.2, 0.5));
        for tick in 30..34 {
            let (_, outcome) = run_mine_tick(&mut mine, tick, true, &mut body, &world, &mut perception);
            assert_eq!(outcome, None);
        }
        assert_eq!(mine.pickup.as_ref().and_then(|p| p.target_id), Some(31));

        // The server hands the table over: the phase ends at once.
        give(&mut world, 36, ItemKind::CraftingTable, 1);
        let (_, outcome) = run_mine_tick(&mut mine, 34, true, &mut body, &world, &mut perception);
        assert_eq!(outcome, Some(Outcome::Confirmed));
    }

    #[test]
    fn the_walk_follows_the_drop_of_the_wanted_kind_not_the_nearest_item() {
        use crate::test_world::{add_item_entity, feet, swept, world};
        use azalea_registry::builtin::ItemKind;
        let cell = BlockPos::new(3, 64, 0);
        let mut world = world();
        add_item_entity(&mut world, 40, ItemKind::Stick, Vec3::new(1.5, 64.2, 0.5));
        add_item_entity(&mut world, 41, ItemKind::CraftingTable, Vec3::new(4.5, 64.2, 0.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut mine = mine_collecting(cell, "crafting_table");
        let _ = run_mine_tick(&mut mine, 0, true, &mut body, &world, &mut perception);
        assert_eq!(
            mine.pickup.as_ref().and_then(|p| p.target_id),
            Some(41),
            "the stick is nearer, the table is what the break dropped"
        );
    }

    #[test]
    fn without_a_certain_drop_a_drop_that_is_gone_still_ends_the_walk() {
        use crate::test_world::{add_item_entity, feet, remove_entity, swept, world};
        use azalea_registry::builtin::ItemKind;
        let cell = BlockPos::new(2, 64, 0);
        let mut world = world();
        add_item_entity(&mut world, 30, ItemKind::Stick, Vec3::new(2.5, 64.2, 0.5));
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut mine = fresh_mine("oak_leaves", Some(cell));
        mine.begin_pickup();
        let _ = run_mine_tick(&mut mine, 0, true, &mut body, &world, &mut perception);
        remove_entity(&mut world, 30);
        let mut ended = None;
        for tick in 1..20 {
            let (_, outcome) = run_mine_tick(&mut mine, tick, true, &mut body, &world, &mut perception);
            if outcome.is_some() {
                ended = Some(tick);
                break;
            }
        }
        assert!(ended.is_some_and(|tick| tick <= 8), "a chance drop ends once it is gone: {ended:?}");
    }
    #[test]
    fn a_dig_through_registers_one_intent_per_block_it_breaks() {
        use crate::runner::Runner;
        use crate::test_world::{set_block, swept};
        use praxion_interpreter::StartAction;
        use praxion_rules::ArgValue;
        // Run B, 2026-11-12: from the second block of a dig-through on, every
        // target was registered again and again while it was being dug. Driven
        // the way production drives it (runner, registration, settled receipt),
        // each block of the way gets exactly one intent.
        let log = BlockPos::new(0, 65, 3);
        let mut world = floor_world();
        set_block(&mut world, log, BlockState::from(BlockKind::OakLog));
        let mut perception = swept(&world);
        for z in [1, 2] {
            set_block(&mut world, BlockPos::new(0, 65, z), BlockState::from(BlockKind::Dirt));
        }
        let mut body = standing_body(&world);
        let mut runner = Runner::new();
        runner
            .start(&StartAction {
                primitive: "mine".to_owned(),
                args: vec![ArgValue::Name("oak_log".to_owned())],
            })
            .expect("mine starts");
        let mut registered = Vec::new();
        let mut breaking: Option<(u64, BlockPos)> = None;
        let mut outcome = None;
        for tick in 0..400u64 {
            let result = {
                let mut ctx = Ctx {
                    tick,
                    body: &body,
                    world: &world,
                    perception: &mut perception,
                    style: praxion_planner::Personality::default(),
                };
                runner.tick(&mut ctx)
            };
            result.frame.apply(&mut body, tick).expect("the mine frame applies");
            body.step(&result.frame.held, &world);
            if let Some(IntentSpec::Dig { pos, .. }) = result.pending {
                registered.push(pos);
                runner.registered(registered.len() as u64);
                // The server takes ten ticks to break the block.
                breaking = Some((tick + 10, pos));
            }
            if let Some((due, pos)) = breaking
                && tick >= due
            {
                breaking = None;
                set_block(&mut world, pos, BlockState::from(BlockKind::Air));
                perception.note_changed(&world, pos);
                runner.settle(Outcome::Confirmed);
            }
            if result.outcome.is_some() {
                outcome = result.outcome;
                break;
            }
        }
        assert_eq!(outcome, Some(Outcome::Confirmed));
        assert_eq!(
            registered,
            vec![BlockPos::new(0, 65, 1), BlockPos::new(0, 65, 2), log],
            "one intent per block, in order"
        );
    }
}
