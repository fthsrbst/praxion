//! `smelt(item)`: cook ONE item in a furnace (decision 2026-11-09, "`smelt(item)`
//! cooks one item per call"). `item` is the output, as in `craft(item)`.
//!
//! The body reaches a furnace with the shared [`Station`], loads one input and
//! one fuel with the craft click triple (no shift-click: Azalea predicts a
//! shift-clicked fuel into the input slot), waits for the result slot, takes it
//! with a shift-click under a `StorageInventory` receipt and always closes the
//! menu. A call cut short leaves its items in the furnace; the next call reads
//! the menu and continues from what it finds.

use std::collections::VecDeque;

use azalea_inventory::ItemStack;
use azalea_registry::builtin::{BlockKind, MenuKind};
use praxion_gamedata as gamedata;
use praxion_headless::{ControlFrame, MenuAction, MenuMode};
use praxion_interpreter::Outcome;
use praxion_world::World;

use crate::common::{idle, item_matches, item_name};
use crate::craft::spread_one_each;
use crate::runner::{Ctx, IntentSpec};
use crate::station::{Station, StationStep};

/// Furnace menu: slot 0 input, 1 fuel, 2 result, then the 36 player slots.
const INPUT_SLOT: usize = 0;
const FUEL_SLOT: usize = 1;
const RESULT_SLOT: usize = 2;
const MENU_SLOTS: usize = 39;
const PLAYER_SLOTS: std::ops::RangeInclusive<usize> = 3..=38;

/// Ticks to wait for the server's full menu content.
const CONTENT_TICKS: u32 = 40;
/// Slack on top of the recipe's cooking time: the menu round trip.
const COOK_SLACK_TICKS: u32 = 60;
/// Ticks to wait for the take receipt (longer than its 120-tick window).
const TAKE_TICKS: u32 = 140;

/// Fuels in the order they are chosen. Each burns at least one smelt, so no
/// burn-time table is needed; planks come before logs (a quarter of a log for
/// the same burn time). Sticks burn half a smelt and are left out.
const FUELS: [&str; 4] = ["coal", "charcoal", "#planks", "#logs"];

#[derive(Debug)]
pub(crate) struct Smelt {
    output: String,
    stage: SmeltStage,
    station: Station,
    clicks: VecDeque<MenuAction>,
    /// Ticks spent in the current stage.
    wait: u32,
    /// Cooking time of the planned recipe, in ticks.
    cook_ticks: u32,
    close_sent: bool,
    /// The receipt settled (or the call failed) while the furnace may still be
    /// open. The close goes out next tick, then the call finishes.
    pub(crate) close_outcome: Option<Outcome>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SmeltStage {
    /// Reach the furnace and open it.
    Open,
    /// The menu is open; wait for its full content.
    WaitContent,
    /// Send the planned loading clicks.
    Clicks,
    /// Wait for the output to appear in the result slot.
    WaitCook,
    /// The shift-click is out; wait for the receipt.
    Take,
}

impl Smelt {
    pub(crate) fn new(output: String) -> Self {
        Self {
            output,
            stage: SmeltStage::Open,
            station: Station::new(BlockKind::Furnace, MenuKind::Furnace, "needs_furnace"),
            clicks: VecDeque::new(),
            wait: 0,
            cook_ticks: 0,
            close_sent: false,
            close_outcome: None,
        }
    }

    /// The receipt settled: close the furnace menu (if open), then finish.
    pub(crate) fn request_close(&mut self, outcome: Outcome) {
        self.close_outcome = Some(outcome);
    }

    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let (frame, pending, outcome) = self.tick_inner(ctx, started);
        // A failure leaves the furnace menu open just like a cut-short call:
        // route it through the close path (close next tick, then report).
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
            if !self.close_sent
                && let Some(container) = ctx.world.open_container()
            {
                frame.menu = Some(MenuAction {
                    window: container.id().max(0) as u32,
                    slot: 0,
                    button: 0,
                });
                frame.menu_mode = MenuMode::Close;
                self.close_sent = true;
            }
            return (frame, None, Some(outcome));
        }
        if gamedata::smelting_recipes_for(&self.output).next().is_none() {
            return (frame, None, Some(Outcome::Failed("no_recipe".into())));
        }
        // The server closes the window when the player walks too far: go back
        // to opening it. The plan is read again from the new menu, so what was
        // already loaded is not loaded twice. A take in flight keeps waiting
        // for its receipt.
        if self.stage != SmeltStage::Open
            && self.stage != SmeltStage::Take
            && furnace_menu(ctx.world).is_none()
        {
            self.stage = SmeltStage::Open;
            self.clicks.clear();
            self.wait = 0;
        }
        self.wait += 1;
        match self.stage {
            SmeltStage::Open => match self.station.tick(ctx) {
                StationStep::Pending(station_frame) => (station_frame, None, None),
                StationStep::Failed(kind) => (frame, None, Some(Outcome::Failed(kind.into()))),
                StationStep::Opened => {
                    self.stage = SmeltStage::WaitContent;
                    self.wait = 0;
                    (frame, None, None)
                }
            },
            SmeltStage::WaitContent => {
                let Some(menu) = furnace_menu(ctx.world) else {
                    return (frame, None, None);
                };
                if menu.slots.len() < MENU_SLOTS {
                    if self.wait > CONTENT_TICKS {
                        return (frame, None, Some(Outcome::Failed("no_menu".into())));
                    }
                    return (frame, None, None);
                }
                match self.plan(&menu) {
                    Ok(()) => (frame, None, None),
                    Err(kind) => (frame, None, Some(Outcome::Failed(kind.into()))),
                }
            }
            SmeltStage::Clicks => {
                if self.wait.is_multiple_of(2) {
                    if let Some(click) = self.clicks.pop_front() {
                        frame.menu = Some(click);
                        frame.menu_mode = MenuMode::Click;
                        return (frame, None, None);
                    }
                    self.stage = SmeltStage::WaitCook;
                    self.wait = 0;
                }
                (frame, None, None)
            }
            SmeltStage::WaitCook => {
                let Some(menu) = furnace_menu(ctx.world) else {
                    return (frame, None, None);
                };
                if holds_output(&menu.slots, &self.output) {
                    return self.take(frame, &menu, started);
                }
                if self.wait > self.cook_ticks + COOK_SLACK_TICKS {
                    return (frame, None, Some(Outcome::Failed("not_cooking".into())));
                }
                (frame, None, None)
            }
            SmeltStage::Take => {
                // The receipt settles through `request_close`; until then the
                // menu stays open and the shift-click is not repeated.
                if self.wait > TAKE_TICKS {
                    return (frame, None, Some(Outcome::Failed("no_result".into())));
                }
                (frame, None, None)
            }
        }
    }

    /// Shift-click the result out and register the storage receipt.
    fn take(
        &mut self,
        mut frame: ControlFrame,
        menu: &FurnaceMenu,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        frame.menu = Some(MenuAction {
            window: menu.window,
            slot: RESULT_SLOT as i16,
            button: 0,
        });
        frame.menu_mode = MenuMode::ShiftClick;
        self.stage = SmeltStage::Take;
        self.wait = 0;
        let pending = (!started).then(|| IntentSpec::StorageInventory {
            primitive: "smelt".to_owned(),
            item: self.output.clone(),
            delta: 1,
            window: 120,
        });
        (frame, pending, None)
    }

    /// Read the menu once and queue what is missing: nothing for an output
    /// already there, an input if slot 0 is empty, a fuel if slot 1 is empty.
    fn plan(&mut self, menu: &FurnaceMenu) -> Result<(), &'static str> {
        let recipes: Vec<&'static gamedata::SmeltingRecipe> =
            gamedata::smelting_recipes_for(&self.output).collect();
        self.clicks.clear();
        self.wait = 0;
        // Leftover from a preempted call: the output is already there.
        if holds_output(&menu.slots, &self.output) {
            self.cook_ticks = 0;
            self.stage = SmeltStage::WaitCook;
            return Ok(());
        }
        let mut remaining: Vec<i32> = menu.slots.iter().map(ItemStack::count).collect();
        let input = &menu.slots[INPUT_SLOT];
        if input.count() > 0 {
            // A valid leftover input is cooked; anything else blocks the slot.
            let recipe = recipes
                .iter()
                .find(|recipe| recipe.ingredient.matches(item_name(input)))
                .ok_or("furnace_busy")?;
            self.cook_ticks = recipe.cooking_time;
        } else {
            let (recipe, source) = recipes
                .iter()
                .find_map(|recipe| {
                    PLAYER_SLOTS
                        .clone()
                        .find(|slot| {
                            remaining[*slot] > 0 && recipe.ingredient.matches(item_name(&menu.slots[*slot]))
                        })
                        .map(|slot| (*recipe, slot))
                })
                .ok_or("nothing_to_smelt")?;
            self.cook_ticks = recipe.cooking_time;
            remaining[source] -= 1;
            self.clicks
                .extend(spread_one_each(menu.window, source, &[INPUT_SLOT]));
        }
        if menu.slots[FUEL_SLOT].count() == 0 {
            // `remaining` already lost the input item, so a stack that is both
            // input and fuel needs two items.
            let source = FUELS
                .iter()
                .find_map(|fuel| {
                    PLAYER_SLOTS
                        .clone()
                        .find(|slot| remaining[*slot] > 0 && item_matches(&menu.slots[*slot], fuel))
                })
                .ok_or("no_fuel")?;
            self.clicks
                .extend(spread_one_each(menu.window, source, &[FUEL_SLOT]));
        }
        self.stage = SmeltStage::Clicks;
        Ok(())
    }
}

/// The open furnace menu's id and slots.
struct FurnaceMenu {
    window: u32,
    slots: Vec<ItemStack>,
}

fn furnace_menu(world: &World) -> Option<FurnaceMenu> {
    let container = world.open_container()?;
    (container.kind() == MenuKind::Furnace).then(|| FurnaceMenu {
        window: container.id().max(0) as u32,
        slots: container.slots().to_vec(),
    })
}

fn holds_output(slots: &[ItemStack], output: &str) -> bool {
    slots.get(RESULT_SLOT).is_some_and(|stack| {
        stack.count() > 0 && gamedata::normalize(item_name(stack)) == gamedata::normalize(output)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_world::{
        close_menu, feet, give, open_menu, set_block, set_menu_content, swept, world,
    };
    use azalea_core::position::BlockPos;
    use azalea_registry::builtin::ItemKind;
    use praxion_headless::Body;
    use praxion_perception::Perception;
    use praxion_planner::Personality;
    use praxion_world::BlockState;

    const WINDOW: i32 = 4;

    type Tick = (ControlFrame, Option<IntentSpec>, Option<Outcome>);

    /// A 39 slot furnace menu with `items` at the given slots.
    fn slots(items: &[(usize, ItemKind, i32)]) -> Vec<ItemStack> {
        let mut slots = vec![ItemStack::Empty; MENU_SLOTS];
        for (slot, kind, count) in items {
            slots[*slot] = ItemStack::new(*kind, *count);
        }
        slots
    }

    /// A world with the furnace menu open and filled.
    fn menu_world(items: &[(usize, ItemKind, i32)]) -> World {
        let mut world = world();
        open_menu(&mut world, WINDOW, MenuKind::Furnace);
        set_menu_content(&mut world, WINDOW, slots(items));
        world
    }

    struct Harness {
        perception: Perception,
        body: Body,
        tick: u64,
    }

    impl Harness {
        fn new(world: &World) -> Self {
            Self {
                perception: swept(world),
                body: Body::new(feet()),
                tick: 0,
            }
        }

        fn run(&mut self, smelt: &mut Smelt, world: &World) -> Tick {
            let mut ctx = Ctx {
                tick: self.tick,
                body: &self.body,
                world,
                perception: &mut self.perception,
                style: Personality::default(),
            };
            let result = smelt.tick(&mut ctx, false);
            result
                .0
                .apply_with_menu(&mut self.body, self.tick)
                .expect("frame applies");
            self.tick += 1;
            result
        }

        /// Run until the stage is `stage`, failing after `max` ticks; returns
        /// every menu action sent on the way, with its tick.
        fn until(
            &mut self,
            smelt: &mut Smelt,
            world: &World,
            stage: SmeltStage,
            max: u32,
        ) -> Vec<(u64, MenuAction, MenuMode)> {
            let mut sent = Vec::new();
            for _ in 0..max {
                if smelt.stage == stage {
                    return sent;
                }
                let tick = self.tick;
                let (frame, _, outcome) = self.run(smelt, world);
                assert!(outcome.is_none(), "unexpected outcome {outcome:?}");
                if let Some(menu) = frame.menu {
                    sent.push((tick, menu, frame.menu_mode));
                }
            }
            panic!("never reached {stage:?}, stuck in {:?}", smelt.stage);
        }

        /// Run until the call reports an outcome; returns it with the tick.
        fn outcome(&mut self, smelt: &mut Smelt, world: &World, max: u32) -> (u64, Outcome) {
            for _ in 0..max {
                let tick = self.tick;
                let (_, _, outcome) = self.run(smelt, world);
                if let Some(outcome) = outcome {
                    return (tick, outcome);
                }
            }
            panic!("no outcome in {max} ticks, stage {:?}", smelt.stage);
        }
    }

    fn click(window: i32, slot: i16, button: i8) -> MenuAction {
        MenuAction {
            window: window as u32,
            slot,
            button,
        }
    }

    /// The loading clicks planned for `items`, as (slot, button) pairs.
    fn plan_for(output: &str, items: &[(usize, ItemKind, i32)]) -> Vec<(i16, i8)> {
        let world = menu_world(items);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new(output.to_owned());
        let sent = harness.until(&mut smelt, &world, SmeltStage::WaitCook, 60);
        sent.iter()
            .map(|(_, menu, _)| (menu.slot, menu.button))
            .collect()
    }

    fn fail_kind(output: &str, items: &[(usize, ItemKind, i32)]) -> Outcome {
        let world = menu_world(items);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new(output.to_owned());
        harness.outcome(&mut smelt, &world, 20).1
    }

    #[test]
    fn input_then_fuel_are_loaded_with_the_click_triple_two_ticks_apart() {
        let world = menu_world(&[(3, ItemKind::RawIron, 2), (4, ItemKind::Coal, 1)]);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new("iron_ingot".to_owned());
        let sent = harness.until(&mut smelt, &world, SmeltStage::WaitCook, 60);
        let actions: Vec<MenuAction> = sent.iter().map(|(_, menu, _)| *menu).collect();
        assert_eq!(
            actions,
            [
                click(WINDOW, 3, 0),
                click(WINDOW, 0, 1),
                click(WINDOW, 3, 0),
                click(WINDOW, 4, 0),
                click(WINDOW, 1, 1),
                click(WINDOW, 4, 0),
            ]
        );
        assert!(sent.iter().all(|(_, _, mode)| *mode == MenuMode::Click));
        for pair in sent.windows(2) {
            assert_eq!(pair[1].0 - pair[0].0, 2, "clicks are two ticks apart");
        }
        assert_eq!(smelt.cook_ticks, 200);
    }

    #[test]
    fn a_filled_result_slot_is_shift_clicked_with_a_storage_receipt() {
        let world = menu_world(&[(2, ItemKind::IronIngot, 1)]);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new("iron_ingot".to_owned());
        let mut found = None;
        for _ in 0..20 {
            let (frame, pending, outcome) = harness.run(&mut smelt, &world);
            assert!(outcome.is_none());
            if frame.menu.is_some() {
                found = Some((frame, pending));
                break;
            }
        }
        let (frame, pending) = found.expect("a take click");
        assert_eq!(frame.menu, Some(click(WINDOW, 2, 0)));
        assert_eq!(frame.menu_mode, MenuMode::ShiftClick);
        assert_eq!(
            pending,
            Some(IntentSpec::StorageInventory {
                primitive: "smelt".to_owned(),
                item: "iron_ingot".to_owned(),
                delta: 1,
                window: 120,
            })
        );
        assert_eq!(smelt.stage, SmeltStage::Take);
    }

    #[test]
    fn failures_have_their_own_kinds() {
        assert_eq!(
            fail_kind("iron_ingot", &[(3, ItemKind::RawIron, 1)]),
            Outcome::Failed("no_fuel".into())
        );
        assert_eq!(
            fail_kind("iron_ingot", &[(4, ItemKind::Coal, 1)]),
            Outcome::Failed("nothing_to_smelt".into())
        );
        // Slot 0 holds something that does not make iron.
        assert_eq!(
            fail_kind(
                "iron_ingot",
                &[(0, ItemKind::OakLog, 1), (3, ItemKind::RawIron, 1), (4, ItemKind::Coal, 1)]
            ),
            Outcome::Failed("furnace_busy".into())
        );
        assert_eq!(
            fail_kind("stick", &[(3, ItemKind::RawIron, 1)]),
            Outcome::Failed("no_recipe".into())
        );
    }

    #[test]
    fn a_furnace_that_never_yields_fails_not_cooking_after_the_cook_time_and_slack() {
        let world = menu_world(&[(3, ItemKind::RawIron, 1), (4, ItemKind::Coal, 1)]);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new("iron_ingot".to_owned());
        harness.until(&mut smelt, &world, SmeltStage::WaitCook, 60);
        let start = harness.tick;
        let (tick, outcome) = harness.outcome(&mut smelt, &world, 400);
        assert_eq!(outcome, Outcome::Failed("not_cooking".into()));
        // 260 waiting ticks, one more to notice, one for the close path.
        assert!((start + 260..=start + 263).contains(&tick), "reported at {tick}");
    }

    #[test]
    fn an_empty_furnace_menu_that_never_fills_fails_no_menu() {
        let mut world = world();
        open_menu(&mut world, WINDOW, MenuKind::Furnace);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new("iron_ingot".to_owned());
        let (_, outcome) = harness.outcome(&mut smelt, &world, 60);
        assert_eq!(outcome, Outcome::Failed("no_menu".into()));
    }

    #[test]
    fn fuel_goes_coal_then_charcoal_then_planks_then_logs() {
        let input = (3, ItemKind::RawIron, 1);
        let fuel_slot = |fuels: &[(usize, ItemKind, i32)]| {
            let mut items = vec![input];
            items.extend_from_slice(fuels);
            plan_for("iron_ingot", &items)[3].0
        };
        // Listed worst first, so priority, not slot order, decides.
        assert_eq!(
            fuel_slot(&[(4, ItemKind::OakLog, 1), (5, ItemKind::OakPlanks, 1), (6, ItemKind::Coal, 1)]),
            6
        );
        assert_eq!(
            fuel_slot(&[(4, ItemKind::OakLog, 1), (5, ItemKind::OakPlanks, 1), (6, ItemKind::Charcoal, 1)]),
            6
        );
        assert_eq!(
            fuel_slot(&[(4, ItemKind::OakLog, 1), (5, ItemKind::OakPlanks, 1)]),
            5
        );
        assert_eq!(fuel_slot(&[(4, ItemKind::OakLog, 1)]), 4);
    }

    #[test]
    fn a_stack_that_is_input_and_fuel_needs_two_items() {
        // Charcoal is smelted from logs, and logs also burn.
        assert_eq!(
            fail_kind("charcoal", &[(3, ItemKind::OakLog, 1)]),
            Outcome::Failed("no_fuel".into())
        );
        let clicks = plan_for("charcoal", &[(3, ItemKind::OakLog, 2)]);
        assert_eq!(clicks, [(3, 0), (0, 1), (3, 0), (3, 0), (1, 1), (3, 0)]);
    }

    #[test]
    fn leftovers_in_the_furnace_are_not_loaded_again() {
        // A valid input already in slot 0: only the fuel is loaded.
        assert_eq!(
            plan_for("iron_ingot", &[(0, ItemKind::RawIron, 1), (4, ItemKind::Coal, 1)]),
            [(4, 0), (1, 1), (4, 0)]
        );
        // Fuel already in slot 1: only the input is loaded.
        assert_eq!(
            plan_for("iron_ingot", &[(1, ItemKind::Coal, 1), (3, ItemKind::RawIron, 1)]),
            [(3, 0), (0, 1), (3, 0)]
        );
        // Both there: nothing to load, straight to the wait.
        assert!(
            plan_for("iron_ingot", &[(0, ItemKind::RawIron, 1), (1, ItemKind::Coal, 1)]).is_empty()
        );
        // Only the leftover input and no fuel anywhere.
        assert_eq!(
            fail_kind("iron_ingot", &[(0, ItemKind::RawIron, 1)]),
            Outcome::Failed("no_fuel".into())
        );
    }

    #[test]
    fn the_server_closing_the_window_while_cooking_goes_back_to_opening() {
        let mut world = menu_world(&[(3, ItemKind::RawIron, 1), (4, ItemKind::Coal, 1)]);
        set_block(
            &mut world,
            BlockPos::new(0, 65, 2),
            BlockState::from(BlockKind::Furnace),
        );
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new("iron_ingot".to_owned());
        harness.until(&mut smelt, &world, SmeltStage::WaitCook, 60);
        for _ in 0..5 {
            let (_, _, outcome) = harness.run(&mut smelt, &world);
            assert!(outcome.is_none());
        }
        close_menu(&mut world, WINDOW);
        let (_, _, outcome) = harness.run(&mut smelt, &world);
        assert!(outcome.is_none(), "{outcome:?}");
        assert_eq!(smelt.stage, SmeltStage::Open);
        // The furnace opens again with the input and fuel already inside.
        open_menu(&mut world, WINDOW, MenuKind::Furnace);
        set_menu_content(
            &mut world,
            WINDOW,
            slots(&[(0, ItemKind::RawIron, 1), (1, ItemKind::Coal, 1)]),
        );
        let sent = harness.until(&mut smelt, &world, SmeltStage::WaitCook, 30);
        assert!(sent.is_empty(), "nothing is loaded twice: {sent:?}");
    }

    #[test]
    fn a_preempted_call_closes_the_furnace_once_and_reports_preempted() {
        let world = menu_world(&[(3, ItemKind::RawIron, 1), (4, ItemKind::Coal, 1)]);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new("iron_ingot".to_owned());
        harness.until(&mut smelt, &world, SmeltStage::WaitCook, 60);
        smelt.request_close(Outcome::Preempted);
        let (frame, _, outcome) = harness.run(&mut smelt, &world);
        assert_eq!(frame.menu_mode, MenuMode::Close);
        assert_eq!(frame.menu, Some(click(WINDOW, 0, 0)));
        assert_eq!(outcome, Some(Outcome::Preempted));
        let (frame, _, outcome) = harness.run(&mut smelt, &world);
        assert!(frame.menu.is_none(), "the close goes out once");
        assert_eq!(outcome, Some(Outcome::Preempted));
    }

    #[test]
    fn a_failed_call_closes_the_furnace_before_it_reports() {
        let world = menu_world(&[(3, ItemKind::RawIron, 1)]);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new("iron_ingot".to_owned());
        let mut closed = false;
        let outcome = loop {
            let (frame, _, outcome) = harness.run(&mut smelt, &world);
            closed |= frame.menu_mode == MenuMode::Close && frame.menu.is_some();
            if let Some(outcome) = outcome {
                break outcome;
            }
        };
        assert_eq!(outcome, Outcome::Failed("no_fuel".into()));
        assert!(closed);
    }

    #[test]
    fn without_a_furnace_the_call_fails_needs_furnace() {
        let mut world = world();
        give(&mut world, 9, ItemKind::RawIron, 1);
        let mut harness = Harness::new(&world);
        let mut smelt = Smelt::new("iron_ingot".to_owned());
        let (_, outcome) = harness.outcome(&mut smelt, &world, 10);
        assert_eq!(outcome, Outcome::Failed("needs_furnace".into()));
    }
}
