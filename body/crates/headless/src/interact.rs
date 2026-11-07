//! The vanilla client's interaction logic, ported from `azalea-client`.
//!
//! One [`ControlFrame`] becomes the packets a vanilla client would send for the
//! same held keys and mouse buttons:
//!
//! | Frame | Packets | Ported from |
//! |---|---|---|
//! | `clicks: [Attack]`, entity in range | `ServerboundAttack`, `ServerboundPunch` | `attack.rs:49-100` |
//! | `clicks: [Attack]`, block in range | `ServerboundPlayerAction::{Start,Stop,Abort}DestroyBlock`, `ServerboundPunch` | `mining.rs:216-388, 540-568, 579-708` |
//! | `clicks: [Use]`, block in range | `ServerboundUseItemOn` | `interact/mod.rs:211-293` |
//! | `clicks: [Use]`, no block | `ServerboundUseItem`, `ServerboundPlayerAction::ReleaseUseItem` | `interact/mod.rs:211-293` |
//! | `select_slot` | `ServerboundSetCarriedItem` | `inventory/mod.rs:287-309` |
//! | `menu` | `ServerboundContainerClick` with the simulated change set | `inventory/mod.rs:167-225` |
//! | `menu_mode: Close` | `ServerboundContainerClose` | `inventory/mod.rs:91-127` |
//!
//! The mining progress itself is [`crate::physics::Body::mine_progress`], a port
//! of `azalea-entity/src/mining.rs`. The raycast is the block traversal of
//! `azalea-physics/src/clip.rs:92-251` over this body's [`Blocks`] view, plus the
//! entity pick of `azalea-client/src/plugins/interact/pick.rs:256-303`.

use azalea_core::{
    direction::Direction,
    game_type::GameMode,
    hit_result::BlockHitResult,
    math::{self, EPSILON},
    position::{BlockPos, Vec3},
};
use azalea_entity::{Pose, PlayerAbilities, dimensions::calculate_dimensions, view_vector};
use azalea_inventory::{
    ItemStack, Menu,
    operations::{ClickOperation, PickupClick, QuickMoveClick, SwapClick, ThrowClick},
};
use azalea_physics::collision::BlockWithShape;
use azalea_protocol::packets::Packet;
use azalea_protocol::packets::game::{
    ServerboundAttack, ServerboundContainerClick, ServerboundContainerClose, ServerboundGamePacket,
    ServerboundPunch, ServerboundSetCarriedItem, ServerboundUseItem,
    ServerboundUseItemOn,
    s_container_click::{HashedActualItem, HashedPatchMap, HashedStack},
    s_interact::InteractionHand,
    s_use_item_on::BlockHit,
};
use azalea_registry::builtin::EntityKind;

use crate::frame::{Click, ControlFrame, MenuAction, MenuMode};
use crate::physics::{Blocks, Body};

/// One entity the crosshair is over.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityPick {
    pub id: i32,
    pub kind: EntityKind,
    pub position: Vec3,
    pub location: Vec3,
}

/// What the crosshair is over.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pick {
    pub block: Option<BlockHitResult>,
    pub entity: Option<EntityPick>,
    /// True when the entity hit is closer than the block hit.
    pub entity_closer: bool,
}

/// The actuator's state between ticks: mining progress, cooldowns and the last
/// selected slot. The body itself stays in [`Body`].
#[derive(Clone, Debug, Default)]
pub struct ActuatorState {
    prediction_seq: u32,
    mining: Option<Mining>,
    last_use: bool,
    using_item: bool,
    attack_cooldown: u32,
    last_selected_slot: Option<u8>,
    /// Ticks before a new destroy may start after a finished one (vanilla's
    /// `mine_delay = 5`, `mining.rs:690`).
    mine_delay: u32,
}

#[derive(Clone, Debug)]
struct Mining {
    pos: BlockPos,
    direction: Direction,
    progress: f32,
    item: ItemStack,
    ticks: u32,
    /// Ticks spent at or above 1.0, waiting for the server's progress to catch
    /// up before the stop packet.
    finishing: u32,
}

/// A packet the actuator wants to send. Most are ordinary protocol packets;
/// `PlayerAction` carries the server's 26.3 action ordinal because the pinned
/// azalea enum lacks `ChangeDestroyDirection` and would shift abort/stop/release.
#[derive(Clone, Debug, PartialEq)]
pub enum Outgoing {
    Packet(ServerboundGamePacket),
    PlayerAction {
        /// The server's `ServerboundPlayerActionPacket$Action` ordinal:
        /// 0 start, 2 abort, 3 stop, 6 release-use-item.
        action: u8,
        pos: BlockPos,
        direction: Direction,
        seq: u32,
    },
}

/// What the actuator reported for one applied frame, for receipts attribution.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AppliedReport {
    pub clicks: usize,
    /// The prediction sequence number of a destroy start or stop.
    pub block_seq: Option<u32>,
    /// The block a destroy start/stop or a use-item-on named.
    pub block_hit: Option<BlockPos>,
    /// The face the destroy start/stop or use-item-on named.
    pub block_face: Option<Direction>,
    /// The cell an action may change and its state just before the action was
    /// planned. A receipt uses this instead of a snapshot taken when it is
    /// registered, because the server's update can arrive in the same tick
    /// batch as the registration.
    pub block_before: Option<(BlockPos, Option<azalea_block::BlockState>)>,
    /// The state id of a container click.
    pub menu_state_id: Option<u32>,
    /// The window a container click or close named.
    pub container_id: Option<i32>,
    /// The entity id an attack packet named.
    pub attack_entity: Option<i32>,
}

impl ActuatorState {
    fn next_seq(&mut self) -> u32 {
        self.prediction_seq = self.prediction_seq.wrapping_add(1);
        self.prediction_seq
    }
}

/// The item in the selected hotbar slot (the main hand), not the cursor stack.
pub fn held_item(world: &praxion_world::World) -> ItemStack {
    let slot = world.selected_hotbar_slot().unwrap_or(0) as usize;
    world
        .inventory()
        .get(36 + slot)
        .cloned()
        .unwrap_or(ItemStack::Empty)
}

/// The block or entity a vanilla client's crosshair would be over.
pub fn pick(world: &praxion_world::World, body: &Body) -> Pick {
    let eye = body.eye_position();
    let look = body.look();
    let block_range = body.block_interaction_range();
    let entity_range = body.entity_interaction_range();
    let block = pick_block(world, eye, look, block_range);
    let max_range = block_range.max(entity_range);
    let end = eye + view_vector(look) * max_range;
    let entity = pick_entity(world, eye, end, entity_range * entity_range);
    let entity_closer = match (&block, &entity) {
        (_, None) => false,
        (None, Some(_)) => true,
        (Some(block), Some(entity)) => {
            entity.location.distance_squared_to(eye) < block.location.distance_squared_to(eye)
        }
    };
    Pick {
        block,
        entity,
        entity_closer,
    }
}

/// `azalea-client/src/plugins/interact/pick.rs:225-243` (`pick_block`) with the
/// traversal of `azalea-physics/src/clip.rs:92-251`.
fn pick_block(
    world: &dyn Blocks,
    eye: Vec3,
    look: azalea_entity::LookDirection,
    range: f64,
) -> Option<BlockHitResult> {
    let end = eye + view_vector(look) * range;
    traverse_blocks(eye, end, |pos| {
        let state = world.block(pos)?;
        if state.is_air() {
            return None;
        }
        state.outline_shape(pos).clip(eye, end, pos)
    })
}

/// The block traversal of `azalea-physics/src/clip.rs:162-251`, over any
/// [`Blocks`] source.
fn traverse_blocks<T>(
    from: Vec3,
    to: Vec3,
    mut hit: impl FnMut(BlockPos) -> Option<T>,
) -> Option<T> {
    if from == to {
        return None;
    }
    let right_after_end = Vec3 {
        x: math::lerp(-EPSILON, to.x, from.x),
        y: math::lerp(-EPSILON, to.y, from.y),
        z: math::lerp(-EPSILON, to.z, from.z),
    };
    let right_before_start = Vec3 {
        x: math::lerp(-EPSILON, from.x, to.x),
        y: math::lerp(-EPSILON, from.y, to.y),
        z: math::lerp(-EPSILON, from.z, to.z),
    };
    let mut current_block = BlockPos::from(right_before_start);
    if let Some(data) = hit(current_block) {
        return Some(data);
    }
    let vec = right_after_end - right_before_start;
    let vec_sign = Vec3 {
        x: math::sign(vec.x),
        y: math::sign(vec.y),
        z: math::sign(vec.z),
    };
    let percentage_step = Vec3 {
        x: if vec_sign.x == 0. {
            f64::MAX
        } else {
            vec_sign.x / vec.x
        },
        y: if vec_sign.y == 0. {
            f64::MAX
        } else {
            vec_sign.y / vec.y
        },
        z: if vec_sign.z == 0. {
            f64::MAX
        } else {
            vec_sign.z / vec.z
        },
    };
    let mut percentage = Vec3 {
        x: percentage_step.x
            * if vec_sign.x > 0. {
                1. - math::fract(right_before_start.x)
            } else {
                math::fract(right_before_start.x)
            },
        y: percentage_step.y
            * if vec_sign.y > 0. {
                1. - math::fract(right_before_start.y)
            } else {
                math::fract(right_before_start.y)
            },
        z: percentage_step.z
            * if vec_sign.z > 0. {
                1. - math::fract(right_before_start.z)
            } else {
                math::fract(right_before_start.z)
            },
    };
    loop {
        if percentage.x > 1. && percentage.y > 1. && percentage.z > 1. {
            return None;
        }
        if percentage.x < percentage.y {
            if percentage.x < percentage.z {
                current_block.x += vec_sign.x as i32;
                percentage.x += percentage_step.x;
            } else {
                current_block.z += vec_sign.z as i32;
                percentage.z += percentage_step.z;
            }
        } else if percentage.y < percentage.z {
            current_block.y += vec_sign.y as i32;
            percentage.y += percentage_step.y;
        } else {
            current_block.z += vec_sign.z as i32;
            percentage.z += percentage_step.z;
        }
        if let Some(data) = hit(current_block) {
            return Some(data);
        }
    }
}

/// `azalea-client/src/plugins/interact/pick.rs:256-303` (`pick_entity`),
/// simplified to a ray against each entity's standing bounding box.
fn pick_entity(
    world: &praxion_world::World,
    eye: Vec3,
    end: Vec3,
    range_sq: f64,
) -> Option<EntityPick> {
    let mut best = range_sq;
    let mut result = None;
    for entity in world.entities() {
        let dimensions = calculate_dimensions(entity.kind(), Pose::Standing);
        let aabb = dimensions.make_bounding_box(entity.position());
        if let Some(location) = aabb.clip(eye, end) {
            let distance = eye.distance_squared_to(location);
            if distance < best {
                best = distance;
                result = Some(EntityPick {
                    id: entity.id(),
                    kind: entity.kind(),
                    position: entity.position(),
                    location,
                });
            }
        }
    }
    result
}

/// Turns one frame into the packets a vanilla client would send.
pub fn plan(
    state: &mut ActuatorState,
    body: &Body,
    world: &praxion_world::World,
    frame: &ControlFrame,
    menu_state_id: u32,
) -> (Vec<Outgoing>, AppliedReport) {
    let mut packets: Vec<Outgoing> = Vec::new();
    let mut report = AppliedReport {
        clicks: frame.clicks.len(),
        ..AppliedReport::default()
    };
    state.attack_cooldown = state.attack_cooldown.saturating_add(1);
    state.mine_delay = state.mine_delay.saturating_sub(1);

    let attack = frame.clicks.contains(&Click::Attack);
    let use_click = frame.clicks.contains(&Click::Use);
    let pick = pick(world, body);
    let game_mode = world.game_mode();
    let creative = game_mode == Some(GameMode::Creative);

    // ------------------------------------------------------------ attack
    if !attack {
        if state.mining.take().is_some() {
            abort_destroy(state, &mut packets);
        }
    } else if pick.entity_closer {
        if let Some(entity) = &pick.entity {
            if state.mining.take().is_some() {
                abort_destroy(state, &mut packets);
            }
            if state.attack_cooldown >= body.attack_cooldown_ticks() {
                packets.push(Outgoing::Packet(
                    ServerboundAttack {
                        entity_id: azalea_core::entity_id::MinecraftEntityId(entity.id),
                    }
                    .into_variant(),
                ));
                packets.push(Outgoing::Packet(ServerboundPunch.into_variant()));
                report.attack_entity = Some(entity.id);
                state.attack_cooldown = 0;
            }
        }
    } else if let Some(block) = &pick.block {
        let target = block.block_pos;
        let mut keep_mining = false;
        // The sequence number for a stop this tick. Taking it before the mining
        // borrow keeps the borrow checker happy; an unused number is harmless
        // (the server only needs acks to be monotonic).
        let stop_seq = if state.mining.is_some() {
            state.next_seq()
        } else {
            0
        };
        if let Some(mining) = &mut state.mining {
            let block_state = world.block(target);
            let changed_item = held_item(world) != mining.item;
            if mining.pos != target
                || block_state.is_none_or(|state| state.is_air())
                || changed_item
            {
                abort_destroy(state, &mut packets);
            } else {
                let progress = if creative {
                    1.
                } else {
                    body.mine_progress(block_state.expect("checked above"), &mining.item)
                };
                mining.progress += progress;
                mining.ticks += 1;
                if mining.progress >= 1. {
                    mining.finishing += 1;
                }
                if mining.ticks % 20 == 0 {
                    log::debug!(
                        "mine progress {:?} {:.4} ticks={} on_ground={} world={:?}",
                        mining.pos,
                        mining.progress,
                        mining.ticks,
                        body.on_ground(),
                        world.block(mining.pos)
                    );
                }
                if mining.progress >= 1. && mining.finishing >= 3 {
                    let seq = stop_seq;
                    log::debug!("mine stop {:?} seq={}", mining.pos, seq);
                    packets.push(Outgoing::PlayerAction {
                        action: 3,
                        pos: mining.pos,
                        direction: mining.direction,
                        seq,
                    });
                    packets.push(Outgoing::Packet(ServerboundPunch.into_variant()));
                    report.block_seq = Some(seq);
                    report.block_hit = Some(mining.pos);
                    report.block_face = Some(mining.direction);
                    report.block_before = Some((mining.pos, world.block(mining.pos)));
                    state.mining = None;
                    state.mine_delay = 5;
                } else {
                    // Vanilla swings every tick while it mines
                    // (`mining.rs:698`).
                    packets.push(Outgoing::Packet(ServerboundPunch.into_variant()));
                    keep_mining = true;
                }
            }
        }
        if !keep_mining && state.mining.is_none() && state.mine_delay == 0 {
            let seq = state.next_seq();
            log::debug!("mine start {:?} seq={}", target, seq);
            packets.push(Outgoing::PlayerAction {
                action: 0,
                pos: target,
                direction: block.direction,
                seq,
            });
            packets.push(Outgoing::Packet(ServerboundPunch.into_variant()));
            report.block_seq = Some(seq);
            report.block_hit = Some(target);
            report.block_face = Some(block.direction);
            report.block_before = Some((target, world.block(target)));
            let block_state = world.block(target);
            let progress = if creative {
                1.
            } else {
                block_state
                    .map(|state| body.mine_progress(state, &held_item(world)))
                    .unwrap_or(0.)
            };
            if progress < 1. {
                state.mining = Some(Mining {
                    pos: target,
                    direction: block.direction,
                    progress,
                    item: held_item(world),
                    ticks: 0,
                    finishing: 0,
                });
            }
        }
    } else if state.mining.take().is_some() {
        abort_destroy(state, &mut packets);
    }

    // --------------------------------------------------------------- use
    if use_click && !state.last_use {
        if let Some(block) = &pick.block {
            let seq = state.next_seq();
            packets.push(Outgoing::Packet(
                ServerboundUseItemOn {
                    hand: InteractionHand::MainHand,
                    block_hit: BlockHit::from(block),
                    seq,
                }
                .into_variant(),
            ));
            report.block_seq = Some(seq);
            report.block_hit = Some(block.block_pos);
            report.block_face = Some(block.direction);
            let placed = block.block_pos.offset_with_direction(block.direction);
            report.block_before = Some((placed, world.block(placed)));
        } else {
            let seq = state.next_seq();
            let (yaw, pitch): (f32, f32) = body.look().into();
            packets.push(Outgoing::Packet(
                ServerboundUseItem {
                    hand: InteractionHand::MainHand,
                    seq,
                    y_rot: yaw,
                    x_rot: pitch,
                }
                .into_variant(),
            ));
            state.using_item = true;
        }
    } else if !use_click && state.last_use && state.using_item {
        // `releaseUsingItem` (`ServerboundPlayerAction::ReleaseUseItem`).
        packets.push(Outgoing::PlayerAction {
            action: 6,
            pos: BlockPos::ZERO,
            direction: Direction::Down,
            seq: 0,
        });
        state.using_item = false;
    }
    state.last_use = use_click;

    // ------------------------------------------------------- select slot
    if let Some(slot) = frame.select_slot
        && state.last_selected_slot != Some(slot)
    {
        packets.push(Outgoing::Packet(
            ServerboundSetCarriedItem { slot: u16::from(slot) }.into_variant(),
        ));
        state.last_selected_slot = Some(slot);
    }

    // -------------------------------------------------------------- menu
    if let Some(action) = frame.menu {
        let container_id = action.window as i32;
        report.container_id = Some(container_id);
        match frame.menu_mode {
            MenuMode::Close => {
                packets.push(Outgoing::Packet(
                    ServerboundContainerClose { container_id }.into_variant(),
                ));
            }
            mode => {
                if let Some(packet) = container_click(world, action, mode, menu_state_id) {
                    packets.push(Outgoing::Packet(packet));
                    report.menu_state_id = Some(menu_state_id);
                }
            }
        }
    }

    (packets, report)
}

fn abort_destroy(state: &mut ActuatorState, packets: &mut Vec<Outgoing>) {
    let Some(mining) = state.mining.take() else {
        return;
    };
    // `mining.rs:540-568`: abort uses sequence 0.
    packets.push(Outgoing::PlayerAction {
        action: 2,
        pos: mining.pos,
        direction: Direction::Down,
        seq: 0,
    });
}

/// `inventory/mod.rs:167-225`: simulate the click on the current menu, diff the
/// slots and send the changed set with the window's state id.
fn container_click(
    world: &praxion_world::World,
    action: MenuAction,
    mode: MenuMode,
    state_id: u32,
) -> Option<ServerboundGamePacket> {
    let mut inventory = inventory_from(world, action.window as i32, state_id)?;
    let operation = click_operation(&action, mode)?;
    let changed_slots = changed_slots(&mut inventory, &operation);
    Some(
        ServerboundContainerClick {
            container_id: action.window as i32,
            state_id,
            slot_num: operation
                .slot_num()
                .map(|slot| slot as i16)
                .unwrap_or(-999),
            button_num: operation.button_num(),
            click_type: operation.click_type(),
            changed_slots,
            carried_item: hashed_stack(&inventory.carried),
        }
        .into_variant(),
    )
}

/// `inventory/mod.rs:187-207`: simulate the click and diff the menu slots. The
/// result is exactly the change set azalea-client sends.
fn changed_slots(
    inventory: &mut azalea_entity::inventory::Inventory,
    operation: &ClickOperation,
) -> indexmap::IndexMap<u16, HashedStack> {
    let old_slots = inventory.menu().slots();
    inventory.simulate_click(operation, &PlayerAbilities::default());
    let new_slots = inventory.menu().slots();
    let mut changed_slots = indexmap::IndexMap::new();
    for (index, (old, new)) in old_slots.iter().zip(new_slots.iter()).enumerate() {
        if old != new {
            changed_slots.insert(index as u16, hashed_stack(new));
        }
    }
    changed_slots
}

fn click_operation(action: &MenuAction, mode: MenuMode) -> Option<ClickOperation> {
    let slot = u16::try_from(action.slot).ok();
    Some(match mode {
        MenuMode::Click => match action.button {
            0 => PickupClick::Left { slot }.into(),
            1 => PickupClick::Right { slot }.into(),
            _ => return None,
        },
        MenuMode::ShiftClick => QuickMoveClick::Left { slot: slot? }.into(),
        MenuMode::Swap => SwapClick {
            source_slot: slot?,
            target_slot: u8::try_from(action.button).ok()?,
        }
        .into(),
        MenuMode::Throw => match action.button {
            0 => ThrowClick::Single { slot: slot? }.into(),
            1 => ThrowClick::All { slot: slot? }.into(),
            _ => return None,
        },
        MenuMode::Clone => azalea_inventory::operations::CloneClick { slot: slot? }.into(),
        MenuMode::Close => return None,
    })
}

/// Rebuilds the client's `Inventory` from the raw store so azalea's own
/// `simulate_click` produces the same change set the vanilla client sends.
fn inventory_from(
    world: &praxion_world::World,
    window: i32,
    state_id: u32,
) -> Option<azalea_entity::inventory::Inventory> {
    let mut inventory = azalea_entity::inventory::Inventory::default();
    inventory.id = window;
    inventory.state_id = state_id;
    inventory.selected_hotbar_slot = world.selected_hotbar_slot().unwrap_or(0) as u8;
    inventory.carried = world.carried_item().clone();
    for (index, stack) in world.inventory().iter().enumerate() {
        if let Some(slot) = inventory.inventory_menu.slot_mut(index) {
            *slot = stack.clone();
        }
    }
    if window != 0 {
        let container = world.open_container()?;
        if container.id() != window {
            return None;
        }
        let mut menu = Menu::from_kind(container.kind());
        for (index, stack) in container.slots().iter().enumerate() {
            if let Some(slot) = menu.slot_mut(index) {
                *slot = stack.clone();
            }
        }
        inventory.container_menu = Some(menu);
    }
    Some(inventory)
}

/// The protocol's `HashedStack`. The 26.3 packet hashes data components; the
/// raw store does not keep them, so the components are sent as empty. Items
/// without components (the first-night items) hash exactly.
fn hashed_stack(item: &ItemStack) -> HashedStack {
    match item {
        ItemStack::Empty => HashedStack(None),
        ItemStack::Present(data) => HashedStack(Some(HashedActualItem {
            kind: data.kind,
            count: data.count,
            components: HashedPatchMap {
                added_components: Vec::new(),
                removed_components: Vec::new(),
            },
        })),
    }
}

#[cfg(test)]
mod tests {
    use azalea_block::BlockState;
    use azalea_entity::inventory::Inventory;
    use azalea_inventory::{Menu, SlotList};
    use azalea_registry::builtin::{BlockKind, ItemKind};

    use super::*;

    struct Floor;

    impl Blocks for Floor {
        fn block(&self, pos: BlockPos) -> Option<BlockState> {
            (pos.y == 63).then_some(BlockState::from(azalea_block::blocks::Stone))
        }
    }

    fn grounded_body() -> Body {
        let mut body = Body::new(Vec3::new(0.5, 64.0, 0.5));
        for _ in 0..20 {
            body.step(&crate::frame::Held::NONE, &Floor);
        }
        assert!(body.on_ground());
        body
    }

    #[test]
    fn destroy_progress_matches_the_vanilla_values() {
        let body = grounded_body();
        let hand = ItemStack::Empty;

        // Dirt: hardness 0.5, no correct tool needed, divisor 30 -> 15 ticks.
        let dirt = BlockState::from(BlockKind::Dirt);
        let progress = body.mine_progress(dirt, &hand);
        assert!(
            (progress * 15.0 - 1.0).abs() < 1.0e-4,
            "dirt by hand: {progress} per tick"
        );

        // Oak log: hardness 2, no correct tool needed -> 60 ticks.
        let log = BlockState::from(BlockKind::OakLog);
        let progress = body.mine_progress(log, &hand);
        assert!(
            (progress * 60.0 - 1.0).abs() < 1.0e-4,
            "oak log by hand: {progress} per tick"
        );

        // Stone: hardness 1.5, wooden pickaxe speed 2, correct tool divisor 30
        // -> 23 ticks (the last tick crosses 1).
        let stone = BlockState::from(azalea_block::blocks::Stone);
        let pickaxe = ItemStack::new(ItemKind::WoodenPickaxe, 1);
        let progress = body.mine_progress(stone, &pickaxe);
        assert!(
            (progress * 22.5 - 1.0).abs() < 1.0e-4,
            "stone with a wooden pickaxe: {progress} per tick"
        );
        assert!(progress * 23.0 >= 1.0);
        assert!(progress * 22.0 < 1.0);
    }

    #[test]
    fn the_traversal_hits_the_face_it_looks_at() {
        struct OneBlock(BlockPos);
        impl Blocks for OneBlock {
            fn block(&self, pos: BlockPos) -> Option<BlockState> {
                (pos == self.0).then_some(BlockState::from(azalea_block::blocks::Stone))
            }
        }
        let world = OneBlock(BlockPos::new(0, 0, 2));
        let hit = pick_block(
            &world,
            Vec3::new(0.5, 0.5, 0.5),
            azalea_entity::LookDirection::new(0., 0.),
            4.5,
        )
        .expect("a hit");
        assert_eq!(hit.block_pos, BlockPos::new(0, 0, 2));
        assert_eq!(hit.direction, Direction::North);
    }

    #[test]
    fn a_miss_returns_nothing() {
        struct Empty;
        impl Blocks for Empty {
            fn block(&self, _pos: BlockPos) -> Option<BlockState> {
                None
            }
        }
        assert!(
            pick_block(
                &Empty,
                Vec3::new(0.5, 0.5, 0.5),
                azalea_entity::LookDirection::new(0., 0.),
                4.5,
            )
            .is_none()
        );
    }

    #[test]
    fn a_two_by_two_craft_change_set_matches_azalea() {
        let mut inventory = Inventory::default();
        *inventory.inventory_menu.slot_mut(9).unwrap() = ItemStack::new(ItemKind::OakPlanks, 4);
        let mut changed = indexmap::IndexMap::new();
        for operation in [
            PickupClick::Left { slot: Some(9) }.into(),
            PickupClick::Right { slot: Some(1) }.into(),
            PickupClick::Right { slot: Some(2) }.into(),
            PickupClick::Right { slot: Some(3) }.into(),
            PickupClick::Right { slot: Some(4) }.into(),
            PickupClick::Left { slot: Some(9) }.into(),
        ] {
            changed.extend(changed_slots(&mut inventory, &operation));
        }
        for slot in [1, 2, 3, 4, 9] {
            assert!(changed.contains_key(&slot), "slot {slot} did not change");
        }
        for slot in 1..=4 {
            let stack = inventory.inventory_menu.slot(slot).unwrap();
            assert_eq!(stack.kind(), ItemKind::OakPlanks);
            assert_eq!(stack.count(), 1);
        }
        assert!(inventory.carried.is_empty());
    }

    #[test]
    fn a_three_by_three_craft_change_set_matches_azalea() {
        let mut inventory = Inventory {
            id: 1,
            container_menu: Some(Menu::Crafting {
                result: ItemStack::Empty,
                grid: SlotList::default(),
                player: SlotList::default(),
            }),
            ..Inventory::default()
        };
        let menu = inventory.container_menu.as_mut().unwrap();
        *menu.slot_mut(10).unwrap() = ItemStack::new(ItemKind::OakPlanks, 3);
        *menu.slot_mut(11).unwrap() = ItemStack::new(ItemKind::Stick, 2);

        let mut changed = indexmap::IndexMap::new();
        for operation in [
            PickupClick::Left { slot: Some(10) }.into(),
            PickupClick::Right { slot: Some(1) }.into(),
            PickupClick::Right { slot: Some(2) }.into(),
            PickupClick::Right { slot: Some(3) }.into(),
            PickupClick::Left { slot: Some(10) }.into(),
            PickupClick::Left { slot: Some(11) }.into(),
            PickupClick::Right { slot: Some(5) }.into(),
            PickupClick::Right { slot: Some(8) }.into(),
            PickupClick::Left { slot: Some(11) }.into(),
        ] {
            changed.extend(changed_slots(&mut inventory, &operation));
        }
        for slot in [1, 2, 3, 5, 8, 10, 11] {
            assert!(changed.contains_key(&slot), "slot {slot} did not change");
        }
        let menu = inventory.container_menu.as_ref().unwrap();
        for slot in [1, 2, 3] {
            assert_eq!(menu.slot(slot).unwrap().kind(), ItemKind::OakPlanks);
        }
        for slot in [5, 8] {
            assert_eq!(menu.slot(slot).unwrap().kind(), ItemKind::Stick);
        }
        assert!(inventory.carried.is_empty());
    }
}
