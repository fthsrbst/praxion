//! Shared helpers: idle frames, aiming, the main-hand select, tool choice and
//! perception queries used by more than one primitive.

use azalea_core::position::{BlockPos, Vec3};
use azalea_inventory::ItemStack;
use azalea_registry::builtin::{BlockKind, EntityKind};
use praxion_gamedata as gamedata;
use praxion_headless::{ControlFrame, Held, Look, MenuAction, MenuMode};
use praxion_perception::{BlockFilter, BlockRegion, Caller};
use praxion_planner::{Inventory as PlanInventory, WALK_TOLERANCE_DEG, look_towards};
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

fn item_matches(stack: &ItemStack, wanted: &str) -> bool {
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

pub(crate) fn held_matches(world: &World, wanted: &str) -> bool {
    let slot = world.selected_hotbar_slot().unwrap_or(0) as usize;
    world
        .inventory()
        .get(36 + slot)
        .is_some_and(|stack| item_matches(stack, wanted))
}

/// The nearest visible block of `wanted` within `radius`.
pub(crate) fn block_target(ctx: &mut Ctx<'_>, wanted: &str, radius: u32) -> Option<BlockPos> {
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

pub(crate) fn entity_target(ctx: &mut Ctx<'_>, wanted: &str, radius: u32) -> Option<(i32, Vec3)> {
    let kind = wanted.parse::<EntityKind>().ok()?;
    ctx.perception
        .nearest_entity(Caller::Other, ctx.world, kind, radius)
        .value()
        .map(|fact| (fact.id, fact.position))
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
}
