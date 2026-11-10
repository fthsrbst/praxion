//! `pillar(block)`: jump and place one block under the feet (decision
//! 2026-11-09 "Five next areas": a mining skill may climb to reach a block above
//! it, at most about three blocks, never in `explore` or `dig_down`, and the
//! column stays). One block per call, so the declared effect (`consumes $block
//! 1`) is exact; the skill calls it again.
//!
//! # The jump and the click, measured
//!
//! The headless physics (`measure_the_jump_window`) gives the feet height after
//! each tick of a jump held on tick 0, standing at y = 64:
//!
//! ```text
//! tick  1     2     3     4      5      6      7      8      9     10    11
//! y     64.42 64.75 65.00 65.17  65.25  65.25  65.18  65.02  64.80 64.50 64.12
//! ```
//!
//! The block goes into the feet cell, whose top is 65.0, so the server refuses
//! the placement unless the body box is above it. The feet are above 65.0 from
//! tick 3 to tick 8, but the first and last of those are marginal (65.001 and
//! 65.024). The click goes out when the feet are at least [`CLEAR_MARGIN`]
//! above the cell top: ticks 4 to 7, a window of four ticks. The jump starts
//! only after the crosshair already points straight down at the support, so no
//! aiming happens in the air.
//!
//! # The cell is scaffolding only once it exists
//!
//! The cell under the feet is remembered as scaffolding
//! ([`praxion_perception::scaffold`]) when the place receipt settles as
//! confirmed, not when the click goes out: a click the server refuses changes
//! no block, and a falsely remembered cell would hide a real block from every
//! target search for the rest of the process (review finding 3).

use azalea_core::position::BlockPos;
use praxion_headless::{Click, ControlFrame, Look};
use praxion_interpreter::Outcome;
use praxion_perception::{Caller, scaffold};
use praxion_planner::blocks;
use praxion_world::BlockState;

use crate::common::{HandSelect, Selecting, count_item, hotbar_index, idle, item_name};
use crate::place::place_support;
use crate::runner::{Ctx, IntentSpec};

/// How far above the feet cell's top the feet must be before the click goes
/// out: the server may see the position a tick late.
const CLEAR_MARGIN: f64 = 0.10;
/// The pitch, in degrees down, the crosshair must reach before the jump.
const DOWN_PITCH: f32 = 90.0;
/// Pitch accepted as "straight down".
const DOWN_TOLERANCE: f32 = 2.0;
/// Ticks of one jump to wait for the clear window before it counts as missed.
const AIR_TICKS: u32 = 14;
/// Ticks to wait for the place receipt after the click.
const RECEIPT_TICKS: u32 = 45;
/// Ticks the preparation (hand, look, safety answers) may take.
const PREPARE_TICKS: u32 = 40;
/// The whole call's bound. It must outlast the worst case the stages allow:
/// two jumps (the retry) and the receipt of the second, or the call reports
/// `stuck` while a valid receipt is still on its way (review finding 6). The
/// registry budget (`rules::registry`, 180) is above this, so the call always
/// gives its own typed kind first.
const CALL_TICKS: u32 = 165;
/// How many blocks `pillar` may stack under the feet, one per call (decision
/// 2026-11-09 "Five next areas": at most about three).
pub(crate) const MAX_COLUMN: u32 = 3;
/// Health the body needs to climb (a free-standing column can be fallen from).
/// `leave_shelter` turns this check off ([`Pillar::climbing_out`]).
const MIN_HEALTH: f32 = 10.0;
/// Jumps one call may make before it fails `no_click` (no click ever went out)
/// or the receipt times out (`no_place`).
const MAX_JUMPS: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Hand, look and safety: everything before the jump.
    Prepare,
    /// In the air: click when the feet clear the cell.
    Air,
    /// The click is out: wait for the receipt.
    Receipt,
}

#[derive(Debug)]
pub(crate) struct Pillar {
    block: String,
    stage: Stage,
    hand: HandSelect,
    ticks: u32,
    stage_ticks: u32,
    jumps: u32,
    /// The cell the block goes into: the feet cell when the call started.
    cell: Option<BlockPos>,
    named_slot: bool,
    /// Refuse the climb below [`MIN_HEALTH`]. Off only inside a shelter's
    /// column, see [`Pillar::climbing_out`].
    health_check: bool,
}

impl Pillar {
    pub(crate) fn new(block: String) -> Self {
        Self {
            block,
            stage: Stage::Prepare,
            hand: HandSelect::default(),
            ticks: 0,
            stage_ticks: 0,
            jumps: 0,
            cell: None,
            named_slot: false,
            health_check: true,
        }
    }

    /// A pillar step out of a dug shelter (`leave_shelter`): the column is the
    /// hole itself, walled by ground on every side, so the climb cannot end in
    /// a fall and the health rule is off. The body takes shelter exactly when
    /// it is hurt and health comes back slowly, so with the rule on a body
    /// sent into the hole at ten health or less was still below it at dawn and
    /// could never climb out (run 9 review, item 1). The fire, fluid, support,
    /// headroom and column rules still hold.
    pub(crate) fn climbing_out(block: String) -> Self {
        Self {
            health_check: false,
            ..Self::new(block)
        }
    }

    pub(crate) fn tick(
        &mut self,
        ctx: &mut Ctx<'_>,
        started: bool,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        let mut frame = idle(ctx);
        self.ticks += 1;
        self.stage_ticks += 1;
        if self.ticks > CALL_TICKS {
            return (frame, None, Some(Outcome::Failed("stuck".into())));
        }
        match self.stage {
            Stage::Receipt => {
                // The receipt settles through the runner; until then nothing
                // more is clicked (a second use could place a second block).
                if self.stage_ticks > RECEIPT_TICKS {
                    return (frame, None, Some(Outcome::Failed("no_place".into())));
                }
                (frame, None, None)
            }
            Stage::Prepare => self.prepare(ctx, frame),
            Stage::Air => {
                let cell = self.cell.expect("set in the prepare stage");
                // Keep looking straight down; the crosshair was aligned before
                // the jump and the body only moves vertically.
                frame.look = down_look(ctx);
                let feet_y = ctx.body.position().y;
                if feet_y >= f64::from(cell.y) + 1.0 + CLEAR_MARGIN {
                    let support = BlockPos::new(cell.x, cell.y - 1, cell.z);
                    let pick = praxion_headless::pick(ctx.world, ctx.body);
                    // Straight down the ray meets the support's top face, or a
                    // replaceable plant or snow layer in the feet cell itself
                    // (short grass has an outline): a use click on that places
                    // the block in the cell too, replacing the plant, as in
                    // vanilla. Seen live on 2026-11-09: a body standing in grass
                    // never clicked and failed `no_click`.
                    let cell_replaceable =
                        cell_state(ctx, cell).is_some_and(blocks::is_replaceable);
                    let on_support = pick.block.as_ref().is_some_and(|hit| {
                        !hit.miss
                            && ((hit.block_pos == support
                                && hit.direction == azalea_core::direction::Direction::Up)
                                || (hit.block_pos == cell
                                    && cell_replaceable))
                    });
                    if on_support {
                        frame.clicks.push(Click::Use);
                        self.stage = Stage::Receipt;
                        self.stage_ticks = 0;
                        let pending = (!started).then(|| IntentSpec::Place {
                            pos: cell,
                            block: self.placed_block(ctx),
                            window: 40,
                        });
                        return (frame, pending, None);
                    }
                }
                if self.stage_ticks > AIR_TICKS && ctx.body.on_ground() {
                    // The window passed: the body is back on the ground.
                    if self.jumps >= MAX_JUMPS {
                        // No click ever went out (a click always moves the
                        // call to `Receipt`), so nothing was placed or
                        // refused: say so instead of `no_place`, which claims
                        // an attempted placement (review finding 6).
                        return (frame, None, Some(Outcome::Failed("no_click".into())));
                    }
                    self.stage = Stage::Prepare;
                    self.stage_ticks = 0;
                }
                (frame, None, None)
            }
        }
    }

    /// The place receipt settled as confirmed: the block really stands in the
    /// cell. Only now is the cell remembered as scaffolding. A click the server
    /// refused (a bad angle, a colliding entity, a lagging position) changes no
    /// block, so remembering it at click time would hide a real block from
    /// every target search for the rest of the process, and would count a
    /// column that does not exist against the three-block limit (review
    /// finding 3).
    pub(crate) fn note_settled(&mut self) {
        if let Some(cell) = self.cell {
            scaffold::note(cell);
        }
    }

    /// Hand, safety and look; the jump goes out on the tick everything holds.
    fn prepare(
        &mut self,
        ctx: &mut Ctx<'_>,
        mut frame: ControlFrame,
    ) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
        if count_item(ctx.world, &self.block) == 0 {
            return (frame, None, Some(Outcome::Failed("no_blocks".into())));
        }
        if self.stage_ticks > PREPARE_TICKS {
            return (frame, None, Some(Outcome::Failed("stuck".into())));
        }
        // Name the block's hotbar slot once per call (the mirror can lag a
        // respawn), then let the hand select take over.
        if !self.named_slot
            && let Some(slot) = hotbar_index(ctx.world, &self.block)
        {
            frame.select_slot = Some(slot);
            self.named_slot = true;
            return (frame, None, None);
        }
        match self.hand.tick(ctx.world, &self.block, &mut frame) {
            Selecting::Held => {}
            Selecting::Pending => return (frame, None, None),
            Selecting::Unavailable => {
                return (frame, None, Some(Outcome::Failed("no_blocks".into())));
            }
        }
        if !ctx.body.on_ground() {
            // A jump or a fall in progress: wait for the ground.
            return (frame, None, None);
        }
        let feet = ctx.body.position().to_block_pos_floor();
        match self.check(ctx, feet) {
            Check::Safe => {}
            Check::Wait => return (frame, None, None),
            Check::Fail(kind) => return (frame, None, Some(Outcome::Failed(kind.into()))),
        }
        self.cell = Some(feet);
        frame.look = down_look(ctx);
        if ctx.body.look().x_rot() < DOWN_PITCH - DOWN_TOLERANCE {
            return (frame, None, None);
        }
        // Everything holds: jump.
        frame.held.jump = true;
        self.jumps += 1;
        self.stage = Stage::Air;
        self.stage_ticks = 0;
        (frame, None, None)
    }

    /// The safety rules, asked of perception, never of the raw world.
    fn check(&self, ctx: &mut Ctx<'_>, feet: BlockPos) -> Check {
        let health = ctx.perception.own_health(ctx.world).value().map(|h| h.health());
        if self.health_check && health.is_some_and(|health| health < MIN_HEALTH) {
            return Check::Fail("unsafe");
        }
        if ctx.perception.burning(ctx.world).value().copied() == Some(true) {
            return Check::Fail("unsafe");
        }
        let support = BlockPos::new(feet.x, feet.y - 1, feet.z);
        if scaffold::column_height(support) >= MAX_COLUMN {
            return Check::Fail("too_high");
        }
        // The cell under the feet: solid, not replaceable, no fluid.
        match cell_state(ctx, support) {
            None => return Check::Wait,
            Some(state) => {
                if blocks::fluid(state).is_some() || !blocks::is_solid(state) || !place_support(state)
                {
                    return Check::Fail("unsafe");
                }
            }
        }
        // Four air cells from the feet up: the feet and head now, and the two
        // above the new feet cell, so the jump is not cut short by a roof and
        // the body is not trapped inside one (critique finding 3).
        for up in 0..4 {
            let cell = BlockPos::new(feet.x, feet.y + up, feet.z);
            match cell_state(ctx, cell) {
                None => return Check::Wait,
                Some(state) if blocks::fluid(state).is_some() => return Check::Fail("unsafe"),
                Some(state) if !state.is_air() && !blocks::is_passable(state) => {
                    return Check::Fail("no_headroom");
                }
                // The feet cell receives the block: a passable thing a click
                // cannot replace (a flower, a torch) would take the click.
                Some(state) if up == 0 && !blocks::is_replaceable(state) => {
                    return Check::Fail("no_headroom");
                }
                Some(_) => {}
            }
        }
        // No fluid beside the column: a known water or lava cell next to the
        // feet or head turns a climb into a swim.
        for dy in -1..=1 {
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let cell = BlockPos::new(feet.x + dx, feet.y + dy, feet.z + dz);
                let near = ctx.perception.known(cell).map(|seen| seen.state);
                if near.is_some_and(|state| blocks::fluid(state).is_some()) {
                    return Check::Fail("unsafe");
                }
            }
        }
        Check::Safe
    }

    /// The block name the place receipt expects: the held item's own name (a
    /// tag argument such as `#planks` names the stack that was chosen).
    fn placed_block(&self, ctx: &Ctx<'_>) -> String {
        let slot = ctx.world.selected_hotbar_slot().unwrap_or(0) as usize;
        ctx.world
            .inventory()
            .get(36 + slot)
            .map(|stack| praxion_gamedata::normalize(item_name(stack)).to_owned())
            .unwrap_or_else(|| self.block.clone())
    }
}

enum Check {
    Safe,
    /// A perception answer is missing this tick.
    Wait,
    Fail(&'static str),
}

/// The state of a cell as perception answers it (sight, else memory).
fn cell_state(ctx: &mut Ctx<'_>, pos: BlockPos) -> Option<BlockState> {
    ctx.perception
        .block(Caller::Other, ctx.world, pos)
        .value()
        .map(|fact| fact.state)
}

/// Keep the yaw, look straight down.
fn down_look(ctx: &Ctx<'_>) -> Look {
    Look::Aim {
        yaw: ctx.body.look().y_rot(),
        pitch: DOWN_PITCH,
    }
}

/// Feet height when a jump held on its first tick is at tick `n` (1-based),
/// from the physics, for tests and docs.
#[cfg(test)]
fn jump_heights(ticks: usize) -> Vec<f64> {
    use azalea_core::position::Vec3;
    use azalea_registry::builtin::BlockKind;
    use praxion_headless::{Blocks, Body, Held};

    struct Ground;

    impl Blocks for Ground {
        fn block(&self, pos: BlockPos) -> Option<BlockState> {
            (pos.y <= 63).then_some(BlockState::from(BlockKind::Stone))
        }
        fn ready(&self, _position: Vec3) -> bool {
            true
        }
    }

    let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
    for _ in 0..5 {
        body.step(&Held::NONE, &Ground);
    }
    assert!(body.on_ground());
    let mut heights = Vec::new();
    for tick in 0..ticks {
        let mut held = Held::NONE;
        held.jump = tick == 0;
        body.step(&held, &Ground);
        heights.push(body.position().y);
    }
    heights
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_world::{feet, give, set_block, set_health, swept, world};
    use azalea_registry::builtin::{BlockKind, ItemKind};
    use praxion_headless::Body;
    use praxion_planner::Personality;
    use praxion_world::World;

    #[test]
    fn measure_the_jump_window() {
        // The numbers in the module docs. The feet cell top is 65.0.
        let heights = jump_heights(12);
        let clear: Vec<usize> = heights
            .iter()
            .enumerate()
            .filter(|(_, y)| **y >= 65.0)
            .map(|(index, _)| index + 1)
            .collect();
        assert_eq!(clear.first(), Some(&3), "the feet clear the cell on tick 3: {heights:?}");
        assert_eq!(clear.last(), Some(&8), "and stop clearing it after tick 8: {heights:?}");
        let click_window: Vec<usize> = heights
            .iter()
            .enumerate()
            .filter(|(_, y)| **y >= 65.0 + CLEAR_MARGIN)
            .map(|(index, _)| index + 1)
            .collect();
        assert_eq!(
            click_window,
            [4, 5, 6, 7],
            "four safe ticks for the click: {heights:?}"
        );
        // The click window ends long before the 40-tick receipt window and the
        // 120-tick budget matter: the jump itself is 11 ticks.
        assert!(heights[10] < 64.2 && heights[11] == 64.0, "{heights:?}");
    }

    /// One pillar tick with the look applied at once (the driver's gaze would
    /// take a few ticks) and the body stepped by the test.
    struct Rig {
        world: World,
        perception: praxion_perception::Perception,
        body: Body,
        pillar: Pillar,
        tick: u64,
    }

    impl Rig {
        fn new(world: World) -> Self {
            let mut body = settled_body();
            // Fall onto the floor so the body is on the ground.
            for _ in 0..5 {
                body.step(&praxion_headless::Held::NONE, &world);
            }
            Self {
                perception: swept(&world),
                world,
                body,
                pillar: Pillar::new("cobblestone".into()),
                tick: 0,
            }
        }

        fn run(&mut self) -> (ControlFrame, Option<IntentSpec>, Option<Outcome>) {
            let mut ctx = Ctx {
                tick: self.tick,
                body: &self.body,
                world: &self.world,
                perception: &mut self.perception,
                style: Personality::default(),
            };
            let result = self.pillar.tick(&mut ctx, false);
            result
                .0
                .apply_with_menu(&mut self.body, self.tick)
                .expect("frame applies");
            self.tick += 1;
            result
        }
    }

    /// A body standing on the test world's floor (y 63 is made stone by the
    /// caller), ready for physics.
    fn settled_body() -> Body {
        Body::new(feet())
    }

    fn floor_world() -> World {
        let mut world = world();
        for x in -3..=3 {
            for z in -3..=3 {
                set_block(&mut world, BlockPos::new(x, 63, z), BlockState::from(BlockKind::Stone));
            }
        }
        give(&mut world, 36, ItemKind::Cobblestone, 4);
        world
    }

    #[test]
    fn with_no_block_item_the_call_fails_no_blocks() {
        scaffold::reset();
        let mut world = world();
        for x in -3..=3 {
            for z in -3..=3 {
                set_block(&mut world, BlockPos::new(x, 63, z), BlockState::from(BlockKind::Stone));
            }
        }
        let mut rig = Rig::new(world);
        let (_, _, outcome) = rig.run();
        assert_eq!(outcome, Some(Outcome::Failed("no_blocks".into())));
    }

    #[test]
    fn a_roof_over_the_landing_cells_fails_no_headroom_and_nothing_is_placed() {
        scaffold::reset();
        let mut world = floor_world();
        // Stone three above the feet: the jump and the new standing cell
        // would hit it.
        set_block(&mut world, BlockPos::new(0, 67, 0), BlockState::from(BlockKind::Stone));
        let mut rig = Rig::new(world);
        let mut outcome = None;
        for _ in 0..PREPARE_TICKS {
            let (frame, _, out) = rig.run();
            assert!(!frame.held.jump, "no jump under a roof");
            assert!(frame.clicks.is_empty());
            if out.is_some() {
                outcome = out;
                break;
            }
        }
        assert_eq!(outcome, Some(Outcome::Failed("no_headroom".into())));
    }

    #[test]
    fn water_beside_the_column_fails_unsafe() {
        scaffold::reset();
        let mut world = floor_world();
        set_block(&mut world, BlockPos::new(1, 64, 0), BlockState::from(BlockKind::Water));
        let mut rig = Rig::new(world);
        let mut outcome = None;
        for _ in 0..PREPARE_TICKS {
            let (_, _, out) = rig.run();
            if out.is_some() {
                outcome = out;
                break;
            }
        }
        assert_eq!(outcome, Some(Outcome::Failed("unsafe".into())));
    }

    #[test]
    fn a_hurt_body_may_not_pillar_but_may_climb_out_of_a_shelter() {
        // Run 9 review item 1: the health rule kept a hurt body in its hole.
        scaffold::reset();
        let mut world = floor_world();
        set_health(&mut world, 6.0);
        let mut rig = Rig::new(world);
        let mut outcome = None;
        for _ in 0..PREPARE_TICKS {
            let (frame, _, out) = rig.run();
            assert!(!frame.held.jump, "no free-standing climb at six health");
            if out.is_some() {
                outcome = out;
                break;
            }
        }
        assert_eq!(outcome, Some(Outcome::Failed("unsafe".into())));

        let mut world = floor_world();
        set_health(&mut world, 6.0);
        let mut rig = Rig::new(world);
        rig.pillar = Pillar::climbing_out("cobblestone".into());
        let mut jumped = false;
        for tick in 0..PREPARE_TICKS {
            let (frame, _, out) = rig.run();
            assert!(out.is_none(), "tick {tick}: {out:?}");
            rig.body.step(&frame.held, &rig.world);
            if frame.held.jump {
                jumped = true;
                break;
            }
        }
        assert!(jumped, "climbing out ignores the health rule");
        scaffold::reset();
    }

    #[test]
    fn a_column_of_three_scaffold_blocks_is_the_limit() {
        scaffold::reset();
        let world = floor_world();
        for y in 61..=63 {
            scaffold::note(BlockPos::new(0, y, 0));
        }
        let mut rig = Rig::new(world);
        let mut outcome = None;
        for _ in 0..PREPARE_TICKS {
            let (_, _, out) = rig.run();
            if out.is_some() {
                outcome = out;
                break;
            }
        }
        assert_eq!(outcome, Some(Outcome::Failed("too_high".into())));
        scaffold::reset();
    }

    #[test]
    fn standing_in_short_grass_the_body_still_clicks_and_places_in_its_feet_cell() {
        // Seen live on 2026-11-09: the feet cell held a plant, the ray met it
        // instead of the support's top face, and the call failed `no_click`.
        scaffold::reset();
        let mut world = floor_world();
        let cell = feet().to_block_pos_floor();
        set_block(&mut world, cell, BlockState::from(BlockKind::ShortGrass));
        let mut rig = Rig::new(world);
        let mut spec = None;
        for tick in 0..40 {
            let (frame, pending, outcome) = rig.run();
            assert!(outcome.is_none(), "tick {tick}: {outcome:?}");
            rig.body.step(&frame.held, &rig.world);
            if frame.clicks.contains(&Click::Use) {
                spec = pending;
                break;
            }
        }
        match spec {
            Some(IntentSpec::Place { pos, .. }) => assert_eq!(pos, cell),
            other => panic!("expected one place click into the feet cell, got {other:?}"),
        }
        scaffold::reset();
    }

    #[test]
    fn a_flower_in_the_feet_cell_is_refused_before_the_jump() {
        scaffold::reset();
        let mut world = floor_world();
        let cell = feet().to_block_pos_floor();
        set_block(&mut world, cell, BlockState::from(BlockKind::Dandelion));
        let mut rig = Rig::new(world);
        let mut outcome = None;
        for _ in 0..40 {
            let (frame, _, result) = rig.run();
            assert!(!frame.held.jump, "no jump into a cell a click cannot replace");
            rig.body.step(&frame.held, &rig.world);
            if result.is_some() {
                outcome = result;
                break;
            }
        }
        assert_eq!(outcome, Some(Outcome::Failed("no_headroom".into())));
        scaffold::reset();
    }

    #[test]
    fn on_flat_ground_the_body_looks_down_jumps_and_clicks_once_with_a_place_receipt() {
        scaffold::reset();
        let world = floor_world();
        let mut rig = Rig::new(world);
        let mut jumped_at = None;
        let mut clicks = Vec::new();
        let mut spec = None;
        for tick in 0..40 {
            let (frame, pending, outcome) = rig.run();
            assert!(outcome.is_none(), "tick {tick}: {outcome:?}");
            if frame.held.jump && jumped_at.is_none() {
                jumped_at = Some(tick);
            }
            if frame.clicks.contains(&Click::Use) {
                clicks.push(tick);
                spec = pending;
            }
            // The test steps the physics (the driver would).
            rig.body.step(&frame.held, &rig.world);
            if !clicks.is_empty() {
                break;
            }
        }
        let jumped_at = jumped_at.expect("the body jumped");
        assert_eq!(clicks.len(), 1, "one click");
        assert!(
            (3..=8).contains(&(clicks[0] - jumped_at)),
            "the click goes out inside the measured window: jump {jumped_at}, click {:?}",
            clicks
        );
        assert_eq!(
            spec,
            Some(IntentSpec::Place {
                pos: BlockPos::new(0, 64, 0),
                block: "cobblestone".to_owned(),
                window: 40,
            })
        );
        assert!(
            !scaffold::is_scaffold(BlockPos::new(0, 64, 0)),
            "a click is not a placed block: the cell waits for the receipt"
        );
        scaffold::reset();
    }

    #[test]
    fn the_cell_becomes_scaffolding_only_when_the_place_receipt_settles() {
        // Review finding 3: noting the cell when the click goes out meant a
        // placement the server refused stayed in the set forever, hiding a real
        // block from every target search and counting a column that does not
        // exist. The runner marks the cell on the confirmed receipt instead.
        use crate::runner::Runner;
        use praxion_interpreter::StartAction;
        use praxion_rules::ArgValue;

        scaffold::reset();
        let cell = BlockPos::new(0, 64, 0);
        let world = floor_world();
        let mut body = settled_body();
        for _ in 0..5 {
            body.step(&praxion_headless::Held::NONE, &world);
        }
        let mut perception = swept(&world);
        let mut runner = Runner::new();
        runner
            .start(&StartAction {
                primitive: "pillar".into(),
                args: vec![ArgValue::Name("cobblestone".into())],
            })
            .expect("pillar starts");

        let mut clicked = false;
        for tick in 0..40 {
            let (frame, pending) = {
                let mut ctx = Ctx {
                    tick,
                    body: &body,
                    world: &world,
                    perception: &mut perception,
                    style: Personality::default(),
                };
                let result = runner.tick(&mut ctx);
                (result.frame, result.pending)
            };
            frame
                .apply_with_menu(&mut body, tick)
                .expect("pillar frame applies");
            body.step(&frame.held, &world);
            if let Some(IntentSpec::Place { pos, .. }) = pending {
                assert_eq!(pos, cell);
                clicked = true;
                runner.registered(1);
                break;
            }
        }
        assert!(clicked, "the pillar clicked");
        assert!(
            !scaffold::is_scaffold(cell),
            "the click alone never marks the cell"
        );
        runner.settle(Outcome::Confirmed);
        assert!(
            scaffold::is_scaffold(cell),
            "the confirmed place receipt marks the cell"
        );
        scaffold::reset();
    }

    #[test]
    fn the_call_budget_outlasts_two_jumps_and_the_receipt() {
        // Review finding 6: `CALL_TICKS` was 110 while the stages allow
        // 45 + 2*40 + 2*14 = 153, so a valid second attempt could be killed as
        // `stuck` with its receipt still on the way.
        let worst = RECEIPT_TICKS + 2 * PREPARE_TICKS + 2 * AIR_TICKS;
        assert!(
            CALL_TICKS > worst,
            "CALL_TICKS {CALL_TICKS} must outlast the worst case {worst}"
        );
    }

    #[test]
    fn a_jump_whose_window_never_clears_the_cell_fails_no_click_not_no_place() {
        // Review finding 6: a click always moves the call to `Receipt`, so an
        // air window that ends without one means nothing was ever placed.
        scaffold::reset();
        let world = floor_world();
        let mut rig = Rig::new(world);
        // The second jump's window, on the ground, never clears the cell (no
        // jump is left to make): the pick never gets a chance to click.
        rig.pillar.stage = Stage::Air;
        rig.pillar.stage_ticks = 0;
        rig.pillar.jumps = MAX_JUMPS;
        rig.pillar.cell = Some(BlockPos::new(0, 64, 0));
        let mut outcome = None;
        for _ in 0..AIR_TICKS + 2 {
            let (frame, _, out) = rig.run();
            assert!(frame.clicks.is_empty(), "no click without clearing the cell");
            if out.is_some() {
                outcome = out;
                break;
            }
        }
        assert_eq!(outcome, Some(Outcome::Failed("no_click".into())));
    }
}
