//! `Station`: open the menu of a placed block (crafting table now, furnace
//! later). `near(block, 4)` is a cheap rayless fact, so a block it calls near
//! can still be out of reach or out of sight; the station walks to it until the
//! crosshair can pick the cell, then sends the Use clicks.

use azalea_core::position::BlockPos;
use azalea_registry::builtin::{BlockKind, MenuKind};
use praxion_headless::{Click, ControlFrame};
use praxion_perception::Caller;

use crate::common::{aim_precise, block_target, idle, pickup_frame, walk_point};
use crate::frontier::nearest_known_block;
use crate::place::{cell_centre, place_support};
use crate::runner::Ctx;

/// How far from the eye a station is looked for in the known map.
const SEARCH_RADIUS: u32 = 5;
/// Walking ticks before the station gives up (a block behind a wall).
const WALK_TICKS: u32 = 80;
/// Ticks in reach, spent aiming and retrying the Use click, before giving up.
const OPEN_TICKS: u32 = 60;
/// Ticks between two clicks while the menu has not opened.
const CLICK_INTERVAL: u32 = 10;

/// What one station tick decided.
#[derive(Debug)]
pub(crate) enum StationStep {
    /// The expected menu is open.
    Opened,
    /// Not open yet: send this frame (walk, aim, click) and tick again.
    Pending(ControlFrame),
    /// Gave up; the caller's failure kind.
    Failed(&'static str),
}

#[derive(Debug)]
pub(crate) struct Station {
    block: BlockKind,
    menu: MenuKind,
    failure: &'static str,
    cell: Option<BlockPos>,
    walk_ticks: u32,
    open_ticks: u32,
    /// `open_ticks` of the last click, to space the retries.
    last_click: Option<u32>,
}

impl Station {
    /// A station of `block` whose Use click opens a `menu`; `failure` is the
    /// failure kind when it cannot be reached or opened.
    pub(crate) fn new(block: BlockKind, menu: MenuKind, failure: &'static str) -> Self {
        Self {
            block,
            menu,
            failure,
            cell: None,
            walk_ticks: 0,
            open_ticks: 0,
            last_click: None,
        }
    }

    pub(crate) fn tick(&mut self, ctx: &mut Ctx<'_>) -> StationStep {
        if ctx
            .world
            .open_container()
            .is_some_and(|container| container.kind() == self.menu)
        {
            return StationStep::Opened;
        }
        if self.cell.is_none_or(|cell| !self.holds_block(ctx, cell)) {
            self.cell = self.resolve(ctx);
        }
        let Some(cell) = self.cell else {
            return StationStep::Failed(self.failure);
        };
        let centre = cell_centre(cell);
        let eye = ctx.body.eye_position();
        let reach = ctx.body.block_interaction_range() - 0.25;
        if eye.distance_to(centre) > reach {
            return self.walk(ctx, cell);
        }
        // In reach: aim at the true centre (a ray at a block's bottom edge can
        // pass under it) and click only when the crosshair is on the cell.
        let mut frame = idle(ctx);
        let (look, aligned) = aim_precise(ctx, centre);
        frame.look = look;
        let pick = praxion_headless::pick(ctx.world, ctx.body);
        let target = pick.block.filter(|hit| !hit.miss);
        let on_cell = target.as_ref().is_some_and(|hit| hit.block_pos == cell);
        // `hit.block_pos` is the cell under the crosshair, not a remembered one.
        let plant = target.as_ref().is_some_and(|hit| {
            hit.block_pos != cell
                && !pick.entity_closer
                && ctx
                    .world
                    .block_state(hit.block_pos)
                    .is_some_and(|state| !state.is_air() && !place_support(state))
        });
        // Only ticks spent aiming or clicking count here; a solid block in the
        // way is walked around on the walk budget instead.
        if !aligned || on_cell || plant {
            self.open_ticks += 1;
            if self.open_ticks > OPEN_TICKS {
                self.log_failure(ctx, cell);
                return StationStep::Failed(self.failure);
            }
        }
        if !aligned {
            return StationStep::Pending(frame);
        }
        let due = self
            .last_click
            .is_none_or(|last| self.open_ticks - last >= CLICK_INTERVAL);
        if on_cell {
            // An entity in front of the table would take the Use click.
            if due && !pick.entity_closer {
                frame.clicks.push(Click::Use);
                self.last_click = Some(self.open_ticks);
            }
            return StationStep::Pending(frame);
        }
        if plant {
            // A plant in the way is broken first.
            if due {
                frame.clicks.push(Click::Attack);
                self.last_click = Some(self.open_ticks);
            }
            return StationStep::Pending(frame);
        }
        // Something solid is in front of the cell: walk toward it, keeping the
        // aim so the next tick is still aligned.
        match self.walk(ctx, cell) {
            StationStep::Pending(mut walk) => {
                walk.look = frame.look;
                StationStep::Pending(walk)
            }
            done => done,
        }
    }

    /// Walk in a straight line toward the cell; `walk_ticks` is the budget.
    fn walk(&mut self, ctx: &Ctx<'_>, cell: BlockPos) -> StationStep {
        self.walk_ticks += 1;
        if self.walk_ticks > WALK_TICKS {
            self.log_failure(ctx, cell);
            return StationStep::Failed(self.failure);
        }
        let feet = ctx.body.position().to_block_pos_floor();
        StationStep::Pending(pickup_frame(ctx, walk_point(feet, cell)))
    }

    /// The cell to use: a visible block of this kind, else a remembered one.
    fn resolve(&self, ctx: &mut Ctx<'_>) -> Option<BlockPos> {
        let name = self.block.to_str();
        block_target(ctx, name, 4).or_else(|| {
            nearest_known_block(ctx.perception, ctx.world, name, SEARCH_RADIUS, &[])
                .map(|(cell, _)| cell)
        })
    }

    /// Whether `cell` still holds the block. Asks perception, which looks at
    /// the cell when it is in sight and falls back to memory otherwise; an
    /// unknown answer keeps the cell.
    fn holds_block(&self, ctx: &mut Ctx<'_>, cell: BlockPos) -> bool {
        match ctx.perception.block(Caller::Other, ctx.world, cell).value() {
            Some(fact) => fact.kind == self.block,
            None => true,
        }
    }

    fn log_failure(&self, ctx: &Ctx<'_>, cell: BlockPos) {
        let pick = praxion_headless::pick(ctx.world, ctx.body);
        log::debug!(
            "station {:?}: gave up on {:?}; eye distance {:.2}, pick {:?}, walked {}, in reach {}",
            self.block,
            cell,
            ctx.body.eye_position().distance_to(cell_centre(cell)),
            pick.block.map(|hit| (hit.block_pos, hit.miss)),
            self.walk_ticks,
            self.open_ticks
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_world::{feet, open_menu, set_block, swept, world};
    use praxion_headless::{Body, Look};
    use praxion_perception::Perception;
    use praxion_planner::Personality;
    use praxion_world::{BlockState, World};

    fn table_station() -> Station {
        Station::new(BlockKind::CraftingTable, MenuKind::Crafting, "needs_table")
    }

    /// One tick: run the station, then apply its frame's look to the body as
    /// the driver would (a look aim takes effect at once here; the body never
    /// moves, the tests assert on what the frames ask for).
    fn step(
        station: &mut Station,
        world: &World,
        perception: &mut Perception,
        body: &mut Body,
        tick: u64,
    ) -> StationStep {
        let mut ctx = Ctx {
            tick,
            body,
            world,
            perception,
            style: Personality::default(),
        };
        let step = station.tick(&mut ctx);
        if let StationStep::Pending(frame) = &step {
            frame.apply(body, tick).expect("frame applies");
        }
        step
    }

    fn table() -> BlockState {
        BlockState::from(BlockKind::CraftingTable)
    }

    #[test]
    fn a_table_beyond_reach_is_walked_to_not_clicked() {
        // `near(crafting_table, 4)` counts integer cell deltas around the eye
        // cell, so a table four cells away diagonally is "near" while its
        // centre is further than the 4.25 reach.
        let mut world = world();
        let cell = BlockPos::new(3, 64, 3);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let eye = body.eye_position();
        assert!(eye.distance_to(cell_centre(cell)) > 4.25, "the setup is out of reach");
        let mut station = table_station();
        let mut walked = 0;
        for tick in 0..u64::from(WALK_TICKS) {
            match step(&mut station, &world, &mut perception, &mut body, tick) {
                StationStep::Pending(frame) => {
                    assert!(frame.clicks.is_empty(), "no click out of reach");
                    // The look turns first; forward is held once aligned.
                    if frame.held.forward {
                        walked += 1;
                    }
                }
                other => panic!("tick {tick}: expected a walk, got {other:?}"),
            }
        }
        assert!(walked > 0, "the body walks toward the table");
    }

    #[test]
    fn a_table_behind_stone_walks_then_fails_at_the_cap() {
        let mut world = world();
        let cell = BlockPos::new(0, 65, 3);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        // The map knows the table; then a stone block appears on the line to
        // it (the eye is at y 65.62), so the pick cannot reach the cell.
        set_block(&mut world, BlockPos::new(0, 65, 2), BlockState::from(BlockKind::Stone));
        let mut body = Body::new(feet());
        let mut station = table_station();
        // The first tick only turns the look (not aligned yet); every later
        // tick walks, so the cap falls on tick 80 or 81, never before.
        let mut failed_at = None;
        for tick in 0..90u64 {
            match step(&mut station, &world, &mut perception, &mut body, tick) {
                StationStep::Pending(frame) => {
                    assert!(frame.clicks.is_empty(), "tick {tick}: stone is in the way");
                }
                StationStep::Failed(kind) => {
                    assert_eq!(kind, "needs_table");
                    failed_at = Some(tick);
                    break;
                }
                StationStep::Opened => panic!("nothing opened a menu"),
            }
        }
        let failed_at = failed_at.expect("the station gives up");
        assert!((80..=81).contains(&failed_at), "gave up at tick {failed_at}");
    }

    #[test]
    fn an_adjacent_pickable_table_is_used_on_the_first_aligned_tick() {
        let mut world = world();
        let cell = BlockPos::new(0, 65, 2);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut station = table_station();
        // Tick 0 aims (the body still looks along +z with pitch 0, which is
        // already a hit for a table at eye height, or turns it there).
        let mut used_at = None;
        for tick in 0..5u64 {
            match step(&mut station, &world, &mut perception, &mut body, tick) {
                StationStep::Pending(frame) => {
                    if frame.clicks.contains(&Click::Use) {
                        used_at = Some(tick);
                        assert!(matches!(frame.look, Look::Aim { .. }));
                        break;
                    }
                }
                other => panic!("tick {tick}: unexpected {other:?}"),
            }
        }
        assert!(
            used_at.is_some_and(|tick| tick <= 1),
            "Use goes out as soon as the aim is on the cell, got {used_at:?}"
        );
    }

    #[test]
    fn a_table_broken_in_sight_is_not_walked_to() {
        let mut world = world();
        let cell = BlockPos::new(0, 65, 2);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut station = table_station();
        for tick in 0..2u64 {
            assert!(matches!(
                step(&mut station, &world, &mut perception, &mut body, tick),
                StationStep::Pending(_)
            ));
        }
        // Someone breaks it while it is in sight.
        set_block(&mut world, cell, BlockState::from(BlockKind::Air));
        let next = step(&mut station, &world, &mut perception, &mut body, 2);
        assert!(
            matches!(next, StationStep::Failed("needs_table")),
            "got {next:?}"
        );
    }

    #[test]
    fn the_expected_menu_being_open_reports_opened() {
        let mut world = world();
        set_block(&mut world, BlockPos::new(0, 65, 2), table());
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut station = table_station();
        open_menu(&mut world, 1, MenuKind::Crafting);
        assert!(matches!(
            step(&mut station, &world, &mut perception, &mut body, 0),
            StationStep::Opened
        ));
    }

    #[test]
    fn a_missing_table_fails_at_once() {
        let world = world();
        let mut perception = swept(&world);
        let mut body = Body::new(feet());
        let mut station = table_station();
        assert!(matches!(
            step(&mut station, &world, &mut perception, &mut body, 0),
            StationStep::Failed("needs_table")
        ));
    }
}
