//! `craft`: open the table if needed, click the recipe into the grid, then
//! shift-click the result and close the menu.

use std::collections::{HashMap, VecDeque};

use azalea_core::position::BlockPos;
use azalea_inventory::ItemStack;
use praxion_gamedata as gamedata;
use praxion_headless::{ControlFrame, MenuAction, MenuMode};
use praxion_interpreter::Outcome;
use praxion_world::World;

use crate::common::{aim_precise, block_target, count_item, idle, item_name};
use crate::frontier::nearest_known_block;
use crate::place::{cell_centre, place_support};
use crate::runner::{Ctx, IntentSpec};

#[derive(Debug)]
pub(crate) struct Craft {
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
    pub(crate) close_outcome: Option<Outcome>,
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
    pub(crate) fn new(output: String) -> Result<Self, String> {
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
    pub(crate) fn request_close(&mut self, outcome: Outcome) {
        self.close_outcome = Some(outcome);
    }

    pub(crate) fn tick(
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
                return (frame, None, Some(Outcome::Failed("no_recipe".into())));
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
                    return (frame, None, Some(Outcome::Failed("needs_table".into())));
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
                            && ctx
                                .world
                                .block_state(hit.block_pos)
                                .is_some_and(|state| !state.is_air() && !place_support(state)) =>
                    {
                        frame.clicks.push(praxion_headless::Click::Attack);
                    }
                    _ => {}
                }
            }
            if self.wait > 60 {
                return (frame, None, Some(Outcome::Failed("needs_table".into())));
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
                    return (frame, None, Some(Outcome::Failed("no_menu".into())));
                }
                return (frame, None, None);
            }
            let Some((window, slots, grid, grid_width, player)) = menu_view(ctx.world) else {
                return (frame, None, Some(Outcome::Failed("no_menu".into())));
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
                    return (frame, None, Some(Outcome::Failed("no_result".into())));
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
                    return (frame, None, Some(Outcome::Failed("no_result".into())));
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
