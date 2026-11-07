//! `place`: put one block against a visible support face.

use azalea_core::direction::Direction;
use azalea_core::position::{BlockPos, Vec3};
use praxion_headless::ControlFrame;
use praxion_interpreter::Outcome;
use praxion_perception::{BlockFilter, BlockRegion, Caller};
use praxion_planner::center;
use praxion_world::BlockState;

use crate::common::{HandSelect, Selecting, aim_precise, count_item, idle};
use crate::runner::{Ctx, IntentSpec};

#[derive(Debug)]
pub(crate) struct Place {
    pub(crate) block: String,
    pub(crate) target: Option<(BlockPos, Direction)>,
    pub(crate) sent: bool,
    pub(crate) waited: u32,
    pub(crate) hand: HandSelect,
    /// Supports whose face the crosshair could not reach; every later plan in
    /// this call skips them.
    pub(crate) rejected: Vec<BlockPos>,
    /// Attack clicks spent on a plant between the crosshair and the support.
    pub(crate) clear_clicks: u32,
}

impl Place {
    pub(crate) fn tick(
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
pub(crate) fn place_support(state: BlockState) -> bool {
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
pub(crate) fn cell_centre(pos: BlockPos) -> Vec3 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use azalea_registry::builtin::BlockKind;
    use praxion_headless::Body;

    fn air() -> BlockState {
        BlockState::from(BlockKind::Air)
    }
    fn stone() -> BlockState {
        BlockState::from(BlockKind::Stone)
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
        assert!(
            overlaps_body(&body, BlockPos::new(10, 65, 10)),
            "the head cell"
        );
        assert!(
            !overlaps_body(&body, BlockPos::new(10, 66, 10)),
            "the cell above the head"
        );
        // Vanilla's 1e-7 inflation: a box lying exactly on a face blocks, as
        // the floor under the feet does; a box 1e-6 away does not.
        assert!(
            overlaps_body(&body, BlockPos::new(10, 63, 10)),
            "the floor under the feet"
        );
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
    fn a_replaceable_block_is_not_a_place_support() {
        assert!(!place_support(air()));
        assert!(!place_support(BlockState::from(BlockKind::ShortGrass)));
        assert!(!place_support(BlockState::from(BlockKind::Snow)));
        assert!(place_support(stone()));
        assert!(place_support(BlockState::from(BlockKind::CraftingTable)));
    }
}
