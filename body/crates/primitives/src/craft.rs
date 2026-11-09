//! `craft`: open the table if needed, click the recipe into the grid, then
//! shift-click the result and close the menu.

use std::collections::{HashMap, VecDeque};

use azalea_inventory::ItemStack;
use azalea_registry::builtin::{BlockKind, MenuKind};
use praxion_gamedata as gamedata;
use praxion_headless::{ControlFrame, MenuAction, MenuMode};
use praxion_interpreter::Outcome;
use praxion_world::World;

use crate::common::{count_item, idle, item_name};
use crate::runner::{Ctx, IntentSpec};
use crate::station::{Station, StationStep};

#[derive(Debug)]
pub(crate) struct Craft {
    output: String,
    recipe: Option<&'static gamedata::Recipe>,
    stage: CraftStage,
    clicks: VecDeque<MenuAction>,
    wait: u32,
    /// Walks to the crafting table and opens it.
    station: Station,
    /// The table menu has been seen; `wait` restarted then, so the wait for
    /// its content does not inherit the ticks spent reaching the table.
    menu_seen: bool,
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
            station: Station::new(BlockKind::CraftingTable, MenuKind::Crafting, "needs_table"),
            menu_seen: false,
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
            match self.station.tick(ctx) {
                StationStep::Pending(station_frame) => return (station_frame, None, None),
                StationStep::Failed(kind) => {
                    return (frame, None, Some(Outcome::Failed(kind.into())));
                }
                // No menu is open here, so the station cannot report it open.
                StationStep::Opened => {}
            }
        }
        if self.clicks.is_empty() && self.stage == CraftStage::Open {
            // Wait for the server's full container content before planning
            // clicks; `open_screen` arrives before `container_set_content`.
            if let Some(container) = ctx.world.open_container()
                && container.slots().len() < 46
            {
                if !self.menu_seen {
                    self.menu_seen = true;
                    self.wait = 0;
                }
                if self.wait > 40 {
                    return (frame, None, Some(Outcome::Failed("no_menu".into())));
                }
                return (frame, None, None);
            }
            let Some(view) = menu_view(ctx.world) else {
                return (frame, None, Some(Outcome::Failed("no_menu".into())));
            };
            let Some(clicks) = build_craft_clicks(
                recipe,
                &view.slots,
                view.window,
                &view.grid,
                view.grid_width,
                &view.player,
            ) else {
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
                if self.wait.is_multiple_of(2) {
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
struct MenuView {
    window: u32,
    slots: Vec<ItemStack>,
    grid: Vec<usize>,
    grid_width: usize,
    player: Vec<usize>,
}

fn menu_view(world: &World) -> Option<MenuView> {
    if let Some(container) = world.open_container() {
        let (grid, width) = match container.kind() {
            azalea_registry::builtin::MenuKind::Crafting => ((1..=9).collect(), 3),
            _ => ((1..=4).collect(), 2),
        };
        let slots = container.slots().to_vec();
        let player: Vec<usize> = (10..slots.len().max(10)).collect();
        Some(MenuView {
            window: container.id().max(0) as u32,
            slots,
            grid,
            grid_width: width,
            player,
        })
    } else {
        let slots = world.inventory().to_vec();
        Some(MenuView {
            window: 0,
            slots,
            grid: (1..=4).collect(),
            grid_width: 2,
            player: (9..=44).collect(),
        })
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
                    if let Some(ingredient) = pattern[row * usize::from(*width) + col]
                        && let Some(slot) = grid.get(row * grid_width + col)
                    {
                        cells.push((*slot, ingredient));
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

/// Pick up the stack in `source`, right-click one item into each of `targets`,
/// and put the rest back. A furnace menu uses the same triple (input slot 0,
/// fuel slot 1).
pub(crate) fn spread_one_each(window: u32, source: usize, targets: &[usize]) -> Vec<MenuAction> {
    let click = |slot: usize, button: i8| MenuAction {
        window,
        slot: slot as i16,
        button,
    };
    let mut clicks = vec![click(source, 0)];
    clicks.extend(targets.iter().map(|slot| click(*slot, 1)));
    clicks.push(click(source, 0));
    clicks
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
        let mut targets = Vec::new();
        while index < cells.len()
            && cells[index].1.effect_item() == effect
            && remaining.get(&source).copied().unwrap_or(0) > targets.len() as i32
        {
            targets.push(cells[index].0);
            index += 1;
        }
        let used = targets.len() as i32;
        clicks.extend(spread_one_each(window, source, &targets));
        *remaining.get_mut(&source)? -= used;
    }
    Some(clicks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_world::{feet, give, open_menu, set_block, set_menu_content, swept, world};
    use azalea_core::position::BlockPos;
    use azalea_registry::builtin::ItemKind;
    use praxion_headless::Body;
    use praxion_perception::Perception;
    use praxion_planner::Personality;
    use praxion_world::BlockState;

    type Tick = (ControlFrame, Option<IntentSpec>, Option<Outcome>);

    /// One craft tick; the frame's look is applied to the body as the driver
    /// would (the body never moves).
    fn run(
        craft: &mut Craft,
        world: &World,
        perception: &mut Perception,
        body: &mut Body,
        tick: u64,
    ) -> Tick {
        let mut ctx = Ctx {
            tick,
            body,
            world,
            perception,
            style: Personality::default(),
        };
        let result = craft.tick(&mut ctx, false);
        result
            .0
            .apply_with_menu(body, tick)
            .expect("frame applies");
        result
    }

    /// A full 46 slot crafting-table menu with `items` in player slots.
    fn menu_slots(items: &[(usize, ItemKind, i32)]) -> Vec<ItemStack> {
        let mut slots = vec![ItemStack::Empty; 46];
        for (slot, kind, count) in items {
            slots[*slot] = ItemStack::new(*kind, *count);
        }
        slots
    }

    fn pickaxe_world() -> World {
        let mut world = world();
        give(&mut world, 9, ItemKind::OakPlanks, 3);
        give(&mut world, 10, ItemKind::Stick, 2);
        world
    }

    fn is_close(frame: &ControlFrame, window: u32) -> bool {
        frame.menu_mode == MenuMode::Close
            && frame.menu.is_some_and(|menu| menu.window == window)
    }

    #[test]
    fn a_table_behind_stone_is_walked_toward_and_fails_at_the_cap() {
        let mut world = pickaxe_world();
        set_block(
            &mut world,
            BlockPos::new(0, 65, 3),
            BlockState::from(BlockKind::CraftingTable),
        );
        let mut perception = swept(&world);
        // The map knows the table; a stone block then appears on the line to
        // it, so the pick cannot reach the cell.
        set_block(
            &mut world,
            BlockPos::new(0, 65, 2),
            BlockState::from(BlockKind::Stone),
        );
        let mut body = Body::new(feet());
        let mut craft = Craft::new("wooden_pickaxe".to_owned()).unwrap();
        // Tick 0 returns the grid items, tick 1 turns the look, ticks after
        // that walk. The station gives up on its 81st walking tick; the close
        // path swallows that tick and reports one tick later.
        let mut reported = None;
        for tick in 0..100u64 {
            let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, tick);
            assert!(!frame.clicks.contains(&praxion_headless::Click::Use));
            if outcome.is_some() {
                reported = Some((tick, frame, outcome));
                break;
            }
        }
        let (tick, frame, outcome) = reported.expect("the craft gives up");
        assert_eq!(outcome, Some(Outcome::Failed("needs_table".into())));
        assert!((82..=84).contains(&tick), "reported at tick {tick}");
        assert!(is_close(&frame, 0), "the close path still runs");
    }

    #[test]
    fn an_adjacent_table_gets_a_use_click_on_the_first_aligned_tick() {
        let mut world = pickaxe_world();
        set_block(
            &mut world,
            BlockPos::new(0, 65, 2),
            BlockState::from(BlockKind::CraftingTable),
        );
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut craft = Craft::new("wooden_pickaxe".to_owned()).unwrap();
        let mut used_at = None;
        for tick in 0..6u64 {
            let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, tick);
            assert!(outcome.is_none());
            if used_at.is_none() && frame.clicks.contains(&praxion_headless::Click::Use) {
                used_at = Some(tick);
            }
        }
        assert!(used_at.is_some_and(|tick| tick <= 3), "got {used_at:?}");
    }

    #[test]
    fn a_menu_that_opens_after_tick_45_does_not_fail_no_menu() {
        let mut world = pickaxe_world();
        set_block(
            &mut world,
            BlockPos::new(0, 65, 2),
            BlockState::from(BlockKind::CraftingTable),
        );
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut craft = Craft::new("wooden_pickaxe".to_owned()).unwrap();
        for tick in 0..=45u64 {
            let (_, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, tick);
            assert!(outcome.is_none(), "tick {tick}");
        }
        open_menu(&mut world, 3, MenuKind::Crafting);
        // The screen is open but its content has not arrived.
        let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, 46);
        assert!(outcome.is_none() && frame.menu.is_none());
        set_menu_content(
            &mut world,
            3,
            menu_slots(&[(10, ItemKind::OakPlanks, 3), (11, ItemKind::Stick, 2)]),
        );
        let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, 47);
        assert!(outcome.is_none(), "got {outcome:?}");
        let click = frame.menu.expect("the first recipe click goes out");
        assert_eq!(click.window, 3);
    }

    /// A craft of `stick` (2x2, no table) whose window-0 grid slot 1 holds
    /// planks.
    fn grid_world() -> World {
        let mut world = world();
        give(&mut world, 9, ItemKind::OakPlanks, 4);
        give(&mut world, 1, ItemKind::OakPlanks, 1);
        world
    }

    #[test]
    fn a_busy_grid_is_closed_once_then_fails_grid_busy() {
        let world = grid_world();
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut craft = Craft::new("stick".to_owned()).unwrap();
        let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, 0);
        assert!(is_close(&frame, 0), "the first tick closes window 0");
        assert!(outcome.is_none());
        // The server never returns the item: 20 ticks of waiting, the failure
        // is swallowed once by the close path, then reported.
        for tick in 1..=21u64 {
            let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, tick);
            assert!(outcome.is_none(), "tick {tick}");
            assert!(frame.menu.is_none(), "tick {tick}: no second close while waiting");
        }
        let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, 22);
        assert_eq!(outcome, Some(Outcome::Failed("grid_busy".into())));
        assert!(is_close(&frame, 0));
    }

    #[test]
    fn a_grid_that_empties_lets_the_craft_plan_its_clicks() {
        let mut world = grid_world();
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut craft = Craft::new("stick".to_owned()).unwrap();
        let (frame, _, _) = run(&mut craft, &world, &mut perception, &mut body, 0);
        assert!(is_close(&frame, 0));
        for tick in 1..=3u64 {
            let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, tick);
            assert!(outcome.is_none() && frame.menu.is_none());
        }
        // The server returned the grid items.
        give(&mut world, 1, ItemKind::OakPlanks, 0);
        let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, 4);
        assert!(outcome.is_none());
        assert_eq!(frame.menu_mode, MenuMode::Click);
        assert!(frame.menu.is_some(), "the first recipe click goes out");
    }

    #[test]
    fn a_craft_cut_short_with_the_grid_in_use_closes_window_0_once() {
        let mut world = world();
        give(&mut world, 9, ItemKind::OakPlanks, 4);
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut craft = Craft::new("stick".to_owned()).unwrap();
        let mut tick = 0u64;
        let first_click = loop {
            let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, tick);
            assert!(outcome.is_none());
            tick += 1;
            if frame.menu_mode == MenuMode::Click && frame.menu.is_some() {
                break frame;
            }
            assert!(tick < 10, "no click planned");
        };
        assert_eq!(first_click.menu.map(|menu| menu.window), Some(0));
        // The click put an item into the grid.
        give(&mut world, 1, ItemKind::OakPlanks, 1);
        craft.request_close(Outcome::Preempted);
        let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, tick);
        assert!(is_close(&frame, 0));
        assert_eq!(outcome, Some(Outcome::Preempted));
        let (frame, _, outcome) = run(&mut craft, &world, &mut perception, &mut body, tick + 1);
        assert!(frame.menu.is_none(), "the close is sent once");
        assert_eq!(outcome, Some(Outcome::Preempted));
    }

    #[test]
    fn a_table_menu_always_closes_whatever_the_outcome() {
        for outcome in [Outcome::Failed("no_result".into()), Outcome::Confirmed] {
            let mut world = pickaxe_world();
            open_menu(&mut world, 5, MenuKind::Crafting);
            set_menu_content(
                &mut world,
                5,
                menu_slots(&[(10, ItemKind::OakPlanks, 3), (11, ItemKind::Stick, 2)]),
            );
            let mut perception = swept(&world);
            let mut body = Body::new(feet());
            let mut craft = Craft::new("wooden_pickaxe".to_owned()).unwrap();
            let (frame, _, settled) = run(&mut craft, &world, &mut perception, &mut body, 0);
            assert!(settled.is_none() && frame.menu.is_some(), "a click is planned");
            craft.request_close(outcome.clone());
            let (frame, _, settled) = run(&mut craft, &world, &mut perception, &mut body, 1);
            assert!(is_close(&frame, 5), "the table window closes for {outcome:?}");
            assert_eq!(settled, Some(outcome));
        }
    }
}
