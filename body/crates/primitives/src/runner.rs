//! Perception-driven primitives: one state machine per primitive, one
//! `ControlFrame` per tick.
//!
//! A primitive reads the world through [`Perception`] (and the planner for
//! `move_to`), never through the raw store. It registers its intent with the
//! receipts engine at the first tick it acts; the driver does that after the
//! actuator reports the prediction sequence number or state id. The primitive
//! ends with the receipt's outcome, or with its own typed failure.

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;

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
        /// Human-readable arguments, for the intent log.
        args: Vec<String>,
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
    /// Bounded diagnostics for the driver's event log.
    pub events: Vec<PrimitiveEvent>,
}

/// A primitive transition the driver writes to the event log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrimitiveEvent {
    /// The frontier `explore` chose to walk to.
    ExploreFrontier {
        cell: BlockPos,
        score: i64,
        candidates: u32,
    },
}

/// The primitive runner. It owns at most one active primitive.
#[derive(Debug, Default)]
pub struct Runner {
    active: Option<Active>,
    receipt: Option<Outcome>,
    outcome: Option<Outcome>,
    intent_id: Option<u64>,
    started: bool,
    /// The memory this runner's `explore` calls share. It is per runner, so
    /// two runners (parallel tests, two bodies) never see each other's
    /// history.
    explore_memory: ExploreMemory,
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
                tool_waited: 0,
                hand: HandSelect::default(),
                pickup: None,
            }),
            "place" => Active::Place(Place {
                block: arg,
                target: None,
                sent: false,
                waited: 0,
                hand: HandSelect::default(),
                rejected: Vec::new(),
                clear_clicks: 0,
            }),
            "craft" => Active::Craft(Craft::new(arg)?),
            "eat" => Active::Eat(Eat {
                item: arg,
                sent: false,
                waited: 0,
                hand: HandSelect::default(),
            }),
            "explore" => Active::Explore(Explore::new(arg, &mut self.explore_memory)?),
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

    /// Whether the active primitive is a continuous one (`explore`,
    /// `move_to`, `wait`): the interpreter stops those by releasing them, not
    /// by waiting for an outcome.
    pub fn is_continuous(&self) -> bool {
        self.is_active()
            && matches!(
                self.active,
                Some(Active::Explore(_) | Active::Move(_) | Active::Wait(_))
            )
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
                events: Vec::new(),
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
                    events: Vec::new(),
                };
            }
        }
        let Some(active) = &mut self.active else {
            return TickResult {
                frame: idle(ctx),
                pending: None,
                outcome: self.outcome.clone(),
                events: Vec::new(),
            };
        };
        let (frame, pending, outcome) = match active {
            Active::Mine(state) => state.tick(ctx, self.started),
            Active::Place(state) => state.tick(ctx, self.started),
            Active::Craft(state) => state.tick(ctx, self.started),
            Active::Eat(state) => state.tick(ctx, self.started),
            Active::Explore(state) => state.tick(ctx, self.started, &mut self.explore_memory),
            Active::Attack(state) => state.tick(ctx, self.started),
            Active::Equip(state) => state.tick(ctx),
            Active::Wait(state) => state.tick(ctx),
            Active::Observe(state) => state.tick(ctx, self.started),
            Active::Move(state) => state.tick(ctx, self.started),
        };
        if let Some(outcome) = &outcome {
            self.finish(outcome.clone());
        }
        let events = match &mut self.active {
            Some(Active::Explore(state)) => std::mem::take(&mut state.events),
            _ => Vec::new(),
        };
        TickResult {
            frame,
            pending,
            outcome,
            events,
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

/// Aim for an interaction: the client's crosshair must hit the intended cell,
/// and the general 4-degree walk tolerance can be off by most of a block at
/// four blocks of reach. The body look is set exactly (bounded only by the
/// style's speed) until the current angle is within
/// [`INTERACT_TOLERANCE_DEG`], and only then may the click go out.
fn aim_precise(ctx: &Ctx<'_>, target: Vec3) -> (Look, bool) {
    let eye = ctx.body.eye_position();
    let (yaw, pitch) = praxion_planner::aim(eye, target);
    let current = ctx.body.look();
    let aligned = praxion_planner::wrap_degrees(yaw - current.y_rot()).abs()
        <= INTERACT_TOLERANCE_DEG
        && (pitch - current.x_rot()).abs() <= INTERACT_TOLERANCE_DEG;
    if aligned {
        (Look::None, true)
    } else {
        (Look::Aim { yaw, pitch }, false)
    }
}

/// The aim error a dig or place accepts before it clicks, in degrees. At four
/// blocks this is under a tenth of a block of crosshair error.
const INTERACT_TOLERANCE_DEG: f32 = 0.25;

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

/// Owned items only: slot 0, the 2x2 grid's result, is a preview (see
/// `praxion_facts::item_count`).
fn count_item(world: &World, wanted: &str) -> u32 {
    world
        .inventory()
        .iter()
        .skip(1)
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

/// How long `mine` tries to bring its tool into the main hand before it mines
/// with what it holds (a swap waits for the server's slot update).
const TOOL_SELECT_TICKS: u32 = 40;

/// How long a swap from the main inventory may take to show in the world
/// before it is sent again.
const SWAP_WAIT_TICKS: u32 = 20;

/// Brings one item into the main hand without repeating a swap.
///
/// The headless client does not predict inventory clicks: the world shows a
/// swap only after the server's slot update. A swap re-sent every tick until
/// then undid itself, because the second click swaps the items back. So a swap
/// is sent once and then waited for, like `equip`; a hotbar key is idempotent
/// and is pressed every tick.
#[derive(Debug, Default)]
struct HandSelect {
    /// Ticks since the last swap was sent, while it has not shown yet.
    swap_waited: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selecting {
    /// The item is in the main hand.
    Held,
    /// A hotbar key or a swap is on its way.
    Pending,
    /// The item is nowhere a click can reach (hotbar or main inventory).
    Unavailable,
}

impl HandSelect {
    fn tick(&mut self, world: &World, wanted: &str, frame: &mut ControlFrame) -> Selecting {
        if held_matches(world, wanted) {
            self.swap_waited = None;
            return Selecting::Held;
        }
        if let Some(slot) = hotbar_index(world, wanted) {
            frame.select_slot = Some(slot);
            return Selecting::Pending;
        }
        if let Some(waited) = self.swap_waited.as_mut() {
            *waited += 1;
            if *waited <= SWAP_WAIT_TICKS {
                return Selecting::Pending;
            }
        }
        if select_item(world, wanted, frame) {
            self.swap_waited = Some(0);
            Selecting::Pending
        } else {
            self.swap_waited = None;
            Selecting::Unavailable
        }
    }
}

/// What `mine` should hold for one block.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ToolChoice {
    /// The held item is as good as anything carried.
    Keep,
    /// Select this item first.
    Select(String),
    /// The block drops nothing without a tool the body does not carry.
    Missing,
}

/// The best item in the main inventory and hotbar (window slots 9 to 44) for
/// `state`, by the vanilla rules: an item the block drops with beats one it
/// does not, then the faster base destroy speed wins. Ties keep the held item,
/// so a body never swaps for nothing.
///
/// The offhand (slot 45) is left out on purpose: no click here moves an item
/// from it into the main hand, so a tool there cannot be used to mine.
fn choose_tool(state: BlockState, inventory: &[ItemStack], held: &ItemStack) -> ToolChoice {
    let rank = |item: &ItemStack| {
        let fit = praxion_headless::tool_fit(state, item);
        (fit.drops, fit.speed)
    };
    let held_rank = rank(held);
    let best = inventory
        .iter()
        .enumerate()
        .filter(|(index, stack)| (9..=44).contains(index) && stack.count() > 0)
        .map(|(_, stack)| (rank(stack), stack))
        .max_by(|(a, _), (b, _)| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let better = |(drops, speed): (bool, f32)| {
        (drops && !held_rank.0) || (drops == held_rank.0 && speed > held_rank.1)
    };
    match best {
        Some((best_rank, stack)) if better(best_rank) => {
            ToolChoice::Select(normalize(item_name(stack)).to_owned())
        }
        _ if !held_rank.0 => ToolChoice::Missing,
        _ => ToolChoice::Keep,
    }
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
    /// Ticks spent selecting the tool before the first attack click.
    tool_waited: u32,
    hand: HandSelect,
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
                    let give_up = selecting == Selecting::Unavailable
                        || self.tool_waited > TOOL_SELECT_TICKS;
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
    hand: HandSelect,
    /// Supports whose face the crosshair could not reach; every later plan in
    /// this call skips them.
    rejected: Vec<BlockPos>,
    /// Attack clicks spent on a plant between the crosshair and the support.
    clear_clicks: u32,
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
        match self.hand.tick(ctx.world, &self.block, &mut frame) {
            Selecting::Held => {}
            Selecting::Pending => return (frame, None, None),
            Selecting::Unavailable => {
                return (frame, None, Some(Outcome::Failed("no_item".into())));
            }
        }
        if self.target.is_none() {
            self.target = place_target(ctx, &self.rejected);
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
        // The body can move into the target cell between planning and the
        // click (a step up makes the old air cell the head cell). The server
        // refuses to place into the player; re-plan instead of clicking.
        if overlaps_body(&ctx.body.bounding_box(), air) {
            self.target = None;
            self.waited += 1;
            if self.waited > 40 {
                return (frame, None, Some(Outcome::Failed("no_place".into())));
            }
            return (frame, None, None);
        }
        let face = cell_centre(solid)
            + Vec3::new(
                f64::from(direction.x()),
                f64::from(direction.y()),
                f64::from(direction.z()),
            ) * 0.5;
        let (look, aligned) = aim_precise(ctx, face);
        frame.look = look;
        if !aligned {
            return (frame, None, None);
        }
        // The server places against what the crosshair hits, not what the
        // plan aimed at. On seed 101 the client's pick hit the tall grass the
        // body stood in (reported `inside`), so the use click named the grass
        // cell, which the server replaced into the body and refused.
        let pick = praxion_headless::pick(ctx.world, ctx.body);
        let hit = pick.block.filter(|hit| !hit.miss);
        match hit {
            Some(hit) if hit.block_pos == solid && hit.direction == direction => {}
            Some(hit)
                if ctx
                    .world
                    .block_state(hit.block_pos)
                    .is_some_and(|state| !state.is_air() && !place_support(state)) =>
            {
                // A plant in the way breaks in one hit, as a player would
                // clear it; the next tick aims again. An entity closer than
                // the plant would take the attack instead, so wait for it.
                self.clear_clicks += 1;
                if self.clear_clicks > PLACE_CLEAR_CLICKS {
                    return (frame, None, Some(Outcome::Failed("no_place".into())));
                }
                if !pick.entity_closer {
                    frame.clicks.push(praxion_headless::Click::Attack);
                }
                return (frame, None, None);
            }
            _ => {
                // Something solid hides the face: plan against another support.
                self.rejected.push(solid);
                self.target = None;
                self.waited += 1;
                if self.waited > 40 {
                    return (frame, None, Some(Outcome::Failed("no_place".into())));
                }
                return (frame, None, None);
            }
        }
        frame.clicks.push(praxion_headless::Click::Use);
        self.sent = true;
        // The 40-tick wait for the receipt starts at the click, not at the
        // first plan.
        self.waited = 0;
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

/// Whether a full block at `cell` would intersect the body's collision box.
/// The comparison is strict, as in `Aabb::intersects_aabb`: a box that only
/// touches the cell's face does not block the placement. A full cube is
/// assumed, which is exact for the crafting table, the only block skills place
/// today; a slab or a torch would need its real collision shape.
fn overlaps_body(body_box: &azalea_core::aabb::Aabb, cell: BlockPos) -> bool {
    let min = Vec3::new(f64::from(cell.x), f64::from(cell.y), f64::from(cell.z));
    // Vanilla inflates the entity query by 1e-7, so a box lying exactly on a
    // face still blocks; inflate the body box the same.
    let body = azalea_core::aabb::Aabb {
        min: body_box.min - Vec3::new(1.0e-7, 1.0e-7, 1.0e-7),
        max: body_box.max + Vec3::new(1.0e-7, 1.0e-7, 1.0e-7),
    };
    body.intersects_aabb(&azalea_core::aabb::Aabb {
        min,
        max: min + Vec3::new(1., 1., 1.),
    })
}

/// The true centre of a cell. `praxion_planner::center` is the centre of the
/// cell's floor (the point a body standing in it occupies), which aimed
/// `place` at a side face's bottom edge.
fn cell_centre(pos: BlockPos) -> Vec3 {
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    )
}

/// Attack clicks `place` may spend breaking a plant that stands between the
/// crosshair and the support (grass breaks on the first; the rest cover the
/// server's block update arriving a few ticks later).
const PLACE_CLEAR_CLICKS: u32 = 20;

fn place_target(ctx: &mut Ctx<'_>, rejected: &[BlockPos]) -> Option<(BlockPos, Direction)> {
    let eye = ctx.body.eye_position();
    let eye_cell = eye.to_block_pos_floor();
    let body_box = ctx.body.bounding_box();
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
        if !place_support(fact.state) || rejected.contains(&fact.pos) {
            continue;
        }
        let delta = eye - cell_centre(fact.pos);
        let direction = Direction::nearest(delta);
        let air = fact.pos.offset_with_direction(direction);
        // Never place into a cell the body's collision box overlaps: the
        // server refuses it (vanilla `isUnobstructed`) and only acks the
        // click. The box is 0.6 wide, so it reaches a neighbour cell whenever
        // the body stands near a block edge; a feet-and-head cell check let
        // seeds 202 and 303 retry such a cell 87 and 9 times.
        if overlaps_body(&body_box, air) {
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

/// The frontier scan radius. It must reach past the sight range, not stay
/// inside it: rays record seen cells out to 64 blocks, so a 64-block box on
/// open ground can be fully known and contain no frontier at all. The known
/// map keeps 96, so 80 still leaves an unknown ring for the flood to find.
const EXPLORE_FRONTIER_RADIUS: u32 = 80;
const EXPLORE_FRONTIER_VERTICAL: i32 = 12;

/// The planner goal radius when `explore` walks to a known target cell. The
/// call itself ends at [`EXPLORE_NEAR_RADIUS`]. Four blocks leaves room for a
/// cave wall's exposed face while staying inside the five-block mine reach.
const TARGET_APPROACH_RADIUS: f32 = 4.0;

/// Ticks one explore path step may take before the follower is considered
/// stuck and the frontier is rejected. Long enough for the jump nudge to free
/// a corner; still shorter than the default 200 so a 1200-tick search can try
/// several directions.
const EXPLORE_STEP_TICKS: u32 = 160;

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
    let tag = wanted.strip_prefix('#');
    let wanted_kind = if tag.is_none() {
        Some(wanted.parse::<BlockKind>().ok()?)
    } else {
        None
    };
    let excluded: HashSet<BlockPos> = exclude.iter().copied().collect();
    perception.nearest_known(
        world,
        radius,
        |pos| excluded.contains(&pos),
        |state| {
            let kind = BlockKind::from(state);
            match (tag, wanted_kind) {
                (Some(tag), _) => gamedata::block_in_tag(kind.to_str(), tag),
                (None, Some(wanted_kind)) => kind == wanted_kind,
                (None, None) => false,
            }
        },
    )
}

/// A frontier the explorer could walk to, with the ordering key it was scored
/// by. The caller may cache a ranked list instead of rescanning the map every
/// tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrontierChoice {
    pub cell: BlockPos,
    /// Squared distance from the start cell. Combined with the avoid and
    /// off-level flags this is the ordering used below.
    pub score: i64,
    /// How many known walkable frontier cells were acceptable.
    pub candidates: u32,
    /// The quality flags this candidate was ordered by: avoided, off level,
    /// wet, uphill. A caller that re-orders the list keeps the best tier
    /// together.
    pub tier: (bool, bool, bool, bool),
}

/// The best frontier, for callers that do not need the score.
pub fn choose_frontier(
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Option<BlockPos> {
    rank_frontiers(start, horizontal, vertical, explored, rejected, known, allow_water)
        .first()
        .map(|choice| choice.cell)
}

/// Ranks the known walkable frontier cells once. The returned list is sorted
/// best first and capped; the cost is the reachability flood plus one pass over
/// its cells, never the full `(2r+1)^3` cube and never a linear scan of the
/// explored list for every cell in it.
pub fn rank_frontiers(
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Vec<FrontierChoice> {
    rank_frontiers_inner(
        None, start, horizontal, vertical, explored, rejected, known, allow_water,
    )
}

/// Like [`rank_frontiers`], but orders by distance from `origin` (the first
/// cell this run explored) so a call walks outward instead of circling the
/// nearest ring of the known map. Seed 202's spawn is plains and the nearest
/// forest is 160 blocks away; nearest-first walked a ring at the sight radius
/// for 4,000 ticks and never reached it.
pub fn rank_frontiers_from(
    origin: BlockPos,
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Vec<FrontierChoice> {
    rank_frontiers_inner(
        Some(origin), start, horizontal, vertical, explored, rejected, known, allow_water,
    )
}

fn rank_frontiers_inner(
    origin: Option<BlockPos>,
    start: BlockPos,
    horizontal: i32,
    vertical: i32,
    explored: &[BlockPos],
    rejected: &[BlockPos],
    known: impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> Vec<FrontierChoice> {
    // Eight blocks: enough that a second pass over the same edge is avoided,
    // small enough that a genuinely new direction still wins.
    const AVOID_RADIUS_SQ: i64 = 8 * 8;
    /// More candidates than an explore budget can try; bounds the allocation.
    const MAX_CHOICES: usize = 64;
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
            if !standable(neighbor, state, &known, allow_water) {
                continue;
            }
            reachable.insert(neighbor);
            queue.push_back(neighbor);
        }
    }

    let explored_set: HashSet<BlockPos> = explored.iter().copied().collect();
    let rejected_set: HashSet<BlockPos> = rejected.iter().copied().collect();
    let mut ranked: Vec<((bool, bool, bool, bool, i64, i64, i32, i32, i32), FrontierChoice)> =
        Vec::new();
    let mut candidates = 0u32;
    for pos in &reachable {
        let pos = *pos;
        if explored_set.contains(&pos) || rejected_set.contains(&pos) {
            continue;
        }
        let Some(state) = known(pos) else {
            continue;
        };
        if !standable(pos, state, &known, allow_water) || !touches_unknown(pos, &known) {
            continue;
        }
        let dx = i64::from(pos.x) - i64::from(start.x);
        let dy = i64::from(pos.y) - i64::from(start.y);
        let dz = i64::from(pos.z) - i64::from(start.z);
        let avoid = explored.iter().any(|explored| {
            let ax = i64::from(explored.x) - i64::from(pos.x);
            let az = i64::from(explored.z) - i64::from(pos.z);
            ax * ax + az * az <= AVOID_RADIUS_SQ
        });
        // Stay on the player's level: a frontier more than two blocks below (a
        // cave) or four above scores worse than any level one, so exploring
        // does not dive into a ravine by accident.
        let off_level = !(-2..=4).contains(&dy);
        // Water is walkable, but a land edge is a better place to look for
        // trees: crossing an ocean with a 1200-tick budget is how a doomed
        // search spends it.
        let wet = praxion_planner::blocks::is_water(state);
        // Do not climb a mountain looking for wood: plains and valleys hold
        // the trees, and an uphill frontier is how a search ended at y=117
        // with the forest still 160 blocks away.
        let uphill = dy > 0;
        let distance_sq = dx * dx + dy * dy + dz * dz;
        // Walk outward from the origin, not around the nearest ring of the
        // known map: a farther frontier wins over a nearer one at the same
        // quality. Without this, seed 202 circled at the sight edge for 4,000
        // ticks while its forest sat 160 blocks away. The plain ranking keeps
        // nearest-first, for callers that want the closest edge.
        let away = origin
            .map(|origin| {
                let ox = i64::from(pos.x) - i64::from(origin.x);
                let oz = i64::from(pos.z) - i64::from(origin.z);
                ox * ox + oz * oz
            })
            .unwrap_or(0);
        candidates = candidates.saturating_add(1);
        let tier = (avoid, off_level, wet, uphill);
        ranked.push((
            (
                avoid,
                off_level,
                wet,
                uphill,
                -away,
                distance_sq,
                pos.x,
                pos.z,
                pos.y,
            ),
            FrontierChoice {
                cell: pos,
                score: distance_sq,
                candidates: 0,
                tier,
            },
        ));
    }
    ranked.sort_by_key(|(key, _)| *key);
    ranked.truncate(MAX_CHOICES);
    ranked
        .into_iter()
        .map(|(_, mut choice)| {
            choice.candidates = candidates;
            choice
        })
        .collect()
}

fn standable(
    pos: BlockPos,
    state: BlockState,
    known: &impl Fn(BlockPos) -> Option<BlockState>,
    allow_water: bool,
) -> bool {
    if praxion_planner::blocks::is_water(state) {
        return allow_water;
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
    /// A ranked set of frontier candidates computed once, consumed by plan
    /// attempts instead of rescanning the whole map every tick.
    candidates: VecDeque<FrontierChoice>,
    /// Next tick at which the known map may be rescanned for a target or a
    /// frontier. The known map changes continuously, so this is a small,
    /// bounded refresh period rather than a cache invalidation storm.
    target_search_tick: u64,
    frontier_scan_tick: u64,
    /// How many `explore` calls this run has made, so a retry starts from a
    /// different frontier instead of repeating the same walk.
    rotation: usize,
    /// Ticks with nowhere to go. A closed or fully known pocket fails early so
    /// the skill's bounded retries can try another direction instead of
    /// sleeping through the whole 1200-tick budget.
    idle_ticks: u32,
    /// Water frontiers and paths through water are allowed. Off at first;
    /// switched on once, when no dry frontier is reachable.
    allow_water: bool,
    /// Walks back to an explored cell since the last new frontier. Two such
    /// walks on seed 101 became an endless shuttle between two cells, each
    /// resetting the idle count, so dry land never counted as exhausted.
    walk_backs: u32,
    /// A plan attempt may not start before this tick. Failed A* calls are the
    /// most expensive thing explore does, so a doomed candidate list is
    /// consumed a few ticks apart, not one per tick.
    next_plan_tick: u64,
    /// How many goals this call has reached. The first few stay nearest-first,
    /// so a nearby forest is found quickly; after that the ranking heads away
    /// from the origin instead of circling the sight ring (seed 202).
    goals: u32,
    /// Bounded diagnostics for the driver's event log.
    events: Vec<PrimitiveEvent>,
    waited: u32,
    budget: u32,
    path_ticks: u64,
}

/// How often `explore` refreshes its target and frontier searches. The
/// completion check runs every tick; these wider scans are the expensive part
/// and the map does not move far in a few ticks.
const EXPLORE_SEARCH_PERIOD_TICKS: u64 = 20;

/// Ticks `explore` may have no target, no ranked candidate and no follower
/// before it fails `not_found`. A closed pocket fails early instead of
/// sleeping through the 1200-tick budget.
const EXPLORE_IDLE_TICKS: u32 = 120;

/// Idle ticks after which `explore` stops insisting on dry land and plans
/// through water too (user decision, 2026-11-03). On seed 101 the body stood on
/// a beach between a three-block bank and a lake: dry-only explore had no
/// frontier for 22,000 ticks. Swimming is the planner's own move set, and the
/// drowning reflex guards it.
const EXPLORE_WATER_AFTER_IDLE_TICKS: u32 = 40;

/// Walks back to explored cells without a new frontier after which dry land
/// counts as exhausted too.
const EXPLORE_WALK_BACKS_BEFORE_WATER: u32 = 2;

/// Plan attempts per tick when consuming a ranked frontier list. One keeps a
/// slow A* out of the tick's tail; the remaining candidates wait for the next
/// tick.
const EXPLORE_PLAN_ATTEMPTS: usize = 1;

/// The planner budget one body primitive may spend inside a tick. The
/// planner's default 100 ms is a standalone-search limit; a 20 Hz body tick
/// cannot spend it, so a route that needs longer is tried again next tick.
const BODY_PLAN_BUDGET: praxion_planner::Budget =
    praxion_planner::Budget::new(Duration::from_millis(6), 30_000);

fn body_planner() -> Planner {
    Planner::with_budget(BODY_PLAN_BUDGET)
}

/// Explored, rejected and blocked cells kept per `explore` call. Old cells are
/// behind the search and no longer influence it; keeping the list bounded is
/// what makes the per-tick cost independent of how long the run has walked.
const EXPLORE_MEMORY_CELLS: usize = 64;

fn remember(cells: &mut Vec<BlockPos>, pos: BlockPos) {
    if cells.contains(&pos) {
        return;
    }
    if cells.len() >= EXPLORE_MEMORY_CELLS {
        cells.remove(0);
    }
    cells.push(pos);
}

/// Cells recent `explore` calls reached or rejected. A call used to forget its
/// explored set when it ended, so the next 1200-tick retry walked the same way.
/// The set is kept for the run, newest last, and bounded.
const RECENT_EXPLORED_CELLS: usize = 128;

/// The memory one run's `explore` calls share. It used to be process-wide
/// statics that two runners (parallel tests, two bodies) shared; `Runner` owns
/// one, so a run only ever sees its own history.
#[derive(Debug, Default)]
struct ExploreMemory {
    /// Cells recent `explore` calls reached or rejected, oldest first.
    recent: VecDeque<BlockPos>,
    /// The highest surface altitude this run's `explore` calls stood at, once
    /// one has run.
    home_y: Option<i32>,
    /// The first cell this run explored, as `(x, z)`.
    origin: Option<(i32, i32)>,
    /// How many `explore` calls this run has started.
    calls: u64,
}

impl ExploreMemory {
    /// The remembered cells, oldest first.
    fn recent(&self) -> Vec<BlockPos> {
        self.recent.iter().copied().collect()
    }

    /// Remember a cell, newest last. An already remembered cell keeps its
    /// place; a full memory drops its oldest cell.
    fn remember(&mut self, pos: BlockPos) {
        if self.recent.contains(&pos) {
            return;
        }
        if self.recent.len() >= RECENT_EXPLORED_CELLS {
            self.recent.pop_front();
        }
        self.recent.push_back(pos);
    }

    /// The highest surface altitude recent `explore` calls stood at. Wood grows
    /// on the surface; an explore that follows one-block descents into a cave
    /// can burn every budget underground, so a candidate much below this line
    /// is refused.
    fn home_y(&mut self, start: i32) -> i32 {
        let home = self.home_y.get_or_insert(start);
        *home = (*home).max(start);
        *home
    }

    /// The first cell this body explored, as (x, z). Frontier ranking pushes
    /// away from it, so a call heads outward instead of circling the nearest
    /// ring of the known map. One run is one body, so a per-run origin is
    /// exact.
    fn origin(&mut self, start: BlockPos) -> BlockPos {
        let (x, z) = *self.origin.get_or_insert((start.x, start.z));
        BlockPos::new(x, start.y, z)
    }

    /// A per-run counter so consecutive `explore` calls (a failed call and its
    /// retry, or a new skill after a failure) start from a different frontier
    /// in the ranked list. Without it, a retry repeated the same walk from an
    /// empty explored set and could burn every 1200-tick budget in the same
    /// direction.
    fn next_rotation(&mut self) -> usize {
        let rotation = (self.calls % 8) as usize;
        self.calls = self.calls.wrapping_add(1);
        rotation
    }
}

impl Explore {
    fn new(target: String, memory: &mut ExploreMemory) -> Result<Self, String> {
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
                //
                // Descending uses the planner's default, safe drop (three
                // blocks, no fall damage). An eight-block drop is survivable
                // but can land in a one-cell pit with no route back up: on
                // seed 303 that ended every later explore in the 120-tick
                // idle (runs/s9d-f11-3-seed303, seq 75 then 86+). A peak is
                // left by walking its slopes, not by dropping off a cliff.
                let mut planner = body_planner();
                planner.set_allow_world_changes(false);
                planner
            },
            follower: None,
            goal: None,
            explored: {
                // Continue from where recent calls walked, so a retry does not
                // repeat the same direction from an empty set.
                let mut explored = memory.recent();
                if explored.len() > EXPLORE_MEMORY_CELLS {
                    explored.drain(..explored.len() - EXPLORE_MEMORY_CELLS);
                }
                explored
            },
            rejected: Vec::new(),
            blocked: Vec::new(),
            candidates: VecDeque::new(),
            target_search_tick: 0,
            frontier_scan_tick: 5,
            rotation: memory.next_rotation(),
            idle_ticks: 0,
            allow_water: false,
            walk_backs: 0,
            next_plan_tick: 0,
            goals: 0,
            events: Vec::new(),
            waited: 0,
            budget: praxion_rules::registry::EXPLORE_MAX_TICKS,
            path_ticks: 0,
        })
    }

    fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
        memory: &mut ExploreMemory,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let frame = idle(ctx);
        self.waited += 1;
        let start = ctx.body.position().to_block_pos_floor();
        let home = memory.home_y(start.y);
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
                self.target_search_tick = 0;
                self.frontier_scan_tick = ctx.tick.saturating_add(5);
                self.idle_ticks = 0;
                self.candidates.clear();
                if let Some(goal) = self.goal.take() {
                    memory.remember(goal);
                    remember(&mut self.explored, goal);
                    self.goals = self.goals.saturating_add(1);
                }
            } else if follower.is_stalled() {
                let progress = follower.progress(ctx.body);
                eprintln!(
                    "explore stalled step={}/{} pos={:?} deviation={:.3} ticks={} goal={:?} on_ground={} in_water={}",
                    progress.step,
                    progress.total,
                    ctx.body.position(),
                    progress.deviation,
                    progress.ticks_on_step,
                    self.goal,
                    ctx.body.on_ground(),
                    ctx.body.is_in_water(),
                );
                let feet = ctx.body.position().to_block_pos_floor();
                let cell = |pos| {
                    ctx.perception
                        .known(pos)
                        .map(|known| block_name(known.state))
                        .unwrap_or_else(|| "unknown".to_owned())
                };
                eprintln!(
                    "explore stall cells feet={} below={} head={} next={:?} next_cell={} next_below={} next_head={}",
                    cell(feet),
                    cell(BlockPos::new(feet.x, feet.y - 1, feet.z)),
                    cell(BlockPos::new(feet.x, feet.y + 1, feet.z)),
                    follower.next_cell(),
                    follower
                        .next_cell()
                        .map(&cell)
                        .unwrap_or_else(|| "-".to_owned()),
                    follower
                        .next_cell()
                        .map(|next| cell(BlockPos::new(next.x, next.y - 1, next.z)))
                        .unwrap_or_else(|| "-".to_owned()),
                    follower
                        .next_cell()
                        .map(|next| cell(BlockPos::new(next.x, next.y + 1, next.z)))
                        .unwrap_or_else(|| "-".to_owned()),
                );
                self.follower = None;
                self.target_search_tick = 0;
                self.frontier_scan_tick = 0;
                self.idle_ticks = 0;
                // Drop the rest of the old ranked list: its first entry is
                // usually the same corner, and replanning to it one tick later
                // is how the body sat at an edge for 160 ticks.
                self.candidates.clear();
                if let Some(goal) = self.goal.take() {
                    memory.remember(goal);
                    remember(&mut self.rejected, goal);
                }
                let stuck = ctx.body.position().to_block_pos_floor();
                remember(&mut self.blocked, stuck);
            } else {
                self.idle_ticks = 0;
                let built = follower.step(ctx.tick, ctx.body.generation(), ctx.body);
                let pending = if started {
                    None
                } else {
                    Some(IntentSpec::Movement {
                        primitive: "explore".to_owned(),
                        args: vec![self.target.clone()],
                        window: self.path_ticks + 40,
                    })
                };
                return (built, pending, None);
            }
        }

        // A plan starts from the feet cell. While the body is in the air that
        // cell is a space it is falling through, not a place it stands: a plan
        // made mid-fall believed in a floor that was not there and the body
        // stalled under its first step (seed 303, body at y=77.25 planning a
        // step to y=78 over air). Wait for solid ground; falling is progress,
        // not idling.
        if !ctx.body.on_ground() && !ctx.body.is_in_water() {
            self.idle_ticks = 0;
            return (frame, None, None);
        }

        // A target the known map already holds is approached directly. The
        // search is the expensive wide scan, so it refreshes on a bounded
        // period instead of every tick.
        if ctx.tick >= self.target_search_tick {
            self.target_search_tick = ctx.tick + EXPLORE_SEARCH_PERIOD_TICKS;
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
                    praxion_planner::PlanStatus::Success
                        if self.allow_water || !plan_crosses_water(&plan) =>
                    {
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
                        self.idle_ticks = 0;
                        return (frame, None, None);
                    }
                    _ => {
                        remember(&mut self.rejected, cell);
                    }
                }
            }
        }

        // No target in sight: rank the frontiers once per refresh period and
        // consume the list with a few plan attempts per tick. An unreachable
        // frontier is rejected and the next candidate is tried; a closed room
        // simply has none and waits for its budget.
        if self.candidates.is_empty() && ctx.tick >= self.frontier_scan_tick {
            let horizontal = EXPLORE_FRONTIER_RADIUS
                .min(ctx.perception.config().known_radius)
                .max(8) as i32;
            let known = |pos: BlockPos| ctx.perception.known(pos).map(|cell| cell.state);
            // Stay nearest-first while a nearby target may still be close, then
            // walk outward from the origin rather than around the sight ring.
            const OUTWARD_AFTER_GOALS: u32 = 8;
            let ranked = if self.goals < OUTWARD_AFTER_GOALS {
                rank_frontiers(
                    start,
                    horizontal,
                    EXPLORE_FRONTIER_VERTICAL,
                    &self.explored,
                    &self.rejected,
                    known,
                    self.allow_water,
                )
            } else {
                rank_frontiers_from(
                    memory.origin(start),
                    start,
                    horizontal,
                    EXPLORE_FRONTIER_VERTICAL,
                    &self.explored,
                    &self.rejected,
                    known,
                    self.allow_water,
                )
            };
            self.candidates = ranked.into();
            self.frontier_scan_tick = ctx.tick + EXPLORE_SEARCH_PERIOD_TICKS;
            self.candidates.retain(|choice| !self.rejected.contains(&choice.cell));
            // Keep the best quality tier (not avoided, on level, dry) before
            // fanning out or preferring a longer leg; distance never overrides
            // a cliff or an explored edge.
            if let Some(tier) = self.candidates.front().map(|choice| choice.tier) {
                self.candidates.retain(|choice| choice.tier == tier);
            }
            if self.rotation > 0 && self.candidates.len() > 1 {
                // Start at a different eighth of the ranked ring per call, so
                // bounded retries fan out around the player instead of all
                // walking the same nearest edge.
                let shift = self.rotation * self.candidates.len() / 8;
                self.candidates.rotate_left(shift.min(self.candidates.len() - 1));
            }
            // The candidate list is already nearest-first (after the
            // rotation); hopping is fine because the follower walks a whole
            // path. Long legs were tried and made the follower stall on
            // terrain, so the score is not inflated here.
            // Stay near the surface: wood does not grow in the cave a chain of
            // one-block descents leads to. Twelve blocks below the highest
            // recent surface still reaches a valley forest but not a cave.
            self.candidates
                .retain(|choice| choice.cell.y >= home - 12 && choice.cell.y <= home + 32);
        }
        if ctx.tick >= self.next_plan_tick {
            for _ in 0..EXPLORE_PLAN_ATTEMPTS {
                let Some(choice) = self.candidates.pop_front() else {
                    break;
                };
                let goal = choice.cell;
                let plan = self.plan_to(ctx, start, Goal::Block(goal));
                match plan.status {
                    praxion_planner::PlanStatus::Success
                        if self.allow_water || !plan_crosses_water(&plan) =>
                    {
                        self.path_ticks = plan.cost.ceil().max(0.) as u64;
                        eprintln!(
                            "explore plan target={:?} frontier={:?} cost={} steps={} candidates={}",
                            self.target,
                            goal,
                            plan.cost,
                            plan.steps.len(),
                            choice.candidates
                        );
                        self.goal = Some(goal);
                        self.follower = Some(Follower::new(plan.steps, ctx.look).with_max_ticks_per_step(EXPLORE_STEP_TICKS));
                        self.idle_ticks = 0;
                        self.walk_backs = 0;
                        self.next_plan_tick = 0;
                        self.events.push(PrimitiveEvent::ExploreFrontier {
                            cell: goal,
                            score: choice.score,
                            candidates: choice.candidates,
                        });
                        return (frame, None, None);
                    }
                    _ => {
                        memory.remember(goal);
                        remember(&mut self.rejected, goal);
                        self.next_plan_tick = ctx.tick.saturating_add(5);
                    }
                }
            }
        }
        self.idle_ticks = self.idle_ticks.saturating_add(1);
        // Dry land is exhausted: allow water once and rescan, with a fresh
        // idle budget. Frontiers rejected because their only path crossed
        // water get another chance.
        if !self.allow_water
            && (self.idle_ticks >= EXPLORE_WATER_AFTER_IDLE_TICKS
                || self.walk_backs >= EXPLORE_WALK_BACKS_BEFORE_WATER)
        {
            eprintln!("explore target={:?}: no dry frontier, allowing water", self.target);
            self.allow_water = true;
            self.walk_backs = 0;
            self.rejected.clear();
            self.candidates.clear();
            self.frontier_scan_tick = ctx.tick;
            self.next_plan_tick = ctx.tick;
            self.idle_ticks = 0;
            return (frame, None, None);
        }
        // A pit with no reachable frontier is not a dead end: walk back to a
        // cell a previous leg reached and scan from there. Without this the
        // body sits at the bottom of the same hole for every retry.
        if self.idle_ticks >= 10
            && !self.explored.is_empty()
            && ctx.tick >= self.next_plan_tick
        {
            for _ in 0..2 {
                if self.explored.is_empty() {
                    break;
                }
                let cell = self.explored.remove(0);
                let plan = self.plan_to(ctx, start, Goal::Near {
                    pos: cell,
                    radius: TARGET_APPROACH_RADIUS,
                });
                if plan.status == praxion_planner::PlanStatus::Success
                    && (self.allow_water || !plan_crosses_water(&plan))
                {
                    self.path_ticks = plan.cost.ceil().max(0.) as u64;
                    self.goal = Some(cell);
                    self.follower = Some(Follower::new(plan.steps, ctx.look).with_max_ticks_per_step(EXPLORE_STEP_TICKS));
                    self.idle_ticks = 0;
                    self.walk_backs += 1;
                    self.next_plan_tick = 0;
                    return (frame, None, None);
                }
                self.next_plan_tick = ctx.tick.saturating_add(5);
            }
        }
        if self.idle_ticks > EXPLORE_IDLE_TICKS {
            return (frame, None, Some(Outcome::Failed("not_found".into())));
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

/// Whether a walking plan enters water. `explore` for wood stays on land: a
/// swim can drown the body, and the skill layer has no safe water handling.
fn plan_crosses_water(plan: &praxion_planner::Plan) -> bool {
    plan.steps.iter().any(|step| {
        matches!(
            step.kind,
            praxion_planner::MoveKind::EnterWater
                | praxion_planner::MoveKind::Swim
                | praxion_planner::MoveKind::SwimUp
                | praxion_planner::MoveKind::ExitWater { .. }
        )
    })
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
    close_sent: bool,
    /// The receipt settled while the table is still open. The craft sends its
    /// close next tick and only then finishes, so the menu does not outlive the
    /// primitive.
    close_outcome: Option<Outcome>,
    /// Items an earlier craft left in the 2x2 grid are returned once, before
    /// this craft plans its clicks.
    grid: GridClear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GridClear {
    Unchecked,
    /// The close went out; waiting this many ticks for the grid to empty.
    Waiting(u32),
    Clear,
}

/// How long a craft waits for the server to return leftover grid items.
const GRID_CLEAR_TICKS: u32 = 20;

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
            close_sent: false,
            close_outcome: None,
            grid: GridClear::Unchecked,
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
        let (frame, pending, outcome) = self.tick_inner(ctx, started);
        // A craft that fails on its own (`no_result`, `no_menu`) leaves its
        // items in the grid or its table open just like one cut short. Route
        // it through the close path: the close goes out next tick, then the
        // failure is reported.
        if let Some(Outcome::Failed(_)) = &outcome
            && self.close_outcome.is_none()
            && !self.close_sent
        {
            self.close_outcome = outcome;
            return (frame, pending, None);
        }
        (frame, pending, outcome)
    }

    fn tick_inner(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        if let Some(outcome) = self.close_outcome.clone() {
            let needs_table = self.recipe.is_some_and(|recipe| recipe.needs_table);
            // A table menu always closes. The inventory's own 2x2 grid closes
            // whenever the craft did not finish cleanly: closing window 0 makes
            // the server return what is in the grid and on the cursor. The
            // cursor is not in the world model, and a reflex that cut a craft
            // right after its pick-up left two logs there: the body thought
            // it had none and went to look for trees (melee smoke test).
            let unclean = outcome != Outcome::Confirmed || inventory_grid_in_use(ctx.world);
            if (needs_table || unclean) && !self.close_sent {
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
        // Items left in the 2x2 grid or on the cursor by an earlier craft
        // would join this recipe's slots: close window 0 once (it costs a
        // tick and returns both), and plan only on an empty grid.
        match self.grid {
            GridClear::Unchecked => {
                if ctx.world.open_container().is_none() {
                    frame.menu = Some(MenuAction {
                        window: 0,
                        slot: 0,
                        button: 0,
                    });
                    frame.menu_mode = MenuMode::Close;
                    self.grid = GridClear::Waiting(0);
                    return (frame, None, None);
                }
                self.grid = GridClear::Clear;
            }
            GridClear::Waiting(ticks) => {
                if inventory_grid_in_use(ctx.world) {
                    if ticks >= GRID_CLEAR_TICKS {
                        return (frame, None, Some(Outcome::Failed("grid_busy".into())));
                    }
                    self.grid = GridClear::Waiting(ticks + 1);
                    return (frame, None, None);
                }
                self.grid = GridClear::Clear;
            }
            GridClear::Clear => {}
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
                // Prefer the block the crosshair can actually pick; fall back
                // to the known map so a remembered table beside the body is
                // still approached and clicked.
                self.table = block_target(ctx, "crafting_table", 4).or_else(|| {
                    nearest_known_block(ctx.perception, ctx.world, "crafting_table", 4, &[])
                        .map(|(cell, _)| cell)
                });
                if self.table.is_none() {
                    return (
                        frame,
                        None,
                        Some(Outcome::Failed("needs_table".into())),
                    );
                }
            }
            let table = self.table.expect("checked above");
            // Aim at the table's true centre: `center` is the cell's floor,
            // and a ray at a block's bottom edge can pass under it (seed 303
            // clicked three times and no menu opened).
            let (look, aligned) = aim_precise(ctx, cell_centre(table));
            frame.look = look;
            // Retry the use click while no menu has opened: one missed click
            // (a block update in the same tick, a slightly early aim) used to
            // doom the whole craft after the single attempt. The click only
            // goes out when the crosshair is on the table, as in `place`; a
            // plant in the way is broken first.
            if aligned && (self.wait <= 1 || self.wait % 10 == 0) {
                let pick = praxion_headless::pick(ctx.world, ctx.body);
                match pick.block.filter(|hit| !hit.miss) {
                    Some(hit) if hit.block_pos == table => {
                        frame.clicks.push(praxion_headless::Click::Use);
                    }
                    Some(hit)
                        if !pick.entity_closer
                            && ctx.world.block_state(hit.block_pos).is_some_and(|state| {
                                !state.is_air() && !place_support(state)
                            }) =>
                    {
                        frame.clicks.push(praxion_headless::Click::Attack);
                    }
                    _ => {}
                }
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

/// Whether the inventory's 2x2 crafting grid (window 0, slots 1 to 4) holds
/// anything. The cursor is not checked: the world model does not track it
/// (slot -1 updates are dropped), so a leftover there is not seen (backlog).
fn inventory_grid_in_use(world: &World) -> bool {
    world
        .inventory()
        .get(1..=4)
        .is_some_and(|slots| slots.iter().any(|stack| stack.count() > 0))
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
    hand: HandSelect,
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
        if !self.sent {
            match self.hand.tick(ctx.world, &self.item, &mut frame) {
                Selecting::Held => {}
                Selecting::Pending => return (frame, None, None),
                Selecting::Unavailable => {
                    return (frame, None, Some(Outcome::Failed("no_item".into())));
                }
            }
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
                args: vec![self.total.to_string()],
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
            planner: body_planner(),
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
                        args: vec![self.target.clone()],
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

/// A block description, used by diagnostics.
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
    fn explore_descends_only_within_the_safe_drop() {
        // Seed 303's peak: explore used an eight-block drop, landed in a
        // one-cell pit and every later call idled 120 ticks. Descending must
        // stay within the planner's safe (damage-free) three-block drop; a peak
        // is left along its slopes.
        let mut memory = ExploreMemory::default();
        let explore = Explore::new("#logs".into(), &mut memory).expect("a known block tag");
        assert_eq!(explore.planner.max_drop(), 3);
    }

    #[test]
    fn a_far_frontier_outranks_the_nearest_ring_cell() {
        // Seed 202 is plains with its forest 160 blocks away. Ranking by
        // distance from the start walks the sight-radius ring; ranking away
        // from the origin heads outward instead.
        let mut map = HashMap::new();
        platform(&mut map, -30, 30, -2, 2, 64);
        let ranked = rank_frontiers_from(
            at(0, 64, 0),
            at(0, 64, 0),
            32,
            8,
            &[],
            &[],
            known(&map),
            true,
        );
        let best = ranked[0];
        assert_eq!(best.cell.x, -30, "the far edge wins: {best:?}");
    }

    #[test]
    fn the_nearest_reachable_frontier_is_chosen() {
        let mut map = HashMap::new();
        platform(&mut map, -1, 1, -1, 1, 64);
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true)
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
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true),
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
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true);
        assert_eq!(chosen, Some(at(1, 64, 0)));
        // Once that one is explored, the island behind the unseen wall must
        // stay unreachable rather than become the next target.
        let chosen = choose_frontier(at(0, 64, 0), 8, 8, &[at(1, 64, 0)], &[], known(&map), true);
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
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true),
            Some(at(1, 64, 0)),
            "a swimmable water cell is a frontier"
        );

        map.insert(at(1, 64, 0), lava());
        assert_eq!(
            choose_frontier(at(0, 64, 0), 8, 8, &[], &[], known(&map), true),
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
            true,
        )
        .expect("a frontier far from the explored one");
        assert_ne!(chosen, at(15, 64, -2), "the nearest side is avoided");
        assert!(chosen.x < 15, "a new direction wins: {chosen:?}");
    }

    #[test]
    fn ranked_frontiers_carry_their_score_and_candidate_count() {
        let mut map = HashMap::new();
        platform(&mut map, -1, 1, -1, 1, 64);
        let ranked = rank_frontiers(at(0, 64, 0), 8, 8, &[], &[], known(&map), true);
        assert!(!ranked.is_empty(), "an open platform edge is a frontier");
        let best = ranked[0];
        assert_eq!(best.cell, at(-1, 64, 0));
        assert_eq!(best.score, 1, "the score is the squared distance");
        assert!(
            best.candidates >= ranked.len() as u32,
            "the count covers the ranked candidates"
        );
        assert!(ranked.len() <= 64, "the list is bounded");
    }

    #[test]
    fn rank_frontiers_does_not_grow_with_the_explored_list() {
        // The old scan tested every explored cell for every cell of a 65^3
        // cube. Now the explored set is a hash set and the candidates come
        // from the reachability flood, so a long rejected list cannot turn
        // the scan into quadratic work. This pins the result, not a timer.
        let mut map = HashMap::new();
        platform(&mut map, -8, 8, -8, 8, 64);
        let rejected: Vec<BlockPos> = (0..500)
            .map(|i| at(i % 16 - 8, 64, i / 16 - 8))
            .collect();
        let ranked = rank_frontiers(at(0, 64, 0), 8, 8, &[], &rejected, known(&map), true);
        assert!(ranked.iter().all(|choice| !rejected.contains(&choice.cell)));
    }

    #[test]
    fn explore_memory_is_bounded() {
        let mut cells = Vec::new();
        for i in 0..(EXPLORE_MEMORY_CELLS as i32 * 2) {
            remember(&mut cells, at(i, 64, 0));
        }
        assert_eq!(cells.len(), EXPLORE_MEMORY_CELLS);
        assert!(cells.contains(&at(EXPLORE_MEMORY_CELLS as i32 * 2 - 1, 64, 0)));
        // Re-remembering an existing cell never grows the list.
        let before = cells.len();
        let first = cells[0];
        remember(&mut cells, first);
        assert_eq!(cells.len(), before);
    }

    fn inventory_with(items: &[(usize, azalea_registry::builtin::ItemKind)]) -> Vec<ItemStack> {
        let mut inventory = vec![ItemStack::Empty; 46];
        for (slot, kind) in items {
            inventory[*slot] = ItemStack::new(*kind, 1);
        }
        inventory
    }

    /// A standing player's box with its feet at the given position.
    fn standing_box(x: f64, y: f64, z: f64) -> azalea_core::aabb::Aabb {
        Body::new(Vec3::new(x, y, z)).bounding_box()
    }

    #[test]
    fn the_body_box_blocks_the_cells_it_overlaps() {
        // Seed 303: the body at x 44.71 reaches 0.01 into the cell east of
        // its feet; the server refused the table there nine times.
        let body = standing_box(44.711, 96.0, 40.70);
        assert!(overlaps_body(&body, BlockPos::new(45, 97, 40)));
        // Seed 202: the same shape, 0.23 into the cell west of its feet.
        let body = standing_box(31.07, 62.0, 41.30);
        assert!(overlaps_body(&body, BlockPos::new(30, 62, 41)));
        // The feet and head cells still count.
        assert!(overlaps_body(&body, BlockPos::new(31, 62, 41)));
        assert!(overlaps_body(&body, BlockPos::new(31, 63, 41)));
        // A box centred in its cell stays 0.2 away from its side neighbours.
        let body = standing_box(10.5, 64.0, 10.5);
        assert!(!overlaps_body(&body, BlockPos::new(11, 64, 10)));
        assert!(overlaps_body(&body, BlockPos::new(10, 65, 10)), "the head cell");
        assert!(!overlaps_body(&body, BlockPos::new(10, 66, 10)), "the cell above the head");
        // Vanilla's 1e-7 inflation: a box lying exactly on a face blocks, as
        // the floor under the feet does; a box 1e-6 away does not.
        assert!(overlaps_body(&body, BlockPos::new(10, 63, 10)), "the floor under the feet");
        let touching = azalea_core::aabb::Aabb {
            min: Vec3::new(10.4, 64.0, 10.2),
            max: Vec3::new(11.0, 65.8, 10.8),
        };
        assert!(overlaps_body(&touching, BlockPos::new(11, 64, 10)));
        let apart = azalea_core::aabb::Aabb {
            min: Vec3::new(10.4, 64.0, 10.2),
            max: Vec3::new(11.0 - 1.0e-6, 65.8, 10.8),
        };
        assert!(!overlaps_body(&apart, BlockPos::new(11, 64, 10)));
    }

    #[test]
    fn mine_selects_a_pickaxe_for_stone() {
        use azalea_registry::builtin::ItemKind;
        let stone = BlockState::from(BlockKind::Stone);
        // The pickaxe sits in the main inventory, the hand holds planks.
        let inventory = inventory_with(&[(20, ItemKind::WoodenPickaxe), (36, ItemKind::OakPlanks)]);
        assert_eq!(
            choose_tool(stone, &inventory, &inventory[36]),
            ToolChoice::Select("wooden_pickaxe".into())
        );
        // A better pickaxe wins over a worse one.
        let inventory = inventory_with(&[(37, ItemKind::WoodenPickaxe), (38, ItemKind::StonePickaxe)]);
        assert_eq!(
            choose_tool(stone, &inventory, &ItemStack::Empty),
            ToolChoice::Select("stone_pickaxe".into())
        );
        // Already holding it: nothing to do.
        assert_eq!(choose_tool(stone, &inventory, &inventory[38]), ToolChoice::Keep);
    }

    #[test]
    fn mine_refuses_stone_without_a_pickaxe() {
        use azalea_registry::builtin::ItemKind;
        let stone = BlockState::from(BlockKind::Stone);
        let inventory = inventory_with(&[(36, ItemKind::OakPlanks), (37, ItemKind::WoodenAxe)]);
        assert_eq!(
            choose_tool(stone, &inventory, &inventory[36]),
            ToolChoice::Missing,
            "stone mined by hand breaks and drops nothing"
        );
    }

    #[test]
    fn mine_uses_the_hand_when_no_tool_helps() {
        use azalea_registry::builtin::ItemKind;
        // A crafting table drops with any item; a pickaxe is no faster on it.
        let table = BlockState::from(BlockKind::CraftingTable);
        let inventory = inventory_with(&[(36, ItemKind::WoodenPickaxe)]);
        assert_eq!(choose_tool(table, &inventory, &ItemStack::Empty), ToolChoice::Keep);
        // An axe speeds a log up.
        let log = BlockState::from(BlockKind::OakLog);
        let inventory = inventory_with(&[(36, ItemKind::WoodenPickaxe), (37, ItemKind::WoodenAxe)]);
        assert_eq!(
            choose_tool(log, &inventory, &inventory[36]),
            ToolChoice::Select("wooden_axe".into())
        );
    }

    #[test]
    fn is_continuous_matches_the_registry() {
        // `Runner::is_continuous` lists the continuous primitives by hand; a
        // new continuous record would otherwise never be released (review,
        // 2026-11-03).
        use praxion_rules::registry::{PrimitiveKind, Primitives};
        let continuous: Vec<&str> = Primitives::standard()
            .iter()
            .filter(|record| record.kind == PrimitiveKind::Continuous)
            .map(|record| record.name)
            .collect();
        assert_eq!(continuous, ["explore", "move_to", "wait"]);
        for (name, arg) in [("explore", "#logs"), ("move_to", "0, 64, 0"), ("wait", "5")] {
            let mut runner = Runner::new();
            runner
                .start(&StartAction {
                    primitive: name.into(),
                    args: vec![ArgValue::Name(arg.into())],
                })
                .unwrap_or_else(|error| panic!("{name}: {error}"));
            assert!(runner.is_continuous(), "{name}");
        }
    }

    #[test]
    fn consecutive_explore_calls_rotate() {
        let mut memory = ExploreMemory::default();
        let first = memory.next_rotation();
        let second = memory.next_rotation();
        assert_ne!(first, second, "a retry starts from a different frontier");
    }

    #[test]
    fn explore_memory_is_per_runner() {
        // Two runners must not share explore's history: the cells one
        // remembers and the rotations one has consumed stay invisible to the
        // other.
        let mut first = ExploreMemory::default();
        let mut second = ExploreMemory::default();
        first.remember(at(1, 64, 2));
        first.next_rotation();
        first.next_rotation();
        assert_eq!(first.recent(), vec![at(1, 64, 2)]);
        assert!(second.recent().is_empty(), "the other run remembers nothing");
        assert_eq!(second.next_rotation(), 0, "the other run has not rotated");

        // The memory is bounded: it keeps the newest cells and drops the
        // oldest, and re-remembering a cell does not grow it.
        let mut memory = ExploreMemory::default();
        for i in 0..(RECENT_EXPLORED_CELLS as i32 + 5) {
            memory.remember(at(i, 64, 0));
        }
        let recent = memory.recent();
        assert_eq!(recent.len(), RECENT_EXPLORED_CELLS);
        assert_eq!(recent[0], at(5, 64, 0), "the five oldest cells were dropped");
        assert_eq!(
            recent[RECENT_EXPLORED_CELLS - 1],
            at(RECENT_EXPLORED_CELLS as i32 + 4, 64, 0),
            "the newest cell is last"
        );
        memory.remember(at(5, 64, 0));
        assert_eq!(memory.recent().len(), RECENT_EXPLORED_CELLS);
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
