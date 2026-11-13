//! Shared helpers: idle frames, aiming, the main-hand select, tool choice and
//! perception queries used by more than one primitive.

use azalea_core::position::{BlockPos, Vec3};
use azalea_inventory::ItemStack;
use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_gamedata as gamedata;
use praxion_headless::{ControlFrame, Held, Look, MenuAction, MenuMode};
use praxion_perception::{BlockFilter, BlockRegion, Caller, Perception, scaffold};
use praxion_planner::{Inventory as PlanInventory, WALK_TOLERANCE_DEG, center, look_towards};
use praxion_world::{BlockDescription, BlockState, World};

use crate::runner::Ctx;

pub(crate) fn idle(ctx: &Ctx<'_>) -> ControlFrame {
    ControlFrame::new(ctx.tick, ctx.body.generation(), Held::NONE)
}

pub(crate) fn aim(ctx: &Ctx<'_>, target: Vec3) -> (Look, bool) {
    let eye = ctx.body.eye_position();
    look_towards(ctx.body.look(), eye, target, WALK_TOLERANCE_DEG)
}

/// Aim for an interaction: the client's crosshair must hit the intended cell,
/// and the general 4-degree walk tolerance can be off by most of a block at
/// four blocks of reach. The request is absolute and unlimited; the driver's
/// gaze applies the human speed, so the body look only reaches it after a few
/// ticks. The click may go out once the current angle is within
/// [`INTERACT_TOLERANCE_DEG`].
pub(crate) fn aim_precise(ctx: &Ctx<'_>, target: Vec3) -> (Look, bool) {
    let eye = ctx.body.eye_position();
    let (yaw, pitch) = praxion_planner::aim(eye, target);
    let current = ctx.body.look();
    let aligned = praxion_planner::wrap_degrees(yaw - current.y_rot()).abs()
        <= INTERACT_TOLERANCE_DEG
        && (pitch - current.x_rot()).abs() <= INTERACT_TOLERANCE_DEG;
    (Look::Aim { yaw, pitch }, aligned)
}

/// The aim error a dig or place accepts before it clicks, in degrees. At four
/// blocks this is under a tenth of a block of crosshair error.
const INTERACT_TOLERANCE_DEG: f32 = 0.25;

pub(crate) fn item_name(stack: &ItemStack) -> &str {
    stack.kind().to_str()
}

pub(crate) fn item_matches(stack: &ItemStack, wanted: &str) -> bool {
    if let Some(tag) = wanted.strip_prefix('#') {
        gamedata::item_in_tag(item_name(stack), tag)
    } else {
        gamedata::normalize(item_name(stack)) == gamedata::normalize(wanted)
    }
}

/// Owned items only: slot 0, the 2x2 grid's result, is a preview (see
/// `praxion_facts::item_count`).
pub(crate) fn count_item(world: &World, wanted: &str) -> u32 {
    world
        .inventory()
        .iter()
        .skip(1)
        .filter(|stack| item_matches(stack, wanted))
        .map(|stack| stack.count().max(0) as u32)
        .sum()
}

pub(crate) fn find_item(world: &World, wanted: &str) -> Option<(usize, ItemStack)> {
    world
        .inventory()
        .iter()
        .enumerate()
        .find(|(_, stack)| item_matches(stack, wanted) && stack.count() > 0)
        .map(|(index, stack)| (index, stack.clone()))
}

/// The hotbar index (0 to 8) holding `wanted`, if any.
pub(crate) fn hotbar_index(world: &World, wanted: &str) -> Option<u8> {
    (36..=44)
        .find(|index| {
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
pub(crate) struct HandSelect {
    /// Ticks since the last swap was sent, while it has not shown yet.
    swap_waited: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selecting {
    /// The item is in the main hand.
    Held,
    /// A hotbar key or a swap is on its way.
    Pending,
    /// The item is nowhere a click can reach (hotbar or main inventory).
    Unavailable,
}

impl HandSelect {
    pub(crate) fn tick(
        &mut self,
        world: &World,
        wanted: &str,
        frame: &mut ControlFrame,
    ) -> Selecting {
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
pub(crate) enum ToolChoice {
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
pub(crate) fn choose_tool(
    state: BlockState,
    inventory: &[ItemStack],
    held: &ItemStack,
) -> ToolChoice {
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
            ToolChoice::Select(gamedata::normalize(item_name(stack)).to_owned())
        }
        _ if !held_rank.0 => ToolChoice::Missing,
        _ => ToolChoice::Keep,
    }
}

/// How long a dig tries to bring its tool into the main hand before it digs
/// with what it holds (a swap waits for the server's slot update).
const TOOL_SELECT_TICKS: u32 = 40;

/// What preparing the tool for one dig decided for this tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolStep {
    /// The right tool is in the hand (or the best available is): click.
    Ready,
    /// A hotbar key or a swap is on its way: the frame carries it.
    Pending,
    /// The block drops nothing with anything the body carries.
    Missing,
}

/// Hold the best tool for `state` before the first attack click of a target; a
/// dig that is already under way keeps its item, because switching restarts
/// the break. Stone mined by hand breaks and drops nothing, which is how a
/// fresh-spawn body mined 21 stone and collected no cobblestone. `waited`
/// counts the ticks spent selecting for this target.
pub(crate) fn hold_tool(
    ctx: &Ctx<'_>,
    state: BlockState,
    hand: &mut HandSelect,
    waited: &mut u32,
    frame: &mut ControlFrame,
) -> ToolStep {
    let held = praxion_headless::held_item(ctx.world);
    match choose_tool(state, ctx.world.inventory(), &held) {
        ToolChoice::Keep => ToolStep::Ready,
        ToolChoice::Missing => ToolStep::Missing,
        ToolChoice::Select(item) => {
            *waited = waited.saturating_add(1);
            let selecting = hand.tick(ctx.world, &item, frame);
            if selecting != Selecting::Unavailable && *waited <= TOOL_SELECT_TICKS {
                return ToolStep::Pending;
            }
            // The better tool cannot be brought into the hand: dig with the
            // held item only if the block still drops with it.
            if !praxion_headless::tool_fit(state, &held).drops {
                return ToolStep::Missing;
            }
            *frame = idle(ctx);
            ToolStep::Ready
        }
    }
}

pub(crate) fn held_matches(world: &World, wanted: &str) -> bool {
    let slot = world.selected_hotbar_slot().unwrap_or(0) as usize;
    world
        .inventory()
        .get(36 + slot)
        .is_some_and(|stack| item_matches(stack, wanted))
}

/// Where to walk while a known cell is out of sight. A cell under the body's
/// own column is only ever seen through the floor, which is never dug, so the
/// body first steps two blocks aside (away from the cell) and looks from there
/// (seed 202 live run 2026-11-08: stone two below the feet, 80 ticks, no_block).
pub(crate) fn walk_point(feet: BlockPos, cell: BlockPos) -> Vec3 {
    let (dx, dz) = (feet.x - cell.x, feet.z - cell.z);
    if cell.y < feet.y && dx.abs() <= 1 && dz.abs() <= 1 {
        let (sx, sz) = match (dx.signum(), dz.signum()) {
            (0, 0) => (1, 0),
            away => away,
        };
        return center(BlockPos::new(cell.x + 2 * sx, feet.y, cell.z + 2 * sz));
    }
    center(cell)
}

/// Steer toward a drop: aim first, then walk, and jump when it is above.
pub(crate) fn pickup_frame(ctx: &Ctx<'_>, target: Vec3) -> ControlFrame {
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

/// Offsets from a cell's centre to the six face points, inset 0.05 into the
/// cell so a ray aimed there still enters it.
const FACE_OFFSETS: [(f64, f64, f64); 6] = [
    (-0.45, 0.0, 0.0),
    (0.45, 0.0, 0.0),
    (0.0, -0.45, 0.0),
    (0.0, 0.45, 0.0),
    (0.0, 0.0, -0.45),
    (0.0, 0.0, 0.45),
];

/// Where the crosshair can land on one cell from where the body stands.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct CellAim {
    /// A point whose pick lands on the cell: its centre, else the nearest inset
    /// face point that does. `None` when no point of the cell can be picked.
    pub(crate) point: Option<Vec3>,
    /// The cell the pick toward the cell's centre lands on (`None` for a miss).
    /// When `point` is `None` this is what hides the cell.
    pub(crate) centre_hit: Option<BlockPos>,
}

/// Where to aim at `cell`. A player clicks the face they can see: try the
/// cell's centre, then the centre of each face (inset a little), nearest to the
/// eye first, and take the first whose pick lands on the cell. The centre ray
/// can enter another cell first (a roof over a table next to the body, a log
/// behind a leaf corner), which only a face point gets around.
///
/// This asks the same pick the click will use (`pick_block_looking`, the
/// client's crosshair), so a chosen point is one the click really lands on;
/// perception's own visibility test may accept a point this pick does not.
pub(crate) fn choose_aim(ctx: &Ctx<'_>, cell: BlockPos) -> CellAim {
    let eye = ctx.body.eye_position();
    let hit_at = |point: Vec3| {
        let (yaw, pitch) = praxion_planner::aim(eye, point);
        praxion_headless::pick_block_looking(ctx.world, ctx.body, yaw, pitch)
            .filter(|hit| !hit.miss)
            .map(|hit| hit.block_pos)
    };
    let centre = cell_centre(cell);
    let centre_hit = hit_at(centre);
    if centre_hit == Some(cell) {
        return CellAim {
            point: Some(centre),
            centre_hit,
        };
    }
    let mut faces: Vec<Vec3> = FACE_OFFSETS
        .iter()
        .map(|(dx, dy, dz)| Vec3::new(centre.x + dx, centre.y + dy, centre.z + dz))
        .collect();
    faces.sort_by(|a, b| eye.distance_to(*a).total_cmp(&eye.distance_to(*b)));
    let point = faces.into_iter().find(|point| hit_at(*point) == Some(cell));
    CellAim { point, centre_hit }
}

/// The true centre of a cell. `praxion_planner::center` is the centre of the
/// cell's floor (the point a body standing in it occupies), which aimed
/// `place` at a side face's bottom edge.
pub(crate) fn cell_centre(pos: BlockPos) -> Vec3 {
    Vec3::new(
        f64::from(pos.x) + 0.5,
        f64::from(pos.y) + 0.5,
        f64::from(pos.z) + 0.5,
    )
}

/// The nearest visible block of `wanted` within `radius`.
pub(crate) fn block_target(ctx: &mut Ctx<'_>, wanted: &str, radius: u32) -> Option<BlockPos> {
    block_target_where(ctx, wanted, radius, |_, _, _| true)
}

/// The nearest visible `wanted` block within `radius` that `accept` allows for
/// its cell and state.
pub(crate) fn block_target_where(
    ctx: &mut Ctx<'_>,
    wanted: &str,
    radius: u32,
    accept: impl Fn(&Perception, BlockPos, BlockState) -> bool,
) -> Option<BlockPos> {
    if let Some(tag) = wanted.strip_prefix('#') {
        let eye = ctx.perception.eye_cell(ctx.world)?;
        let region = BlockRegion::around(eye, radius);
        let answer = ctx
            .perception
            .blocks_in(Caller::Other, ctx.world, region, BlockFilter::Any);
        let blocks = answer.value()?;
        return blocks
            .iter()
            .filter(|fact| {
                gamedata::block_in_tag(fact.kind.to_str(), tag)
                    && !scaffold::is_scaffold(fact.pos)
                    && accept(&*ctx.perception, fact.pos, fact.state)
            })
            .min_by(|a, b| a.distance.total_cmp(&b.distance))
            .map(|fact| fact.pos);
    }
    let kind = wanted.parse::<BlockKind>().ok()?;
    // The body's own scaffolding is never a target
    // (`praxion_perception::scaffold`).
    let accept = |perception: &Perception, pos: BlockPos, state: BlockState| {
        !scaffold::is_scaffold(pos) && accept(perception, pos, state)
    };
    let nearest = ctx
        .perception
        .nearest_block(Caller::Other, ctx.world, kind, radius)
        .value()
        .map(|fact| (fact.pos, fact.state));
    if let Some((pos, state)) = nearest
        && accept(&*ctx.perception, pos, state)
    {
        return Some(pos);
    }
    // The nearest one is refused: look at every visible one in the radius.
    let eye = ctx.perception.eye_cell(ctx.world)?;
    let region = BlockRegion::around(eye, radius);
    let answer = ctx
        .perception
        .blocks_in(Caller::Other, ctx.world, region, BlockFilter::Any);
    answer
        .value()?
        .iter()
        .filter(|fact| fact.kind == kind && accept(&*ctx.perception, fact.pos, fact.state))
        .min_by(|a, b| a.distance.total_cmp(&b.distance))
        .map(|fact| fact.pos)
}

pub(crate) fn entity_target(ctx: &mut Ctx<'_>, wanted: &str, radius: u32) -> Option<(i32, Vec3)> {
    let kind = wanted.parse::<EntityKind>().ok()?;
    ctx.perception
        .nearest_entity(Caller::Other, ctx.world, kind, radius)
        .value()
        .map(|fact| (fact.id, fact.position))
}

/// The own food level for a plan's sprint gate, or `None` before the server
/// said (a movement nicety, not a safety rule: unknown allows the sprint).
pub(crate) fn plan_food(ctx: &mut Ctx<'_>) -> Option<u32> {
    ctx.perception.own_food(ctx.world).value().copied()
}

pub(crate) fn plan_inventory(world: &World) -> PlanInventory {
    let hotbar = (36..=44)
        .filter_map(|index| world.inventory().get(index))
        .filter(|stack| !stack.is_empty())
        .map(|stack| praxion_planner::HotbarItem {
            item: gamedata::normalize(item_name(stack)).to_owned(),
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

    fn inventory_with(items: &[(usize, azalea_registry::builtin::ItemKind)]) -> Vec<ItemStack> {
        let mut inventory = vec![ItemStack::Empty; 46];
        for (slot, kind) in items {
            inventory[*slot] = ItemStack::new(*kind, 1);
        }
        inventory
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
        let inventory =
            inventory_with(&[(37, ItemKind::WoodenPickaxe), (38, ItemKind::StonePickaxe)]);
        assert_eq!(
            choose_tool(stone, &inventory, &ItemStack::Empty),
            ToolChoice::Select("stone_pickaxe".into())
        );
        // Already holding it: nothing to do.
        assert_eq!(
            choose_tool(stone, &inventory, &inventory[38]),
            ToolChoice::Keep
        );
    }

    #[test]
    fn an_axe_beats_the_held_pickaxe_on_a_log_and_a_shovel_beats_it_on_dirt_sand_and_gravel() {
        // Decision 2026-11-09 "Five next areas": the right tool, held or in
        // the inventory. A pickaxe is no faster than a hand on wood or earth.
        use azalea_registry::builtin::ItemKind;
        let log = BlockState::from(BlockKind::OakLog);
        let inventory = inventory_with(&[(20, ItemKind::StoneAxe), (36, ItemKind::WoodenPickaxe)]);
        assert_eq!(
            choose_tool(log, &inventory, &inventory[36]),
            ToolChoice::Select("stone_axe".into()),
            "an axe in the main inventory, a pickaxe in hand"
        );
        // Holding the axe already: nothing to do, and a worse axe is not swapped in.
        let inventory = inventory_with(&[(36, ItemKind::StoneAxe), (37, ItemKind::WoodenAxe)]);
        assert_eq!(choose_tool(log, &inventory, &inventory[36]), ToolChoice::Keep);

        for earth in [BlockKind::Dirt, BlockKind::Sand, BlockKind::Gravel] {
            let state = BlockState::from(earth);
            let inventory =
                inventory_with(&[(21, ItemKind::WoodenShovel), (36, ItemKind::StonePickaxe)]);
            assert_eq!(
                choose_tool(state, &inventory, &inventory[36]),
                ToolChoice::Select("wooden_shovel".into()),
                "{earth:?}"
            );
        }
        // A shovel is no help on a log: the axe wins there.
        let inventory = inventory_with(&[(20, ItemKind::WoodenShovel), (21, ItemKind::WoodenAxe)]);
        assert_eq!(
            choose_tool(log, &inventory, &ItemStack::Empty),
            ToolChoice::Select("wooden_axe".into())
        );
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
        assert_eq!(
            choose_tool(table, &inventory, &ItemStack::Empty),
            ToolChoice::Keep
        );
        // An axe speeds a log up.
        let log = BlockState::from(BlockKind::OakLog);
        let inventory = inventory_with(&[(36, ItemKind::WoodenPickaxe), (37, ItemKind::WoodenAxe)]);
        assert_eq!(
            choose_tool(log, &inventory, &inventory[36]),
            ToolChoice::Select("wooden_axe".into())
        );
    }

    #[test]
    fn hand_select_sends_one_swap_then_resends_after_the_wait() {
        use azalea_registry::builtin::ItemKind;
        use crate::test_world::{give, hold, world};

        let mut world = world();
        hold(&mut world, ItemKind::OakPlanks);
        give(&mut world, 20, ItemKind::WoodenPickaxe, 1);
        let mut hand = HandSelect::default();

        let mut frame = ControlFrame::new(0, 0, Held::NONE);
        assert_eq!(
            hand.tick(&world, "wooden_pickaxe", &mut frame),
            Selecting::Pending
        );
        assert!(frame.menu.is_some(), "the first tick sends the swap");
        assert_eq!(frame.select_slot, Some(0));

        for tick in 1..=SWAP_WAIT_TICKS {
            let mut frame = ControlFrame::new(u64::from(tick), 0, Held::NONE);
            assert_eq!(
                hand.tick(&world, "wooden_pickaxe", &mut frame),
                Selecting::Pending
            );
            assert!(
                frame.menu.is_none(),
                "no new swap while waiting at tick {tick}"
            );
        }

        let mut frame = ControlFrame::new(u64::from(SWAP_WAIT_TICKS + 1), 0, Held::NONE);
        assert_eq!(
            hand.tick(&world, "wooden_pickaxe", &mut frame),
            Selecting::Pending
        );
        assert!(
            frame.menu.is_some(),
            "the swap is sent again after {SWAP_WAIT_TICKS} ticks without it landing"
        );
    }

    #[test]
    fn hold_tool_falls_back_to_the_held_item_after_the_tool_wait() {
        use azalea_registry::builtin::ItemKind;
        use crate::test_world::{give, hold, world as make_world};
        use praxion_headless::Body;
        use praxion_perception::Perception;

        // A pickaxe sits in the main inventory but the swap never lands (the
        // server sends no slot update). After the tool wait the dig holds the
        // planks; stone then drops nothing, so it is `needs_tool`.
        let mut world = make_world();
        hold(&mut world, ItemKind::OakPlanks);
        give(&mut world, 20, ItemKind::WoodenPickaxe, 1);
        let stone = BlockState::from(BlockKind::Stone);
        let body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        let mut perception = Perception::new(praxion_perception::Config::default());
        let mut hand = HandSelect::default();
        let mut waited = 0;
        for tick in 0..TOOL_SELECT_TICKS {
            let mut frame = ControlFrame::new(u64::from(tick), body.generation(), Held::NONE);
            let step = {
                let ctx = Ctx {
                    tick: u64::from(tick),
                    body: &body,
                    world: &world,
                    perception: &mut perception,
                    style: praxion_planner::Personality::default(),
                };
                hold_tool(&ctx, stone, &mut hand, &mut waited, &mut frame)
            };
            assert_eq!(step, ToolStep::Pending, "the pickaxe is still on its way");
        }
        let mut frame = ControlFrame::new(
            u64::from(TOOL_SELECT_TICKS),
            body.generation(),
            Held::NONE,
        );
        let step = {
            let ctx = Ctx {
                tick: u64::from(TOOL_SELECT_TICKS),
                body: &body,
                world: &world,
                perception: &mut perception,
                style: praxion_planner::Personality::default(),
            };
            hold_tool(&ctx, stone, &mut hand, &mut waited, &mut frame)
        };
        assert_eq!(
            step,
            ToolStep::Missing,
            "planks never drop stone, so the fallback refuses"
        );
        assert!(frame.menu.is_none(), "the fallback frame carries no swap");

        // A log still drops with the item already in hand, so the fallback
        // digs with it instead of failing.
        let mut log_world = make_world();
        hold(&mut log_world, ItemKind::StonePickaxe);
        give(&mut log_world, 20, ItemKind::WoodenAxe, 1);
        let log = BlockState::from(BlockKind::OakLog);
        let mut hand = HandSelect::default();
        let mut waited = 0;
        for tick in 0..TOOL_SELECT_TICKS {
            let mut frame = ControlFrame::new(u64::from(tick), body.generation(), Held::NONE);
            let step = {
                let ctx = Ctx {
                    tick: u64::from(tick),
                    body: &body,
                    world: &log_world,
                    perception: &mut perception,
                    style: praxion_planner::Personality::default(),
                };
                hold_tool(&ctx, log, &mut hand, &mut waited, &mut frame)
            };
            assert_eq!(step, ToolStep::Pending, "the axe is still on its way");
        }
        let mut frame = ControlFrame::new(
            u64::from(TOOL_SELECT_TICKS),
            body.generation(),
            Held::NONE,
        );
        let step = {
            let ctx = Ctx {
                tick: u64::from(TOOL_SELECT_TICKS),
                body: &body,
                world: &log_world,
                perception: &mut perception,
                style: praxion_planner::Personality::default(),
            };
            hold_tool(&ctx, log, &mut hand, &mut waited, &mut frame)
        };
        assert_eq!(
            step,
            ToolStep::Ready,
            "the pickaxe in hand still drops the log"
        );
        assert!(frame.menu.is_none(), "the fallback frame carries no swap");
    }
}
