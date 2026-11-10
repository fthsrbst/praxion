//! `Station`: open the menu of a placed block (crafting table now, furnace
//! later). `near(block, 4)` is a cheap rayless fact, so a block it calls near
//! can still be out of reach or out of sight; the station walks to it until the
//! crosshair can pick the cell, then sends the Use clicks.

use azalea_core::position::{BlockPos, Vec3};
use azalea_registry::builtin::{BlockKind, MenuKind};
use praxion_headless::{Click, ControlFrame};
use praxion_perception::Caller;

use crate::common::{
    aim_precise, block_target, cell_centre, choose_aim, idle, pickup_frame, walk_point,
};
use crate::frontier::nearest_known_block;
use crate::place::place_support;
use crate::runner::Ctx;

/// How far from the eye a station is looked for in the known map.
const SEARCH_RADIUS: u32 = 5;
/// Walking ticks before the station gives up (a block behind a wall).
const WALK_TICKS: u32 = 80;
/// Ticks in reach, spent aiming and retrying the Use click, before giving up.
const OPEN_TICKS: u32 = 60;
/// Ticks between two clicks while the menu has not opened.
const CLICK_INTERVAL: u32 = 10;

/// How far from a cell the body stands when it backs off from a blocked view.
const BACK_OFF_RADIUS: f64 = 2.5;

/// Where the station aims this tick.
enum Aim {
    /// A point whose ray first hits the cell.
    Point(Vec3),
    /// The cell's centre, with a plant in front of it to break first.
    Plant(Vec3),
    /// No point of the cell can be picked from here.
    Blocked,
}

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
        // In reach: aim where the crosshair lands on the cell, and click only
        // when it does.
        let aim = self.choose_aim(ctx, cell);
        let (point, plant) = match aim {
            Aim::Point(point) => (point, false),
            Aim::Plant(point) => (point, true),
            Aim::Blocked => return self.back_off(ctx, cell),
        };
        let mut frame = idle(ctx);
        let (look, aligned) = aim_precise(ctx, point);
        frame.look = look;
        // Only ticks spent aiming or clicking count here; getting out of the
        // way of a blocked view is on the walk budget.
        self.open_ticks += 1;
        if self.open_ticks > OPEN_TICKS {
            self.log_failure(ctx, cell);
            return StationStep::Failed(self.failure);
        }
        if !aligned {
            return StationStep::Pending(frame);
        }
        let due = self
            .last_click
            .is_none_or(|last| self.open_ticks - last >= CLICK_INTERVAL);
        let pick = praxion_headless::pick(ctx.world, ctx.body);
        // `hit.block_pos` is the cell under the crosshair, not a remembered one.
        let hit = pick.block.as_ref().filter(|hit| !hit.miss);
        let on_cell = hit.is_some_and(|hit| hit.block_pos == cell);
        let on_other = hit.is_some_and(|hit| hit.block_pos != cell);
        // An entity in front of the table would take the click.
        if due && !pick.entity_closer {
            if on_cell {
                frame.clicks.push(Click::Use);
                self.last_click = Some(self.open_ticks);
            } else if plant && on_other {
                // A plant in the way is broken first.
                frame.clicks.push(Click::Attack);
                self.last_click = Some(self.open_ticks);
            }
        }
        StationStep::Pending(frame)
    }

    /// Where to aim: a point whose pick lands on the cell
    /// ([`choose_aim`]), else the centre when a plant hides it (the plant is
    /// broken first), else nothing.
    fn choose_aim(&self, ctx: &Ctx<'_>, cell: BlockPos) -> Aim {
        let aim = choose_aim(ctx, cell);
        if let Some(point) = aim.point {
            return Aim::Point(point);
        }
        let plant_in_the_way = aim.centre_hit.is_some_and(|pos| {
            ctx.world
                .block_state(pos)
                .is_some_and(|state| !state.is_air() && !place_support(state))
        });
        if plant_in_the_way {
            Aim::Plant(cell_centre(cell))
        } else {
            Aim::Blocked
        }
    }

    /// No point of the cell can be picked from here. Walk to a spot about
    /// [`BACK_OFF_RADIUS`] blocks from it: straight away from it when the body
    /// is close (a wall beside it hides the faces), else a quarter turn around
    /// it to look from another side. `walk_ticks` is the budget.
    fn back_off(&mut self, ctx: &Ctx<'_>, cell: BlockPos) -> StationStep {
        self.walk_ticks += 1;
        if self.walk_ticks > WALK_TICKS {
            self.log_failure(ctx, cell);
            return StationStep::Failed(self.failure);
        }
        let position = ctx.body.position();
        let centre = cell_centre(cell);
        let (dx, dz) = (position.x - centre.x, position.z - centre.z);
        let horizontal = dx.hypot(dz);
        let (ux, uz) = if horizontal < 1e-3 {
            (1.0, 0.0)
        } else {
            (dx / horizontal, dz / horizontal)
        };
        let (ux, uz) = if horizontal < BACK_OFF_RADIUS - 0.5 {
            (ux, uz)
        } else {
            (-uz, ux)
        };
        let spot = Vec3::new(
            centre.x + ux * BACK_OFF_RADIUS,
            position.y,
            centre.z + uz * BACK_OFF_RADIUS,
        );
        StationStep::Pending(pickup_frame(ctx, spot))
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
    use azalea_core::position::Vec3;
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

    /// Run the station until a `Use` click goes out; returns its tick.
    fn use_tick(
        station: &mut Station,
        world: &World,
        perception: &mut Perception,
        body: &mut Body,
        max: u64,
    ) -> Option<u64> {
        for tick in 0..max {
            match step(station, world, perception, body, tick) {
                StationStep::Pending(frame) if frame.clicks.contains(&Click::Use) => {
                    return Some(tick);
                }
                StationStep::Pending(_) => {}
                other => panic!("tick {tick}: {other:?}"),
            }
        }
        None
    }

    #[test]
    fn a_table_under_a_roof_is_clicked_on_the_face_the_body_can_see() {
        // Live failure: standing next to the table, the ray to its centre
        // enters the table's column in the cell above it, which is dirt.
        let mut world = world();
        let cell = BlockPos::new(1, 64, 0);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        set_block(&mut world, BlockPos::new(1, 65, 0), BlockState::from(BlockKind::Dirt));
        let mut body = Body::new(feet());
        let mut station = table_station();
        let tick = use_tick(&mut station, &world, &mut perception, &mut body, 10);
        assert!(tick.is_some(), "a Use click goes out");
        let pick = praxion_headless::pick(&world, &body);
        let hit = pick.block.expect("the crosshair is on a block");
        assert_eq!(hit.block_pos, cell, "the click lands on the table");
        open_menu(&mut world, 1, MenuKind::Crafting);
        assert!(matches!(
            step(&mut station, &world, &mut perception, &mut body, 11),
            StationStep::Opened
        ));
    }

    #[test]
    fn only_the_side_face_is_open_so_that_face_is_clicked() {
        // Roof above the table: the top and both z faces are behind the dirt,
        // the east face is behind the table itself. Only the west face works.
        let mut world = world();
        let cell = BlockPos::new(1, 64, 0);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        set_block(&mut world, BlockPos::new(1, 65, 0), BlockState::from(BlockKind::Dirt));
        let mut body = Body::new(feet());
        let mut station = table_station();
        use_tick(&mut station, &world, &mut perception, &mut body, 10).expect("a Use click");
        let hit = praxion_headless::pick(&world, &body).block.expect("a block");
        assert_eq!(hit.block_pos, cell);
        assert_eq!(hit.direction, azalea_core::direction::Direction::West);
    }

    #[test]
    fn a_table_with_no_visible_face_makes_the_body_back_off() {
        // Table one below and one beside the body; dirt over it and stone
        // under the body hide every face from the eye.
        let mut world = world();
        let cell = BlockPos::new(1, 63, 0);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        set_block(&mut world, BlockPos::new(1, 64, 0), BlockState::from(BlockKind::Dirt));
        set_block(&mut world, BlockPos::new(0, 63, 0), BlockState::from(BlockKind::Stone));
        let mut body = Body::new(feet());
        let mut station = table_station();
        let mut walked = false;
        for tick in 0..6u64 {
            match step(&mut station, &world, &mut perception, &mut body, tick) {
                StationStep::Pending(frame) => {
                    assert!(frame.clicks.is_empty(), "tick {tick}: nothing to click");
                    walked |= frame.held.forward;
                }
                other => panic!("tick {tick}: {other:?}"),
            }
        }
        assert!(walked, "the body walks");
        // The table is east of the body; it walks west, away from it.
        let yaw = body.look().y_rot().rem_euclid(360.0);
        assert!((yaw - 90.0).abs() < 10.0, "faces west, yaw {yaw}");
    }

    #[test]
    fn the_face_nearest_the_eye_is_chosen_when_several_work() {
        // Table diagonally ahead with dirt over the body's side of it: the
        // centre ray is blocked. The north (-z), east and bottom faces all
        // take a Use; north is the nearest to the eye, and it comes last in
        // the face list order, so only the nearest-first sort picks it.
        let mut world = world();
        let cell = BlockPos::new(1, 64, 1);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        set_block(&mut world, BlockPos::new(0, 65, 1), BlockState::from(BlockKind::Dirt));
        let mut body = Body::new(feet());
        let mut station = table_station();
        use_tick(&mut station, &world, &mut perception, &mut body, 10).expect("a Use click");
        let hit = praxion_headless::pick(&world, &body).block.expect("a block");
        assert_eq!(hit.block_pos, cell);
        assert_eq!(hit.direction, azalea_core::direction::Direction::North);
    }

    #[test]
    fn a_blocked_view_from_two_blocks_away_walks_around_the_table() {
        // Wall between the body and the table, body 3 blocks from it: the
        // back-off is a quarter turn around the table, not a step toward it.
        let mut world = world();
        let cell = BlockPos::new(3, 64, 0);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        for y in 64..67 {
            for z in -1..=1 {
                set_block(&mut world, BlockPos::new(2, y, z), BlockState::from(BlockKind::Stone));
            }
        }
        let mut body = Body::new(feet());
        let mut station = table_station();
        let mut forward = false;
        for tick in 0..6u64 {
            match step(&mut station, &world, &mut perception, &mut body, tick) {
                StationStep::Pending(frame) => {
                    assert!(frame.clicks.is_empty());
                    forward |= frame.held.forward;
                }
                other => panic!("tick {tick}: {other:?}"),
            }
        }
        assert!(forward, "the body walks");
        // The body is west of the table; (dx, dz) = (-3, 0), so the quarter
        // turn target is at +z / -z of the table, not along x: yaw faces
        // south (0) or north (180), not east (270) or west (90).
        let yaw = body.look().y_rot().rem_euclid(360.0);
        let along_x = (yaw - 90.0).abs() < 30.0 || (yaw - 270.0).abs() < 30.0;
        assert!(!along_x, "walks tangentially, yaw {yaw}");
    }

    #[test]
    fn a_back_off_ends_with_a_use_click_and_opened_from_the_new_spot() {
        let mut world = world();
        let cell = BlockPos::new(1, 63, 0);
        set_block(&mut world, cell, table());
        let mut perception = swept(&world);
        set_block(&mut world, BlockPos::new(1, 64, 0), BlockState::from(BlockKind::Dirt));
        set_block(&mut world, BlockPos::new(0, 63, 0), BlockState::from(BlockKind::Stone));
        let mut body = Body::new(feet());
        let mut station = table_station();
        for tick in 0..3u64 {
            assert!(matches!(
                step(&mut station, &world, &mut perception, &mut body, tick),
                StationStep::Pending(_)
            ));
        }
        // The body now stands two blocks back, clear of the stone column:
        // rebuild it there with the view open.
        let mut body = Body::new(Vec3::new(-1.5, 64.0, 0.5));
        set_block(&mut world, BlockPos::new(1, 64, 0), BlockState::from(BlockKind::Air));
        let tick = use_tick(&mut station, &world, &mut perception, &mut body, 20);
        assert!(tick.is_some(), "a Use click from the new spot");
        open_menu(&mut world, 1, MenuKind::Crafting);
        assert!(matches!(
            step(&mut station, &world, &mut perception, &mut body, 30),
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
