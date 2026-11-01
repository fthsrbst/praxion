//! Block classification for the planner: what can be walked through, stood
//! on, broken, or must be avoided.
//!
//! The collision shape comes from `azalea-physics` (its generated table is the
//! single source for shapes, so the planner and the physics agree). Hardness
//! and the default friction/jump behaviour come from `azalea-block`'s
//! generated `BlockBehavior`.
//!
//! TODO(game data): the tool class of a block (pickaxe / axe / shovel / none)
//! and the tier speeds are a small hand-written table below. The Mojang data
//! reports carry the `mineable/*` block tags; read them when `praxion-gamedata`
//! grows tag support. `praxion_gamedata::block_drops` is already the source
//! for whether a block has a loot table and for a named tool requirement; it
//! currently names no tool for the eight blocks it covers.

use azalea_block::{
    BlockState, BlockTrait,
    fluid_state::{FluidKind, FluidState},
};
use azalea_physics::collision::BlockWithShape;
use azalea_registry::builtin::BlockKind;

use crate::Inventory;

/// The tool family that speeds a block up.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolClass {
    Pickaxe,
    Axe,
    Shovel,
    None,
}

/// Blocks that hurt or cannot be occupied: lava and fire for their damage,
/// magma for its damage, cactus and sweet berry bushes for contact damage,
/// powder snow for sinking. The planner never walks into them and never digs a
/// block that would let a fluid reach the path.
pub fn is_hazard(state: BlockState) -> bool {
    matches!(
        BlockKind::from(state),
        BlockKind::Lava
            | BlockKind::Fire
            | BlockKind::SoulFire
            | BlockKind::MagmaBlock
            | BlockKind::Cactus
            | BlockKind::SweetBerryBush
            | BlockKind::PowderSnow
    )
}

/// The fluid at a cell, if any.
pub fn fluid(state: BlockState) -> Option<FluidKind> {
    let fluid = FluidState::from(state);
    (!fluid.is_empty()).then_some(fluid.kind)
}

/// A cell the player's body can occupy on land (feet or head). Water is not
/// passable on foot; it is swum, so [`is_water`] is checked separately.
pub fn is_passable(state: BlockState) -> bool {
    state.is_collision_shape_empty() && fluid(state).is_none() && !is_hazard(state)
}

/// A swimmable water cell: water, or a waterlogged plant with no collision
/// shape. A waterlogged solid (a stair, a slab) is not water a body can enter,
/// and lava is never water.
pub fn is_water(state: BlockState) -> bool {
    fluid(state) == Some(FluidKind::Water) && state.is_collision_shape_empty()
}

/// A block the player can stand on. Full cubes only: partial floors (slabs,
/// stairs, snow layers) are a known abstraction gap of this step, not a
/// physical claim. Real terrain paths are checked against this same rule.
pub fn is_solid(state: BlockState) -> bool {
    state.is_collision_shape_full() && fluid(state).is_none()
}

/// A block that can be removed by mining. Fluids and blocks with a negative
/// hardness (bedrock, barrier, …) are not.
pub fn is_breakable(state: BlockState) -> bool {
    if fluid(state).is_some() {
        return false;
    }
    hardness(state) >= 0.
}

/// A cell a block may be placed into, ignoring protection: air kinds and the
/// replaceable plants/snow of the vanilla `replaceable` tag that Demo-0 meets.
/// Torches, rails and other non-replaceable decorations are refused, which is
/// conservative.
pub fn is_replaceable(state: BlockState) -> bool {
    matches!(
        BlockKind::from(state),
        BlockKind::Air
            | BlockKind::CaveAir
            | BlockKind::VoidAir
            | BlockKind::ShortGrass
            | BlockKind::TallGrass
            | BlockKind::Fern
            | BlockKind::DeadBush
            | BlockKind::Snow
    )
}

/// The vanilla `destroy_time` (hardness) of a block, in seconds. Negative means
/// unbreakable.
pub fn hardness(state: BlockState) -> f32 {
    Block::behavior(state).destroy_time
}

/// The block's registry name, e.g. `stone` or `oak_log`.
pub fn name(state: BlockState) -> &'static str {
    Block::id(state)
}

/// The tool family of a block. `praxion-gamedata`'s loot table wins when it
/// names a required tool; otherwise the small table below applies. See the
/// module's TODO.
pub fn tool_class(state: BlockState) -> ToolClass {
    if let Some(tool) = required_tool(state) {
        if let Some((_, class)) = split_tool(tool) {
            return class;
        }
    }
    match BlockKind::from(state) {
        BlockKind::Stone
        | BlockKind::Cobblestone
        | BlockKind::Deepslate
        | BlockKind::IronOre
        | BlockKind::CoalOre
        | BlockKind::Obsidian => ToolClass::Pickaxe,
        BlockKind::Dirt
        | BlockKind::GrassBlock
        | BlockKind::Sand
        | BlockKind::Gravel
        | BlockKind::Clay => ToolClass::Shovel,
        BlockKind::OakLog
        | BlockKind::OakPlanks
        | BlockKind::BirchLog
        | BlockKind::SpruceLog => ToolClass::Axe,
        _ => ToolClass::None,
    }
}

/// The tool the block's 26.3 loot table names as required, when it names one.
/// The eight blocks `praxion-gamedata` covers currently name none, so this is
/// a no-op that starts working when the generated table grows.
pub fn required_tool(state: BlockState) -> Option<&'static str> {
    praxion_gamedata::block_drops(name(state))
        .and_then(|drops| drops.iter().find_map(|drop| drop.tool))
}

/// The speed of the best hotbar tool for this block. Hand speed is `1.0`;
/// the tier speeds are the vanilla damage multipliers, scaled to the
/// `hardness * 100 / speed` formula in [`dig_cost_ticks`]. See the TODO.
fn tool_speed(inventory: &Inventory, class: ToolClass) -> f32 {
    if class == ToolClass::None {
        return 1.;
    }
    inventory
        .hotbar
        .iter()
        .filter_map(|item| {
            let (tier, item_class) = split_tool(&item.item)?;
            (item_class == class).then_some(tier_speed(tier))
        })
        .fold(1., f32::max)
}

fn split_tool(item: &str) -> Option<(&str, ToolClass)> {
    for (suffix, class) in [
        ("_pickaxe", ToolClass::Pickaxe),
        ("_axe", ToolClass::Axe),
        ("_shovel", ToolClass::Shovel),
    ] {
        if let Some(tier) = item.strip_suffix(suffix) {
            return Some((tier, class));
        }
    }
    None
}

fn tier_speed(tier: &str) -> f32 {
    match tier {
        // Relative vanilla break speeds for a stone block (hand = 1).
        "wooden" => 6.5,
        "stone" => 12.5,
        "iron" => 19.,
        "diamond" => 25.,
        "netherite" => 29.,
        "golden" => 12.5,
        _ => 1.,
    }
}

/// The expected ticks a `mine` of `state` costs with the best hotbar tool.
/// `hardness * 100 / speed` is the vanilla break-time shape (hand speed 1);
/// the extra penalty is Azalea's `BLOCK_BREAK_ADDITIONAL_PENALTY`, applied by
/// the move, not here.
pub fn dig_cost_ticks(state: BlockState, inventory: &Inventory) -> f32 {
    let hardness = hardness(state);
    if hardness <= 0. {
        // Instant-break blocks (grass, flowers, torches) still cost a tick.
        return 1.;
    }
    let speed = tool_speed(inventory, tool_class(state));
    (hardness * 100. / speed).max(1.)
}

/// A small wrapper so `BlockTrait`'s object methods can be called without the
/// caller naming the box type.
struct Block;

impl Block {
    fn behavior(state: BlockState) -> azalea_block::BlockBehavior {
        Box::<dyn BlockTrait>::from(state).behavior()
    }

    fn id(state: BlockState) -> &'static str {
        let block: Box<dyn BlockTrait> = Box::<dyn BlockTrait>::from(state);
        block.id()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(kind: BlockKind) -> BlockState {
        BlockState::from(kind)
    }

    #[test]
    fn classification_follows_the_shapes_and_fluids() {
        assert!(is_passable(state(BlockKind::Air)));
        assert!(!is_passable(state(BlockKind::Stone)));
        assert!(is_solid(state(BlockKind::Stone)));
        assert!(!is_solid(state(BlockKind::Water)));
        assert_eq!(fluid(state(BlockKind::Lava)), Some(FluidKind::Lava));
        assert!(is_water(state(BlockKind::Water)));
        assert!(!is_water(state(BlockKind::Lava)));
        assert!(is_hazard(state(BlockKind::Lava)));
        assert!(!is_breakable(state(BlockKind::Bedrock)));
        assert!(is_breakable(state(BlockKind::Stone)));
    }

    #[test]
    fn the_loot_table_is_consulted_and_names_no_tool_yet() {
        assert!(praxion_gamedata::block_drops("stone").is_some());
        assert_eq!(required_tool(state(BlockKind::Stone)), None);
        assert_eq!(tool_class(state(BlockKind::Stone)), ToolClass::Pickaxe);
        assert_eq!(tool_class(state(BlockKind::Dirt)), ToolClass::Shovel);
        assert_eq!(tool_class(state(BlockKind::OakLog)), ToolClass::Axe);
        assert_eq!(tool_class(state(BlockKind::Glass)), ToolClass::None);
    }

    #[test]
    fn a_pickaxe_beats_a_hand_on_stone() {
        let hand = Inventory::empty();
        let iron = Inventory::with(&[("iron_pickaxe", 1)]);
        assert!(dig_cost_ticks(state(BlockKind::Stone), &iron)
            < dig_cost_ticks(state(BlockKind::Stone), &hand));
    }
}
