//! Perception-driven primitives: one state machine per primitive, one
//! `ControlFrame` per tick.
//!
//! A primitive reads the world through [`Perception`] (and the planner for
//! `move_to`), never through the raw store. It registers its intent with the
//! receipts engine at the first tick it acts; the driver does that after the
//! actuator reports the prediction sequence number or state id. The primitive
//! ends with the receipt's outcome, or with its own typed failure.

use std::collections::{HashMap, HashSet, VecDeque};

use azalea_core::direction::Direction;
use azalea_core::position::{BlockPos, Vec3};
use azalea_inventory::ItemStack;
use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_gamedata as gamedata;
use praxion_headless::{Body, ControlFrame, Held, Look, MenuAction, MenuMode};
use praxion_interpreter::{Outcome, StartAction};
use praxion_perception::{Answer, BlockFilter, BlockRegion, Caller, EntityFilter, Perception};
use praxion_planner::{
    Follower, Goal, Inventory as PlanInventory, LookStyle, PerceptionBlocks, Planner,
    PlanningState, center, look_towards,
};
use praxion_rules::{ArgValue, FactCall};
use praxion_world::{BlockDescription, BlockState, World};

/// One tick of the primitive's world.
pub struct Ctx<'a> {
    pub tick: u64,
    pub body: &'a Body,
    pub world: &'a World,
    pub perception: &'a mut Perception,
    pub look: LookStyle,
}

/// What the driver registers with the receipts engine after the frame is
/// applied. The actuator's report supplies the sequence number or state id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentSpec {
    Dig {
        pos: BlockPos,
        window: u64,
    },
    Place {
        pos: BlockPos,
        block: String,
        window: u64,
    },
    Inventory {
        primitive: String,
        item: String,
        delta: i64,
        window: u64,
    },
    /// `craft`: the output must reach the player's storage.
    StorageInventory {
        primitive: String,
        item: String,
        delta: i64,
        window: u64,
    },
    Movement {
        primitive: String,
        window: u64,
    },
    SlotClick {
        container_id: i32,
        slot: i16,
        window: u64,
    },
    Attack {
        entity_id: i32,
        window: u64,
    },
    Fact {
        fact: FactCall,
        window: u64,
    },
}

/// One tick's output.
#[derive(Clone, Debug)]
pub struct TickResult {
    pub frame: ControlFrame,
    /// Register this intent if the actuator reported the key it needs.
    pub pending: Option<IntentSpec>,
    pub outcome: Option<Outcome>,
}

/// The primitive runner. It owns at most one active primitive.
#[derive(Debug, Default)]
pub struct Runner {
    active: Option<Active>,
    receipt: Option<Outcome>,
    outcome: Option<Outcome>,
    intent_id: Option<u64>,
    started: bool,
}

#[derive(Debug)]
enum Active {
    Mine(Mine),
    Place(Place),
    Craft(Craft),
    Eat(Eat),
    Explore(Explore),
    Attack(Attack),
    Equip(Equip),
    Wait(Wait),
    Observe(Observe),
    Move(Move),
}

impl Runner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts one primitive. Returns a typed error for an unknown primitive or
    /// a malformed argument.
    pub fn start(&mut self, action: &StartAction) -> Result<(), String> {
        self.receipt = None;
        self.outcome = None;
        self.intent_id = None;
        self.started = false;
        let arg = action
            .args
            .first()
            .and_then(ArgValue::name)
            .unwrap_or_default()
            .to_string();
        let active = match action.primitive.as_str() {
            "mine" => Active::Mine(Mine {
                name: arg,
                target: None,
                waited: 0,
                pickup: None,
            }),
            "place" => Active::Place(Place {
                block: arg,
                target: None,
                sent: false,
                waited: 0,
            }),
            "craft" => Active::Craft(Craft::new(arg)?),
            "eat" => Active::Eat(Eat {
                item: arg,
                sent: false,
                waited: 0,
            }),
            "explore" => Active::Explore(Explore::new(arg)?),
            "attack" => Active::Attack(Attack {
                kind: arg,
                entity: None,
                target: None,
                waited: 0,
            }),
            "equip" => Active::Equip(Equip {
                item: arg,
                stage: EquipStage::Select,
                waited: 0,
            }),
            "wait" => {
                let ticks = action
                    .args
                    .first()
                    .and_then(ArgValue::as_int)
                    .unwrap_or(1)
                    .max(1) as u32;
                Active::Wait(Wait {
                    remaining: ticks,
                    total: ticks,
                })
            }
            "observe" => {
                let fact = match action.args.first() {
                    Some(ArgValue::Fact(call)) => call.clone(),
                    _ => return Err("observe needs a fact call".to_owned()),
                };
                Active::Observe(Observe {
                    fact,
                    target: None,
                    waited: 0,
                })
            }
            "move_to" => Active::Move(Move::new(arg)),
            other => return Err(format!("unknown primitive `{other}`")),
        };
        self.active = Some(active);
        Ok(())
    }

    /// The receipt settled. A confirmed discrete primitive finishes; `attack`
    /// keeps going until its target is gone, a `craft` first closes its open
    /// crafting table, and `move_to` treats "unrefuted" as "not corrected",
    /// not as "arrived": the follower's own goal check ends it.
    pub fn settle(&mut self, outcome: Outcome) {
        self.receipt = Some(outcome.clone());
        match &mut self.active {
            Some(Active::Craft(craft)) => {
                craft.request_close(outcome);
                return;
            }
            Some(Active::Mine(mine)) if outcome == Outcome::Confirmed => {
                // The break is confirmed; the drop is still on the ground.
                mine.begin_pickup();
                return;
            }
            Some(Active::Move(_)) if outcome == Outcome::Confirmed => return,
            Some(Active::Explore(_)) if outcome == Outcome::Confirmed => return,
            _ => {}
        }
        let keep_going = matches!(self.active, Some(Active::Attack(_)))
            && outcome == Outcome::Confirmed;
        if !keep_going {
            self.finish(outcome);
        }
    }

    /// The driver registered the intent for the pending action.
    pub fn registered(&mut self, intent_id: u64) {
        self.intent_id = Some(intent_id);
        self.started = true;
    }

    pub fn intent_id(&self) -> Option<u64> {
        self.intent_id
    }

    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some() && self.outcome.is_none()
    }

    /// Cancel the active primitive as `Preempted` (the interpreter's flight
    /// deadline fired). Without this the primitive would keep running while
    /// the interpreter retries, and the retry would be dropped as "busy".
    pub fn cancel(&mut self) -> Option<Outcome> {
        if !self.is_active() {
            return None;
        }
        self.finish(Outcome::Preempted);
        // A later run must never inherit this run's receipt.
        self.intent_id = None;
        self.started = false;
        self.outcome.clone()
    }

    pub fn tick(&mut self, ctx: &mut Ctx<'_>) -> TickResult {
        if let Some(outcome) = &self.outcome {
            return TickResult {
                frame: idle(ctx),
                pending: None,
                outcome: Some(outcome.clone()),
            };
        }
        // A failed or timed-out receipt ends the primitive. A confirmed one
        // ends every discrete primitive except `attack`, which keeps going
        // until its target is gone.
        if let Some(receipt) = self.receipt.clone() {
            let attack = matches!(self.active, Some(Active::Attack(_)));
            let craft_closing = matches!(
                &self.active,
                Some(Active::Craft(craft)) if craft.close_outcome.is_some()
            );
            let move_unrefuted = matches!(self.active, Some(Active::Move(_)))
                && receipt == Outcome::Confirmed;
            let explore_unrefuted = matches!(self.active, Some(Active::Explore(_)))
                && receipt == Outcome::Confirmed;
            let mine_pickup = matches!(
                &self.active,
                Some(Active::Mine(mine)) if mine.pickup.is_some()
            ) && receipt == Outcome::Confirmed;
            if receipt != Outcome::Confirmed
                || (!attack && !craft_closing && !move_unrefuted && !explore_unrefuted && !mine_pickup)
            {
                self.finish(receipt.clone());
                return TickResult {
                    frame: idle(ctx),
                    pending: None,
                    outcome: Some(receipt),
                };
            }
        }
        let Some(active) = &mut self.active else {
            return TickResult {
                frame: idle(ctx),
                pending: None,
                outcome: self.outcome.clone(),
            };
        };
        let (frame, pending, outcome) = match active {
            Active::Mine(state) => state.tick(ctx, self.started),
            Active::Place(state) => state.tick(ctx, self.started),
            Active::Craft(state) => state.tick(ctx, self.started),
            Active::Eat(state) => state.tick(ctx, self.started),
            Active::Explore(state) => state.tick(ctx, self.started),
            Active::Attack(state) => state.tick(ctx, self.started),
            Active::Equip(state) => state.tick(ctx),
            Active::Wait(state) => state.tick(ctx),
            Active::Observe(state) => state.tick(ctx, self.started),
            Active::Move(state) => state.tick(ctx, self.started),
        };
        if let Some(outcome) = &outcome {
            self.finish(outcome.clone());
        }
        TickResult {
            frame,
            pending,
            outcome,
        }
    }

    fn finish(&mut self, outcome: Outcome) {
        self.outcome = Some(outcome);
        self.active = None;
    }
}

fn idle(ctx: &Ctx<'_>) -> ControlFrame {
    ControlFrame::new(ctx.tick, ctx.body.generation(), Held::NONE)
}

fn aim(ctx: &Ctx<'_>, target: Vec3) -> (Look, bool) {
    let eye = ctx.body.eye_position();
    look_towards(ctx.look, ctx.body.look(), eye, target)
}

fn item_name(stack: &ItemStack) -> &str {
    stack.kind().to_str()
}

fn normalize(name: &str) -> &str {
    name.strip_prefix("minecraft:").unwrap_or(name)
}

fn item_matches(stack: &ItemStack, wanted: &str) -> bool {
    if let Some(tag) = wanted.strip_prefix('#') {
        gamedata::item_in_tag(item_name(stack), tag)
    } else {
        normalize(item_name(stack)) == normalize(wanted)
    }
}

fn count_item(world: &World, wanted: &str) -> u32 {
    world
        .inventory()
        .iter()
        .filter(|stack| item_matches(stack, wanted))
        .map(|stack| stack.count().max(0) as u32)
        .sum()
}

fn find_item(world: &World, wanted: &str) -> Option<(usize, ItemStack)> {
    world
        .inventory()
        .iter()
        .enumerate()
        .find(|(_, stack)| item_matches(stack, wanted) && stack.count() > 0)
        .map(|(index, stack)| (index, stack.clone()))
}

/// The hotbar index (0 to 8) holding `wanted`, if any.
fn hotbar_index(world: &World, wanted: &str) -> Option<u8> {
    (36..=44).find(|index| {
        world
            .inventory()
            .get(*index)
            .is_some_and(|stack| item_matches(stack, wanted) && stack.count() > 0)
    })
    .map(|index| (index - 36) as u8)
}

/// A frame that selects `wanted`: the hotbar key when it is already in the
/// hotbar, or a swap click from the main inventory.
fn select_item(world: &World, wanted: &str, frame: &mut ControlFrame) -> bool {
    if let Some(slot) = hotbar_index(world, wanted) {
        frame.select_slot = Some(slot);
        return true;
    }
    let Some((index, _)) = find_item(world, wanted) else {
        return false;
    };
    if !(9..=35).contains(&index) {
        return false;
    }
    frame.menu = Some(MenuAction {
        window: 0,
        slot: index as i16,
        button: 0,
    });
    frame.menu_mode = MenuMode::Swap;
    frame.select_slot = Some(0);
    true
}

fn held_matches(world: &World, wanted: &str) -> bool {
    let slot = world.selected_hotbar_slot().unwrap_or(0) as usize;
    world
        .inventory()
        .get(36 + slot)
        .is_some_and(|stack| item_matches(stack, wanted))
}

/// The nearest visible block of `wanted` within `radius`.
fn block_target(ctx: &mut Ctx<'_>, wanted: &str, radius: u32) -> Option<BlockPos> {
    if let Some(tag) = wanted.strip_prefix('#') {
        let eye = ctx.perception.eye_cell(ctx.world)?;
        let region = BlockRegion::around(eye, radius);
        let answer = ctx
            .perception
            .blocks_in(Caller::Other, ctx.world, region, BlockFilter::Any);
        let blocks = answer.value()?;
        return blocks
            .iter()
            .filter(|fact| gamedata::block_in_tag(fact.kind.to_str(), tag))
            .min_by(|a, b| a.distance.total_cmp(&b.distance))
            .map(|fact| fact.pos);
    }
    let kind = wanted.parse::<BlockKind>().ok()?;
    ctx.perception
        .nearest_block(Caller::Other, ctx.world, kind, radius)
        .value()
        .map(|fact| fact.pos)
}

fn entity_target(ctx: &mut Ctx<'_>, wanted: &str, radius: u32) -> Option<(i32, Vec3)> {
    let kind = wanted.parse::<EntityKind>().ok()?;
    ctx.perception
        .nearest_entity(Caller::Other, ctx.world, kind, radius)
        .value()
        .map(|fact| (fact.id, fact.position))
}

// ----------------------------------------------------------------------
// mine
// ----------------------------------------------------------------------

#[derive(Debug)]
struct Mine {
    name: String,
    target: Option<BlockPos>,
    waited: u32,
    /// After the break is confirmed the runner walks to the drop before it
    /// settles, so the item is picked up. The mine receipt itself stays the
    /// block-break receipt; no inventory receipt is invented here.
    pickup: Option<Pickup>,
}

impl Mine {
    fn begin_pickup(&mut self) {
        if self.pickup.is_none() {
            self.pickup = Some(Pickup::default());
        }
    }

    fn tick(
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
                    let answer = ctx.perception.nearest_block(
                        Caller::Other,
                        ctx.world,
                        kind,
                        5,
                    );
                    eprintln!("mine: nearest {:?} -> {:?}", self.name, answer);
                }
                self.waited += 1;
                if self.waited > 40 {
                    return (frame, None, Some(Outcome::Failed("no_block".into())));
                }
                return (frame, None, None);
            }
        }
        let target = self.target.expect("checked above");
        let (look, aligned) = aim(ctx, center(target));
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
struct Pickup {
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
    let (look, aligned) = look_towards(ctx.look, ctx.body.look(), eye, aim_point);
    frame.look = look;
    if aligned {
        frame.held.forward = true;
        if target.y > ctx.body.position().y + 0.5 && ctx.body.on_ground() {
            frame.held.jump = true;
        }
    }
    frame
}

// ----------------------------------------------------------------------
// place
// ----------------------------------------------------------------------

#[derive(Debug)]
struct Place {
    block: String,
    target: Option<(BlockPos, Direction)>,
    sent: bool,
    waited: u32,
}

impl Place {
    fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        // One use_item_on per placement. After it is sent the stack may be
        // empty (a one-item stack) while the receipt is still in flight; a
        // second Use could place a second block against another face.
        if self.sent {
            self.waited += 1;
            if self.waited > 40 {
                return (frame, None, Some(Outcome::Failed("no_place".into())));
            }
            return (frame, None, None);
        }
        if count_item(ctx.world, &self.block) == 0 {
            return (frame, None, Some(Outcome::Failed("no_item".into())));
        }
        if !held_matches(ctx.world, &self.block) {
            if !select_item(ctx.world, &self.block, &mut frame) {
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            }
            return (frame, None, None);
        }
        if self.target.is_none() {
            self.target = place_target(ctx, &self.block);
            if self.target.is_none() {
                self.waited += 1;
                if self.waited > 40 {
                    return (frame, None, Some(Outcome::Failed("no_place".into())));
                }
                return (frame, None, None);
            }
        }
        let (solid, direction) = self.target.expect("checked above");
        let air = solid.offset_with_direction(direction);
        // The body can move onto the target cell between planning and the
        // click (a step up makes the old air cell the head cell). The server
        // refuses to place into the player; re-plan instead of clicking.
        let feet = ctx.body.position().to_block_pos_floor();
        if air == feet || air == feet.up(1) {
            self.target = None;
            return (frame, None, None);
        }
        let face = center(solid)
            + Vec3::new(
                f64::from(direction.x()),
                f64::from(direction.y()),
                f64::from(direction.z()),
            ) * 0.5;
        let (look, aligned) = aim(ctx, face);
        frame.look = look;
        if !aligned {
            return (frame, None, None);
        }
        frame.clicks.push(praxion_headless::Click::Use);
        self.sent = true;
        let pending = if started {
            None
        } else {
            Some(IntentSpec::Place {
                pos: air,
                block: self.block.clone(),
                window: 40,
            })
        };
        (frame, pending, None)
    }
}

/// Whether a block can be the support of a placed block. A replaceable block
/// is not: the server replaces the clicked cell itself, so the place's
/// adjacent-cell target would miss the change.
fn place_support(state: BlockState) -> bool {
    !praxion_planner::blocks::is_replaceable(state)
}

fn place_target(ctx: &mut Ctx<'_>, wanted: &str) -> Option<(BlockPos, Direction)> {
    let eye = ctx.body.eye_position();
    let eye_cell = eye.to_block_pos_floor();
    let feet = ctx.body.position().to_block_pos_floor();
    let region = BlockRegion::around(eye_cell, 4);
    let answer = ctx
        .perception
        .blocks_in(Caller::Other, ctx.world, region, BlockFilter::Any);
    let blocks = answer.value()?;
    let mut best: Option<(f64, BlockPos, Direction)> = None;
    for fact in blocks {
        // Never click a replaceable block (air, grass, snow, a flower): the
        // server replaces the clicked cell instead of placing against its
        // face, so the receipt's adjacent-cell target would miss the change.
        if !place_support(fact.state) {
            continue;
        }
        let delta = eye - center(fact.pos);
        let direction = Direction::nearest(delta);
        let air = fact.pos.offset_with_direction(direction);
        // Never place into the body's own feet or head cell.
        if air == feet || air == feet.up(1) {
            continue;
        }
        let Some(state) = ctx
            .perception
            .block(Caller::Other, ctx.world, air)
            .value()
            .map(|fact| fact.state)
        else {
            continue;
        };
        if !state.is_air() {
            continue;
        }
        let distance = eye.distance_to(center(air));
        if best.as_ref().is_none_or(|(best, _, _)| distance < *best) {
            best = Some((distance, fact.pos, direction));
        }
    }
    let _ = wanted;
    best.map(|(_, pos, direction)| (pos, direction))
}

// ----------------------------------------------------------------------
// near fact and explore
// ----------------------------------------------------------------------

/// How far `explore` looks for its target before it reports `not_found`. The
/// gathering skills use the same radius in their `not near(T, R)` guard.
pub const EXPLORE_NEAR_RADIUS: u32 = praxion_rules::registry::EXPLORE_NEAR_RADIUS;

/// How far `explore` looks for its target before it falls back to a frontier
/// of the known map.
pub const EXPLORE_SEARCH_RADIUS: u32 = praxion_rules::registry::EXPLORE_SEARCH_RADIUS;

/// The frontier scan radius. A plan is the only time this costs anything, so
/// it uses the same 32 blocks the sweep fills.
const EXPLORE_FRONTIER_RADIUS: u32 = 32;

/// The planner goal radius when `explore` walks to a known target cell. The
/// call itself ends at [`EXPLORE_NEAR_RADIUS`]. Four blocks leaves room for a
/// cave wall's exposed face while staying inside the five-block mine reach.
const TARGET_APPROACH_RADIUS: f32 = 4.0;

/// Ticks one explore path step may take before the follower is considered
/// stuck and the frontier is rejected. Shorter than the default 200 so a
/// 1200-tick search can try several directions.
const EXPLORE_STEP_TICKS: u32 = 100;

/// The `near` fact over the perception known map: a target is near when a cell
/// this perception has seen inside `radius` holds it. No rays are spent, and
/// the answer is `false` when the map knows of none, so `not near(T, R)` can
/// start an exploration. `None` only when the eye itself is not known yet.
pub fn near_block(
    perception: &Perception,
    world: &World,
    wanted: &str,
    radius: u32,
) -> Option<bool> {
    perception.eye_cell(world)?;
    Some(nearest_known_block(perception, world, wanted, radius, &[]).is_some())
}

/// The nearest cell in the perception known map that holds `wanted`, with its
/// distance from the eye. `None` when nothing is known inside `radius`.
/// `exclude` skips cells a failed plan already rejected.
pub fn nearest_known_block(
    perception: &Perception,
    world: &World,
    wanted: &str,
    radius: u32,
    exclude: &[BlockPos],
) -> Option<(BlockPos, f64)> {
    let eye = perception.eye(world)?;
    let eye_cell = eye.to_block_pos_floor();
    let r = i32::try_from(radius).unwrap_or(i32::MAX);
    let r2 = i64::from(r) * i64::from(r);
    let tag = wanted.strip_prefix('#');
    if tag.is_none() && wanted.parse::<BlockKind>().is_err() {
        return None;
    }
    let mut best: Option<(i64, BlockPos)> = None;
    for dy in -r..=r {
        for dz in -r..=r {
            for dx in -r..=r {
                let distance_sq = i64::from(dx) * i64::from(dx)
                    + i64::from(dy) * i64::from(dy)
                    + i64::from(dz) * i64::from(dz);
                if distance_sq > r2 {
                    continue;
                }
                if best.is_some_and(|(best, _)| distance_sq >= best) {
                    continue;
                }
                let pos = BlockPos::new(
                    eye_cell.x.saturating_add(dx),
                    eye_cell.y.saturating_add(dy),
                    eye_cell.z.saturating_add(dz),
                );
                if exclude.contains(&pos) {
                    continue;
                }
                let Some(remembered) = perception.known(pos) else {
                    continue;
                };
                let name = BlockKind::from(remembered.state).to_str();
                let matches = match tag {
                    Some(tag) => gamedata::block_in_tag(name, tag),
                    None => normalize(name) == normalize(wanted),
                };
                if matches {
                    best = Some((distance_sq, pos));
                }
            }
        }
    }
    best.map(|(_, pos)| (pos, eye.distance_to(center(pos))))
}

/// A known walkable cell next to unknown map cells: where exploring can make
/// the map bigger. `known` is the known map (`None` is unknown).
///
/// The cell must be seen and standable (passable land on a solid floor, or
/// water), so a cell behind an unseen wall is unknown and never chosen. Cells
/// already explored or rejected by a failed plan are skipped, a candidate near
/// an explored one scores worse so a search moves on, and a frontier on the
/// player's level beats one deep in a cave.
pub fn choose_frontier(
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
) -> Option<BlockPos> {
    // Eight blocks: enough that a second pass over the same edge is avoided,
    // small enough that a genuinely new direction still wins.
    const AVOID_RADIUS_SQ: i64 = 8 * 8;
    let horizontal_sq_limit = i64::from(horizontal) * i64::from(horizontal);
    let in_box = |pos: BlockPos| {
        let dx = i64::from(pos.x) - i64::from(start.x);
        let dy = i64::from(pos.y) - i64::from(start.y);
        let dz = i64::from(pos.z) - i64::from(start.z);
        dx.abs() <= i64::from(horizontal)
            && dy.abs() <= i64::from(vertical)
            && dz.abs() <= i64::from(horizontal)
            && dx * dx + dz * dz <= horizontal_sq_limit
    };

    // Only frontier cells reachable through known walkable cells count. An
    // unseen wall is an unknown cell, so it stops this flood and a cell behind
    // it is never chosen.
    let mut reachable: HashSet<BlockPos> = HashSet::new();
    let mut queue: VecDeque<BlockPos> = VecDeque::new();
    reachable.insert(start);
    queue.push_back(start);
    while let Some(cell) = queue.pop_front() {
        for neighbor in walk_neighbors(cell) {
            if !in_box(neighbor) || reachable.contains(&neighbor) {
                continue;
            }
            let Some(state) = known(neighbor) else {
                continue;
            };
            if !standable(neighbor, state, &known) {
                continue;
            }
            reachable.insert(neighbor);
            queue.push_back(neighbor);
        }
    }

    let mut best: Option<(bool, bool, i64, i32, i32, i32)> = None;
    for dy in -vertical..=vertical {
        for dz in -horizontal..=horizontal {
            for dx in -horizontal..=horizontal {
                let horizontal_sq = i64::from(dx) * i64::from(dx) + i64::from(dz) * i64::from(dz);
                if horizontal_sq > horizontal_sq_limit {
                    continue;
                }
                let pos = BlockPos::new(
                    start.x.saturating_add(dx),
                    start.y.saturating_add(dy),
                    start.z.saturating_add(dz),
                );
                if explored.contains(&pos) || rejected.contains(&pos) || !reachable.contains(&pos) {
                    continue;
                }
                let Some(state) = known(pos) else {
                    continue;
                };
                if !standable(pos, state, &known) {
                    continue;
                }
                if !touches_unknown(pos, &known) {
                    continue;
                }
                let avoid = explored.iter().any(|explored| {
                    let ax = i64::from(explored.x) - i64::from(pos.x);
                    let az = i64::from(explored.z) - i64::from(pos.z);
                    ax * ax + az * az <= AVOID_RADIUS_SQ
                });
                // Stay on the player's level: a frontier more than two blocks
                // below (a cave) or four above scores worse than any level
                // one, so exploring does not dive into a ravine by accident.
                let off_level = !(-2..=4).contains(&dy);
                let distance_sq = i64::from(dx) * i64::from(dx)
                    + i64::from(dy) * i64::from(dy)
                    + i64::from(dz) * i64::from(dz);
                let candidate = (avoid, off_level, distance_sq, pos.x, pos.z, pos.y);
                if best.is_none_or(|best| candidate < best) {
                    best = Some(candidate);
                }
            }
        }
    }
    best.map(|(_, _, _, x, z, y)| BlockPos::new(x, y, z))
}

fn standable(pos: BlockPos, state: BlockState, known: &impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    if praxion_planner::blocks::is_water(state) {
        return true;
    }
    if !praxion_planner::blocks::is_passable(state) {
        return false;
    }
    known(BlockPos::new(pos.x, pos.y.saturating_sub(1), pos.z))
        .is_some_and(praxion_planner::blocks::is_solid)
}

fn touches_unknown(pos: BlockPos, known: &impl Fn(BlockPos) -> Option<BlockState>) -> bool {
    [
        BlockPos::new(pos.x.saturating_add(1), pos.y, pos.z),
        BlockPos::new(pos.x.saturating_sub(1), pos.y, pos.z),
        BlockPos::new(pos.x, pos.y, pos.z.saturating_add(1)),
        BlockPos::new(pos.x, pos.y, pos.z.saturating_sub(1)),
        BlockPos::new(pos.x, pos.y.saturating_add(1), pos.z),
        BlockPos::new(pos.x, pos.y.saturating_sub(1), pos.z),
    ]
    .into_iter()
    .any(|neighbor| known(neighbor).is_none())
}

/// The cells a walking player can reach from `pos` in one move, for the
/// frontier reachability flood: eight directions, one block up, three down.
fn walk_neighbors(pos: BlockPos) -> Vec<BlockPos> {
    let mut neighbors = Vec::with_capacity(40);
    for dx in -1..=1 {
        for dz in -1..=1 {
            if dx == 0 && dz == 0 {
                continue;
            }
            for dy in -3..=1 {
                neighbors.push(BlockPos::new(
                    pos.x.saturating_add(dx),
                    pos.y.saturating_add(dy),
                    pos.z.saturating_add(dz),
                ));
            }
        }
    }
    neighbors
}

#[derive(Debug)]
struct Explore {
    target: String,
    planner: Planner,
    follower: Option<Follower>,
    goal: Option<BlockPos>,
    /// Frontier cells this call has reached; new choices stay away from them.
    explored: Vec<BlockPos>,
    /// Frontier cells the planner could not reach.
    rejected: Vec<BlockPos>,
    /// Cells the follower stalled in; plans treat them as blocked so the next
    /// route does not repeat the same obstacle.
    blocked: Vec<BlockPos>,
    waited: u32,
    budget: u32,
    path_ticks: u64,
}

impl Explore {
    fn new(target: String) -> Result<Self, String> {
        if let Some(tag) = target.strip_prefix('#') {
            if gamedata::block_tag(tag).is_none() {
                return Err(format!("unknown block tag `#{tag}`"));
            }
        } else if target.parse::<BlockKind>().is_err() {
            return Err(format!("unknown block `{target}`"));
        }
        Ok(Self {
            target,
            planner: {
                // Explore walks; it never digs or places. A frontier that
                // needs a world change is rejected and another one is chosen,
                // so the follower never stalls on a dig/place step it cannot
                // execute (the layer above owns those primitives).
                let mut planner = Planner::new();
                planner.set_allow_world_changes(false);
                planner
            },
            follower: None,
            goal: None,
            explored: Vec::new(),
            rejected: Vec::new(),
            blocked: Vec::new(),
            waited: 0,
            budget: praxion_rules::registry::EXPLORE_MAX_TICKS,
            path_ticks: 0,
        })
    }

    fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let frame = idle(ctx);
        self.waited += 1;
        let start = ctx.body.position().to_block_pos_floor();
        // The completion check is cheap: a 5-block scan. The wider 32-block
        // target search runs only when a new plan is needed, never while a
        // follower is walking, so exploring keeps the 20 TPS tick rate.
        if near_block(ctx.perception, ctx.world, &self.target, EXPLORE_NEAR_RADIUS) == Some(true) {
            return (frame, None, Some(Outcome::Confirmed));
        }
        if self.waited > self.budget {
            return (frame, None, Some(Outcome::Failed("not_found".into())));
        }

        if let Some(follower) = &mut self.follower {
            if follower.is_done() {
                self.follower = None;
                if let Some(goal) = self.goal.take() {
                    self.explored.push(goal);
                }
            } else if follower.is_stalled() {
                let progress = follower.progress(ctx.body);
                eprintln!(
                    "explore stalled step={}/{} pos={:?} deviation={:.3} ticks={}",
                    progress.step,
                    progress.total,
                    ctx.body.position(),
                    progress.deviation,
                    progress.ticks_on_step
                );
                self.follower = None;
                if let Some(goal) = self.goal.take() {
                    self.rejected.push(goal);
                }
                let stuck = ctx.body.position().to_block_pos_floor();
                if !self.blocked.contains(&stuck) {
                    self.blocked.push(stuck);
                }
            } else {
                let built = follower.step(ctx.tick, ctx.body.generation(), ctx.body);
                let pending = if started {
                    None
                } else {
                    Some(IntentSpec::Movement {
                        primitive: "explore".to_owned(),
                        window: self.path_ticks + 40,
                    })
                };
                return (built, pending, None);
            }
        }

        // A target the known map already holds is approached directly; the
        // search radius is the same 32 blocks the skills use.
        let target = nearest_known_block(
            ctx.perception,
            ctx.world,
            &self.target,
            EXPLORE_SEARCH_RADIUS,
            &self.rejected,
        );
        // The target is in sight but not yet in reach: walk to it.
        if let Some((cell, _)) = target
            && !self.rejected.contains(&cell)
        {
            let plan = self.plan_to(ctx, start, Goal::Near {
                pos: cell,
                radius: TARGET_APPROACH_RADIUS,
            });
            match plan.status {
                praxion_planner::PlanStatus::Success => {
                    self.path_ticks = plan.cost.ceil().max(0.) as u64;
                    eprintln!(
                        "explore approach target={:?} cell={:?} cost={} steps={}",
                        self.target,
                        cell,
                        plan.cost,
                        plan.steps.len()
                    );
                    self.goal = Some(cell);
                    self.follower = Some(Follower::new(plan.steps, ctx.look).with_max_ticks_per_step(EXPLORE_STEP_TICKS));
                    return (frame, None, None);
                }
                _ => {
                    self.rejected.push(cell);
                }
            }
        }

        // No target in sight: a few plan attempts per tick, so an unreachable
        // frontier is rejected and the next candidate is tried. A closed room
        // simply has none and waits for its budget.
        let radius = (ctx.perception.config().sweep_radius.min(EXPLORE_FRONTIER_RADIUS))
            .max(8) as i32;
        for _ in 0..8 {
            let goal = {
                let known = |pos: BlockPos| ctx.perception.known(pos).map(|cell| cell.state);
                choose_frontier(
                    start,
                    radius,
                    radius,
                    &self.explored,
                    &self.rejected,
                    known,
                )
            };
            let Some(goal) = goal else {
                break;
            };
            let plan = self.plan_to(ctx, start, Goal::Block(goal));
            match plan.status {
                praxion_planner::PlanStatus::Success => {
                    self.path_ticks = plan.cost.ceil().max(0.) as u64;
                    eprintln!(
                        "explore plan target={:?} frontier={:?} cost={} steps={}",
                        self.target,
                        goal,
                        plan.cost,
                        plan.steps.len()
                    );
                    self.goal = Some(goal);
                    self.follower = Some(Follower::new(plan.steps, ctx.look).with_max_ticks_per_step(EXPLORE_STEP_TICKS));
                    return (frame, None, None);
                }
                _ => {
                    self.rejected.push(goal);
                }
            }
        }
        (frame, None, None)
    }

    fn plan_to(&self, ctx: &mut Ctx<'_>, start: BlockPos, goal: Goal) -> praxion_planner::Plan {
        let inventory = plan_inventory(ctx.world);
        let mut state = PlanningState::new(inventory);
        state.blacklist.extend(self.blocked.iter().copied());
        let mut source = PerceptionBlocks {
            perception: ctx.perception,
            world: ctx.world,
        };
        self.planner.plan(&mut source, &state, start, goal)
    }
}

// ----------------------------------------------------------------------
// craft
// ----------------------------------------------------------------------

#[derive(Debug)]
struct Craft {
    output: String,
    recipe: Option<&'static gamedata::Recipe>,
    stage: CraftStage,
    clicks: VecDeque<MenuAction>,
    wait: u32,
    table: Option<BlockPos>,
    table_used: bool,
    close_sent: bool,
    /// The receipt settled while the table is still open. The craft sends its
    /// close next tick and only then finishes, so the menu does not outlive the
    /// primitive.
    close_outcome: Option<Outcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CraftStage {
    Open,
    Clicks,
    WaitResult,
    Take,
}

impl Craft {
    fn new(output: String) -> Result<Self, String> {
        Ok(Self {
            output,
            recipe: None,
            stage: CraftStage::Open,
            clicks: VecDeque::new(),
            wait: 0,
            table: None,
            table_used: false,
            close_sent: false,
            close_outcome: None,
        })
    }

    /// The receipt settled: close the table (if one is open), then finish.
    fn request_close(&mut self, outcome: Outcome) {
        self.close_outcome = Some(outcome);
    }

    fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        if let Some(outcome) = self.close_outcome.clone() {
            let needs_table = self.recipe.is_some_and(|recipe| recipe.needs_table);
            if needs_table && !self.close_sent {
                frame.menu = Some(MenuAction {
                    window: menu_window(ctx.world),
                    slot: 0,
                    button: 0,
                });
                frame.menu_mode = MenuMode::Close;
                self.close_sent = true;
            }
            return (frame, None, Some(outcome));
        }
        self.wait += 1;
        if self.recipe.is_none() {
            self.recipe = resolve_recipe(ctx.world, &self.output);
            if self.recipe.is_none() {
                return (
                    frame,
                    None,
                    Some(Outcome::Failed("no_recipe".into())),
                );
            }
        }
        let recipe = self.recipe.expect("checked above");
        let open = ctx.world.open_container().map(|c| (c.id(), c.kind()));
        if recipe.needs_table && open.is_none() && self.stage == CraftStage::Open {
            if self.table.is_none() {
                self.table = block_target(ctx, "crafting_table", 4);
                if self.table.is_none() {
                    return (
                        frame,
                        None,
                        Some(Outcome::Failed("needs_table".into())),
                    );
                }
            }
            let table = self.table.expect("checked above");
            let (look, aligned) = aim(ctx, center(table));
            frame.look = look;
            if aligned && !self.table_used {
                frame.clicks.push(praxion_headless::Click::Use);
                self.table_used = true;
            }
            if self.wait > 60 {
                return (
                    frame,
                    None,
                    Some(Outcome::Failed("needs_table".into())),
                );
            }
            return (frame, None, None);
        }
        if self.clicks.is_empty() && self.stage == CraftStage::Open {
            // Wait for the server's full container content before planning
            // clicks; `open_screen` arrives before `container_set_content`.
            if let Some(container) = ctx.world.open_container()
                && container.slots().len() < 46
            {
                if self.wait > 40 {
                    return (
                        frame,
                        None,
                        Some(Outcome::Failed("no_menu".into())),
                    );
                }
                return (frame, None, None);
            }
            let Some((window, slots, grid, grid_width, player)) = menu_view(ctx.world) else {
                return (
                    frame,
                    None,
                    Some(Outcome::Failed("no_menu".into())),
                );
            };
            let Some(clicks) =
                build_craft_clicks(recipe, &slots, window, &grid, grid_width, &player)
            else {
                return (
                    frame,
                    None,
                    Some(Outcome::Failed("missing_ingredients".into())),
                );
            };
            self.clicks = clicks.into();
            self.stage = CraftStage::Clicks;
            self.wait = 0;
        }
        match self.stage {
            CraftStage::Open => (frame, None, None),
            CraftStage::Clicks => {
                if self.wait % 2 == 0 {
                    if let Some(click) = self.clicks.pop_front() {
                        frame.menu = Some(click);
                        frame.menu_mode = MenuMode::Click;
                        let pending = if started {
                            None
                        } else {
                            Some(IntentSpec::StorageInventory {
                                primitive: "craft".to_owned(),
                                item: self.output.clone(),
                                delta: i64::from(recipe.output_count),
                                window: 120,
                            })
                        };
                        return (frame, pending, None);
                    }
                    self.stage = CraftStage::WaitResult;
                    self.wait = 0;
                }
                (frame, None, None)
            }
            CraftStage::WaitResult => {
                let result = menu_slot(ctx.world, 0);
                if result.is_some_and(|stack| !stack.is_empty()) {
                    frame.menu = Some(MenuAction {
                        window: menu_window(ctx.world),
                        slot: 0,
                        button: 0,
                    });
                    frame.menu_mode = MenuMode::ShiftClick;
                    self.stage = CraftStage::Take;
                    self.wait = 0;
                } else if self.wait > 80 {
                    return (
                        frame,
                        None,
                        Some(Outcome::Failed("no_result".into())),
                    );
                }
                (frame, None, None)
            }
            CraftStage::Take => {
                // Wait for the receipt: the server reports the shift-clicked
                // result in the open menu's player slots, and the craft closes
                // the menu only once that evidence has settled (see
                // `request_close`). The window is longer than the receipt's
                // 120-tick `StorageInventory` window.
                if self.wait > 140 {
                    return (
                        frame,
                        None,
                        Some(Outcome::Failed("no_result".into())),
                    );
                }
                (frame, None, None)
            }
        }
    }
}

fn resolve_recipe(world: &World, wanted: &str) -> Option<&'static gamedata::Recipe> {
    let candidates: Vec<&'static gamedata::Recipe> = if let Some(tag) = wanted.strip_prefix('#') {
        gamedata::recipes_for_tag(tag).collect()
    } else {
        gamedata::recipes_for(wanted).collect()
    };
    candidates.into_iter().find(|recipe| {
        recipe
            .inputs()
            .iter()
            .all(|input| count_item(world, &input.item) >= input.count)
    })
}

fn menu_window(world: &World) -> u32 {
    world
        .open_container()
        .map(|container| container.id().max(0) as u32)
        .unwrap_or(0)
}

fn menu_slot(world: &World, slot: usize) -> Option<ItemStack> {
    if let Some(container) = world.open_container() {
        container.slots().get(slot).cloned()
    } else {
        world.inventory().get(slot).cloned()
    }
}

/// The open menu's slots, its grid slot indexes, the grid width and the player
/// slot range.
fn menu_view(world: &World) -> Option<(u32, Vec<ItemStack>, Vec<usize>, usize, Vec<usize>)> {
    if let Some(container) = world.open_container() {
        let (grid, width) = match container.kind() {
            azalea_registry::builtin::MenuKind::Crafting => ((1..=9).collect(), 3),
            _ => ((1..=4).collect(), 2),
        };
        let slots = container.slots().to_vec();
        let player: Vec<usize> = (10..slots.len().max(10)).collect();
        Some((container.id().max(0) as u32, slots, grid, width, player))
    } else {
        let slots = world.inventory().to_vec();
        Some((0, slots, (1..=4).collect(), 2, (9..=44).collect()))
    }
}

fn recipe_cells(
    recipe: &gamedata::Recipe,
    grid: &[usize],
    grid_width: usize,
) -> Vec<(usize, gamedata::Ingredient)> {
    let mut cells = Vec::new();
    match &recipe.shape {
        gamedata::Shape::Shaped {
            width,
            height,
            grid: pattern,
        } => {
            for row in 0..usize::from(*height) {
                for col in 0..usize::from(*width) {
                    if let Some(ingredient) = pattern[row * usize::from(*width) + col] {
                        if let Some(slot) = grid.get(row * grid_width + col) {
                            cells.push((*slot, ingredient));
                        }
                    }
                }
            }
        }
        gamedata::Shape::Shapeless { ingredients } => {
            for (index, ingredient) in ingredients.iter().enumerate() {
                if let Some(slot) = grid.get(index) {
                    cells.push((*slot, *ingredient));
                }
            }
        }
    }
    cells
}

fn build_craft_clicks(
    recipe: &gamedata::Recipe,
    slots: &[ItemStack],
    window: u32,
    grid: &[usize],
    grid_width: usize,
    player: &[usize],
) -> Option<Vec<MenuAction>> {
    let cells = recipe_cells(recipe, grid, grid_width);
    if cells.is_empty() {
        return None;
    }
    let mut remaining: HashMap<usize, i32> = player
        .iter()
        .map(|index| (*index, slots.get(*index).map(ItemStack::count).unwrap_or(0)))
        .collect();
    let mut clicks = Vec::new();
    let mut index = 0;
    while index < cells.len() {
        let (_, ingredient) = &cells[index];
        let effect = ingredient.effect_item();
        let source = player.iter().copied().find(|slot| {
            remaining.get(slot).copied().unwrap_or(0) > 0
                && slots
                    .get(*slot)
                    .is_some_and(|stack| ingredient.matches(item_name(stack)))
        })?;
        clicks.push(MenuAction {
            window,
            slot: source as i16,
            button: 0,
        });
        let mut used = 0;
        while index < cells.len()
            && cells[index].1.effect_item() == effect
            && remaining.get(&source).copied().unwrap_or(0) > used
        {
            clicks.push(MenuAction {
                window,
                slot: cells[index].0 as i16,
                button: 1,
            });
            used += 1;
            index += 1;
        }
        clicks.push(MenuAction {
            window,
            slot: source as i16,
            button: 0,
        });
        *remaining.get_mut(&source)? -= used;
    }
    Some(clicks)
}

// ----------------------------------------------------------------------
// eat
// ----------------------------------------------------------------------

#[derive(Debug)]
struct Eat {
    item: String,
    sent: bool,
    waited: u32,
}

impl Eat {
    fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        // After the first Use the food stack can be empty while the receipt is
        // still in flight; only a before-use empty stack is `no_item`.
        if !self.sent && count_item(ctx.world, &self.item) == 0 {
            return (frame, None, Some(Outcome::Failed("no_item".into())));
        }
        if !held_matches(ctx.world, &self.item) && !self.sent {
            if !select_item(ctx.world, &self.item, &mut frame) {
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            }
            return (frame, None, None);
        }
        self.waited += 1;
        if self.waited > 140 {
            return (frame, None, Some(Outcome::Failed("not_hungry".into())));
        }
        frame.clicks.push(praxion_headless::Click::Use);
        self.sent = true;
        let pending = if started {
            None
        } else {
            Some(IntentSpec::Inventory {
                primitive: "eat".to_owned(),
                item: self.item.clone(),
                delta: -1,
                window: 80,
            })
        };
        let _ = self.sent;
        (frame, pending, None)
    }
}

// ----------------------------------------------------------------------
// attack
// ----------------------------------------------------------------------

#[derive(Debug)]
struct Attack {
    kind: String,
    entity: Option<i32>,
    target: Option<Vec3>,
    waited: u32,
}

impl Attack {
    fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        if let Some(id) = self.entity
            && ctx.world.entity(id).is_none()
        {
            return (frame, None, Some(Outcome::Confirmed));
        }
        if self.entity.is_none() {
            if let Some((id, position)) = entity_target(ctx, &self.kind, 3) {
                self.entity = Some(id);
                self.target = Some(position);
            } else {
                self.waited += 1;
                if self.waited > 40 {
                    return (frame, None, Some(Outcome::Failed("no_target".into())));
                }
                return (frame, None, None);
            }
        }
        let Some(target) = self.target else {
            return (frame, None, Some(Outcome::Failed("no_target".into())));
        };
        let aim_point = Vec3::new(target.x, target.y + 1.0, target.z);
        let (look, aligned) = aim(ctx, aim_point);
        frame.look = look;
        if aligned {
            frame.clicks.push(praxion_headless::Click::Attack);
        }
        self.waited += 1;
        if self.waited > 400 {
            return (frame, None, Some(Outcome::Failed("target_alive".into())));
        }
        let pending = if started {
            None
        } else {
            self.entity.map(|entity_id| IntentSpec::Attack {
                entity_id,
                window: 20,
            })
        };
        (frame, pending, None)
    }
}

// ----------------------------------------------------------------------
// equip
// ----------------------------------------------------------------------

#[derive(Debug)]
struct Equip {
    item: String,
    stage: EquipStage,
    waited: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EquipStage {
    Select,
    Wait,
}

impl Equip {
    fn tick(&mut self, ctx: &mut Ctx<'_>) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        self.waited += 1;
        if hotbar_index(ctx.world, &self.item).is_some() {
            if held_matches(ctx.world, &self.item) {
                return (frame, None, Some(Outcome::Confirmed));
            }
            if let Some(slot) = hotbar_index(ctx.world, &self.item) {
                frame.select_slot = Some(slot);
            }
            return (frame, None, None);
        }
        if self.stage == EquipStage::Select {
            let Some((index, _)) = find_item(ctx.world, &self.item) else {
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            };
            if !(9..=35).contains(&index) {
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            }
            frame.menu = Some(MenuAction {
                window: 0,
                slot: index as i16,
                button: 0,
            });
            frame.menu_mode = MenuMode::Swap;
            self.stage = EquipStage::Wait;
            self.waited = 0;
            return (frame, None, None);
        }
        if self.waited > 40 {
            return (frame, None, Some(Outcome::Failed("no_item".into())));
        }
        if let Some(slot) = hotbar_index(ctx.world, &self.item) {
            frame.select_slot = Some(slot);
        }
        (frame, None, None)
    }
}

// ----------------------------------------------------------------------
// wait
// ----------------------------------------------------------------------

#[derive(Debug)]
struct Wait {
    remaining: u32,
    total: u32,
}

impl Wait {
    fn tick(&mut self, ctx: &mut Ctx<'_>) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let frame = idle(ctx);
        if self.remaining == 0 {
            return (frame, None, Some(Outcome::Confirmed));
        }
        self.remaining -= 1;
        let pending = if self.remaining + 1 == self.total {
            Some(IntentSpec::Movement {
                primitive: "wait".to_owned(),
                window: u64::from(self.total) + 20,
            })
        } else {
            None
        };
        (frame, pending, None)
    }
}

// ----------------------------------------------------------------------
// observe
// ----------------------------------------------------------------------

#[derive(Debug)]
struct Observe {
    fact: FactCall,
    target: Option<Vec3>,
    waited: u32,
}

impl Observe {
    fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        if fact_known(ctx, &self.fact) {
            let pending = if started {
                None
            } else {
                Some(IntentSpec::Fact {
                    fact: self.fact.clone(),
                    window: 40,
                })
            };
            return (frame, pending, Some(Outcome::Confirmed));
        }
        if self.target.is_none() {
            self.target = observe_target(ctx, &self.fact);
        }
        if let Some(target) = self.target {
            let (look, aligned) = aim(ctx, target);
            frame.look = look;
            if aligned {
                let pending = if started {
                    None
                } else {
                    Some(IntentSpec::Fact {
                        fact: self.fact.clone(),
                        window: 40,
                    })
                };
                return (frame, pending, None);
            }
        }
        self.waited += 1;
        if self.waited > 80 {
            return (
                frame,
                None,
                Some(Outcome::Failed("fact_unknown".into())),
            );
        }
        (frame, None, None)
    }
}

fn observe_target(ctx: &mut Ctx<'_>, fact: &FactCall) -> Option<Vec3> {
    match fact.name.as_str() {
        "near" => {
            let name = fact.args.first()?.name()?;
            block_target(ctx, name, 16).map(center)
        }
        "near_entity" => {
            let name = fact.args.first()?.name()?;
            entity_target(ctx, name, 16).map(|(_, position)| position)
        }
        _ => Some(ctx.body.position()),
    }
}

pub fn fact_known(ctx: &mut Ctx<'_>, fact: &FactCall) -> bool {
    match fact.name.as_str() {
        "count" | "has" => true,
        "near" => {
            let Some(name) = fact.args.first().and_then(ArgValue::name) else {
                return false;
            };
            let radius = fact
                .args
                .get(1)
                .and_then(ArgValue::as_int)
                .unwrap_or(0)
                .max(0) as u32;
            block_target(ctx, name, radius).is_some()
        }
        "near_entity" => {
            let Some(name) = fact.args.first().and_then(ArgValue::name) else {
                return false;
            };
            let radius = fact
                .args
                .get(1)
                .and_then(ArgValue::as_int)
                .unwrap_or(0)
                .max(0) as u32;
            entity_target(ctx, name, radius).is_some()
        }
        _ => false,
    }
}

// ----------------------------------------------------------------------
// move_to
// ----------------------------------------------------------------------

/// How close `move_to` must get to a block or entity. One block of horizontal
/// or diagonal clearance, or standing on top of it, is 1.0-1.41. A radius of
/// 2.0 would also accept the cell two blocks directly below a marker, which
/// let a follower stop under a ledge instead of climbing onto it.
const MOVE_TO_RADIUS: f32 = 1.5;

#[derive(Debug)]
struct Move {
    target: String,
    planner: Planner,
    follower: Option<Follower>,
    observe: Option<(Vec3, u32)>,
    tries: u32,
    waited: u32,
    path_ticks: u64,
    /// The block the first plan resolved. A replan (after a stall) must not
    /// re-resolve it: swimming can put the marker behind a ledge, and losing
    /// sight of a chosen goal is not the same as the goal disappearing.
    block_goal: Option<BlockPos>,
}

impl Move {
    fn new(target: String) -> Self {
        Self {
            target,
            planner: Planner::new(),
            follower: None,
            observe: None,
            tries: 0,
            waited: 0,
            path_ticks: 0,
            block_goal: None,
        }
    }

    fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        self.waited += 1;
        if let Some((point, ticks)) = self.observe {
            let (look, aligned) = aim(ctx, point);
            frame.look = look;
            if aligned || self.waited - ticks > 20 {
                self.observe = None;
            }
            return (frame, None, None);
        }
        if let Some(follower) = &mut self.follower {
            if follower.is_done() {
                return (frame, None, Some(Outcome::Confirmed));
            }
            if follower.is_stalled() {
                let progress = follower.progress(ctx.body);
                eprintln!(
                    "move_to stalled step={}/{} pos={:?} deviation={:.3} ticks={}",
                    progress.step,
                    progress.total,
                    ctx.body.position(),
                    progress.deviation,
                    progress.ticks_on_step
                );
                self.follower = None;
                self.tries += 1;
            } else {
                let next = follower.next_step();
                let _ = next;
                let built = follower.step(ctx.tick, ctx.body.generation(), ctx.body);
                let pending = if started {
                    None
                } else {
                    Some(IntentSpec::Movement {
                        primitive: "move_to".to_owned(),
                        window: self.path_ticks + 40,
                    })
                };
                return (built, pending, None);
            }
        }
        if self.tries > 3 {
            return (frame, None, Some(Outcome::Failed("no_path".into())));
        }
        let start = ctx.body.position().to_block_pos_floor();
        let goal = match self.resolve_goal(ctx) {
            Some(goal) => goal,
            None => {
                if self.waited > 80 {
                    return (frame, None, Some(Outcome::Failed("no_target".into())));
                }
                return (frame, None, None);
            }
        };
        let inventory = plan_inventory(ctx.world);
        let state = PlanningState::new(inventory);
        let plan = {
            let mut source = PerceptionBlocks {
                perception: ctx.perception,
                world: ctx.world,
            };
            self.planner.plan(&mut source, &state, start, goal)
        };
        self.path_ticks = plan.cost.ceil().max(0.) as u64;
        eprintln!(
            "move_to plan status={:?} cost={} start={:?} goal={:?} tries={} steps={:?}",
            plan.status,
            plan.cost,
            start,
            goal,
            self.tries,
            plan.steps
                .iter()
                .map(|step| (step.kind.name(), step.from, step.to))
                .collect::<Vec<_>>()
        );
        match plan.status {
            praxion_planner::PlanStatus::Success => {
                self.follower = Some(Follower::new(plan.steps, ctx.look));
            }
            praxion_planner::PlanStatus::Partial { cell } => {
                self.tries += 1;
                self.observe = Some((center(cell), self.waited));
                return (frame, None, None);
            }
            praxion_planner::PlanStatus::Timeout => {
                self.tries += 1;
            }
            praxion_planner::PlanStatus::NoPath { .. } => {
                return (frame, None, Some(Outcome::Failed("no_path".into())));
            }
        }
        (frame, None, None)
    }

    fn resolve_goal(&mut self, ctx: &mut Ctx<'_>) -> Option<Goal> {
        // `Goal::Block` wants the feet in exactly that cell; a solid block (a
        // tree, a marker) can never be entered. Move *to* the block means the
        // nearest standable cell within reach of it, the same rule entities
        // use.
        if self.target.parse::<EntityKind>().is_ok() {
            // An entity moves, so its goal is resolved again for each plan.
            let (_, position) = entity_target(ctx, &self.target, 32)?;
            let cell = position.to_block_pos_floor();
            return Some(Goal::Near {
                pos: cell,
                radius: MOVE_TO_RADIUS,
            });
        }
        if self.block_goal.is_none() {
            self.block_goal = block_target(ctx, &self.target, 32);
        }
        self.block_goal.map(|pos| Goal::Near {
            pos,
            radius: MOVE_TO_RADIUS,
        })
    }
}

fn plan_inventory(world: &World) -> PlanInventory {
    let hotbar = (36..=44)
        .filter_map(|index| world.inventory().get(index))
        .filter(|stack| !stack.is_empty())
        .map(|stack| praxion_planner::HotbarItem {
            item: normalize(item_name(stack)).to_owned(),
            count: stack.count().max(0) as u32,
        })
        .collect();
    PlanInventory { hotbar }
}

/// The block a `craft` result is taken from, for tests and diagnostics.
pub fn answer_is_known<T>(answer: &Answer<T>) -> bool {
    answer.is_known()
}

/// A block description, used by the live driver's summary.
pub fn block_name(state: azalea_block::BlockState) -> String {
    BlockDescription::of(state).name().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use praxion_headless::Body;
    use praxion_perception::EntityFact;

    fn air() -> BlockState {
        BlockState::from(BlockKind::Air)
    }

    fn stone() -> BlockState {
        BlockState::from(BlockKind::Stone)
    }

    fn water() -> BlockState {
        BlockState::from(BlockKind::Water)
    }

    fn lava() -> BlockState {
        BlockState::from(BlockKind::Lava)
    }

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    /// A known platform: air at `y` and `y + 1`, stone floor below and a stone
    /// ceiling above, over the given box. Nothing outside the box is known.
    fn platform(
        map: &mut HashMap<BlockPos, BlockState>,
        x0: i32,
        x1: i32,
        z0: i32,
        z1: i32,
        y: i32,
    ) {
        for x in x0..=x1 {
            for z in z0..=z1 {
                map.insert(at(x, y - 1, z), stone());
                map.insert(at(x, y, z), air());
                map.insert(at(x, y + 1, z), air());
                map.insert(at(x, y + 2, z), stone());
            }
        }
    }

    fn known(
        map: &HashMap<BlockPos, BlockState>,
    ) -> impl Fn(BlockPos) -> Option<BlockState> + '_ {
        move |pos| map.get(&pos).copied()
    }

    #[test]
    fn the_nearest_reachable_frontier_is_chosen() {
        let mut map = HashMap::new();
        platform(&mut map, -1, 1, -1, 1, 64);
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map))
            .expect("a frontier on the platform edge");
        // All four edge cells are one block away; the deterministic tie-break
        // picks the smallest x, then z.
        assert_eq!(chosen, at(-1, 64, 0));
        assert!(map.contains_key(&chosen), "a frontier cell is a known cell");
        assert!(touches_unknown(chosen, &known(&map)));
    }

    #[test]
    fn a_closed_known_room_has_no_frontier() {
        let mut map = HashMap::new();
        platform(&mut map, -1, 1, -1, 1, 64);
        // Seal the sides; the floor and ceiling are already known stone.
        for y in 64..=66 {
            for x in -2..=2 {
                map.insert(at(x, y, -2), stone());
                map.insert(at(x, y, 2), stone());
            }
            for z in -2..=2 {
                map.insert(at(-2, y, z), stone());
                map.insert(at(2, y, z), stone());
            }
        }
        assert_eq!(
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map)),
            None,
            "a sealed room offers nothing to explore"
        );
    }

    #[test]
    fn a_cell_behind_an_unseen_wall_is_never_chosen() {
        let mut map = HashMap::new();
        // Known corridor from x = 0 to 1, with a remembered island at x = 3.
        for x in 0..=1 {
            map.insert(at(x, 63, 0), stone());
            map.insert(at(x, 64, 0), air());
            map.insert(at(x, 65, 0), stone());
            map.insert(at(x, 64, -1), stone());
            map.insert(at(x, 64, 1), stone());
        }
        map.insert(at(-1, 64, 0), stone());
        for x in 3..=4 {
            map.insert(at(x, 63, 0), stone());
            map.insert(at(x, 64, 0), air());
            map.insert(at(x, 65, 0), stone());
            map.insert(at(x, 64, -1), stone());
            map.insert(at(x, 64, 1), stone());
        }
        // x = 2 is unknown: an unseen wall. The near frontier is x = 1.
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map));
        assert_eq!(chosen, Some(at(1, 64, 0)));
        // Once that one is explored, the island behind the unseen wall must
        // stay unreachable rather than become the next target.
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[at(1, 64, 0)], &[], known(&map));
        assert_eq!(chosen, None, "the unseen wall blocks the island");
    }

    #[test]
    fn water_is_walkable_and_lava_is_not() {
        let mut map = HashMap::new();
        // A one-wide corridor whose far cell is unknown.
        for x in 0..=1 {
            map.insert(at(x, 63, 0), stone());
            map.insert(at(x, 64, 0), if x == 0 { air() } else { water() });
            map.insert(at(x, 65, 0), stone());
            map.insert(at(x, 64, -1), stone());
            map.insert(at(x, 64, 1), stone());
        }
        map.insert(at(-1, 64, 0), stone());
        assert_eq!(
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map)),
            Some(at(1, 64, 0)),
            "a swimmable water cell is a frontier"
        );

        map.insert(at(1, 64, 0), lava());
        assert_eq!(
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map)),
            None,
            "lava is a hazard, not walkable"
        );
    }

    #[test]
    fn explored_frontiers_are_scored_away() {
        let mut map = HashMap::new();
        platform(&mut map, 0, 30, -2, 2, 64);
        let chosen = choose_frontier(
            at(15, 64, 0),
            32,
            8,
            &[at(15, 64, 2)],
            &[],
            known(&map),
        )
        .expect("a frontier far from the explored one");
        assert_ne!(chosen, at(15, 64, -2), "the nearest side is avoided");
        assert!(chosen.x < 15, "a new direction wins: {chosen:?}");
    }

    #[test]
    fn a_replaceable_block_is_not_a_place_support() {
        assert!(!place_support(air()));
        assert!(!place_support(BlockState::from(BlockKind::ShortGrass)));
        assert!(!place_support(BlockState::from(BlockKind::Snow)));
        assert!(place_support(stone()));
        assert!(place_support(BlockState::from(BlockKind::CraftingTable)));
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
                    look: LookStyle::default(),
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
        assert!(pickup_arrived(Vec3::new(0.5, 64.0, 0.9), Vec3::new(0.5, 64.0, 1.2)));
        assert!(!pickup_arrived(Vec3::new(0.5, 64.0, 0.5), Vec3::new(0.5, 64.0, 2.0)));
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
